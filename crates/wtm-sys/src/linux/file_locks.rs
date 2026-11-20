//! Which processes keep a file or folder busy: open descriptors, working
//! directories and memory-mapped files under the path (what `lsof` and
//! `fuser` report). Read-only. Other users' processes need root.

use std::fs;
use std::path::Path;

use crate::linux::util::{self, Stat};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Locker {
    pub pid: u32,
    pub create_time: i64,
    pub name: String,
    pub service: String,
    #[serde(default)]
    pub reason: Hold,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Hold {
    WorkingDirectory,
    #[default]
    #[serde(other)]
    OpenFile,
}

/// `candidate` is `folder` or lies inside it, by whole names.
pub fn inside(folder: &str, candidate: &str) -> bool {
    let folder = folder.trim_end_matches('/');
    let candidate = candidate.trim_end_matches('/');
    if folder.is_empty() {
        return candidate.starts_with('/');
    }
    candidate == folder
        || candidate
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct Report {
    pub path: String,
    pub files_checked: usize,
    pub partial: bool,
    pub warnings: Vec<String>,
    pub lockers: Vec<Locker>,
}

/// The systemd service a process belongs to, from its cgroup.
fn service_of(pid: u32) -> String {
    fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .ok()
        .and_then(|c| {
            c.lines().find_map(|l| {
                let path = l.splitn(3, ':').nth(2)?;
                if !path.starts_with("/system.slice/") {
                    return None;
                }
                path.rsplit('/')
                    .find(|p| p.ends_with(".service"))
                    .map(|s| s.trim_end_matches(".service").to_owned())
            })
        })
        .unwrap_or_default()
}

pub fn scan(path: &Path) -> Result<Report, String> {
    let target = fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let target = target.to_string_lossy().into_owned();
    let mut report = Report {
        path: target.clone(),
        ..Default::default()
    };
    let mut denied = 0usize;
    for pid in util::numeric_entries("/proc") {
        let Ok(stat) = Stat::read(pid) else { continue };
        if stat.kernel_thread() {
            continue;
        }
        let mut reason = None;
        if fs::read_link(format!("/proc/{pid}/cwd"))
            .is_ok_and(|c| inside(&target, &c.to_string_lossy()))
        {
            reason = Some(Hold::WorkingDirectory);
        }
        match fs::read_dir(format!("/proc/{pid}/fd")) {
            Ok(rd) => {
                for e in rd.flatten() {
                    report.files_checked += 1;
                    if fs::read_link(e.path()).is_ok_and(|t| {
                        inside(&target, t.to_string_lossy().trim_end_matches(" (deleted)"))
                    }) {
                        reason = Some(reason.unwrap_or(Hold::OpenFile));
                        break;
                    }
                }
            }
            Err(_) => denied += 1,
        }
        if reason.is_none()
            && let Ok(maps) = fs::read_to_string(format!("/proc/{pid}/maps"))
            && maps.lines().any(|l| {
                l.split_whitespace()
                    .nth(5)
                    .is_some_and(|p| inside(&target, p))
            })
        {
            reason = Some(Hold::OpenFile);
        }
        if let Some(reason) = reason {
            report.lockers.push(Locker {
                pid,
                create_time: stat.create_time(),
                name: crate::process::process_name(pid, &stat.comm),
                service: service_of(pid),
                reason,
            });
        }
    }
    if denied > 0 {
        report.partial = true;
        report.warnings.push(format!(
            "{denied} processes of other users could not be checked (run as root to see them)"
        ));
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_itself() {
        assert!(inside("/data", "/data/x"));
        assert!(!inside("/data", "/database"));
        let dir = std::env::temp_dir().join(format!("wtm-locks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = std::fs::File::create(dir.join("held")).unwrap();
        let r = scan(&dir).unwrap();
        assert!(r.lockers.iter().any(|l| l.pid == std::process::id()));
        drop(f);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
