//! System-wide CPU, network and memory counters from /proc and /sys.

use std::fs;

use crate::linux::util::{self, kv_bytes, read_trim, read_u64};
use crate::{Error, Result};

/// Cumulative system CPU times in 100 ns units. `kernel` includes `idle`,
/// as on Windows; I/O wait counts as idle.
#[derive(Debug, Clone, Copy, Default)]
pub struct CpuTimes {
    pub idle: u64,
    pub kernel: u64,
    pub user: u64,
    /// softirq time.
    pub dpc: u64,
    /// hardirq time.
    pub interrupt: u64,
    /// Interrupts serviced so far (aggregate only).
    pub interrupts: u64,
}

fn parse_cpu_line(line: &str) -> Option<CpuTimes> {
    let mut f = line.split_whitespace();
    f.next()?;
    let v: Vec<u64> = f.map(|x| x.parse().unwrap_or(0)).collect();
    let g = |i: usize| util::ticks_to_100ns(v.get(i).copied().unwrap_or(0));
    let (user, nice, system, idle, iowait, irq, softirq, steal) =
        (g(0), g(1), g(2), g(3), g(4), g(5), g(6), g(7));
    let idle = idle + iowait;
    Some(CpuTimes {
        idle,
        kernel: system + irq + softirq + steal + idle,
        user: user + nice,
        dpc: softirq,
        interrupt: irq,
        interrupts: 0,
    })
}

pub fn cpu_times() -> Result<CpuTimes> {
    let stat = fs::read_to_string("/proc/stat").map_err(|e| Error::io("/proc/stat", e))?;
    let mut t = stat
        .lines()
        .find(|l| l.starts_with("cpu "))
        .and_then(parse_cpu_line)
        .ok_or(Error::Unsupported("/proc/stat cpu line"))?;
    t.interrupts = stat
        .lines()
        .find_map(|l| {
            l.strip_prefix("intr ")?
                .split_whitespace()
                .next()?
                .parse()
                .ok()
        })
        .unwrap_or(0);
    Ok(t)
}

/// Cumulative times of each online logical processor, in CPU order.
pub fn per_cpu_times() -> Result<Vec<CpuTimes>> {
    let stat = fs::read_to_string("/proc/stat").map_err(|e| Error::io("/proc/stat", e))?;
    Ok(stat
        .lines()
        .filter(|l| l.starts_with("cpu") && l.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
        .filter_map(parse_cpu_line)
        .collect())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NetCounters {
    pub received: u64,
    pub sent: u64,
}

/// A network interface that is up. `luid` is the interface index.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NetAdapter {
    pub luid: u64,
    pub alias: String,
    pub description: String,
    /// IANA ifType, as Windows reports it: 6 Ethernet, 71 Wi-Fi, 131
    /// tunnel, 243 mobile broadband.
    pub if_type: u32,
    /// Bits per second.
    pub link_speed: u64,
    pub counters: NetCounters,
}

const MAX_ADAPTERS: usize = 32;

pub(crate) fn if_type(name: &str) -> u32 {
    let base = format!("/sys/class/net/{name}");
    if fs::metadata(format!("{base}/wireless")).is_ok()
        || fs::metadata(format!("{base}/phy80211")).is_ok()
    {
        return 71;
    }
    let devtype = fs::read_to_string(format!("{base}/uevent"))
        .ok()
        .and_then(|u| {
            u.lines()
                .find_map(|l| l.strip_prefix("DEVTYPE=").map(str::to_owned))
        });
    if matches!(devtype.as_deref(), Some("wwan"))
        || name.starts_with("wwan")
        || name.starts_with("ww")
    {
        return 243;
    }
    match read_u64(format!("{base}/type")).unwrap_or(1) {
        1 => {
            if fs::metadata(format!("{base}/tun_flags")).is_ok() {
                131
            } else {
                6
            }
        }
        // ARPHRD_NONE (tun, wireguard), PPP, IP-in-IP tunnels.
        65534 | 512 | 768 | 769 | 776 | 778 => 131,
        772 => 24,
        _ => 1,
    }
}

/// The udev model name of an interface, or its driver.
pub(crate) fn if_description(name: &str, index: u64) -> String {
    if let Ok(db) = fs::read_to_string(format!("/run/udev/data/n{index}")) {
        for key in ["E:ID_MODEL_FROM_DATABASE=", "E:ID_MODEL="] {
            if let Some(v) = db.lines().find_map(|l| l.strip_prefix(key)) {
                return v.replace("\\x20", " ").trim().to_owned();
            }
        }
    }
    fs::read_link(format!("/sys/class/net/{name}/device/driver"))
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| name.to_owned())
}

pub fn net_adapters() -> Result<Vec<NetAdapter>> {
    let rd = fs::read_dir("/sys/class/net").map_err(|e| Error::io("/sys/class/net", e))?;
    let mut out = Vec::new();
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    for name in names {
        if name == "lo" || name.starts_with("veth") {
            continue;
        }
        let base = format!("/sys/class/net/{name}");
        let oper = read_trim(format!("{base}/operstate")).unwrap_or_default();
        let carrier = read_u64(format!("{base}/carrier")).unwrap_or(0) == 1;
        if !(oper == "up" || (oper == "unknown" && carrier)) {
            continue;
        }
        let index = read_u64(format!("{base}/ifindex")).unwrap_or(0);
        let speed = read_trim(format!("{base}/speed"))
            .and_then(|s| s.parse::<i64>().ok())
            .filter(|s| *s > 0)
            .map_or(0, |mbps| mbps as u64 * 1_000_000);
        out.push(NetAdapter {
            luid: index,
            description: if_description(&name, index),
            if_type: if_type(&name),
            link_speed: speed,
            counters: NetCounters {
                received: read_u64(format!("{base}/statistics/rx_bytes")).unwrap_or(0),
                sent: read_u64(format!("{base}/statistics/tx_bytes")).unwrap_or(0),
            },
            alias: name,
        });
        if out.len() == MAX_ADAPTERS {
            break;
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MemoryStatus {
    pub load_percent: u32,
    pub total_phys: u64,
    pub avail_phys: u64,
    /// RAM plus swap: what can be committed before the OOM killer.
    pub commit_limit: u64,
    pub commit_avail: u64,
}

pub fn memory_status() -> Result<MemoryStatus> {
    let m = fs::read_to_string("/proc/meminfo").map_err(|e| Error::io("/proc/meminfo", e))?;
    let total = kv_bytes(&m, "MemTotal").unwrap_or(0);
    let avail = kv_bytes(&m, "MemAvailable").unwrap_or_else(|| {
        kv_bytes(&m, "MemFree").unwrap_or(0) + kv_bytes(&m, "Cached").unwrap_or(0)
    });
    let limit = total + kv_bytes(&m, "SwapTotal").unwrap_or(0);
    let committed = kv_bytes(&m, "Committed_AS").unwrap_or(0);
    Ok(MemoryStatus {
        load_percent: ((total - avail.min(total)) * 100)
            .checked_div(total)
            .unwrap_or(0) as u32,
        total_phys: total,
        avail_phys: avail,
        commit_limit: limit,
        commit_avail: limit.saturating_sub(committed),
    })
}

pub fn logical_cpu_count() -> u32 {
    // SAFETY: sysconf takes no pointers.
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    (n.max(1)) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_counters() {
        let t = cpu_times().unwrap();
        assert!(t.kernel >= t.idle);
        assert_eq!(per_cpu_times().unwrap().len() as u32, logical_cpu_count());
        let m = memory_status().unwrap();
        assert!(m.total_phys > 0 && m.avail_phys <= m.total_phys);
        assert!(m.load_percent <= 100);
        net_adapters().unwrap();
    }

    #[test]
    fn parses_cpu_line() {
        let t = parse_cpu_line("cpu0 100 0 50 800 50 0 0 0 0 0").unwrap();
        let tick = util::ticks_to_100ns(1);
        assert_eq!(t.user, 100 * tick);
        assert_eq!(t.idle, 850 * tick);
        assert_eq!(t.kernel, 900 * tick);
    }
}
