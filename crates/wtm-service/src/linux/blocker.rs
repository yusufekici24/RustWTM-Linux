//! Keeps listed programs from starting, the Linux way: a fanotify exec
//! guard (see `wtm_sys::exec_guard`) refuses files of those names. Unlike
//! Windows' registry entries, nothing outlives the service: when it stops,
//! programs start again, so `remove_all` has nothing left to clean.

use std::collections::BTreeSet;
use std::io;
use std::sync::Mutex;

use wtm_sys::exec_guard::{ExecGuard, NEVER_BLOCKED};

pub struct Blocker {
    guard: Mutex<Option<ExecGuard>>,
    /// Names wanted, also when the guard could not start (tests, no root).
    wanted: Mutex<BTreeSet<String>>,
    live: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncReport {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    /// Names that are never blocked (they would lock the machine out).
    pub conflicts: Vec<String>,
}

/// Linux programs have no `.exe`; a Windows-style name in the config is
/// matched without it.
fn normalize(name: &str) -> String {
    let n = name.trim();
    n.strip_suffix(".exe")
        .or_else(|| n.strip_suffix(".EXE"))
        .unwrap_or(n)
        .to_owned()
}

impl Blocker {
    /// The real guard; needs root. `debugger` (the Windows command run
    /// instead of a blocked program) is not used.
    pub fn system(_debugger: String) -> io::Result<Self> {
        let guard = ExecGuard::start(Box::new(|pid, path| {
            let session = wtm_sys::process::session_of(pid);
            if session != 0 {
                let name = path.rsplit('/').next().unwrap_or(path);
                let _ = wtm_sys::sessions::send_message(
                    session,
                    "RustWTM",
                    &format!("{name} is blocked by your administrator and was not started."),
                );
            }
        }))
        .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self {
            guard: Mutex::new(Some(guard)),
            wanted: Mutex::default(),
            live: true,
        })
    }

    /// A blocker that only keeps the list (tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn detached() -> Self {
        Self {
            guard: Mutex::new(None),
            wanted: Mutex::default(),
            live: false,
        }
    }

    pub fn blocked(&self) -> BTreeSet<String> {
        self.wanted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn sync(&self, names: &[String]) -> io::Result<SyncReport> {
        let mut report = SyncReport::default();
        let mut wanted = BTreeSet::new();
        for n in names.iter().map(|n| normalize(n)) {
            if NEVER_BLOCKED.contains(&n.as_str()) {
                report.conflicts.push(n);
            } else if !n.is_empty() {
                wanted.insert(n);
            }
        }
        let mut current = self
            .wanted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        report.removed = current.difference(&wanted).cloned().collect();
        report.added = wanted.difference(&current).cloned().collect();
        if let Some(g) = self
            .guard
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
        {
            g.set(&wanted);
        }
        *current = wanted;
        let _ = self.live;
        Ok(report)
    }

    pub fn remove_all(&self) -> (Vec<String>, Vec<String>) {
        let removed: Vec<String> = self.blocked().into_iter().collect();
        let _ = self.sync(&[]);
        (removed, Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sync_reports_changes_and_refuses_core_programs() {
        let b = Blocker::detached();
        let r = b
            .sync(&["xmrig.exe".into(), "bash".into(), "miner".into()])
            .unwrap();
        assert_eq!(r.added, ["miner", "xmrig"]);
        assert_eq!(r.conflicts, ["bash"]);
        let r = b.sync(&["miner".into()]).unwrap();
        assert_eq!(r.removed, ["xmrig"]);
        assert_eq!(b.remove_all().0, ["miner"]);
        assert!(b.blocked().is_empty());
    }
}
