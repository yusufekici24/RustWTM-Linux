//! Per-process control on Linux: signals through a pidfd (so a reused PID
//! is never hit), nice / scheduling policy, CPU affinity, I/O priority and
//! core dumps.
//!
//! Linux keeps nice, affinity and I/O priority per thread, so the setters
//! apply them to every thread of the process, as Windows applies a
//! process's priority class to all of its threads.

use std::fs;
use std::ops::BitOr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

use crate::linux::util::{self, Stat, last_os};
use crate::{Error, Result};

/// Access rights requested when opening a process. Linux checks
/// permissions per operation, so these only document intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Access(u32);

pub(crate) fn unsupported() -> Error {
    Error::Unsupported("this process query")
}

impl Access {
    pub const QUERY: Self = Self(0x1);
    pub const QUERY_INFORMATION: Self = Self(0x2);
    pub const TERMINATE: Self = Self(0x4);
    pub const SUSPEND_RESUME: Self = Self(0x8);
    pub const SET_INFORMATION: Self = Self(0x10);
    pub const SET_LIMITED_INFORMATION: Self = Self(0x20);
    pub const SET_QUOTA: Self = Self(0x40);
    pub const SYNCHRONIZE: Self = Self(0x80);
    pub const DUP_HANDLE: Self = Self(0x100);
    pub const READ_PARAMETERS: Self = Self(0x200);
    pub const DUMP: Self = Self(0x400);
    pub const DUMP_CLONE: Self = Self(0x800);
}

impl BitOr for Access {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PriorityClass {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl PriorityClass {
    /// The nice value a class maps to. Realtime maps to the strongest nice
    /// rather than SCHED_FIFO, which could freeze the machine.
    pub(crate) fn nice(self) -> i32 {
        match self {
            Self::Idle => 19,
            Self::BelowNormal => 10,
            Self::Normal => 0,
            Self::AboveNormal => -5,
            Self::High => -10,
            Self::Realtime => -20,
        }
    }

    pub(crate) fn from_nice(nice: i64, policy: u32) -> Self {
        if matches!(policy, 1 | 2) {
            return Self::Realtime;
        }
        if policy == 5 {
            return Self::Idle;
        }
        match nice {
            15.. => Self::Idle,
            1..=14 => Self::BelowNormal,
            0 => Self::Normal,
            -9..=-1 => Self::AboveNormal,
            -19..=-10 => Self::High,
            _ => Self::Realtime,
        }
    }
}

/// "Efficiency mode". Linux has no EcoQoS; `On` maps to the SCHED_BATCH
/// policy (no wake-up preemption, scheduled as CPU-bound background work).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum EcoQos {
    SystemManaged,
    On,
    Off,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum IoPriority {
    Background,
    Low,
    Normal,
    High,
    Critical,
}

impl IoPriority {
    pub fn hint(self) -> u32 {
        match self {
            Self::Background => 0,
            Self::Low => 1,
            Self::Normal => 2,
            Self::High => 3,
            Self::Critical => 4,
        }
    }

    pub fn from_hint(hint: u32) -> Option<Self> {
        Some(match hint {
            0 => Self::Background,
            1 => Self::Low,
            2 => Self::Normal,
            3 => Self::High,
            4 => Self::Critical,
            _ => return None,
        })
    }

    /// `ioprio_set` value: class << 13 | data.
    fn ioprio(self) -> i32 {
        const RT: i32 = 1;
        const BE: i32 = 2;
        const IDLE: i32 = 3;
        let (class, data) = match self {
            Self::Background => (IDLE, 0),
            Self::Low => (BE, 7),
            Self::Normal => (BE, 4),
            Self::High => (BE, 0),
            Self::Critical => (RT, 4),
        };
        (class << 13) | data
    }

    fn from_ioprio(value: i32, nice: i64) -> Self {
        let (class, data) = (value >> 13, value & 0xff);
        match class {
            1 => Self::Critical,
            3 => Self::Background,
            2 => match data {
                0..=1 => Self::High,
                2..=5 => Self::Normal,
                _ => Self::Low,
            },
            // IOPRIO_CLASS_NONE follows the CPU nice: (nice + 20) / 5.
            _ => match (nice + 20) / 5 {
                0..=1 => Self::High,
                2..=5 => Self::Normal,
                _ => Self::Low,
            },
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum MemoryPriority {
    VeryLow,
    Low,
    Medium,
    BelowNormal,
    Normal,
}

impl MemoryPriority {
    pub fn value(self) -> u32 {
        match self {
            Self::VeryLow => 1,
            Self::Low => 2,
            Self::Medium => 3,
            Self::BelowNormal => 4,
            Self::Normal => 5,
        }
    }

    pub fn from_value(value: u32) -> Option<Self> {
        Some(match value {
            0 | 1 => Self::VeryLow,
            2 => Self::Low,
            3 => Self::Medium,
            4 => Self::BelowNormal,
            5 => Self::Normal,
            _ => return None,
        })
    }

    /// Linux has no standby-list priority; the nearest knob is how eagerly
    /// the OOM killer picks the process (`oom_score_adj`).
    fn oom_adj(self) -> i32 {
        match self {
            Self::VeryLow => 800,
            Self::Low => 500,
            Self::Medium => 300,
            Self::BelowNormal => 100,
            Self::Normal => 0,
        }
    }

    fn from_oom_adj(adj: i64) -> Self {
        match adj {
            650.. => Self::VeryLow,
            400..=649 => Self::Low,
            200..=399 => Self::Medium,
            50..=199 => Self::BelowNormal,
            _ => Self::Normal,
        }
    }
}

/// Lowers (`true`) or restores this process's OOM preference.
pub fn set_own_memory_priority(low: bool) -> Result<()> {
    // Raising oom_score_adj never needs privileges; lowering back to 0 may.
    let value = if low { "300" } else { "0" };
    fs::write("/proc/self/oom_score_adj", value).map_err(|e| Error::io("oom_score_adj", e))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Architecture {
    X86,
    X64,
    Arm,
    Arm64,
    Other(u16),
}

impl Architecture {
    /// From an `IMAGE_FILE_MACHINE_*` value.
    pub fn from_machine(machine: u16) -> Self {
        match machine {
            0x014c => Self::X86,
            0x8664 => Self::X64,
            0x01c0 | 0x01c4 => Self::Arm,
            0xaa64 => Self::Arm64,
            other => Self::Other(other),
        }
    }

    /// From an ELF `e_machine`.
    pub(crate) fn from_elf(machine: u16) -> Self {
        match machine {
            3 => Self::X86,
            62 => Self::X64,
            40 => Self::Arm,
            183 => Self::Arm64,
            other => Self::Other(other),
        }
    }

    pub fn label(self) -> String {
        match self {
            Self::X86 => "x86".into(),
            Self::X64 => "x64".into(),
            Self::Arm => "ARM".into(),
            Self::Arm64 => "ARM64".into(),
            Self::Other(m) => format!("0x{m:04x}"),
        }
    }
}

/// Windows integrity levels, mapped from Linux credentials: root is High,
/// kernel threads System, everyone else Medium.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Integrity {
    Untrusted,
    Low,
    Medium,
    MediumPlus,
    High,
    System,
    Protected,
    Other(u32),
}

impl Integrity {
    pub fn from_rid(rid: u32) -> Self {
        match rid {
            0x0000 => Self::Untrusted,
            0x1000 => Self::Low,
            0x2000 => Self::Medium,
            0x2100 => Self::MediumPlus,
            0x3000 => Self::High,
            0x4000 => Self::System,
            0x5000 => Self::Protected,
            other => Self::Other(other),
        }
    }
}

/// An open process: a pidfd plus the start time it was opened with.
#[derive(Debug)]
pub struct ProcessHandle {
    pidfd: OwnedFd,
    pid: u32,
    start: i64,
}

fn pidfd_open(pid: u32) -> Result<OwnedFd> {
    // SAFETY: pidfd_open takes a PID and flags; it returns a new fd or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid as libc::pid_t, 0u32) };
    if fd < 0 {
        return Err(last_os("pidfd_open"));
    }
    // SAFETY: the kernel returned a fresh fd that nothing else owns.
    Ok(unsafe { OwnedFd::from_raw_fd(fd as i32) })
}

impl ProcessHandle {
    pub fn open(pid: u32, _access: Access) -> Result<Self> {
        if pid == 0 {
            return Err(Error::Os {
                call: "pidfd_open",
                errno: libc::EINVAL,
            });
        }
        let pidfd = pidfd_open(pid)?;
        let start = Stat::read(pid)
            .map_err(|e| Error::io("/proc/PID/stat", e))?
            .create_time();
        Ok(Self { pidfd, pid, start })
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Fails with `Reused` once the PID belongs to another process, so
    /// /proc reads and per-thread setters never reach a stranger.
    pub(crate) fn check(&self) -> Result<()> {
        match Stat::read(self.pid) {
            Ok(s) if s.create_time() == self.start => Ok(()),
            Ok(_) => Err(Error::Reused { pid: self.pid }),
            Err(e) => Err(Error::io("/proc/PID/stat", e)),
        }
    }

    pub(crate) fn proc_path(&self, leaf: &str) -> String {
        format!("/proc/{}/{leaf}", self.pid)
    }

    fn signal(&self, sig: i32) -> Result<()> {
        // SAFETY: valid pidfd owned by self; no siginfo.
        let rc = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.pidfd.as_raw_fd(),
                sig,
                std::ptr::null::<libc::siginfo_t>(),
                0u32,
            )
        };
        if rc < 0 {
            Err(last_os("pidfd_send_signal"))
        } else {
            Ok(())
        }
    }

    fn tids(&self) -> Vec<u32> {
        util::numeric_entries(&self.proc_path("task"))
    }

    /// X11 / Wayland have no per-process GDI or USER objects.
    pub fn gui_objects(&self) -> (u32, u32) {
        (0, 0)
    }

    pub fn create_time(&self) -> Result<i64> {
        Ok(self.start)
    }

    pub fn image_path(&self) -> Result<String> {
        self.check()?;
        let link =
            fs::read_link(self.proc_path("exe")).map_err(|e| Error::io("readlink exe", e))?;
        let text = link.to_string_lossy();
        Ok(text.strip_suffix(" (deleted)").unwrap_or(&text).to_owned())
    }

    pub fn command_line(&self) -> Result<String> {
        self.check()?;
        let raw = fs::read(self.proc_path("cmdline")).map_err(|e| Error::io("cmdline", e))?;
        Ok(join_cmdline(&raw))
    }

    pub fn current_directory(&self) -> Result<String> {
        self.check()?;
        fs::read_link(self.proc_path("cwd"))
            .map(|p| p.to_string_lossy().into_owned())
            .map_err(|e| Error::io("readlink cwd", e))
    }

    pub fn user(&self) -> Result<String> {
        let uid = util::process_uid(self.pid).ok_or_else(unsupported)?;
        Ok(util::user_name(uid).unwrap_or_else(|| uid.to_string()))
    }

    pub fn elevated(&self) -> Result<bool> {
        Ok(util::process_euid(self.pid).ok_or_else(unsupported)? == 0)
    }

    pub fn integrity(&self) -> Result<Integrity> {
        let stat = Stat::read(self.pid).map_err(|e| Error::io("stat", e))?;
        if stat.kernel_thread() {
            return Ok(Integrity::System);
        }
        Ok(match util::process_euid(self.pid) {
            Some(0) => Integrity::High,
            Some(_) => Integrity::Medium,
            None => Integrity::Other(0),
        })
    }

    pub fn architecture(&self) -> Result<Architecture> {
        self.check()?;
        let mut header = [0u8; 20];
        let mut f = fs::File::open(self.proc_path("exe")).map_err(|e| Error::io("open exe", e))?;
        std::io::Read::read_exact(&mut f, &mut header).map_err(|e| Error::io("read exe", e))?;
        if &header[..4] != b"\x7fELF" {
            return Ok(Architecture::Other(0));
        }
        let machine = if header[5] == 2 {
            u16::from_be_bytes([header[18], header[19]])
        } else {
            u16::from_le_bytes([header[18], header[19]])
        };
        Ok(Architecture::from_elf(machine))
    }

    /// Pages the process's private memory out (`process_madvise` with
    /// MADV_PAGEOUT, Linux 5.10+), the counterpart of emptying a working set.
    pub fn trim_working_set(&self) -> Result<()> {
        self.check()?;
        const MADV_PAGEOUT: i32 = 21;
        let maps = fs::read_to_string(self.proc_path("maps")).map_err(|e| Error::io("maps", e))?;
        let ranges: Vec<libc::iovec> = maps
            .lines()
            .filter_map(|l| {
                let mut f = l.split_whitespace();
                let (range, perms) = (f.next()?, f.next()?);
                // Private, readable mappings; skip [vvar]/[vsyscall].
                if !perms.starts_with('r') || !perms.ends_with('p') || l.contains("[v") {
                    return None;
                }
                let (a, b) = range.split_once('-')?;
                let a = usize::from_str_radix(a, 16).ok()?;
                let b = usize::from_str_radix(b, 16).ok()?;
                Some(libc::iovec {
                    iov_base: a as *mut _,
                    iov_len: b - a,
                })
            })
            .collect();
        let mut first_error = None;
        for chunk in ranges.chunks(512) {
            // SAFETY: the iovecs describe the target's address space, which
            // the kernel only reads as ranges; nothing in ours is touched.
            let rc = unsafe {
                libc::syscall(
                    libc::SYS_process_madvise,
                    self.pidfd.as_raw_fd(),
                    chunk.as_ptr(),
                    chunk.len(),
                    MADV_PAGEOUT,
                    0u32,
                )
            };
            if rc < 0 && first_error.is_none() {
                first_error = Some(last_os("process_madvise"));
            }
        }
        match first_error {
            Some(e) if e.is_access_denied() => Err(e),
            _ => Ok(()),
        }
    }

    /// PID 1 cannot be killed without taking the system down.
    pub fn is_critical(&self) -> Result<bool> {
        Ok(self.pid == 1)
    }

    pub fn wait_exit(&self, timeout: Duration) -> Result<bool> {
        let mut pfd = libc::pollfd {
            fd: self.pidfd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX);
        // SAFETY: one valid pollfd.
        let rc = unsafe { libc::poll(&mut pfd, 1, ms) };
        if rc < 0 {
            return Err(last_os("poll"));
        }
        Ok(rc > 0)
    }

    /// Writes a core dump with `gcore` (gdb). Returns `false` because the
    /// process stays stopped for the whole write (no snapshot clone).
    pub fn write_dump(&self, path: &Path) -> Result<bool> {
        self.check()?;
        let prefix = path.with_extension("");
        let out = std::process::Command::new("gcore")
            .arg("-o")
            .arg(&prefix)
            .arg(self.pid.to_string())
            .stdin(std::process::Stdio::null())
            .output()
            .map_err(|_| {
                Error::Message("writing a dump needs gdb (sudo apt install gdb)".into())
            })?;
        let written = prefix.with_extension(self.pid.to_string());
        if !out.status.success() || !written.exists() {
            let _ = fs::remove_file(&written);
            return Err(Error::Message(format!(
                "gcore failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        fs::rename(&written, path).map_err(|e| Error::io("rename dump", e))?;
        Ok(false)
    }

    pub fn terminate(&self, _exit_code: u32) -> Result<()> {
        self.signal(libc::SIGKILL)
    }

    /// Asks the process to exit (SIGTERM), the polite counterpart of
    /// closing its windows.
    pub fn request_exit(&self) -> Result<()> {
        self.signal(libc::SIGTERM)
    }

    pub fn suspend(&self) -> Result<()> {
        self.signal(libc::SIGSTOP)
    }

    pub fn resume(&self) -> Result<()> {
        self.signal(libc::SIGCONT)
    }

    pub fn priority(&self) -> Result<PriorityClass> {
        let s = Stat::read(self.pid).map_err(|e| Error::io("stat", e))?;
        Ok(PriorityClass::from_nice(s.nice, s.policy))
    }

    pub fn set_priority(&self, priority: PriorityClass) -> Result<()> {
        self.check()?;
        self.each_thread("setpriority", |tid| {
            // SAFETY: plain syscall on a TID.
            let rc = unsafe { libc::setpriority(libc::PRIO_PROCESS, tid, priority.nice()) };
            if rc < 0 {
                Err(last_os("setpriority"))
            } else {
                Ok(())
            }
        })
    }

    /// Applies `f` to every thread; the first error that is not a thread
    /// exiting meanwhile is returned after all threads were tried.
    fn each_thread(&self, _call: &'static str, mut f: impl FnMut(u32) -> Result<()>) -> Result<()> {
        let mut first = None;
        let mut any = false;
        for tid in self.tids() {
            match f(tid) {
                Ok(()) => any = true,
                Err(e) if e.is_invalid_parameter() => {}
                Err(e) => {
                    first.get_or_insert(e);
                }
            }
        }
        match first {
            Some(e) if !any => Err(e),
            Some(e) if e.is_access_denied() => Err(e),
            _ => Ok(()),
        }
    }

    /// (process mask, system mask) over the first 64 logical processors.
    pub fn affinity(&self) -> Result<(u64, u64)> {
        let set = get_affinity(self.pid)?;
        let system = crate::cpu::online_mask();
        Ok((set, system))
    }

    pub fn set_affinity(&self, mask: u64) -> Result<()> {
        let cpus: Vec<u32> = (0..64).filter(|i| mask & (1 << i) != 0).collect();
        self.set_cpus(&cpus)
    }

    pub(crate) fn set_cpus(&self, cpus: &[u32]) -> Result<()> {
        self.check()?;
        // SAFETY: cpu_set_t is plain data; zeroed is the empty set.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        for &c in cpus {
            // SAFETY: CPU_SET bounds-checks against the set size.
            unsafe { libc::CPU_SET(c as usize, &mut set) };
        }
        self.each_thread("sched_setaffinity", |tid| {
            // SAFETY: valid set for its size.
            let rc =
                unsafe { libc::sched_setaffinity(tid as i32, size_of::<libc::cpu_set_t>(), &set) };
            if rc < 0 {
                Err(last_os("sched_setaffinity"))
            } else {
                Ok(())
            }
        })
    }

    pub fn groups(&self) -> Result<Vec<u16>> {
        Ok(vec![0])
    }

    pub fn core_affinity(
        &self,
        selection: crate::cpu::CoreSelection,
    ) -> std::result::Result<u64, String> {
        let topology = crate::cpu::topology().map_err(|e| e.to_string())?;
        let (_, allowed) = self.affinity().map_err(|e| e.to_string())?;
        topology.mask(selection, 0, allowed)
    }

    pub fn eco_qos(&self) -> Result<EcoQos> {
        let s = Stat::read(self.pid).map_err(|e| Error::io("stat", e))?;
        Ok(match s.policy {
            3 | 5 => EcoQos::On,
            _ => EcoQos::SystemManaged,
        })
    }

    pub fn set_eco_qos(&self, mode: EcoQos) -> Result<()> {
        self.check()?;
        let policy = match mode {
            EcoQos::On => libc::SCHED_BATCH,
            EcoQos::SystemManaged | EcoQos::Off => libc::SCHED_OTHER,
        };
        let param = libc::sched_param { sched_priority: 0 };
        self.each_thread("sched_setscheduler", |tid| {
            // Leave real-time threads (audio, compositors) alone.
            if matches!(Stat::read_task(self.pid, tid).map(|s| s.policy), Ok(1 | 2)) {
                return Ok(());
            }
            // SAFETY: valid sched_param.
            let rc = unsafe { libc::sched_setscheduler(tid as i32, policy, &param) };
            if rc < 0 {
                Err(last_os("sched_setscheduler"))
            } else {
                Ok(())
            }
        })
    }

    pub fn set_io_priority(&self, priority: IoPriority) -> Result<()> {
        self.check()?;
        const IOPRIO_WHO_PROCESS: i32 = 1;
        let value = priority.ioprio();
        self.each_thread("ioprio_set", |tid| {
            // SAFETY: plain syscall.
            let rc = unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, tid, value) };
            if rc < 0 {
                Err(last_os("ioprio_set"))
            } else {
                Ok(())
            }
        })
    }

    pub fn io_priority(&self) -> Result<IoPriority> {
        const IOPRIO_WHO_PROCESS: i32 = 1;
        // SAFETY: plain syscall.
        let rc = unsafe { libc::syscall(libc::SYS_ioprio_get, IOPRIO_WHO_PROCESS, self.pid) };
        if rc < 0 {
            return Err(last_os("ioprio_get"));
        }
        let nice = Stat::read(self.pid).map(|s| s.nice).unwrap_or(0);
        Ok(IoPriority::from_ioprio(rc as i32, nice))
    }

    pub fn memory_priority(&self) -> Result<MemoryPriority> {
        let adj = util::read_i64(self.proc_path("oom_score_adj")).ok_or_else(unsupported)?;
        Ok(MemoryPriority::from_oom_adj(adj))
    }

    pub fn set_memory_priority(&self, priority: MemoryPriority) -> Result<()> {
        self.check()?;
        fs::write(
            self.proc_path("oom_score_adj"),
            priority.oom_adj().to_string(),
        )
        .map_err(|e| Error::io("oom_score_adj", e))
    }

    pub fn set_normal_memory_priority(&self) -> Result<()> {
        self.set_memory_priority(MemoryPriority::Normal)
    }

    /// The snap or Flatpak a process belongs to, like an AppX package name.
    pub fn package_full_name(&self) -> Result<Option<String>> {
        let cgroup =
            fs::read_to_string(self.proc_path("cgroup")).map_err(|e| Error::io("cgroup", e))?;
        Ok(package_from_cgroup(&cgroup))
    }

    /// The CPUs the process is confined to; empty when it may use all.
    pub fn default_cpu_sets(&self) -> Result<Vec<u32>> {
        let mask = get_affinity_list(self.pid)?;
        let online = crate::cpu::online_cpus();
        if online.iter().all(|c| mask.contains(c)) {
            Ok(Vec::new())
        } else {
            Ok(mask)
        }
    }

    /// Confines the process to CPU numbers `ids`; empty lifts it.
    pub fn set_default_cpu_sets(&self, ids: &[u32]) -> Result<()> {
        if ids.is_empty() {
            self.set_cpus(&crate::cpu::online_cpus())
        } else {
            self.set_cpus(ids)
        }
    }

    /// Linux does not trim working sets the way Windows does; keeping
    /// pages resident needs mlock inside the process. Accepted as a no-op.
    pub fn lock_working_set(&self, _min_bytes: usize) -> Result<()> {
        Ok(())
    }
}

/// Raises the calling thread a little (needs CAP_SYS_NICE; otherwise a
/// harmless failure the callers ignore).
pub fn raise_current_thread() -> Result<()> {
    // SAFETY: gettid / setpriority take plain values.
    let rc = unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::setpriority(libc::PRIO_PROCESS, tid, -2)
    };
    if rc < 0 {
        Err(last_os("setpriority"))
    } else {
        Ok(())
    }
}

/// Background mode for the calling thread: SCHED_IDLE and idle I/O class.
pub fn enter_background_mode() -> Result<()> {
    let param = libc::sched_param { sched_priority: 0 };
    // SAFETY: valid sched_param; 0 is the calling thread.
    let rc = unsafe { libc::sched_setscheduler(0, libc::SCHED_IDLE, &param) };
    if rc < 0 {
        return Err(last_os("sched_setscheduler"));
    }
    // SAFETY: plain syscall; IOPRIO_WHO_PROCESS 1, who 0 = calling thread.
    unsafe { libc::syscall(libc::SYS_ioprio_set, 1, 0, 3 << 13) };
    Ok(())
}

fn get_affinity_list(pid: u32) -> Result<Vec<u32>> {
    // SAFETY: zeroed cpu_set_t is valid.
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    // SAFETY: valid set for its size.
    let rc = unsafe { libc::sched_getaffinity(pid as i32, size_of::<libc::cpu_set_t>(), &mut set) };
    if rc < 0 {
        return Err(last_os("sched_getaffinity"));
    }
    Ok((0..libc::CPU_SETSIZE as u32)
        // SAFETY: index within CPU_SETSIZE.
        .filter(|&i| unsafe { libc::CPU_ISSET(i as usize, &set) })
        .collect())
}

fn get_affinity(pid: u32) -> Result<u64> {
    Ok(get_affinity_list(pid)?
        .into_iter()
        .filter(|&i| i < 64)
        .fold(0u64, |m, i| m | (1 << i)))
}

/// Command-line bytes (NUL-separated) as one line, quoting arguments with
/// spaces.
pub(crate) fn join_cmdline(raw: &[u8]) -> String {
    let text = String::from_utf8_lossy(raw);
    let args: Vec<&str> = text.trim_end_matches('\0').split('\0').collect();
    // A process that rewrote its title has one argument with spaces.
    if args.len() == 1 {
        return args[0].to_owned();
    }
    args.iter()
        .map(|a| {
            if a.contains(' ') || a.is_empty() {
                format!("\"{a}\"")
            } else {
                (*a).to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn package_from_cgroup(cgroup: &str) -> Option<String> {
    for line in cgroup.lines() {
        let path = line.splitn(3, ':').nth(2).unwrap_or_default();
        for part in path.split('/') {
            if let Some(rest) = part.strip_prefix("snap.") {
                let name = rest.split('.').next().unwrap_or(rest);
                return Some(format!("snap:{name}"));
            }
            if let Some(rest) = part.strip_prefix("app-flatpak-") {
                let id = rest.rsplit_once('-').map_or(rest, |(id, _)| id);
                return Some(format!("flatpak:{id}"));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opens_self_and_reads() {
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        assert!(h.image_path().unwrap().starts_with('/'));
        assert!(!h.command_line().unwrap().is_empty());
        assert_eq!(
            h.create_time().unwrap(),
            Stat::read(std::process::id()).unwrap().create_time()
        );
        assert!(matches!(
            h.architecture().unwrap(),
            Architecture::X64 | Architecture::Arm64
        ));
        assert!(!h.wait_exit(Duration::from_millis(1)).unwrap());
        let (mask, system) = h.affinity().unwrap();
        assert!(mask != 0 && mask & system == mask);
    }

    #[test]
    fn controls_a_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let h = ProcessHandle::open(child.id(), Access::TERMINATE).unwrap();
        h.set_priority(PriorityClass::BelowNormal).unwrap();
        assert_eq!(h.priority().unwrap(), PriorityClass::BelowNormal);
        h.set_io_priority(IoPriority::Low).unwrap();
        assert_eq!(h.io_priority().unwrap(), IoPriority::Low);
        h.set_eco_qos(EcoQos::On).unwrap();
        assert_eq!(h.eco_qos().unwrap(), EcoQos::On);
        h.suspend().unwrap();
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(Stat::read(child.id()).unwrap().state, 'T');
        h.resume().unwrap();
        h.terminate(1).unwrap();
        assert!(h.wait_exit(Duration::from_secs(5)).unwrap());
        let _ = child.wait();
    }

    #[test]
    fn missing_pid_is_invalid_parameter() {
        let e = ProcessHandle::open(0x3fff_fff0, Access::QUERY).unwrap_err();
        assert!(e.is_invalid_parameter(), "{e}");
    }

    #[test]
    fn cmdline_and_packages() {
        assert_eq!(
            join_cmdline(b"/bin/ls\0-la\0my dir\0"),
            "/bin/ls -la \"my dir\""
        );
        assert_eq!(
            package_from_cgroup(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/snap.firefox.firefox-1234.scope"
            ),
            Some("snap:firefox".into())
        );
        assert_eq!(
            package_from_cgroup(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-org.gimp.GIMP-5678.scope"
            ),
            Some("flatpak:org.gimp.GIMP".into())
        );
        assert_eq!(package_from_cgroup("0::/system.slice/cron.service"), None);
    }

    #[test]
    fn io_priorities_round_trip() {
        for p in [
            IoPriority::Background,
            IoPriority::Low,
            IoPriority::Normal,
            IoPriority::High,
            IoPriority::Critical,
        ] {
            assert_eq!(IoPriority::from_ioprio(p.ioprio(), 0), p);
        }
        for m in [
            MemoryPriority::VeryLow,
            MemoryPriority::Low,
            MemoryPriority::Medium,
            MemoryPriority::BelowNormal,
            MemoryPriority::Normal,
        ] {
            assert_eq!(MemoryPriority::from_oom_adj(i64::from(m.oom_adj())), m);
        }
    }
}
