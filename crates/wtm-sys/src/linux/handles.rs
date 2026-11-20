//! The open file descriptors of a process (/proc/PID/fd), as handles: the
//! kind is what the descriptor refers to (File, Directory, Socket, Pipe,
//! Device, anon_inode type such as eventfd), the name its target, and the
//! "access" its open flags.

use std::fs;
use std::time::{Duration, Instant};

use crate::Result;
use crate::control::ProcessHandle;

pub const MOST_HANDLES: usize = 200_000;
const MOST_NAMED: usize = 5_000;
const NAMING_BUDGET: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HandleInfo {
    pub value: u64,
    pub kind: String,
    pub name: Option<String>,
    pub access: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HandleReport {
    pub total: usize,
    pub handles: Vec<HandleInfo>,
    pub can_name: bool,
    pub names_incomplete: bool,
}

impl HandleReport {
    pub fn too_many(&self) -> bool {
        self.total > MOST_HANDLES
    }
}

pub(crate) fn classify(target: &str) -> (String, Option<String>) {
    if let Some(rest) = target.strip_prefix("socket:[") {
        return (
            "Socket".into(),
            Some(format!("socket {}", rest.trim_end_matches(']'))),
        );
    }
    if let Some(rest) = target.strip_prefix("pipe:[") {
        return (
            "Pipe".into(),
            Some(format!("pipe {}", rest.trim_end_matches(']'))),
        );
    }
    if let Some(rest) = target.strip_prefix("anon_inode:") {
        let kind = rest.trim_matches(|c| c == '[' || c == ']');
        return (kind.to_owned(), None);
    }
    if target.starts_with("/dev/") {
        return ("Device".into(), Some(target.to_owned()));
    }
    let path = target.trim_end_matches(" (deleted)");
    let kind = if fs::metadata(path).is_ok_and(|m| m.is_dir()) {
        "Directory"
    } else {
        "File"
    };
    (kind.into(), Some(target.to_owned()))
}

impl ProcessHandle {
    pub fn handles(&self, can_name: bool) -> Result<HandleReport> {
        self.check()?;
        let rd = fs::read_dir(self.proc_path("fd")).map_err(|e| crate::Error::io("fd", e))?;
        let mut fds: Vec<u64> = rd
            .flatten()
            .filter_map(|e| e.file_name().to_str()?.parse().ok())
            .collect();
        fds.sort_unstable();
        let mut report = HandleReport {
            total: fds.len(),
            can_name,
            ..Default::default()
        };
        if report.too_many() {
            return Ok(report);
        }
        let started = Instant::now();
        for (i, fd) in fds.into_iter().enumerate() {
            let named = can_name && i < MOST_NAMED && started.elapsed() < NAMING_BUDGET;
            if can_name && !named {
                report.names_incomplete = true;
            }
            let target = fs::read_link(self.proc_path(&format!("fd/{fd}")))
                .map(|t| t.to_string_lossy().into_owned())
                .unwrap_or_default();
            let (kind, name) = classify(&target);
            let access = fs::read_to_string(self.proc_path(&format!("fdinfo/{fd}")))
                .ok()
                .and_then(|i| {
                    i.lines()
                        .find_map(|l| u32::from_str_radix(l.strip_prefix("flags:")?.trim(), 8).ok())
                })
                .unwrap_or(0);
            report.handles.push(HandleInfo {
                value: fd,
                kind,
                name: if named { name } else { None },
                access,
            });
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Access;

    #[test]
    fn lists_own_descriptors() {
        let f = std::fs::File::open("/proc/self/stat").unwrap();
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        let r = h.handles(true).unwrap();
        assert!(r.total >= 3 && !r.too_many());
        assert!(
            r.handles.iter().any(
                |x| x.kind == "File" && x.name.as_deref().is_some_and(|n| n.ends_with("/stat"))
            )
        );
        drop(f);
        assert_eq!(classify("socket:[123]").0, "Socket");
        assert_eq!(classify("anon_inode:[eventfd]").0, "eventfd");
    }
}
