//! Shared helpers for the Linux back end: procfs parsing, time units and
//! small file readers.
//!
//! The crates above were written against Windows units, so this back end
//! keeps them: times are FILETIME-style 100 ns units (creation times count
//! from 1601-01-01 UTC), sizes are bytes.

use std::fs;
use std::io;
use std::path::Path;
use std::sync::OnceLock;

/// 100 ns intervals between 1601-01-01 and 1970-01-01.
pub(crate) const UNIX_EPOCH_AS_FILETIME: i64 = 116_444_736_000_000_000;

/// Clock ticks per second (`sysconf(_SC_CLK_TCK)`), normally 100.
pub(crate) fn clock_ticks() -> u64 {
    static TICKS: OnceLock<u64> = OnceLock::new();
    *TICKS.get_or_init(|| {
        // SAFETY: sysconf takes no pointers.
        let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if t > 0 { t as u64 } else { 100 }
    })
}

/// Page size in bytes.
pub(crate) fn page_size() -> u64 {
    static PAGE: OnceLock<u64> = OnceLock::new();
    *PAGE.get_or_init(|| {
        // SAFETY: sysconf takes no pointers.
        let p = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if p > 0 { p as u64 } else { 4096 }
    })
}

/// Clock ticks to 100 ns units.
pub(crate) fn ticks_to_100ns(ticks: u64) -> u64 {
    ticks.saturating_mul(10_000_000) / clock_ticks()
}

/// Boot time in seconds since the Unix epoch (`btime` in /proc/stat).
pub(crate) fn boot_time() -> u64 {
    static BOOT: OnceLock<u64> = OnceLock::new();
    *BOOT.get_or_init(|| {
        fs::read_to_string("/proc/stat")
            .ok()
            .and_then(|s| {
                s.lines()
                    .find_map(|l| l.strip_prefix("btime ").map(|v| v.trim().parse().ok()))
                    .flatten()
            })
            .unwrap_or(0)
    })
}

/// A process start time (ticks after boot, field 22 of /proc/PID/stat) as a
/// FILETIME. Stable for the life of the process, so it identifies a PID
/// across reuse just like the Windows creation time.
pub(crate) fn start_ticks_to_filetime(ticks: u64) -> i64 {
    let boot = boot_time() as i64 * 10_000_000;
    UNIX_EPOCH_AS_FILETIME + boot + ticks_to_100ns(ticks) as i64
}

/// Now as a FILETIME.
pub(crate) fn now_filetime() -> i64 {
    let d = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    UNIX_EPOCH_AS_FILETIME + (d.as_nanos() / 100) as i64
}

pub(crate) fn read_trim(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

pub(crate) fn read_u64(path: impl AsRef<Path>) -> Option<u64> {
    read_trim(path)?.parse().ok()
}

pub(crate) fn read_i64(path: impl AsRef<Path>) -> Option<i64> {
    read_trim(path)?.parse().ok()
}

/// Values of a `Key: value [kB]` file (/proc/meminfo, /proc/PID/status) in
/// bytes when the unit is kB, raw otherwise.
pub(crate) fn kv_bytes(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|l| {
        let rest = l.strip_prefix(key)?.strip_prefix(':')?;
        let mut parts = rest.split_whitespace();
        let n: u64 = parts.next()?.parse().ok()?;
        Some(match parts.next() {
            Some("kB") => n * 1024,
            _ => n,
        })
    })
}

/// Raw text of a `Key: value` line.
pub(crate) fn kv_text<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix(':').map(str::trim))
}

/// Parsed /proc/PID/stat. The command name may contain spaces and
/// parentheses, so fields are split after the last `)`.
#[derive(Debug, Clone, Default)]
#[allow(dead_code)]
pub(crate) struct Stat {
    pub pid: u32,
    pub comm: String,
    pub state: char,
    pub ppid: u32,
    pub pgrp: i32,
    pub session: i32,
    pub tty: i32,
    pub flags: u32,
    pub minflt: u64,
    pub majflt: u64,
    pub utime: u64,
    pub stime: u64,
    pub priority: i64,
    pub nice: i64,
    pub threads: u32,
    pub starttime: u64,
    pub vsize: u64,
    pub rss_pages: u64,
    pub processor: i32,
    pub rt_priority: u32,
    pub policy: u32,
    pub blkio_ticks: u64,
}

/// `PF_KTHREAD`: the task is a kernel thread.
pub(crate) const PF_KTHREAD: u32 = 0x0020_0000;

impl Stat {
    pub(crate) fn parse(text: &str) -> Option<Self> {
        let open = text.find('(')?;
        let close = text.rfind(')')?;
        let pid = text[..open].trim().parse().ok()?;
        let comm = text[open + 1..close].to_owned();
        let f: Vec<&str> = text[close + 1..].split_whitespace().collect();
        // f[0] is field 3 (state).
        let n = |i: usize| -> u64 { f.get(i).and_then(|v| v.parse().ok()).unwrap_or(0) };
        let s = |i: usize| -> i64 { f.get(i).and_then(|v| v.parse().ok()).unwrap_or(0) };
        Some(Self {
            pid,
            comm,
            state: f.first().and_then(|v| v.chars().next()).unwrap_or('?'),
            ppid: n(1) as u32,
            pgrp: s(2) as i32,
            session: s(3) as i32,
            tty: s(4) as i32,
            flags: n(6) as u32,
            minflt: n(7),
            majflt: n(9),
            utime: n(11),
            stime: n(12),
            priority: s(15),
            nice: s(16),
            threads: n(17) as u32,
            starttime: n(19),
            vsize: n(20),
            rss_pages: n(21),
            processor: s(36) as i32,
            rt_priority: n(37) as u32,
            policy: n(38) as u32,
            blkio_ticks: n(39),
        })
    }

    pub(crate) fn read(pid: u32) -> io::Result<Self> {
        let text = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        Self::parse(&text).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad stat"))
    }

    pub(crate) fn read_task(pid: u32, tid: u32) -> io::Result<Self> {
        let text = fs::read_to_string(format!("/proc/{pid}/task/{tid}/stat"))?;
        Self::parse(&text).ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "bad stat"))
    }

    pub(crate) fn kernel_thread(&self) -> bool {
        self.flags & PF_KTHREAD != 0 || self.pid == 2 || self.ppid == 2
    }

    pub(crate) fn create_time(&self) -> i64 {
        start_ticks_to_filetime(self.starttime)
    }
}

/// Numeric directory entries of `dir` (PIDs under /proc, TIDs under task/).
pub(crate) fn numeric_entries(dir: &str) -> Vec<u32> {
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok())
        .collect()
}

/// User name for a UID from /etc/passwd (NSS-backed lookups via getpwuid_r
/// so domain users from SSSD resolve too).
pub(crate) fn user_name(uid: u32) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    // SAFETY: zeroed passwd is a valid out-parameter.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: all pointers are valid for the call; buf outlives the reads.
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        )
    };
    if rc != 0 || result.is_null() {
        return None;
    }
    // SAFETY: pw_name points into buf and is NUL-terminated.
    let name = unsafe { std::ffi::CStr::from_ptr(pwd.pw_name) };
    Some(name.to_string_lossy().into_owned())
}

/// UID for a user name.
#[allow(dead_code)]
pub(crate) fn user_id(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    let mut buf = vec![0u8; 4096];
    // SAFETY: zeroed passwd is a valid out-parameter.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: all pointers are valid for the call.
    let rc = unsafe {
        libc::getpwnam_r(
            c.as_ptr(),
            &mut pwd,
            buf.as_mut_ptr().cast(),
            buf.len(),
            &mut result,
        )
    };
    (rc == 0 && !result.is_null()).then_some(pwd.pw_uid)
}

/// Real UID of a process (first field of `Uid:` in /proc/PID/status).
pub(crate) fn process_uid(pid: u32) -> Option<u32> {
    let s = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    kv_text(&s, "Uid")?.split_whitespace().next()?.parse().ok()
}

/// Effective UID of a process.
pub(crate) fn process_euid(pid: u32) -> Option<u32> {
    let s = fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    kv_text(&s, "Uid")?.split_whitespace().nth(1)?.parse().ok()
}

pub(crate) fn is_root() -> bool {
    // SAFETY: geteuid takes no arguments.
    unsafe { libc::geteuid() == 0 }
}

/// Runs a program and returns stdout when it exits successfully.
pub(crate) fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Runs a program, mapping failure to an error carrying its stderr.
pub(crate) fn run_checked(program: &str, args: &[&str]) -> crate::Result<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| crate::Error::Message(format!("{program}: {e}")))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(crate::Error::Message(format!(
            "{program} {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// The interface language from the POSIX locale variables.
pub(crate) fn ui_language() -> String {
    let vars = ["LC_ALL", "LC_MESSAGES", "LANGUAGE", "LANG"];
    let value = vars
        .iter()
        .find_map(|v| {
            std::env::var(v)
                .ok()
                .filter(|s| !s.is_empty() && s != "C" && s != "POSIX")
        })
        .or_else(|| {
            // A system service has no session locale; use the machine's.
            fs::read_to_string("/etc/default/locale")
                .ok()
                .and_then(|s| {
                    s.lines().find_map(|l| {
                        l.strip_prefix("LANG=")
                            .map(|v| v.trim_matches('"').to_owned())
                    })
                })
        })
        .unwrap_or_default();
    let code = value.get(..2).unwrap_or("").to_ascii_lowercase();
    match code.as_str() {
        "tr" | "de" | "fr" | "es" => code,
        _ => "en".to_owned(),
    }
}

/// A short, human label for an errno.
pub(crate) fn errno_text(errno: i32) -> String {
    io::Error::from_raw_os_error(errno).to_string()
}

/// The calling thread's errno as an error from `call`.
pub(crate) fn last_os(call: &'static str) -> crate::Error {
    crate::Error::Os {
        call,
        errno: io::Error::last_os_error().raw_os_error().unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_stat_with_parentheses_in_name() {
        let line = "1234 (we (ird) name) S 1 1234 1234 0 -1 4194560 100 0 2 0 7 3 0 0 20 0 4 0 555 10000 200 18446744073709551615 0 0 0 0 0 0 0 0 0 0 0 0 17 3 0 0 0 0 0";
        let s = Stat::parse(line).unwrap();
        assert_eq!(s.pid, 1234);
        assert_eq!(s.comm, "we (ird) name");
        assert_eq!(s.state, 'S');
        assert_eq!(s.ppid, 1);
        assert_eq!(s.utime, 7);
        assert_eq!(s.stime, 3);
        assert_eq!(s.threads, 4);
        assert_eq!(s.starttime, 555);
        assert_eq!(s.rss_pages, 200);
        assert_eq!(s.processor, 3);
    }

    #[test]
    fn reads_own_process() {
        let me = std::process::id();
        let s = Stat::read(me).unwrap();
        assert_eq!(s.pid, me);
        assert!(s.create_time() > UNIX_EPOCH_AS_FILETIME);
        assert!(s.create_time() <= now_filetime() + 10_000_000);
    }

    #[test]
    fn kv_parses_kb_lines() {
        let t = "MemTotal:       16000000 kB\nHugePages_Total:       0\n";
        assert_eq!(kv_bytes(t, "MemTotal"), Some(16_000_000 * 1024));
        assert_eq!(kv_bytes(t, "HugePages_Total"), Some(0));
        assert_eq!(kv_bytes(t, "Missing"), None);
    }
}
