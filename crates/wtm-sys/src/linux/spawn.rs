//! Starting programs "under" another process: as that process's user (so
//! a program started for the desktop user runs with that user's rights
//! even when RustWTM runs as root), optionally stopped until resumed.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use crate::control::{Access, ProcessHandle};
use crate::{Error, Result};

#[derive(Debug)]
pub struct Spawned {
    pub pid: u32,
    child: Mutex<Child>,
}

impl Spawned {
    pub fn resume(&self) -> Result<()> {
        // SAFETY: plain signal to our own child.
        if unsafe { libc::kill(self.pid as i32, libc::SIGCONT) } != 0 {
            return Err(crate::linux::util::last_os("kill"));
        }
        Ok(())
    }

    pub fn discard(&self) -> Result<()> {
        let mut c = self
            .child
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        c.kill().map_err(|e| Error::io("kill", e))?;
        let _ = c.wait();
        Ok(())
    }

    pub fn wait(&self, timeout: Duration) -> Option<u32> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            {
                let mut c = self
                    .child
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if let Ok(Some(status)) = c.try_wait() {
                    return Some(status.code().unwrap_or(1) as u32);
                }
            }
            if std::time::Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn spawn(cmd: Command, suspended: bool) -> Result<Spawned> {
    // A stopped child must have exec'd already, or spawning would wait for
    // it forever: a small shell stops itself, then becomes the program.
    let mut cmd = if suspended {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", "kill -STOP $$; exec \"$@\"", "sh"])
            .arg(cmd.get_program())
            .args(cmd.get_args());
        c
    } else {
        cmd
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let child = cmd.spawn().map_err(|e| Error::io("spawn", e))?;
    Ok(Spawned {
        pid: child.id(),
        child: Mutex::new(child),
    })
}

pub fn with_parent(
    parent: u32,
    application: &Path,
    arguments: &str,
    suspended: bool,
) -> Result<Spawned> {
    let args = crate::shell::split_args(arguments);
    let uid = crate::linux::util::process_uid(parent).ok_or(Error::Os {
        call: "parent",
        errno: libc::ESRCH,
    })?;
    // SAFETY: getuid takes no arguments.
    let me = unsafe { libc::getuid() };
    let cmd = if uid != me && crate::linux::util::is_root() {
        let user = crate::linux::util::user_name(uid).ok_or(Error::Os {
            call: "getpwuid",
            errno: libc::ESRCH,
        })?;
        let mut c = Command::new("runuser");
        c.args(["-u", &user, "--"]).arg(application).args(args);
        c
    } else {
        let mut c = Command::new(application);
        c.args(args);
        c
    };
    spawn(cmd, suspended)
}

pub fn bypassing_debugger(application: &Path, arguments: &str) -> Result<Spawned> {
    let mut c = Command::new(application);
    c.args(crate::shell::split_args(arguments));
    spawn(c, false)
}

pub fn wait_for_exit(pid: u32, timeout: Duration) -> Option<bool> {
    let h = ProcessHandle::open(pid, Access::SYNCHRONIZE).ok()?;
    h.wait_exit(timeout).ok()
}

pub fn terminate(pid: u32) -> Result<()> {
    ProcessHandle::open(pid, Access::TERMINATE)?.terminate(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_suspended_and_resumes() {
        let s = with_parent(
            std::process::id(),
            Path::new("/bin/sh"),
            "-c 'exit 3'",
            true,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(crate::linux::util::Stat::read(s.pid).unwrap().state, 'T');
        s.resume().unwrap();
        assert_eq!(s.wait(Duration::from_secs(5)), Some(3));
    }
}
