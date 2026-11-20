//! "Job objects" as systemd transient scopes in `rustwtm.slice`: the first
//! process assigned creates the scope (StartTransientUnit with its PID),
//! later ones are attached to it (AttachProcessesToUnit); children follow
//! their parent's cgroup as they do a Windows job. CPU caps are cgroup
//! `cpu.max` (CPUQuota), memory limits `memory.max` (MemoryMax). Talks to
//! the system manager with `busctl`, so it needs root.
//!
//! cgroups have no per-process memory limit; the per-process and the
//! job-wide limit both become the scope's MemoryMax (the smaller wins).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::control::ProcessHandle;
use crate::linux::util::{run, run_checked};
use crate::{Error, Result};

/// Windows `JOB_OBJECT_LIMIT_*` bits, for [`Job::limit_flags`].
const LIMIT_PROCESS_MEMORY: u32 = 0x100;
const LIMIT_JOB_MEMORY: u32 = 0x200;
const LIMIT_BREAKAWAY_OK: u32 = 0x800;

#[derive(Debug, Default)]
struct Limits {
    cpu_percent: Option<f32>,
    process_memory: Option<u64>,
    job_memory: Option<u64>,
    created: bool,
}

#[derive(Debug)]
pub struct Job {
    unit: String,
    limits: Mutex<Limits>,
}

static NEXT: AtomicU32 = AtomicU32::new(1);

impl Job {
    pub fn create() -> Result<Self> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        Ok(Self {
            unit: format!("rustwtm-job-{}-{n}.scope", std::process::id()),
            limits: Mutex::new(Limits::default()),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Limits> {
        self.limits
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn limit_flags(&self) -> Result<u32> {
        let l = self.lock();
        let mut flags = LIMIT_BREAKAWAY_OK;
        if l.process_memory.is_some() {
            flags |= LIMIT_PROCESS_MEMORY;
        }
        if l.job_memory.is_some() {
            flags |= LIMIT_JOB_MEMORY;
        }
        Ok(flags)
    }

    fn quota_usec(percent: f32) -> u64 {
        let cpus = crate::system::logical_cpu_count() as f64;
        (f64::from(percent.clamp(0.01, 100.0)) / 100.0 * cpus * 1_000_000.0)
            .round()
            .max(1000.0) as u64
    }

    fn memory_max(l: &Limits) -> Option<u64> {
        match (l.process_memory, l.job_memory) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// The properties for `systemctl set-property`.
    fn properties(l: &Limits) -> Vec<String> {
        vec![
            match l.cpu_percent {
                Some(p) => format!("CPUQuota={}%", Self::quota_usec(p) / 10_000),
                None => "CPUQuota=".into(),
            },
            match Self::memory_max(l) {
                Some(b) => format!("MemoryMax={b}"),
                None => "MemoryMax=infinity".into(),
            },
        ]
    }

    fn apply(&self, l: &Limits) -> Result<()> {
        if !l.created {
            return Ok(());
        }
        let mut args = vec![
            "set-property".to_owned(),
            "--runtime".to_owned(),
            self.unit.clone(),
        ];
        args.extend(Self::properties(l));
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        run_checked("systemctl", &refs).map(drop)
    }

    pub fn assign(&self, process: &ProcessHandle) -> Result<()> {
        process.check()?;
        let pid = process.pid().to_string();
        let mut l = self.lock();
        if !l.created {
            // StartTransientUnit(name, mode, properties, aux).
            let mut args: Vec<String> = [
                "call",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "StartTransientUnit",
                "ssa(sv)a(sa(sv))",
                &self.unit,
                "fail",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect();
            let mut props: Vec<Vec<String>> = vec![
                vec!["PIDs".into(), "au".into(), "1".into(), pid.clone()],
                vec!["Slice".into(), "s".into(), "rustwtm.slice".into()],
                vec![
                    "Description".into(),
                    "s".into(),
                    "RustWTM application limit".into(),
                ],
            ];
            if let Some(p) = l.cpu_percent {
                props.push(vec![
                    "CPUQuotaPerSecUSec".into(),
                    "t".into(),
                    Self::quota_usec(p).to_string(),
                ]);
            }
            if let Some(m) = Self::memory_max(&l) {
                props.push(vec!["MemoryMax".into(), "t".into(), m.to_string()]);
            }
            args.push(props.len().to_string());
            for p in props {
                args.extend(p);
            }
            args.push("0".into());
            let refs: Vec<&str> = args.iter().map(String::as_str).collect();
            run_checked("busctl", &refs)?;
            l.created = true;
            return Ok(());
        }
        run_checked(
            "busctl",
            &[
                "call",
                "org.freedesktop.systemd1",
                "/org/freedesktop/systemd1",
                "org.freedesktop.systemd1.Manager",
                "AttachProcessesToUnit",
                "ssau",
                &self.unit,
                "",
                "1",
                &pid,
            ],
        )
        .map(drop)
    }

    /// Caps the job at `percent` of the whole machine (all CPUs).
    pub fn set_cpu_cap(&self, percent: Option<f32>) -> Result<()> {
        let mut l = self.lock();
        l.cpu_percent = percent;
        self.apply(&l)
    }

    pub fn set_process_memory_limit(&self, bytes: Option<u64>) -> Result<()> {
        let mut l = self.lock();
        l.process_memory = bytes;
        self.apply(&l)
    }

    pub fn set_job_memory_limit(&self, bytes: Option<u64>) -> Result<()> {
        let mut l = self.lock();
        l.job_memory = bytes;
        self.apply(&l)
    }

    pub fn contains(&self, process: &ProcessHandle) -> Result<bool> {
        let cgroup = std::fs::read_to_string(process.proc_path("cgroup"))
            .map_err(|e| Error::io("cgroup", e))?;
        Ok(cgroup
            .lines()
            .any(|l| l.ends_with(&format!("/{}", self.unit))))
    }

    #[cfg(test)]
    pub(crate) fn unit(&self) -> &str {
        &self.unit
    }
}

/// Whether systemd is the init system (job scopes need it).
#[allow(dead_code)]
pub(crate) fn systemd_running() -> bool {
    std::path::Path::new("/run/systemd/system").is_dir()
        && run("systemctl", &["is-system-running"]).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_limits() {
        let j = Job::create().unwrap();
        assert!(j.unit().starts_with("rustwtm-job-"));
        assert_eq!(j.limit_flags().unwrap(), LIMIT_BREAKAWAY_OK);
        j.set_cpu_cap(Some(50.0)).unwrap();
        j.set_job_memory_limit(Some(1 << 30)).unwrap();
        j.set_process_memory_limit(Some(1 << 29)).unwrap();
        let l = j.lock();
        assert_eq!(Job::memory_max(&l), Some(1 << 29));
        let props = Job::properties(&l);
        let cpus = crate::system::logical_cpu_count() as u64;
        assert_eq!(props[0], format!("CPUQuota={}%", 50 * cpus));
        let _ = systemd_running();
    }
}
