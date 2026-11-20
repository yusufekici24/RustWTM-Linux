//! System-wide process enumeration from /proc.
//!
//! Kernel threads (children of kthreadd) are folded into one `kthreadd`
//! entry, as Windows folds kernel work into "System", so their CPU time is
//! still counted.

use std::fs;

use crate::Result;
use crate::linux::util::{self, Stat};

/// One process as reported by the kernel. Times are in 100 ns units.
#[derive(Debug, Clone)]
pub struct RawProcess {
    pub pid: u32,
    pub parent_pid: u32,
    pub name: String,
    /// FILETIME (100 ns intervals since 1601-01-01 UTC).
    pub create_time: i64,
    pub user_time: u64,
    pub kernel_time: u64,
    pub cycle_time: u64,
    pub base_priority: i32,
    pub session_id: u32,
    pub thread_count: u32,
    pub handle_count: u32,
    pub working_set: u64,
    pub private_working_set: u64,
    /// Private committed bytes.
    pub private_bytes: u64,
    pub virtual_size: u64,
    pub page_faults: u32,
    pub peak_working_set: u64,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
    pub io_other_bytes: u64,
    pub io_reads: u64,
    pub io_writes: u64,
    pub io_other: u64,
    /// Stopped by SIGSTOP (RustWTM's suspend) or a debugger.
    pub suspended: bool,
}

/// One thread. `state` and `wait_reason` use the Windows codes the crates
/// above understand: 2 Running, 5 Waiting, 4 Terminated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ThreadInfo {
    pub tid: u32,
    pub state: u32,
    pub wait_reason: u32,
    pub context_switches: u32,
    pub create_time: i64,
    pub kernel_time: u64,
    pub user_time: u64,
    pub priority: i32,
    pub base_priority: i32,
    pub start_address: u64,
}

impl ThreadInfo {
    /// Waiting on something that may lead to another thread or process
    /// (uninterruptible sleep, `D`), rather than sleeping or idle.
    pub fn blocked(&self) -> bool {
        const IDLE: [u32; 5] = [4, 5, 11, 12, 15];
        self.state == 5 && !IDLE.contains(&self.wait_reason)
    }

    /// Stopped (`T`/`t`).
    pub fn suspended(&self) -> bool {
        self.state == 5 && matches!(self.wait_reason, 5 | 12)
    }
}

#[derive(Debug, Default)]
pub struct ProcessSnapshotter {
    last_count: usize,
}

/// kthreadd, the parent every kernel thread hangs off.
const KTHREADD: u32 = 2;

impl ProcessSnapshotter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn snapshot(&mut self) -> Result<Vec<RawProcess>> {
        self.collect(|_, _| true)
    }

    /// Like [`snapshot`](Self::snapshot), but reads only the processes
    /// `keep` accepts, given each one's PID and creation time.
    pub fn snapshot_where(
        &mut self,
        keep: impl FnMut(u32, i64) -> bool,
    ) -> Result<Vec<RawProcess>> {
        self.collect(keep)
    }

    fn collect(&mut self, mut keep: impl FnMut(u32, i64) -> bool) -> Result<Vec<RawProcess>> {
        let pids = util::numeric_entries("/proc");
        let mut out = Vec::with_capacity(self.last_count + 32);
        let mut kernel: Option<RawProcess> = None;
        let mut count = 0usize;
        for pid in pids {
            // A process can exit between listing and reading: skip it.
            let Ok(stat) = Stat::read(pid) else { continue };
            if stat.kernel_thread() {
                let k = kernel.get_or_insert_with(|| {
                    let mut k = empty(KTHREADD, "kthreadd");
                    k.create_time = util::start_ticks_to_filetime(0);
                    k
                });
                if pid == KTHREADD {
                    k.create_time = stat.create_time();
                }
                k.user_time += util::ticks_to_100ns(stat.utime);
                k.kernel_time += util::ticks_to_100ns(stat.stime);
                k.cycle_time = k.user_time + k.kernel_time;
                k.thread_count += 1;
                continue;
            }
            count += 1;
            if !keep(pid, stat.create_time()) {
                continue;
            }
            out.push(read_process(pid, &stat));
        }
        if let Some(k) = kernel {
            count += 1;
            if keep(k.pid, k.create_time) {
                out.push(k);
            }
        }
        self.last_count = count;
        Ok(out)
    }

    /// The threads of `pid`; empty when the process is gone.
    pub fn threads(&mut self, pid: u32) -> Result<Vec<ThreadInfo>> {
        Ok(threads(pid))
    }

    /// The threads of `pid` if it is still the process created at
    /// `create_time` (0 skips that check).
    pub fn threads_of(&mut self, pid: u32, create_time: i64) -> Result<Vec<ThreadInfo>> {
        if create_time != 0 {
            match Stat::read(pid) {
                Ok(s) if s.create_time() == create_time => {}
                _ => return Ok(Vec::new()),
            }
        }
        Ok(threads(pid))
    }
}

fn empty(pid: u32, name: &str) -> RawProcess {
    RawProcess {
        pid,
        parent_pid: 0,
        name: name.to_owned(),
        create_time: 0,
        user_time: 0,
        kernel_time: 0,
        cycle_time: 0,
        base_priority: 8,
        session_id: 0,
        thread_count: 0,
        handle_count: 0,
        working_set: 0,
        private_working_set: 0,
        private_bytes: 0,
        virtual_size: 0,
        page_faults: 0,
        peak_working_set: 0,
        paged_pool: 0,
        nonpaged_pool: 0,
        io_read_bytes: 0,
        io_write_bytes: 0,
        io_other_bytes: 0,
        io_reads: 0,
        io_writes: 0,
        io_other: 0,
        suspended: false,
    }
}

fn read_process(pid: u32, stat: &Stat) -> RawProcess {
    let page = util::page_size();
    let mut p = empty(pid, &process_name(pid, &stat.comm));
    p.parent_pid = if stat.ppid == KTHREADD {
        KTHREADD
    } else {
        stat.ppid
    };
    p.create_time = stat.create_time();
    p.user_time = util::ticks_to_100ns(stat.utime);
    p.kernel_time = util::ticks_to_100ns(stat.stime);
    p.cycle_time = p.user_time + p.kernel_time;
    p.base_priority = base_priority(stat.nice, stat.policy);
    p.session_id = session_of(pid);
    p.thread_count = stat.threads;
    p.virtual_size = stat.vsize;
    p.page_faults = (stat.minflt + stat.majflt).min(u64::from(u32::MAX)) as u32;
    p.suspended = matches!(stat.state, 'T' | 't');

    if let Ok(statm) = fs::read_to_string(format!("/proc/{pid}/statm")) {
        let f: Vec<u64> = statm
            .split_whitespace()
            .filter_map(|v| v.parse().ok())
            .collect();
        if f.len() >= 3 {
            p.working_set = f[1] * page;
            p.private_working_set = f[1].saturating_sub(f[2]) * page;
        }
    } else {
        p.working_set = stat.rss_pages * page;
    }
    if let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) {
        p.peak_working_set = util::kv_bytes(&status, "VmHWM").unwrap_or(p.working_set);
        let anon = util::kv_bytes(&status, "RssAnon").unwrap_or(p.private_working_set);
        let swap = util::kv_bytes(&status, "VmSwap").unwrap_or(0);
        p.private_working_set = anon;
        p.private_bytes = anon + swap;
        // Page tables and kernel stacks are the closest to pool charges.
        p.nonpaged_pool = util::kv_bytes(&status, "VmPTE").unwrap_or(0);
    }
    if let Ok(io) = fs::read_to_string(format!("/proc/{pid}/io")) {
        let get = |k: &str| {
            io.lines()
                .find_map(|l| l.strip_prefix(k)?.strip_prefix(": ")?.trim().parse().ok())
                .unwrap_or(0u64)
        };
        p.io_read_bytes = get("rchar");
        p.io_write_bytes = get("wchar");
        p.io_reads = get("syscr");
        p.io_writes = get("syscw");
    }
    p.handle_count = fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|d| d.count() as u32)
        .unwrap_or(0);
    p
}

/// The program name: the basename of argv[0] when it extends the 15-byte
/// kernel `comm` (which truncates), the `comm` otherwise.
pub(crate) fn process_name(pid: u32, comm: &str) -> String {
    if let Ok(cmdline) = fs::read(format!("/proc/{pid}/cmdline")) {
        let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
        let argv0 = String::from_utf8_lossy(argv0);
        let base = argv0
            .split(' ')
            .next()
            .unwrap_or_default()
            .rsplit('/')
            .next()
            .unwrap_or_default();
        if !base.is_empty() && base.len() > comm.len() && base.starts_with(comm) {
            return base.to_owned();
        }
    }
    comm.to_owned()
}

/// A Windows-style base priority from nice and the scheduling policy, so
/// the "Priority" column and the rules keep their meaning.
pub(crate) fn base_priority(nice: i64, policy: u32) -> i32 {
    const SCHED_FIFO: u32 = 1;
    const SCHED_RR: u32 = 2;
    const SCHED_IDLE: u32 = 5;
    match policy {
        SCHED_FIFO | SCHED_RR => 24,
        SCHED_IDLE => 4,
        _ => match nice {
            15.. => 4,
            1..=14 => 6,
            0 => 8,
            -9..=-1 => 10,
            _ => 13,
        },
    }
}

/// The "session" of a process: the UID of the user slice it runs in, 0 for
/// system services. Desktop programs, user services and terminals of one
/// user share it, like a Windows session.
pub fn session_of(pid: u32) -> u32 {
    let Ok(cgroup) = fs::read_to_string(format!("/proc/{pid}/cgroup")) else {
        return 0;
    };
    session_from_cgroup(&cgroup)
}

pub(crate) fn session_from_cgroup(cgroup: &str) -> u32 {
    cgroup
        .lines()
        .find_map(|l| {
            let path = l.splitn(3, ':').nth(2)?;
            let rest = path.split("/user-").nth(1)?;
            rest.split(".slice").next()?.parse().ok()
        })
        .unwrap_or(0)
}

pub(crate) fn threads(pid: u32) -> Vec<ThreadInfo> {
    let mut out = Vec::new();
    for tid in util::numeric_entries(&format!("/proc/{pid}/task")) {
        let Ok(s) = Stat::read_task(pid, tid) else {
            continue;
        };
        let (state, wait_reason) = match s.state {
            'R' => (2, 0),
            // Uninterruptible: Executive, counts as blocked.
            'D' => (5, 0),
            // Stopped: Suspended.
            'T' | 't' => (5, 5),
            'Z' | 'X' => (4, 0),
            // Interruptible sleep: WrQueue, idle.
            _ => (5, 15),
        };
        let switches = fs::read_to_string(format!("/proc/{pid}/task/{tid}/status"))
            .map(|st| {
                util::kv_bytes(&st, "voluntary_ctxt_switches").unwrap_or(0)
                    + util::kv_bytes(&st, "nonvoluntary_ctxt_switches").unwrap_or(0)
            })
            .unwrap_or(0);
        let prio = base_priority(s.nice, s.policy);
        out.push(ThreadInfo {
            tid,
            state,
            wait_reason,
            context_switches: switches.min(u64::from(u32::MAX)) as u32,
            create_time: s.create_time(),
            kernel_time: util::ticks_to_100ns(s.stime),
            user_time: util::ticks_to_100ns(s.utime),
            priority: prio,
            base_priority: prio,
            start_address: 0,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_contains_self_with_sane_values() {
        let mut s = ProcessSnapshotter::new();
        let all = s.snapshot().unwrap();
        let me = all.iter().find(|p| p.pid == std::process::id()).unwrap();
        assert!(me.thread_count >= 1);
        assert!(me.working_set > 0);
        assert!(me.create_time > 0);
        assert!(me.handle_count >= 3);
        // Kernel threads are folded, never listed one by one.
        assert!(
            all.iter()
                .filter(|p| p.parent_pid == KTHREADD && p.pid != KTHREADD)
                .count()
                == 0
        );
    }

    #[test]
    fn snapshot_where_filters() {
        let me = std::process::id();
        let mut s = ProcessSnapshotter::new();
        let only = s.snapshot_where(|pid, _| pid == me).unwrap();
        assert_eq!(only.len(), 1);
        assert!(!s.threads(me).unwrap().is_empty());
    }

    #[test]
    fn sessions_follow_user_slices() {
        assert_eq!(
            session_from_cgroup("0::/user.slice/user-1000.slice/session-2.scope\n"),
            1000
        );
        assert_eq!(session_from_cgroup("0::/system.slice/cron.service\n"), 0);
    }

    #[test]
    fn priorities_map_to_windows_bases() {
        assert_eq!(base_priority(0, 0), 8);
        assert_eq!(base_priority(19, 0), 4);
        assert_eq!(base_priority(-10, 0), 13);
        assert_eq!(base_priority(0, 2), 24);
    }
}
