//! Why a process does not go away on Linux:
//! - it has exited but its parent has not collected it (a zombie): the
//!   parent is the "holder";
//! - it waits in the kernel uninterruptibly (`D`: a hung disk, NFS or CIFS
//!   server or driver), where even SIGKILL waits;
//! - it is stopped and traced by a debugger;
//! - this user may not signal it (another user's process);
//! - it is PID 1.

use crate::control::{Access, ProcessHandle};
use crate::linux::util::{self, Stat};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Holder {
    pub pid: u32,
    pub name: String,
    pub handles: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EndReport {
    pub deleting: bool,
    pub exited: bool,
    pub threads: u32,
    pub protection: Option<String>,
    pub critical: bool,
    pub can_end: Option<bool>,
    pub open_error: Option<String>,
    pub holders: Vec<Holder>,
    pub holders_known: bool,
    pub guards: Vec<(String, String)>,
}

pub fn protection_label(protection: u8) -> String {
    let level = match protection & 0x07 {
        1 => "light protection",
        2 => "full protection",
        _ => "no protection",
    };
    level.to_owned()
}

fn name_of(pid: u32) -> String {
    Stat::read(pid)
        .map(|s| crate::process::process_name(pid, &s.comm))
        .unwrap_or_else(|_| pid.to_string())
}

pub fn why_alive(pid: u32, create_time: i64) -> crate::Result<EndReport> {
    let stat = Stat::read(pid).map_err(|e| crate::Error::io("stat", e))?;
    if create_time != 0 && stat.create_time() != create_time {
        return Err(crate::Error::Reused { pid });
    }
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).unwrap_or_default();
    let tracer: u32 = util::kv_text(&status, "TracerPid")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let zombie = matches!(stat.state, 'Z' | 'X');
    let mut holders = Vec::new();
    if zombie && stat.ppid != 0 {
        holders.push(Holder {
            pid: stat.ppid,
            name: name_of(stat.ppid),
            handles: 1,
        });
    }
    if tracer != 0 {
        holders.push(Holder {
            pid: tracer,
            name: name_of(tracer),
            handles: 1,
        });
    }
    let mut guards = Vec::new();
    if stat.state == 'D' {
        let wchan = util::numeric_entries(&format!("/proc/{pid}/task"))
            .into_iter()
            .find_map(|tid| crate::threads::wait_channel(pid, tid))
            .unwrap_or_else(|| "kernel".into());
        guards.push((
            wchan,
            "waits in the kernel uninterruptibly; it ends once that wait returns".into(),
        ));
    }
    // kill(pid, 0) tells whether this process may signal it.
    // SAFETY: signal 0 only checks permissions.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    let (can_end, open_error) = match ProcessHandle::open(pid, Access::TERMINATE) {
        Ok(_) if rc == 0 => (Some(true), None),
        Ok(_) if errno == libc::EPERM => (Some(false), None),
        Ok(_) => (Some(true), None),
        Err(e) => (None, Some(e.to_string())),
    };
    Ok(EndReport {
        deleting: zombie,
        exited: zombie,
        threads: stat.threads,
        protection: None,
        critical: pid == 1,
        can_end,
        open_error,
        holders,
        holders_known: true,
        guards,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explains_a_zombie() {
        let child = std::process::Command::new("true").spawn().unwrap();
        let pid = child.id();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let r = why_alive(pid, 0).unwrap();
        assert!(r.exited && r.deleting);
        assert_eq!(r.holders[0].pid, std::process::id());
        drop(child);
        let me = why_alive(std::process::id(), 0).unwrap();
        assert_eq!(me.can_end, Some(true));
    }
}
