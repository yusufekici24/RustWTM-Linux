//! Why a program does not respond, on Linux: threads in uninterruptible
//! sleep (`D`, usually waiting for a disk, NFS or CIFS server or a driver)
//! and the kernel function each waits in (wchan). There is no window-level
//! "not responding" flag a task manager can read under X11 or Wayland.

use std::time::{Duration, Instant};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HungWindow {
    pub pid: u32,
    pub tid: u32,
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum WaitKind {
    CriticalSection,
    SendMessage,
    Mutex,
    Alpc,
    Com,
    Thread,
    Process,
    ComActivation,
    Socket,
    Smb,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LinkStatus {
    Running,
    Blocked,
    Owned,
    Abandoned,
    NoAccess,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Link {
    Thread {
        pid: u32,
        tid: u32,
        status: LinkStatus,
    },
    Object {
        kind: WaitKind,
        name: String,
        status: LinkStatus,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Chain {
    pub links: Vec<Link>,
    pub cycle: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WaitReport {
    pub chains: Vec<Chain>,
    pub threads: usize,
    pub analyzed: usize,
    pub skipped: usize,
    #[serde(default)]
    pub complete: bool,
    #[serde(default)]
    pub busy: bool,
}

impl WaitReport {
    pub fn deadlocked(&self) -> bool {
        self.chains.iter().any(|c| c.cycle)
    }
}

pub fn hung_windows() -> Vec<HungWindow> {
    Vec::new()
}

/// What the kernel function a thread waits in says about the wait.
fn kind_of(wchan: &str) -> WaitKind {
    if wchan.contains("futex") {
        WaitKind::Mutex
    } else if wchan.contains("cifs") || wchan.contains("smb") || wchan.contains("nfs") {
        WaitKind::Smb
    } else if wchan.contains("sk_")
        || wchan.contains("tcp")
        || wchan.contains("unix_")
        || wchan.contains("inet")
    {
        WaitKind::Socket
    } else if wchan.contains("wait4") || wchan.contains("do_wait") {
        WaitKind::Process
    } else {
        WaitKind::Unknown
    }
}

pub fn wait_chains(pid: u32, budget: Duration) -> crate::Result<WaitReport> {
    let started = Instant::now();
    let tids = crate::linux::util::numeric_entries(&format!("/proc/{pid}/task"));
    if tids.is_empty() {
        return Err(crate::Error::Os {
            call: "wait chains",
            errno: libc::ESRCH,
        });
    }
    let mut report = WaitReport {
        chains: Vec::new(),
        threads: tids.len(),
        analyzed: 0,
        skipped: 0,
        complete: true,
        busy: false,
    };
    for tid in tids {
        if started.elapsed() > budget {
            report.complete = false;
            report.skipped += 1;
            continue;
        }
        report.analyzed += 1;
        let Ok(stat) = crate::linux::util::Stat::read_task(pid, tid) else {
            continue;
        };
        if stat.state != 'D' {
            continue;
        }
        let wchan = crate::threads::wait_channel(pid, tid).unwrap_or_else(|| "kernel".into());
        report.chains.push(Chain {
            links: vec![
                Link::Thread {
                    pid,
                    tid,
                    status: LinkStatus::Blocked,
                },
                Link::Object {
                    kind: kind_of(&wchan),
                    name: wchan,
                    status: LinkStatus::Blocked,
                },
            ],
            cycle: false,
        });
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_self() {
        let r = wait_chains(std::process::id(), Duration::from_secs(1)).unwrap();
        assert!(r.threads >= 1 && r.complete && !r.deadlocked());
        assert_eq!(kind_of("futex_wait_queue"), WaitKind::Mutex);
    }
}
