//! Activity of each physical disk from /proc/diskstats.
//!
//! Disks are numbered in the order of their kernel names (nvme0n1, nvme1n1,
//! sda, ...), the counterpart of Windows' PhysicalDriveN. The "letters" of
//! a disk are the mount points of filesystems on it, through partitions and
//! device-mapper layers (LUKS, LVM), with spaces escaped as `\040` like
//! /proc/mounts does.

use std::collections::BTreeMap;
use std::fs;
use std::time::Instant;

use crate::Result;
use crate::linux::util::read_trim;

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DiskUsage {
    /// Disk number and mount points, e.g. `0 / /home`.
    pub name: String,
    pub active_percent: f32,
    pub read_rate: f64,
    pub write_rate: f64,
    #[serde(default)]
    pub response_ms: f32,
    #[serde(default)]
    pub queue_length: f32,
}

impl DiskUsage {
    pub fn number(&self) -> Option<u32> {
        self.name.split_whitespace().next()?.parse().ok()
    }

    /// The mount points in the name.
    pub fn letters(&self) -> impl Iterator<Item = &str> {
        self.name.split_whitespace().skip(1)
    }
}

/// Whole disks worth listing, in number order.
pub(crate) fn disks() -> Vec<String> {
    let Ok(rd) = fs::read_dir("/sys/block") else {
        return Vec::new();
    };
    let mut names: Vec<String> = rd
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| {
            !(n.starts_with("loop")
                || n.starts_with("ram")
                || n.starts_with("zram")
                || n.starts_with("dm-")
                || n.starts_with("md")
                || n.starts_with("sr")
                || n.starts_with("fd")
                || n.starts_with("nbd"))
        })
        // Drives with no medium (empty card readers) have size 0.
        .filter(|n| crate::linux::util::read_u64(format!("/sys/block/{n}/size")).unwrap_or(0) > 0)
        .collect();
    names.sort_by(|a, b| crate::collate::natural(a, b));
    names
}

pub(crate) fn disk_name(number: u32) -> Option<String> {
    disks().into_iter().nth(number as usize)
}

/// The whole disks under a block device given as `major:minor`, following
/// partitions and device-mapper slaves.
pub(crate) fn disks_under(dev: &str, out: &mut Vec<String>, depth: u32) {
    if depth > 8 {
        return;
    }
    let Ok(real) = fs::canonicalize(format!("/sys/dev/block/{dev}")) else {
        return;
    };
    let slaves = real.join("slaves");
    if let Ok(rd) = fs::read_dir(&slaves) {
        let mut any = false;
        for e in rd.flatten() {
            any = true;
            if let Some(d) = read_trim(e.path().join("dev")) {
                disks_under(&d, out, depth + 1);
            }
        }
        if any {
            return;
        }
    }
    // .../block/<disk>[/<partition>]
    let parts: Vec<String> = real
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    if let Some(i) = parts.iter().position(|p| p == "block")
        && let Some(disk) = parts.get(i + 1)
        && !out.contains(disk)
    {
        out.push(disk.clone());
    }
}

/// One line of /proc/self/mountinfo.
#[derive(Debug, Clone)]
pub(crate) struct Mount {
    pub dev: String,
    pub point: String,
    pub fstype: String,
    pub source: String,
}

pub(crate) fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && i + 4 <= b.len()
            && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c))
        {
            out.push((b[i + 1] - b'0') * 64 + (b[i + 2] - b'0') * 8 + (b[i + 3] - b'0'));
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn mounts() -> Vec<Mount> {
    let Ok(text) = fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|l| {
            let (left, right) = l.split_once(" - ")?;
            let left: Vec<&str> = left.split(' ').collect();
            let right: Vec<&str> = right.split(' ').collect();
            Some(Mount {
                dev: left.get(2)?.to_string(),
                point: unescape(left.get(4)?),
                fstype: right.first()?.to_string(),
                source: unescape(right.get(1).copied().unwrap_or("")),
            })
        })
        .collect()
}

/// Filesystems that hold user data, as opposed to kernel and package
/// mounts (snaps are squashfs images).
pub(crate) fn real_filesystem(m: &Mount) -> bool {
    const SKIP: &[&str] = &[
        "proc",
        "sysfs",
        "tmpfs",
        "devtmpfs",
        "devpts",
        "cgroup",
        "cgroup2",
        "securityfs",
        "pstore",
        "efivarfs",
        "bpf",
        "debugfs",
        "tracefs",
        "configfs",
        "fusectl",
        "mqueue",
        "hugetlbfs",
        "autofs",
        "binfmt_misc",
        "nsfs",
        "squashfs",
        "overlay",
        "ramfs",
        "rpc_pipefs",
        "fuse.portal",
        "fuse.gvfsd-fuse",
        "fuse.snapfuse",
        "nfsd",
    ];
    !SKIP.contains(&m.fstype.as_str())
        && !m.point.starts_with("/snap/")
        && !m.point.starts_with("/var/snap/")
        && !m.point.starts_with("/run/user/")
        && !m.point.starts_with("/proc")
        && !m.point.starts_with("/sys")
}

/// Mount points per disk name.
pub(crate) fn mount_points() -> BTreeMap<String, Vec<String>> {
    let mut by_disk: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for m in mounts().into_iter().filter(real_filesystem) {
        let mut under = Vec::new();
        disks_under(&m.dev, &mut under, 0);
        for d in under {
            let points = by_disk.entry(d).or_default();
            let escaped = m.point.replace(' ', "\\040");
            if !points.contains(&escaped) {
                points.push(escaped);
            }
        }
    }
    by_disk
}

#[derive(Debug, Clone, Copy, Default)]
struct Raw {
    sectors_read: u64,
    sectors_written: u64,
    ios: u64,
    io_ms: u64,
    weighted_ms: u64,
}

fn read_stats() -> BTreeMap<String, Raw> {
    let mut out = BTreeMap::new();
    let Ok(text) = fs::read_to_string("/proc/diskstats") else {
        return out;
    };
    for l in text.lines() {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 14 {
            continue;
        }
        let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
        out.insert(
            f[2].to_owned(),
            Raw {
                sectors_read: n(5),
                sectors_written: n(9),
                ios: n(3) + n(7),
                io_ms: n(12),
                weighted_ms: n(13),
            },
        );
    }
    out
}

#[derive(Debug)]
pub struct DiskSampler {
    prev: Option<(Instant, BTreeMap<String, Raw>)>,
    names: Vec<String>,
    points: BTreeMap<String, Vec<String>>,
    refreshed: Option<Instant>,
}

impl DiskSampler {
    pub fn new() -> Result<Self> {
        Ok(Self {
            prev: None,
            names: Vec::new(),
            points: BTreeMap::new(),
            refreshed: None,
        })
    }

    pub fn sample(&mut self) -> Result<Vec<DiskUsage>> {
        let now = Instant::now();
        // Disks and mounts change rarely; re-read them every 10 seconds.
        if self
            .refreshed
            .is_none_or(|t| now.duration_since(t).as_secs() >= 10)
        {
            self.names = disks();
            self.points = mount_points();
            self.refreshed = Some(now);
        }
        let stats = read_stats();
        let mut out = Vec::with_capacity(self.names.len());
        for (number, name) in self.names.iter().enumerate() {
            let Some(cur) = stats.get(name) else { continue };
            let mut usage = DiskUsage {
                name: std::iter::once(number.to_string())
                    .chain(self.points.get(name).into_iter().flatten().cloned())
                    .collect::<Vec<_>>()
                    .join(" "),
                ..Default::default()
            };
            if let Some((at, prev)) = &self.prev
                && let Some(p) = prev.get(name)
            {
                let secs = now.duration_since(*at).as_secs_f64().max(1e-3);
                let ms = secs * 1000.0;
                usage.read_rate =
                    cur.sectors_read.saturating_sub(p.sectors_read) as f64 * 512.0 / secs;
                usage.write_rate =
                    cur.sectors_written.saturating_sub(p.sectors_written) as f64 * 512.0 / secs;
                usage.active_percent = (cur.io_ms.saturating_sub(p.io_ms) as f64 / ms * 100.0)
                    .clamp(0.0, 100.0) as f32;
                let ios = cur.ios.saturating_sub(p.ios);
                let weighted = cur.weighted_ms.saturating_sub(p.weighted_ms) as f64;
                if ios > 0 {
                    usage.response_ms = (weighted / ios as f64).clamp(0.0, 1e7) as f32;
                }
                usage.queue_length = (weighted / ms).clamp(0.0, 1e6) as f32;
            }
            out.push(usage);
        }
        self.prev = Some((now, stats));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_carry_numbers_and_points() {
        let d = DiskUsage {
            name: "1 / /home".into(),
            ..Default::default()
        };
        assert_eq!(d.number(), Some(1));
        assert_eq!(d.letters().collect::<Vec<_>>(), ["/", "/home"]);
    }

    #[test]
    fn unescapes_mountinfo() {
        assert_eq!(unescape("/media/a\\040b"), "/media/a b");
        assert_eq!(unescape("/plain"), "/plain");
    }

    #[test]
    fn samples_twice() {
        let mut s = DiskSampler::new().unwrap();
        s.sample().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        for d in s.sample().unwrap() {
            assert!(d.number().is_some());
            assert!((0.0..=100.0).contains(&d.active_percent));
        }
    }
}
