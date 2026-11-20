//! Mounted filesystems as "volumes": the root is the mount point, the label
//! comes from /dev/disk/by-label. Network filesystems (CIFS, NFS, SSHFS) are
//! sized on a helper thread with a deadline, because a dead server can hang
//! statvfs.

use std::fs;
use std::sync::mpsc;
use std::time::Duration;

use crate::disk::{self, Mount};

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DriveKind {
    Fixed,
    Removable,
    Network,
    Optical,
    RamDisk,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Volume {
    pub root: String,
    pub label: String,
    pub file_system: String,
    pub kind: DriveKind,
    pub total: u64,
    pub free: u64,
}

impl Volume {
    pub fn used(&self) -> u64 {
        self.total.saturating_sub(self.free)
    }

    pub fn used_percent(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.used() as f64 / self.total as f64 * 100.0) as f32
        }
    }
}

const NETWORK: &[&str] = &[
    "cifs",
    "smb3",
    "smbfs",
    "nfs",
    "nfs4",
    "fuse.sshfs",
    "sshfs",
    "9p",
    "ceph",
    "glusterfs",
    "davfs",
    "fuse.rclone",
];

fn kind(m: &Mount) -> DriveKind {
    let fs = m.fstype.as_str();
    if NETWORK.contains(&fs) {
        return DriveKind::Network;
    }
    if matches!(fs, "iso9660" | "udf") {
        return DriveKind::Optical;
    }
    if matches!(fs, "tmpfs" | "ramfs") {
        return DriveKind::RamDisk;
    }
    let mut under = Vec::new();
    disk::disks_under(&m.dev, &mut under, 0);
    if under.is_empty() {
        return DriveKind::Unknown;
    }
    if under.iter().any(|d| removable(d)) {
        DriveKind::Removable
    } else {
        DriveKind::Fixed
    }
}

pub(crate) fn removable(disk: &str) -> bool {
    if crate::linux::util::read_u64(format!("/sys/block/{disk}/removable")) == Some(1) {
        return true;
    }
    // USB sticks and card readers often report removable=0.
    fs::canonicalize(format!("/sys/block/{disk}"))
        .map(|p| p.to_string_lossy().contains("/usb"))
        .unwrap_or(false)
}

fn label_of(source: &str) -> String {
    let Ok(target) = fs::canonicalize(source) else {
        return String::new();
    };
    let Ok(rd) = fs::read_dir("/dev/disk/by-label") else {
        return String::new();
    };
    rd.flatten()
        .find(|e| fs::canonicalize(e.path()).is_ok_and(|p| p == target))
        .map(|e| disk::unescape(&e.file_name().to_string_lossy().replace("\\x20", " ")))
        .unwrap_or_default()
}

pub(crate) fn statvfs(path: &str) -> Option<(u64, u64)> {
    let c = std::ffi::CString::new(path).ok()?;
    // SAFETY: zeroed statvfs is a valid out-parameter.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: valid C string and out-pointer.
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = st.f_frsize as u64;
    Some((st.f_blocks as u64 * frsize, st.f_bavail as u64 * frsize))
}

fn sized(path: &str, network: bool) -> Option<(u64, u64)> {
    if !network {
        return statvfs(path);
    }
    let (tx, rx) = mpsc::channel();
    let owned = path.to_owned();
    std::thread::Builder::new()
        .name("wtm-statvfs".into())
        .spawn(move || {
            let _ = tx.send(statvfs(&owned));
        })
        .ok()?;
    rx.recv_timeout(Duration::from_millis(800)).ok().flatten()
}

fn volume(m: &Mount) -> Option<Volume> {
    let kind = kind(m);
    let (total, free) = sized(&m.point, kind == DriveKind::Network)?;
    Some(Volume {
        root: m.point.clone(),
        label: if kind == DriveKind::Network {
            m.source.clone()
        } else {
            label_of(&m.source)
        },
        file_system: m.fstype.clone(),
        kind,
        total,
        free,
    })
}

/// Every mounted filesystem that holds data. A filesystem mounted twice
/// (bind mounts) is listed once, at its shortest path.
pub fn volumes() -> Vec<Volume> {
    let mut seen: Vec<String> = Vec::new();
    let mut mounts: Vec<Mount> = disk::mounts()
        .into_iter()
        .filter(disk::real_filesystem)
        .collect();
    mounts.sort_by_key(|m| m.point.len());
    mounts
        .into_iter()
        .filter(|m| {
            let key = format!("{}|{}", m.dev, m.source);
            let fresh = !seen.contains(&key);
            seen.push(key);
            fresh
        })
        .filter_map(|m| volume(&m))
        .collect()
}

/// The volumes mounted at `roots` (mount points; a trailing `\` from the
/// Windows-shaped callers and `\040` escapes are accepted).
pub fn volumes_at(roots: &[String]) -> Vec<Volume> {
    let wanted: Vec<String> = roots
        .iter()
        .map(|r| {
            let r = disk::unescape(r.trim_end_matches('\\'));
            if r.len() > 1 {
                r.trim_end_matches('/').to_owned()
            } else {
                r
            }
        })
        .collect();
    disk::mounts()
        .into_iter()
        .filter(|m| wanted.contains(&m.point))
        .take(64)
        .filter_map(|m| volume(&m))
        .map(|mut v| {
            v.root = v.root.replace(' ', "\\040");
            v
        })
        .collect()
}

/// `path` is on a local, non-removable disk.
pub fn on_fixed_drive(path: &str) -> bool {
    if !path.starts_with('/') {
        return false;
    }
    let Some(m) = disk::mounts()
        .into_iter()
        .filter(|m| {
            path == m.point
                || m.point == "/"
                || path.starts_with(&format!("{}/", m.point.trim_end_matches('/')))
        })
        .max_by_key(|m| m.point.len())
    else {
        return false;
    };
    kind(&m) == DriveKind::Fixed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_root() {
        let all = volumes();
        assert!(all.iter().all(|v| v.free <= v.total));
        let at = volumes_at(&["/\\".to_owned()]);
        assert!(at.len() <= 1);
    }
}
