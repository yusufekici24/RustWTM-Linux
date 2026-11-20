//! The processor facts Task Manager lists under the CPU graph, from
//! /proc/cpuinfo and sysfs, plus scheduler activity from /proc/stat.

use std::cell::RefCell;
use std::collections::BTreeSet;
use std::fs;
use std::time::Instant;

use crate::linux::util::{read_trim, read_u64};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CpuInfo {
    pub name: Option<String>,
    pub base_mhz: Option<u32>,
    pub layout: Layout,
    pub virtualization: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Layout {
    pub sockets: u32,
    pub cores: u32,
    pub logical: u32,
    pub l1: u64,
    pub l2: u64,
    pub l3: u64,
}

fn cache_size(text: &str) -> u64 {
    let t = text.trim();
    let (n, mult) = if let Some(k) = t.strip_suffix('K') {
        (k, 1024)
    } else if let Some(m) = t.strip_suffix('M') {
        (m, 1 << 20)
    } else {
        (t, 1)
    };
    n.parse::<u64>().unwrap_or(0) * mult
}

pub(crate) fn layout() -> Layout {
    let cpus = crate::cpu::online_cpus();
    let mut packages = BTreeSet::new();
    let mut cores = BTreeSet::new();
    // (level, shared cpu list, type) so a shared cache counts once.
    let mut caches = BTreeSet::new();
    let mut l = Layout {
        logical: cpus.len() as u32,
        ..Default::default()
    };
    for c in &cpus {
        let base = format!("/sys/devices/system/cpu/cpu{c}");
        let pkg = read_u64(format!("{base}/topology/physical_package_id")).unwrap_or(0);
        packages.insert(pkg);
        cores.insert((
            pkg,
            read_u64(format!("{base}/topology/core_id")).unwrap_or(u64::from(*c)),
        ));
        let Ok(rd) = fs::read_dir(format!("{base}/cache")) else {
            continue;
        };
        for idx in rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join("level").exists())
        {
            let level = read_u64(idx.join("level")).unwrap_or(0);
            let shared = read_trim(idx.join("shared_cpu_list")).unwrap_or_default();
            let kind = read_trim(idx.join("type")).unwrap_or_default();
            if caches.insert((level, shared, kind)) {
                let size = cache_size(&read_trim(idx.join("size")).unwrap_or_default());
                match level {
                    1 => l.l1 += size,
                    2 => l.l2 += size,
                    3 => l.l3 += size,
                    _ => {}
                }
            }
        }
    }
    l.sockets = packages.len() as u32;
    l.cores = cores.len() as u32;
    l
}

pub fn info() -> CpuInfo {
    let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let field = |key: &str| {
        cpuinfo.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().to_owned())
        })
    };
    let name = field("model name")
        .or_else(|| field("Hardware"))
        .or_else(|| read_trim("/sys/firmware/devicetree/base/model"))
        .filter(|n| !n.is_empty());
    let base_mhz = read_u64("/sys/devices/system/cpu/cpu0/cpufreq/base_frequency")
        .map(|khz| (khz / 1000) as u32)
        .filter(|m| *m > 0);
    let flags = field("flags").unwrap_or_default();
    let virtualization = if flags.is_empty() {
        None
    } else {
        Some(flags.split_whitespace().any(|f| f == "vmx" || f == "svm"))
    };
    CpuInfo {
        name,
        base_mhz,
        layout: layout(),
        virtualization,
    }
}

pub fn uptime() -> std::time::Duration {
    let secs = read_trim("/proc/uptime")
        .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok())
        .unwrap_or(0.0);
    std::time::Duration::from_secs_f64(secs)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Activity {
    pub current_mhz: Option<f64>,
    pub context_switches: Option<f64>,
    pub system_calls: Option<f64>,
    pub queue_length: Option<f64>,
}

struct Counters {
    at: Instant,
    ctxt: u64,
    cores: Vec<f64>,
}

thread_local! {
    static COUNTERS: RefCell<Option<Counters>> = const { RefCell::new(None) };
}

fn read_core_mhz() -> Vec<f64> {
    crate::cpu::online_cpus()
        .into_iter()
        .filter_map(|c| {
            read_u64(format!(
                "/sys/devices/system/cpu/cpu{c}/cpufreq/scaling_cur_freq"
            ))
            .map(|khz| khz as f64 / 1000.0)
        })
        .collect()
}

pub fn core_mhz() -> Vec<f64> {
    COUNTERS.with(|cell| {
        cell.try_borrow()
            .ok()
            .and_then(|c| c.as_ref().map(|c| c.cores.clone()))
            .unwrap_or_default()
    })
}

pub fn activity() -> Activity {
    let stat = fs::read_to_string("/proc/stat").unwrap_or_default();
    let value = |key: &str| {
        stat.lines().find_map(|l| {
            l.strip_prefix(key)?
                .split_whitespace()
                .next()?
                .parse::<u64>()
                .ok()
        })
    };
    let ctxt = value("ctxt ").unwrap_or(0);
    let running = value("procs_running ");
    let cores = read_core_mhz();
    let current = (!cores.is_empty()).then(|| cores.iter().sum::<f64>() / cores.len() as f64);
    let now = Instant::now();
    COUNTERS.with(|cell| {
        let Ok(mut slot) = cell.try_borrow_mut() else {
            return Activity::default();
        };
        let rate = slot.as_ref().map(|prev| {
            let secs = now.duration_since(prev.at).as_secs_f64().max(1e-3);
            ctxt.saturating_sub(prev.ctxt) as f64 / secs
        });
        *slot = Some(Counters {
            at: now,
            ctxt,
            cores,
        });
        Activity {
            current_mhz: current,
            context_switches: rate,
            system_calls: None,
            // Runnable tasks beyond the one reading this.
            queue_length: running.map(|r| r.saturating_sub(1) as f64),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_facts() {
        let i = info();
        assert!(i.layout.logical >= 1);
        assert!(i.layout.cores >= 1 && i.layout.cores <= i.layout.logical);
        assert!(uptime().as_secs() > 0);
        assert!(activity().context_switches.is_none());
        assert!(activity().context_switches.is_some());
        assert_eq!(cache_size("48K"), 48 * 1024);
    }
}
