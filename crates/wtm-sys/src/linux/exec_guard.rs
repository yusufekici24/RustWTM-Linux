//! Keeping programs from starting, the Linux counterpart of Image File
//! Execution Options: a fanotify group with `FAN_OPEN_EXEC_PERM` on every
//! mounted filesystem asks this process before any file is opened to be
//! executed; files whose name is on the list are refused (the caller gets
//! EPERM), everything else is allowed at once.
//!
//! The answer thread does nothing but read the event, compare a name and
//! reply, so exec on the machine is never held up; when this process ends,
//! the kernel allows whatever was pending. Needs CAP_SYS_ADMIN (root).

use std::collections::BTreeSet;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use crate::linux::util::last_os;
use crate::{Error, Result};

/// What a refusal reports: PID of the process that tried, and the path.
pub type OnBlock = Box<dyn Fn(u32, &str) + Send + Sync>;

/// Only the answering thread holds the fanotify descriptor, in a descriptor
/// table of its own (see `open_group`): if it ever stops (or panics), the
/// group closes with it and the kernel allows every pending and future exec,
/// so the machine can never hang on it.
pub struct ExecGuard {
    names: Arc<RwLock<BTreeSet<String>>>,
    stop: Arc<AtomicBool>,
}

impl std::fmt::Debug for ExecGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExecGuard")
            .field("names", &self.names)
            .finish_non_exhaustive()
    }
}

/// Names that are never refused, whatever the list says: refusing them
/// would lock the machine or RustWTM itself out.
pub const NEVER_BLOCKED: &[&str] = &[
    "systemd",
    "init",
    "sh",
    "bash",
    "dash",
    "zsh",
    "sudo",
    "su",
    "pkexec",
    "login",
    "sshd",
    "agetty",
    "gdm3",
    "gdm-session-worker",
    "gnome-shell",
    "gnome-session-binary",
    "Xorg",
    "Xwayland",
    "dbus-daemon",
    "dbus-broker",
    "polkitd",
    "systemctl",
    "apt",
    "apt-get",
    "dpkg",
    "env",
    "ld-linux-x86-64.so.2",
    "rustwtm",
    "wtm",
    "wtm-service",
    "runuser",
    "busctl",
    "nft",
];

fn mount_points() -> Vec<String> {
    // One mark per filesystem (FAN_MARK_FILESYSTEM) covers its mounts.
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for m in crate::disk::mounts() {
        let skip = [
            "proc",
            "sysfs",
            "devpts",
            "cgroup2",
            "cgroup",
            "securityfs",
            "debugfs",
            "tracefs",
            "configfs",
            "bpf",
            "pstore",
            "efivarfs",
            "fusectl",
            "mqueue",
            "hugetlbfs",
            "binfmt_misc",
            "autofs",
            "nsfs",
            "rpc_pipefs",
        ];
        if skip.contains(&m.fstype.as_str()) || !seen.insert(m.dev.clone()) {
            continue;
        }
        out.push(m.point);
    }
    out
}

/// Opens the fanotify group in a file-descriptor table private to the
/// calling thread. Any program another thread of this process starts gets a
/// copy of the process's table until its exec completes; if the group
/// descriptor were in it, a child blocked on its own exec permission would
/// keep the group alive after this thread stopped answering, and every exec
/// on the machine would hang. With a private table nobody else ever holds the
/// group: when this thread ends (or the process dies), the kernel allows
/// everything again. The calling thread must never start programs.
fn open_group() -> Result<OwnedFd> {
    // SAFETY: plain syscalls on the calling thread's own table.
    unsafe {
        if libc::unshare(libc::CLONE_FILES) != 0 {
            return Err(last_os("unshare(CLONE_FILES)"));
        }
        // Drop the copies of the process's descriptors (sockets, pipes) this
        // table now holds, so none of them is kept open by it.
        libc::close_range(3, u32::MAX, 0);
    }
    // SAFETY: plain syscall; the fd is owned below.
    let raw = unsafe {
        libc::fanotify_init(
            libc::FAN_CLASS_CONTENT
                | libc::FAN_CLOEXEC
                | libc::FAN_UNLIMITED_QUEUE
                | libc::FAN_UNLIMITED_MARKS,
            (libc::O_RDONLY | libc::O_CLOEXEC | libc::O_LARGEFILE) as u32,
        )
    };
    if raw < 0 {
        return Err(last_os("fanotify_init"));
    }
    // SAFETY: fresh fd owned here.
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    let mut marked = 0;
    for point in mount_points() {
        let Ok(c) = std::ffi::CString::new(point) else {
            continue;
        };
        // SAFETY: valid fd and C string.
        let rc = unsafe {
            libc::fanotify_mark(
                fd.as_raw_fd(),
                libc::FAN_MARK_ADD | libc::FAN_MARK_FILESYSTEM,
                libc::FAN_OPEN_EXEC_PERM,
                libc::AT_FDCWD,
                c.as_ptr(),
            )
        };
        if rc == 0 {
            marked += 1;
        }
    }
    if marked == 0 {
        return Err(Error::Message(
            "fanotify: no filesystem could be watched".into(),
        ));
    }
    Ok(fd)
}

impl ExecGuard {
    pub fn start(on_block: OnBlock) -> Result<Self> {
        let names = Arc::new(RwLock::new(BTreeSet::new()));
        let stop = Arc::new(AtomicBool::new(false));
        // Refusals are reported from a thread of their own: the callback may
        // start programs (notifications), and an exec started from the
        // answering thread would wait for that very thread.
        let (hits_tx, hits_rx) = std::sync::mpsc::channel::<(u32, String)>();
        std::thread::Builder::new()
            .name("exec-guard-report".into())
            .spawn(move || {
                for (pid, path) in hits_rx {
                    on_block(pid, &path);
                }
            })
            .map_err(|e| Error::io("spawn exec-guard-report", e))?;
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel::<Result<()>>(1);
        let (n, s) = (Arc::clone(&names), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("exec-guard".into())
            .spawn(move || {
                let fd = match open_group() {
                    Ok(fd) => {
                        let _ = ready_tx.send(Ok(()));
                        fd
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                        return;
                    }
                };
                answer(&fd, &n, &s, &hits_tx);
            })
            .map_err(|e| Error::io("spawn exec-guard", e))?;
        ready_rx
            .recv()
            .map_err(|_| Error::Message("exec-guard thread ended".into()))??;
        Ok(Self { names, stop })
    }

    /// Replaces the list of refused file names (compared exactly, as
    /// Linux names are case-sensitive).
    pub fn set(&self, names: &BTreeSet<String>) {
        let mut w = self
            .names
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *w = names
            .iter()
            .filter(|n| !NEVER_BLOCKED.contains(&n.as_str()))
            .cloned()
            .collect();
    }

    pub fn blocked(&self) -> BTreeSet<String> {
        self.names
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Drop for ExecGuard {
    fn drop(&mut self) {
        // The thread sees it within half a second and closes the group.
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn answer(
    fd: &OwnedFd,
    names: &RwLock<BTreeSet<String>>,
    stop: &AtomicBool,
    hits: &std::sync::mpsc::Sender<(u32, String)>,
) {
    let mut buf = vec![0u8; 64 * 1024];
    let meta = size_of::<libc::fanotify_event_metadata>();
    while !stop.load(Ordering::Relaxed) {
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut pfd, 1, 500) };
        if ready <= 0 {
            continue;
        }
        // SAFETY: buf is writable for its length.
        let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            continue;
        }
        let mut at = 0usize;
        while at + meta <= n as usize {
            // SAFETY: the kernel wrote whole event records; read_unaligned
            // copes with any alignment.
            let ev: libc::fanotify_event_metadata =
                unsafe { std::ptr::read_unaligned(buf.as_ptr().add(at).cast()) };
            if ev.event_len < meta as u32 {
                break;
            }
            at += ev.event_len as usize;
            if ev.fd < 0 {
                continue;
            }
            // SAFETY: the event carries an fd we now own.
            let file = unsafe { OwnedFd::from_raw_fd(ev.fd) };
            // thread-self: the event fd is in this thread's own table.
            let path = std::fs::read_link(format!("/proc/thread-self/fd/{}", file.as_raw_fd()))
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let name = path.rsplit('/').next().unwrap_or("");
            let deny = ev.mask & libc::FAN_OPEN_EXEC_PERM != 0
                && !name.is_empty()
                && names.read().map(|s| s.contains(name)).unwrap_or(false);
            let reply = libc::fanotify_response {
                fd: file.as_raw_fd(),
                response: if deny {
                    libc::FAN_DENY
                } else {
                    libc::FAN_ALLOW
                },
            };
            // SAFETY: writes the plain response struct.
            unsafe {
                libc::write(
                    fd.as_raw_fd(),
                    (&raw const reply).cast(),
                    size_of::<libc::fanotify_response>(),
                );
            }
            if deny {
                let _ = hits.send((ev.pid as u32, path));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_listed_names_as_root() {
        if !crate::linux::util::is_root() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("wtm-guard-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let blocked = dir.join("wtm-blocked-test-prog");
        std::fs::copy("/bin/true", &blocked).unwrap();
        let hits = Arc::new(std::sync::Mutex::new(Vec::new()));
        let h = Arc::clone(&hits);
        let guard = match ExecGuard::start(Box::new(move |pid, p| {
            h.lock().unwrap().push((pid, p.to_owned()))
        })) {
            Ok(g) => g,
            Err(e) => {
                eprintln!("fanotify unavailable here: {e}");
                return;
            }
        };
        guard.set(&BTreeSet::from([
            "wtm-blocked-test-prog".to_owned(),
            "sh".to_owned(),
        ]));
        assert!(
            !guard.blocked().contains("sh"),
            "never-blocked names are dropped"
        );
        let denied = std::process::Command::new(&blocked).status();
        assert!(denied.is_err() || !denied.unwrap().success());
        assert!(
            std::process::Command::new("/bin/true")
                .status()
                .unwrap()
                .success()
        );
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            hits.lock()
                .unwrap()
                .iter()
                .any(|(_, p)| p.ends_with("wtm-blocked-test-prog"))
        );
        drop(guard);
        std::thread::sleep(std::time::Duration::from_millis(700));
        assert!(
            std::process::Command::new(&blocked)
                .status()
                .unwrap()
                .success()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
