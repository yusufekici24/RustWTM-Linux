//! Thread helpers. Linux threads have no start address to read from
//! outside; the wait reasons are the codes [`crate::process`] assigns.

use crate::{Error, Result};

pub fn win32_start_address(_tid: u32) -> Result<u64> {
    Err(Error::Unsupported("thread start addresses"))
}

pub fn current_thread_id() -> u32 {
    // SAFETY: gettid takes no arguments.
    unsafe { libc::syscall(libc::SYS_gettid) as u32 }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleRange {
    pub name: String,
    pub base: u64,
    pub size: u64,
}

pub fn symbolize(address: u64, modules: &[ModuleRange]) -> String {
    modules
        .iter()
        .find(|m| address >= m.base && address - m.base < m.size)
        .map_or_else(
            || format!("0x{address:x}"),
            |m| format!("{}+0x{:x}", m.name, address - m.base),
        )
}

/// The reason codes `process::threads` uses for the Linux task states.
pub fn wait_reason_name(reason: u32) -> String {
    match reason {
        0 => "Uninterruptible (I/O)".into(),
        5 => "Stopped".into(),
        15 => "Sleeping".into(),
        other => other.to_string(),
    }
}

/// The kernel function a thread waits in (/proc/PID/task/TID/wchan), such
/// as `do_epoll_wait` or `futex_wait_queue`; `None` while it runs.
pub fn wait_channel(pid: u32, tid: u32) -> Option<String> {
    let w = std::fs::read_to_string(format!("/proc/{pid}/task/{tid}/wchan")).ok()?;
    let w = w.trim();
    (!w.is_empty() && w != "0").then(|| w.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_ids() {
        let m = [ModuleRange {
            name: "app".into(),
            base: 0x1000,
            size: 0x100,
        }];
        assert_eq!(symbolize(0x10ff, &m), "app+0xff");
        assert_eq!(symbolize(0x1100, &m), "0x1100");
        assert!(current_thread_id() > 0);
        assert_eq!(wait_reason_name(15), "Sleeping");
    }
}
