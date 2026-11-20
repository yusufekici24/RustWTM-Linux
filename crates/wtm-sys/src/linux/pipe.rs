//! Local IPC over Unix domain sockets, standing in for Windows named pipes.
//!
//! A pipe name `\\.\pipe\NAME` maps to a socket path:
//! - `RustWTM` (the service) to `/run/rustwtm/rustwtm.sock`, a root-owned
//!   directory nobody else can create sockets in, so the name cannot be
//!   squatted;
//! - `docker_engine` to `/var/run/docker.sock`;
//! - an absolute path to itself;
//! - anything else to `$XDG_RUNTIME_DIR/rustwtm/NAME.sock` (root:
//!   `/run/rustwtm/NAME.sock`).
//!
//! The listener's SDDL decides the socket's mode: one that lets
//! interactive users (IU), everyone (WD) or users (BU/AU) in gives 0666,
//! anything else 0600. Peers are identified with SO_PEERCRED.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use crate::linux::util::{self, last_os};
use crate::{Error, Result};

/// Kept for the Windows API's sake; Linux checks the socket's mode.
pub const CLIENT_ACCESS: u32 = 0x0012_008B;

pub(crate) const SERVICE_DIR: &str = "/run/rustwtm";

pub fn socket_path(name: &str) -> PathBuf {
    if name.starts_with('/') {
        return PathBuf::from(name);
    }
    let short = name.strip_prefix(r"\\.\pipe\").unwrap_or(name);
    if short.eq_ignore_ascii_case("RustWTM") {
        return PathBuf::from(SERVICE_DIR).join("rustwtm.sock");
    }
    if short == "docker_engine" {
        return PathBuf::from("/var/run/docker.sock");
    }
    let safe: String = short
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "-_.".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let dir = if util::is_root() {
        PathBuf::from(SERVICE_DIR)
    } else {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("rustwtm")
    };
    dir.join(format!("{safe}.sock"))
}

pub fn open_client(name: &str) -> io::Result<File> {
    let stream = UnixStream::connect(socket_path(name))?;
    Ok(File::from(OwnedFd::from(stream)))
}

/// Whether the SDDL lets non-administrators connect.
pub(crate) fn sddl_lets_users_in(sddl: &str) -> bool {
    [";;;IU)", ";;;WD)", ";;;BU)", ";;;AU)"]
        .iter()
        .any(|t| sddl.contains(t))
}

#[derive(Debug)]
pub struct PipeListener {
    listener: UnixListener,
    path: PathBuf,
}

impl PipeListener {
    pub fn bind(name: &str, sddl: &str) -> Result<Self> {
        let path = socket_path(name);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io("create socket dir", e))?;
            if dir == std::path::Path::new(SERVICE_DIR) {
                let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755));
            }
        }
        // A socket left by a previous run refuses connections; one in use
        // accepts them and must not be taken over.
        if path.exists() {
            if UnixStream::connect(&path).is_ok() {
                return Err(Error::Os {
                    call: "bind",
                    errno: libc::EADDRINUSE,
                });
            }
            let _ = std::fs::remove_file(&path);
        }
        let listener = UnixListener::bind(&path).map_err(|e| Error::io("bind", e))?;
        let mode = if sddl_lets_users_in(sddl) {
            0o666
        } else {
            0o600
        };
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))
            .map_err(|e| Error::io("chmod socket", e))?;
        Ok(Self { listener, path })
    }

    pub fn accept(&mut self) -> Result<PipeConnection> {
        let (stream, _) = self.listener.accept().map_err(|e| Error::io("accept", e))?;
        Ok(PipeConnection { stream })
    }
}

impl Drop for PipeListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn peer(fd: i32) -> Result<libc::ucred> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: cred is writable for len bytes.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(last_os("getsockopt(SO_PEERCRED)"));
    }
    Ok(cred)
}

pub fn server_pid(client: &File) -> Result<u32> {
    Ok(peer(client.as_raw_fd())?.pid as u32)
}

#[derive(Debug)]
pub struct PipeConnection {
    stream: UnixStream,
}

/// The SID Samba gives a Unix UID, so code that compares SIDs keeps
/// working: `S-1-22-1-<uid>`.
pub(crate) fn uid_sid(uid: u32) -> String {
    format!("S-1-22-1-{uid}")
}

/// Root, or a member of the groups that may administer the machine.
pub(crate) fn uid_is_admin(uid: u32, gid: u32) -> bool {
    if uid == 0 {
        return true;
    }
    let Some(name) = util::user_name(uid) else {
        return false;
    };
    let Ok(c) = std::ffi::CString::new(name) else {
        return false;
    };
    let mut groups = vec![0 as libc::gid_t; 256];
    let mut n = groups.len() as i32;
    // SAFETY: groups is writable for n entries.
    let rc = unsafe { libc::getgrouplist(c.as_ptr(), gid, groups.as_mut_ptr(), &mut n) };
    if rc < 0 {
        return false;
    }
    groups.truncate(n.max(0) as usize);
    const ADMIN: [&str; 4] = ["sudo", "admin", "wheel", "rustwtm-admins"];
    groups
        .iter()
        .any(|g| group_name(*g).is_some_and(|n| ADMIN.contains(&n.as_str())))
}

pub(crate) fn group_name(gid: u32) -> Option<String> {
    let mut buf = vec![0u8; 8192];
    // SAFETY: zeroed group is a valid out-parameter.
    let mut grp: libc::group = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::group = std::ptr::null_mut();
    // SAFETY: all pointers valid for the call.
    let rc = unsafe {
        libc::getgrgid_r(
            gid,
            &mut grp,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    // SAFETY: gr_name points into buf.
    Some(
        unsafe { std::ffi::CStr::from_ptr(grp.gr_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

impl PipeConnection {
    pub fn client_pid(&self) -> Result<u32> {
        Ok(peer(self.stream.as_raw_fd())?.pid as u32)
    }

    pub fn client_session_id(&self) -> Result<u32> {
        let cred = peer(self.stream.as_raw_fd())?;
        // The user slice of the client; a client outside one (sudo in a
        // root shell, a system unit) is the session of its UID.
        match crate::process::session_of(cred.pid as u32) {
            0 => Ok(cred.uid),
            s => Ok(s),
        }
    }

    /// Runs `f` with the client's file-system identity (setfsuid /
    /// setfsgid on this thread), so file checks inside are the client's.
    pub fn as_client<T>(&self, f: impl FnOnce() -> T) -> Result<T> {
        let cred = peer(self.stream.as_raw_fd())?;
        if !util::is_root() {
            return Ok(f());
        }
        // SAFETY: setfsuid / setfsgid only change this thread's fs ids.
        let (old_uid, old_gid) = unsafe {
            let g = libc::setfsgid(cred.gid);
            let u = libc::setfsuid(cred.uid);
            (u as u32, g as u32)
        };
        let result = f();
        // SAFETY: restores the ids saved above.
        unsafe {
            libc::setfsuid(old_uid);
            libc::setfsgid(old_gid);
        }
        Ok(result)
    }

    pub fn client_user_sid(&self) -> Result<String> {
        Ok(uid_sid(peer(self.stream.as_raw_fd())?.uid))
    }

    pub fn client_is_admin(&self) -> Result<bool> {
        let cred = peer(self.stream.as_raw_fd())?;
        Ok(uid_is_admin(cred.uid, cred.gid))
    }
}

impl Read for PipeConnection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.stream.read(buf)
    }
}

impl Write for PipeConnection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stream.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_peer() {
        let name = format!(r"\\.\pipe\wtm-sys-test-{}", std::process::id());
        let mut l = PipeListener::bind(&name, "D:P(A;;GA;;;SY)(A;;0x12008b;;;IU)").unwrap();
        let t = std::thread::spawn(move || {
            let mut c = l.accept().unwrap();
            assert_eq!(c.client_pid().unwrap(), std::process::id());
            let mut b = [0u8; 5];
            c.read_exact(&mut b).unwrap();
            c.write_all(&b).unwrap();
            c.client_session_id().unwrap();
        });
        let mut f = open_client(&name).unwrap();
        assert_eq!(server_pid(&f).unwrap(), std::process::id());
        f.write_all(b"hello").unwrap();
        let mut b = [0u8; 5];
        f.read_exact(&mut b).unwrap();
        assert_eq!(&b, b"hello");
        t.join().unwrap();
    }

    #[test]
    fn maps_names() {
        assert_eq!(
            socket_path(r"\\.\pipe\RustWTM"),
            PathBuf::from("/run/rustwtm/rustwtm.sock")
        );
        assert_eq!(
            socket_path(r"\\.\pipe\docker_engine"),
            PathBuf::from("/var/run/docker.sock")
        );
        assert!(!sddl_lets_users_in("D:P(A;;GA;;;SY)"));
        assert_eq!(uid_sid(1000), "S-1-22-1-1000");
    }
}
