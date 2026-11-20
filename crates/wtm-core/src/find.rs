//! Process Explorer's "Find handle or DLL": every process whose loaded
//! modules or open handles have a name containing the search text.
//!
//! Processes are searched one at a time, with the same rights and limits as
//! the inspector's Modules and Handles views: this program's own rights,
//! never the service's; lsass.exe and lsaiso.exe are never opened. A
//! process's handle names are read under the Handles view's caps, and file
//! names share its single helper thread, so a file that never answers
//! leaves later processes' file names unread for the rest of the search (a
//! Handles panel opened meanwhile says so too). The search reports what it
//! could not read, so that no hits never claims more than it checked.
//! Cancelling takes effect between processes: within about three seconds.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::control::{self, ProcessRef};

/// What matched: a loaded module, or a handle of the given object type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HitKind {
    Dll,
    Handle(String),
}

/// One match: the process (with its start time, so a reused process ID is
/// never mistaken for it) and the name that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindHit {
    pub process: ProcessRef,
    pub kind: HitKind,
    pub name: String,
}

/// How far a search got and what it could not read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindProgress {
    pub searched: usize,
    pub total: usize,
    /// Processes that could not be opened, or that hold the PC's sign-in
    /// secrets and are never opened.
    pub unreadable: usize,
    /// Processes whose handle names were not all read.
    pub incomplete: usize,
    pub hits: usize,
    /// Stopped at the hit cap or the time limit; the rest was not searched.
    pub capped: bool,
    pub timed_out: bool,
    pub done: bool,
}

/// Caps a search; tests make them small.
#[derive(Debug, Clone, Copy)]
pub struct FindLimits {
    pub hits: usize,
    pub budget: Duration,
}

impl Default for FindLimits {
    fn default() -> Self {
        Self {
            hits: 1_000,
            budget: Duration::from_secs(60),
        }
    }
}

/// Texts shorter than this match too much to be worth a search.
pub const SHORTEST_QUERY: usize = 3;

/// Searches every process for `query` (ignoring case). `report` is called
/// after each process with its hits and the progress so far, and once more
/// when the search ends; it returns the final progress.
pub fn find(
    query: &str,
    cancel: &AtomicBool,
    limits: FindLimits,
    report: impl FnMut(Vec<FindHit>, &FindProgress),
) -> FindProgress {
    // The idle and System processes hold no modules or handles of their own
    // that this program may read.
    let processes = || {
        wtm_sys::process::ProcessSnapshotter::new()
            .snapshot()
            .unwrap_or_default()
            .into_iter()
            .filter(|p| p.pid > 4)
            .map(|p| ProcessRef {
                pid: p.pid,
                create_time: p.create_time,
                name: p.name,
            })
            .collect()
    };
    find_in(query, processes, cancel, limits, report)
}

/// [`find`] over the processes `list` gives; tests search only their own.
pub(crate) fn find_in(
    query: &str,
    list: impl FnOnce() -> Vec<ProcessRef>,
    cancel: &AtomicBool,
    limits: FindLimits,
    mut report: impl FnMut(Vec<FindHit>, &FindProgress),
) -> FindProgress {
    let needle = query.trim().to_lowercase();
    let mut progress = FindProgress::default();
    if needle.chars().count() < SHORTEST_QUERY {
        progress.done = true;
        report(Vec::new(), &progress);
        return progress;
    }
    let start = Instant::now();
    let processes = list();
    progress.total = processes.len();
    for target in processes {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        if start.elapsed() > limits.budget {
            progress.timed_out = true;
            break;
        }
        let mut hits = Vec::new();
        match search_one(&target, &needle) {
            Some((found, incomplete)) => {
                hits = found;
                progress.incomplete += usize::from(incomplete);
            }
            None => progress.unreadable += 1,
        }
        progress.searched += 1;
        let room = limits.hits.saturating_sub(progress.hits);
        if hits.len() >= room {
            hits.truncate(room);
            progress.capped = true;
        }
        progress.hits += hits.len();
        let full = progress.capped;
        report(hits, &progress);
        if full {
            break;
        }
    }
    progress.done = true;
    report(Vec::new(), &progress);
    progress
}

/// The matches in one process and whether its handle names were not all
/// read; `None` when it could not be searched at all.
fn search_one(target: &ProcessRef, needle: &str) -> Option<(Vec<FindHit>, bool)> {
    let modules = control::modules_checked(target).ok();
    let handles = control::handles(target).ok();
    if modules.is_none() && handles.is_none() {
        return None;
    }
    let modules_unread = modules.is_none();
    let matches = |name: &str| name.to_lowercase().contains(needle);
    let mut hits: Vec<FindHit> = modules
        .unwrap_or_default()
        .into_iter()
        .filter(|m| matches(&m.path))
        .map(|m| FindHit {
            process: target.clone(),
            kind: HitKind::Dll,
            name: m.path,
        })
        .collect();
    // A module list that could not be read (it can fail while the process
    // loads DLLs) leaves the process searched only in part, too.
    let incomplete = modules_unread
        || handles
            .as_ref()
            .is_none_or(|r| r.names_incomplete || !r.can_name || r.too_many());
    // Only the matches are kept; the full list goes at once.
    for h in handles.map(|r| r.handles).unwrap_or_default() {
        if let Some(name) = h.name.filter(|n| matches(n)) {
            hits.push(FindHit {
                process: target.clone(),
                kind: HitKind::Handle(h.kind),
                name,
            });
        }
    }
    Some((hits, incomplete))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> ProcessRef {
        let created = wtm_sys::control::ProcessHandle::open(
            std::process::id(),
            wtm_sys::control::Access::QUERY,
        )
        .unwrap()
        .create_time()
        .unwrap();
        ProcessRef {
            pid: std::process::id(),
            create_time: created,
            name: "me.exe".into(),
        }
    }

    /// Searches this test process only: other programs' handles and modules
    /// are their own business, and a full search takes seconds.
    #[cfg(target_os = "linux")]
    #[test]
    fn finds_its_own_shared_objects() {
        let (hits, progress) = own_search("libc.so", FindLimits::default());
        assert!(hits.iter().any(|h| h.kind == HitKind::Dll), "{hits:?}");
        assert!(progress.done);
    }

    fn own_search(query: &str, limits: FindLimits) -> (Vec<FindHit>, FindProgress) {
        let cancel = AtomicBool::new(false);
        let mut all = Vec::new();
        let progress = find_in(
            query,
            || vec![me()],
            &cancel,
            limits,
            |hits, _| all.extend(hits),
        );
        (all, progress)
    }

    #[cfg(windows)]
    #[test]
    fn finds_its_own_file_and_dll() {
        let unique = format!("rustwtm-find-{}", std::process::id());
        let dir = std::env::temp_dir().join(&unique);
        std::fs::create_dir_all(&dir).unwrap();
        let file = std::fs::File::create(dir.join("needle.txt")).unwrap();
        // Other tests in this binary read handles too, and file names share
        // one helper: a search that overlapped them says its names are
        // incomplete, and is asked again.
        let deadline = Instant::now() + Duration::from_secs(20);
        let (hits, progress) = loop {
            let (hits, progress) = own_search(&unique, FindLimits::default());
            if progress.incomplete == 0 || Instant::now() > deadline {
                break (hits, progress);
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert_eq!(progress.incomplete, 0);
        let me = std::process::id();
        assert!(
            hits.iter().any(|h| h.process.pid == me
                && h.kind == HitKind::Handle("File".into())
                && h.name.to_lowercase().ends_with("needle.txt")),
            "{hits:?}"
        );
        assert!(hits.iter().all(|h| h.process.create_time != 0));
        assert!(progress.done && progress.searched == 1 && !progress.capped);
        // The DLLs it loaded match by path.
        let (dlls, _) = own_search("ntdll.dll", FindLimits::default());
        assert!(dlls.iter().any(|h| h.kind == HitKind::Dll));
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn dlls_match_by_path_and_hits_stop_at_the_cap() {
        let (hits, progress) = own_search(
            ".dll",
            FindLimits {
                hits: 2,
                ..FindLimits::default()
            },
        );
        assert_eq!(hits.len(), 2);
        assert!(hits.iter().all(|h| h.kind == HitKind::Dll));
        assert!(progress.capped && progress.hits == 2 && progress.done);
    }

    #[test]
    fn short_texts_and_cancelled_searches_find_nothing() {
        let (hits, progress) = own_search(" ab ", FindLimits::default());
        assert!(hits.is_empty() && progress.done && progress.total == 0);
        let cancel = AtomicBool::new(true);
        let mut found = 0;
        let progress = find_in(
            "ntdll",
            || vec![me()],
            &cancel,
            FindLimits::default(),
            |hits, _| found += hits.len(),
        );
        assert_eq!((found, progress.searched), (0, 0));
        assert!(progress.done);
    }

    #[test]
    fn a_32_bit_processs_own_dlls_are_searched() {
        let cmd = std::path::Path::new(r"C:\Windows\SysWOW64\cmd.exe");
        if !cmd.exists() {
            return;
        }
        // `pause` waits on the open pipe until the child is ended.
        let mut child = std::process::Command::new(cmd)
            .args(["/d", "/c", "pause"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let created =
            wtm_sys::control::ProcessHandle::open(child.id(), wtm_sys::control::Access::QUERY)
                .unwrap()
                .create_time()
                .unwrap();
        let target = ProcessRef {
            pid: child.id(),
            create_time: created,
            name: "cmd.exe".into(),
        };
        let cancel = AtomicBool::new(false);
        // The 32-bit loader maps SysWOW64's DLLs as the child starts.
        let deadline = Instant::now() + Duration::from_secs(5);
        let found = loop {
            let mut hits = Vec::new();
            find_in(
                r"syswow64\kernel32.dll",
                || vec![target.clone()],
                &cancel,
                FindLimits::default(),
                |h, _| hits.extend(h),
            );
            if !hits.is_empty() || Instant::now() > deadline {
                break hits;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(found.iter().any(|h| h.kind == HitKind::Dll), "{found:?}");
    }

    #[test]
    fn the_sign_in_secrets_holders_are_never_opened() {
        let me = ProcessRef {
            pid: std::process::id(),
            create_time: 0,
            name: "lsass.exe".into(),
        };
        // Refused by name before anything is opened, for modules too.
        assert!(matches!(
            control::modules_checked(&me),
            Err(control::ControlError::Protected { .. })
        ));
        assert!(search_one(&me, "ntdll").is_none());
    }
}
