//! The handles a process holds, as Process Explorer's handle view lists
//! them: type, name, value and granted access.
//!
//! The list comes from the process itself (`ProcessHandleInformation`,
//! which needs `PROCESS_QUERY_INFORMATION`), not from the system-wide
//! handle table. Names need `PROCESS_DUP_HANDLE`: each handle is copied
//! into this process with no access rights, named and closed at once, one
//! at a time, so a copy never outlives the target's own handle for long.
//!
//! Naming a file can block forever (a synchronous pipe with a read
//! pending holds the file object's lock), so file handles are only ever
//! touched on a helper thread with a deadline. A helper that misses it is
//! left behind; until it returns, no other is started and file names are
//! skipped. Other object types are named directly.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    DUPLICATE_HANDLE_OPTIONS, DuplicateHandle, HANDLE, NTSTATUS, STATUS_BUFFER_OVERFLOW,
    STATUS_BUFFER_TOO_SMALL, STATUS_INFO_LENGTH_MISMATCH,
};
use windows::Win32::Storage::FileSystem::{FILE_TYPE_DISK, GetFileType};
use windows::Win32::System::Threading::{
    GetCurrentProcess, GetProcessId, GetProcessIdOfThread, GetThreadId, PROCESS_DUP_HANDLE,
    PROCESS_QUERY_LIMITED_INFORMATION, THREAD_QUERY_LIMITED_INFORMATION,
};

use crate::Result;
use crate::control::ProcessHandle;
use crate::handle::OwnedHandle;

windows::core::link!("ntdll.dll" "system" fn NtQueryInformationProcess(
    process_handle: HANDLE,
    class: u32,
    info: *mut core::ffi::c_void,
    len: u32,
    returned: *mut u32,
) -> NTSTATUS);
windows::core::link!("ntdll.dll" "system" fn NtQueryObject(
    handle: HANDLE,
    class: u32,
    info: *mut core::ffi::c_void,
    len: u32,
    returned: *mut u32,
) -> NTSTATUS);

/// `PROCESSINFOCLASS::ProcessHandleInformation` (Windows 8+).
const PROCESS_HANDLE_INFORMATION: u32 = 51;
/// `OBJECT_INFORMATION_CLASS` values.
const OBJECT_NAME_INFORMATION: u32 = 1;
const OBJECT_TYPE_INFORMATION: u32 = 2;
const OBJECT_TYPES_INFORMATION: u32 = 3;

/// Handles listed at most. The list's size comes from the target, which
/// may leak millions; past this the read is refused rather than allocated.
pub const MOST_HANDLES: usize = 200_000;
/// Handles named at most per read, and the time spent naming them.
const MOST_NAMED: usize = 5_000;
const NAMING_BUDGET: Duration = Duration::from_secs(3);
/// How long one file name may take before its helper is given up on.
const FILE_DEADLINE: Duration = Duration::from_millis(500);

/// One handle the process holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HandleInfo {
    pub value: u64,
    /// The object type, such as "File", "Key" or "Event"; empty when
    /// Windows did not name the type.
    pub kind: String,
    /// The object's name: a path for files (`C:\…`) and keys (`HKLM\…`),
    /// "name (PID)" for processes; `None` for unnamed objects and those
    /// not read.
    pub name: Option<String>,
    /// The access rights granted to the handle.
    pub access: u32,
}

/// The handles of a process and how many of their names could be read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HandleReport {
    /// How many handles the process holds.
    pub total: usize,
    /// Empty when `total` exceeds [`MOST_HANDLES`].
    pub handles: Vec<HandleInfo>,
    /// Whether names could be read at all (`PROCESS_DUP_HANDLE`).
    pub can_name: bool,
    /// Some names were not read: the cap or the time budget was reached,
    /// or a file did not answer in time.
    pub names_incomplete: bool,
}

impl HandleReport {
    pub fn too_many(&self) -> bool {
        self.total > MOST_HANDLES
    }
}

/// Raw `PROCESS_HANDLE_TABLE_ENTRY_INFO`.
#[repr(C)]
#[allow(dead_code)]
#[derive(Clone, Copy)]
struct Entry {
    value: usize,
    handle_count: usize,
    pointer_count: usize,
    access: u32,
    type_index: u32,
    attributes: u32,
    reserved: u32,
}

#[cfg(target_pointer_width = "64")]
const _: () = assert!(size_of::<Entry>() == 40);

/// What names a file handle, given the copy in this process. A seam: tests
/// pass one that blocks.
pub(crate) type FileNamer = Arc<dyn Fn(HANDLE) -> Option<String> + Send + Sync>;

/// Set while a file-naming helper runs; one that missed its deadline keeps
/// it set until it returns.
static FILE_HELPER_BUSY: AtomicBool = AtomicBool::new(false);

impl ProcessHandle {
    /// The process's handles. Needs
    /// [`Access::QUERY_INFORMATION`](crate::control::Access::QUERY_INFORMATION);
    /// with [`Access::DUP_HANDLE`](crate::control::Access::DUP_HANDLE) as
    /// well, their names.
    pub fn handles(&self, can_name: bool) -> Result<HandleReport> {
        self.handles_with(can_name, disk_file_namer(), &FILE_HELPER_BUSY)
    }

    pub(crate) fn handles_with(
        &self,
        can_name: bool,
        file_namer: FileNamer,
        busy: &'static AtomicBool,
    ) -> Result<HandleReport> {
        let (total, entries) = self.handle_entries()?;
        let mut report = HandleReport {
            total,
            can_name,
            ..HandleReport::default()
        };
        let Some(entries) = entries else {
            return Ok(report);
        };
        let types = object_types().unwrap_or_default();
        report.handles = entries
            .iter()
            .map(|e| HandleInfo {
                value: e.value as u64,
                kind: types.get(&e.type_index).cloned().unwrap_or_default(),
                name: None,
                access: e.access,
            })
            .collect();
        if can_name {
            report.names_incomplete = self.name_handles(&mut report.handles, file_namer, busy);
        }
        Ok(report)
    }

    /// The handle count and, unless it exceeds [`MOST_HANDLES`], the
    /// entries.
    fn handle_entries(&self) -> Result<(usize, Option<Vec<Entry>>)> {
        const HEADER: usize = 2 * size_of::<usize>();
        let most_bytes = HEADER + MOST_HANDLES * size_of::<Entry>();
        let mut buf = vec![0u64; 64 * 1024 / 8];
        loop {
            let capacity = buf.len() * 8;
            let mut needed = 0u32;
            // SAFETY: `buf` is writable for `capacity` bytes.
            let status = unsafe {
                NtQueryInformationProcess(
                    self.raw(),
                    PROCESS_HANDLE_INFORMATION,
                    buf.as_mut_ptr().cast(),
                    capacity as u32,
                    &mut needed,
                )
            };
            if status == STATUS_INFO_LENGTH_MISMATCH || status == STATUS_BUFFER_TOO_SMALL {
                let needed = (needed as usize).max(capacity + 1);
                if needed > most_bytes {
                    // Only the count is kept, above the cap even when the
                    // list grew past it while being read.
                    let count = needed.saturating_sub(HEADER) / size_of::<Entry>();
                    return Ok((count.max(MOST_HANDLES + 1), None));
                }
                // Room for handles opened meanwhile.
                let grown = (needed + needed / 8).min(most_bytes);
                buf.resize(grown.div_ceil(8), 0);
                continue;
            }
            nt_ok("NtQueryInformationProcess", status)?;
            let count = buf[0] as usize;
            let fits = (capacity - HEADER) / size_of::<Entry>();
            if count > MOST_HANDLES {
                return Ok((count, None));
            }
            let base = buf.as_ptr().cast::<u8>();
            let entries = (0..count.min(fits))
                .map(|i| {
                    // SAFETY: entry `i` lies wholly within the buffer, which
                    // is 8-byte aligned, as is the entry stride.
                    unsafe {
                        base.add(HEADER + i * size_of::<Entry>())
                            .cast::<Entry>()
                            .read()
                    }
                })
                .collect();
            return Ok((count, Some(entries)));
        }
    }

    /// Fills in names; returns whether some were not read.
    fn name_handles(
        &self,
        handles: &mut [HandleInfo],
        file_namer: FileNamer,
        busy: &'static AtomicBool,
    ) -> bool {
        let start = Instant::now();
        let mut order: Vec<usize> = (0..handles.len()).collect();
        // Named objects first: unnamed events can number thousands.
        order.sort_by_key(|&i| priority(&handles[i].kind));
        let mut incomplete = false;
        let mut files = Vec::new();
        let mut process_names: Option<HashMap<u32, String>> = None;
        for (named, i) in order.into_iter().enumerate() {
            if named >= MOST_NAMED || start.elapsed() > NAMING_BUDGET {
                incomplete = true;
                break;
            }
            let h = &mut handles[i];
            match h.kind.as_str() {
                "File" => files.push(i),
                "Process" | "Thread" => {
                    let names = process_names.get_or_insert_with(snapshot_names);
                    h.name = self.process_or_thread_name(h, names);
                }
                _ => {
                    // The type is confirmed on the copy itself, which the
                    // object manager answers without taking a file's lock:
                    // a file whose type index was missed or misread never
                    // reaches the unguarded name query.
                    let Some(copy) = self.duplicate(h.value, 0) else {
                        continue;
                    };
                    match object_type(copy.raw()) {
                        Some(actual) if actual == "File" => {
                            h.kind = actual;
                            files.push(i);
                        }
                        Some(actual) => {
                            if h.kind.is_empty() {
                                h.kind = actual;
                            }
                            h.name = object_name(copy.raw()).map(|n| friendly_key(&n));
                        }
                        None => incomplete = true,
                    }
                }
            }
        }
        if !files.is_empty() {
            let budget = NAMING_BUDGET.saturating_sub(start.elapsed());
            incomplete |= self.name_files(handles, &files, file_namer, busy, budget);
        }
        incomplete
    }

    /// "name (PID)" for a process, "name (PID): TID" for a thread.
    fn process_or_thread_name(
        &self,
        h: &HandleInfo,
        names: &HashMap<u32, String>,
    ) -> Option<String> {
        let label = |pid: u32| match names.get(&pid) {
            Some(name) => format!("{name} ({pid})"),
            None => format!("({pid})"),
        };
        if h.kind == "Process" {
            let copy = self.duplicate(h.value, PROCESS_QUERY_LIMITED_INFORMATION.0)?;
            // SAFETY: `copy` is a live process handle with query rights.
            let pid = unsafe { GetProcessId(copy.raw()) };
            (pid != 0).then(|| label(pid))
        } else {
            let copy = self.duplicate(h.value, THREAD_QUERY_LIMITED_INFORMATION.0)?;
            // SAFETY: `copy` is a live thread handle with query rights.
            let (pid, tid) = unsafe { (GetProcessIdOfThread(copy.raw()), GetThreadId(copy.raw())) };
            (tid != 0).then(|| format!("{}: {tid}", label(pid)))
        }
    }

    /// A copy of the target's handle `value` in this process with only
    /// `access`; never the source's own rights, never closing the source.
    fn duplicate(&self, value: u64, access: u32) -> Option<OwnedHandle> {
        duplicate_from(self.raw(), value, access)
    }

    /// Names file handles on a helper thread, each within
    /// [`FILE_DEADLINE`]; returns whether some were not named.
    fn name_files(
        &self,
        handles: &mut [HandleInfo],
        files: &[usize],
        file_namer: FileNamer,
        busy: &'static AtomicBool,
        budget: Duration,
    ) -> bool {
        // A helper still stuck from an earlier read: none is started.
        if busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return true;
        }
        // The helper's own copy of the process handle, closed when it ends.
        let Some(source) =
            duplicate_from(current_process(), self.raw().0 as u64, PROCESS_DUP_HANDLE.0)
        else {
            busy.store(false, Ordering::Release);
            return true;
        };
        let jobs: Vec<(usize, u64)> = files.iter().map(|&i| (i, handles[i].value)).collect();
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel::<(usize, Option<String>)>();
        let stop = cancel.clone();
        let spawned = std::thread::Builder::new()
            .name("handle-names".into())
            .stack_size(64 * 1024)
            .spawn(move || {
                // Clears `busy` however the helper ends, however late.
                struct Done(&'static AtomicBool);
                impl Drop for Done {
                    fn drop(&mut self) {
                        self.0.store(false, Ordering::Release);
                    }
                }
                let _done = Done(busy);
                for (i, value) in jobs {
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    // One copy at a time, closed before the next.
                    let name = duplicate_from(source.raw(), value, 0)
                        .and_then(|copy| file_namer(copy.raw()));
                    if tx.send((i, name)).is_err() {
                        break;
                    }
                }
            });
        if spawned.is_err() {
            busy.store(false, Ordering::Release);
            return true;
        }
        let end = Instant::now() + budget;
        let mut received = 0;
        while received < files.len() {
            let left = end.saturating_duration_since(Instant::now());
            match rx.recv_timeout(FILE_DEADLINE.min(left)) {
                Ok((i, name)) => {
                    handles[i].name = name;
                    received += 1;
                }
                Err(_) => break,
            }
        }
        if received < files.len() {
            // Stopped after its current file; if that one hangs, the helper
            // stays behind and keeps `busy` set until it returns.
            cancel.store(true, Ordering::Release);
            return true;
        }
        false
    }
}

/// Copies `value` from `process` into this process with `access` only.
fn duplicate_from(process: HANDLE, value: u64, access: u32) -> Option<OwnedHandle> {
    let mut copy = HANDLE::default();
    // SAFETY: `copy` receives a new handle owned by the returned guard. No
    // DUPLICATE_CLOSE_SOURCE and no DUPLICATE_SAME_ACCESS: the source is
    // untouched and the copy gets only `access`.
    unsafe {
        DuplicateHandle(
            process,
            HANDLE(value as usize as *mut core::ffi::c_void),
            current_process(),
            &mut copy,
            access,
            false,
            DUPLICATE_HANDLE_OPTIONS(0),
        )
        .ok()?
    };
    Some(OwnedHandle::new(copy))
}

fn current_process() -> HANDLE {
    // SAFETY: returns the constant pseudo-handle of this process.
    unsafe { GetCurrentProcess() }
}

/// Named kinds first, in the order they are most often looked for.
fn priority(kind: &str) -> u8 {
    match kind {
        "File" => 0,
        "Key" => 1,
        "Section" | "Directory" | "Mutant" | "ALPC Port" => 2,
        "Process" | "Thread" => 3,
        "Semaphore" | "SymbolicLink" | "Desktop" | "WindowStation" | "Token" => 4,
        _ => 5,
    }
}

/// Names disk files as drive paths; the drive letters are read once per
/// list rather than once per file.
fn disk_file_namer() -> FileNamer {
    let devices = crate::disktrace::dos_devices();
    Arc::new(move |copy| file_name(copy, &devices))
}

/// The type name of the object `handle` refers to ("File", "Event", …).
fn object_type(handle: HANDLE) -> Option<String> {
    const MOST: usize = 8 * 1024;
    let mut buf = vec![0u64; 512 / 8];
    loop {
        let capacity = buf.len() * 8;
        let mut needed = 0u32;
        // SAFETY: `buf` is writable for `capacity` bytes.
        let status = unsafe {
            NtQueryObject(
                handle,
                OBJECT_TYPE_INFORMATION,
                buf.as_mut_ptr().cast(),
                capacity as u32,
                &mut needed,
            )
        };
        if status == STATUS_INFO_LENGTH_MISMATCH
            && (needed as usize) > capacity
            && (needed as usize) <= MOST
        {
            buf.resize((needed as usize).div_ceil(8), 0);
            continue;
        }
        if status.is_err() {
            return None;
        }
        return unicode_string_at(&buf, 0).filter(|n| !n.is_empty());
    }
}

/// A disk file's path; other files (pipes, consoles, sockets) are not
/// named, since asking can block.
fn file_name(copy: HANDLE, devices: &[(String, String)]) -> Option<String> {
    // SAFETY: `copy` is a live handle owned by the caller.
    if unsafe { GetFileType(copy) } != FILE_TYPE_DISK {
        return None;
    }
    let name = object_name(copy)?;
    Some(crate::disktrace::dos_path(&name, devices))
}

/// The object manager's name of `handle`, if it has one.
fn object_name(handle: HANDLE) -> Option<String> {
    // Names are at most 32 767 characters; most fit in the first try.
    const MOST: usize = 70 * 1024;
    let mut buf = vec![0u64; 1024 / 8];
    loop {
        let capacity = buf.len() * 8;
        let mut needed = 0u32;
        // SAFETY: `buf` is writable for `capacity` bytes.
        let status = unsafe {
            NtQueryObject(
                handle,
                OBJECT_NAME_INFORMATION,
                buf.as_mut_ptr().cast(),
                capacity as u32,
                &mut needed,
            )
        };
        if (status == STATUS_INFO_LENGTH_MISMATCH
            || status == STATUS_BUFFER_OVERFLOW
            || status == STATUS_BUFFER_TOO_SMALL)
            && (needed as usize) > capacity
            && (needed as usize) <= MOST
        {
            buf.resize((needed as usize).div_ceil(8), 0);
            continue;
        }
        if status.is_err() {
            return None;
        }
        return unicode_string_at(&buf, 0).filter(|n| !n.is_empty());
    }
}

/// The text of the `UNICODE_STRING` at byte `offset` of `buf`, if its
/// characters lie inside `buf`.
fn unicode_string_at(buf: &[u64], offset: usize) -> Option<String> {
    let bytes = buf.len() * 8;
    if offset + 16 > bytes {
        return None;
    }
    let base = buf.as_ptr().cast::<u8>();
    // SAFETY: the 16-byte header lies within `buf` (checked above).
    let (length, pointer) = unsafe {
        let length = base.add(offset).cast::<u16>().read_unaligned();
        let pointer = base.add(offset + 8).cast::<usize>().read_unaligned();
        (usize::from(length), pointer)
    };
    if length == 0 || pointer == 0 {
        return Some(String::new());
    }
    let start = base as usize;
    if pointer < start || pointer + length > start + bytes {
        return None;
    }
    // SAFETY: the characters lie within `buf` (checked above).
    let units =
        unsafe { std::slice::from_raw_parts(base.add(pointer - start).cast::<u16>(), length / 2) };
    Some(String::from_utf16_lossy(units))
}

/// Object type indexes to names ("File", "Key", …), from one query.
fn object_types() -> Result<HashMap<u32, String>> {
    const MOST: usize = 1 << 20;
    let mut buf = vec![0u64; 16 * 1024 / 8];
    let status = loop {
        let capacity = buf.len() * 8;
        let mut needed = 0u32;
        // SAFETY: `buf` is writable for `capacity` bytes; no handle is
        // needed for this class.
        let status = unsafe {
            NtQueryObject(
                HANDLE::default(),
                OBJECT_TYPES_INFORMATION,
                buf.as_mut_ptr().cast(),
                capacity as u32,
                &mut needed,
            )
        };
        // The length it reports is not always right: doubled instead.
        if status == STATUS_INFO_LENGTH_MISMATCH && capacity * 2 <= MOST {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        break status;
    };
    nt_ok("NtQueryObject", status)?;
    Ok(parse_types(&buf))
}

/// `OBJECT_TYPES_INFORMATION`: a count, then each `OBJECT_TYPE_INFORMATION`
/// followed by its name, each aligned to a pointer.
fn parse_types(buf: &[u64]) -> HashMap<u32, String> {
    /// `sizeof(OBJECT_TYPE_INFORMATION)` on 64-bit Windows.
    const ENTRY: usize = 104;
    /// Offset of its `TypeIndex` byte.
    const TYPE_INDEX: usize = 90;
    let bytes = buf.len() * 8;
    let base = buf.as_ptr().cast::<u8>();
    let mut out = HashMap::new();
    if bytes < 8 {
        return out;
    }
    // SAFETY: the count lies within `buf`.
    let count = unsafe { base.cast::<u32>().read() } as usize;
    let mut offset = 8;
    for _ in 0..count.min(512) {
        if offset + ENTRY > bytes {
            break;
        }
        // SAFETY: the entry lies within `buf` (checked above).
        let (index, maximum) = unsafe {
            (
                *base.add(offset + TYPE_INDEX),
                usize::from(base.add(offset + 2).cast::<u16>().read_unaligned()),
            )
        };
        if let Some(name) = unicode_string_at(buf, offset) {
            out.insert(u32::from(index), name);
        }
        offset = (offset + ENTRY + maximum).next_multiple_of(8);
    }
    out
}

/// Registry paths as regedit names them: `HKLM\…`, `HKU\…`.
fn friendly_key(name: &str) -> String {
    for (nt, short) in [(r"\REGISTRY\MACHINE", "HKLM"), (r"\REGISTRY\USER", "HKU")] {
        if let Some(head) = name.get(..nt.len())
            && head.eq_ignore_ascii_case(nt)
        {
            return format!("{short}{}", &name[nt.len()..]);
        }
    }
    name.to_owned()
}

fn snapshot_names() -> HashMap<u32, String> {
    crate::process::ProcessSnapshotter::new()
        .snapshot()
        .unwrap_or_default()
        .into_iter()
        .map(|p| (p.pid, p.name))
        .collect()
}

fn nt_ok(call: &'static str, status: NTSTATUS) -> Result<()> {
    if status.is_ok() {
        Ok(())
    } else {
        Err(crate::Error::Nt {
            call,
            status: status.0 as u32,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Access;
    use std::os::windows::io::AsRawHandle;

    #[test]
    fn names_its_own_event_file_and_process_handles() {
        let event_name = format!("RustWTM-handles-test-{}", std::process::id());
        let wide: Vec<u16> = event_name.encode_utf16().chain(Some(0)).collect();
        // SAFETY: `wide` is NUL-terminated; the event is closed by the guard.
        let event = OwnedHandle::new(unsafe {
            windows::Win32::System::Threading::CreateEventW(
                None,
                true,
                false,
                windows::core::PCWSTR(wide.as_ptr()),
            )
            .unwrap()
        });
        let dir = std::env::temp_dir().join(format!("rustwtm-handles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("held.txt");
        let file = std::fs::File::create(&path).unwrap();
        let me = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();

        let h = ProcessHandle::open(
            std::process::id(),
            Access::QUERY_INFORMATION | Access::DUP_HANDLE,
        )
        .unwrap();
        // Its own helper flag: tests run in parallel.
        static BUSY: AtomicBool = AtomicBool::new(false);
        let report = h.handles_with(true, disk_file_namer(), &BUSY).unwrap();
        let find = |value: usize| {
            report
                .handles
                .iter()
                .find(|x| x.value == value as u64)
                .cloned()
                .unwrap()
        };
        let e = find(event.raw().0 as usize);
        assert_eq!(e.kind, "Event");
        assert!(e.name.as_deref().unwrap().ends_with(&event_name), "{e:?}");
        let f = find(file.as_raw_handle() as usize);
        assert_eq!(f.kind, "File");
        let shown = f.name.clone().unwrap().to_lowercase();
        assert!(shown.ends_with(r"\held.txt"), "{f:?}");
        assert!(!shown.starts_with(r"\device"), "a drive path: {f:?}");
        let p = find(me.raw().0 as usize);
        assert_eq!(p.kind, "Process");
        assert!(
            p.name
                .as_deref()
                .unwrap()
                .ends_with(&format!("({})", std::process::id())),
            "{p:?}"
        );
        assert!(report.total >= report.handles.len() && report.can_name);
        assert!(!report.too_many());

        // Without the right to copy handles: types, no names.
        let bare = ProcessHandle::open(std::process::id(), Access::QUERY_INFORMATION)
            .unwrap()
            .handles(false)
            .unwrap();
        assert!(bare.handles.iter().all(|x| x.name.is_none()));
        assert!(bare.handles.iter().any(|x| x.kind == "File"));

        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_never_answers_is_given_up_on() {
        static BUSY: AtomicBool = AtomicBool::new(false);
        let (release, wait) = mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        // Blocks on the first file until released, as a pipe can.
        let stuck: FileNamer = Arc::new(move |_| {
            let _ = wait.lock().unwrap().recv();
            None
        });
        let dir = std::env::temp_dir().join(format!("rustwtm-stuck-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = std::fs::File::create(dir.join("a.txt")).unwrap();
        let h = ProcessHandle::open(
            std::process::id(),
            Access::QUERY_INFORMATION | Access::DUP_HANDLE,
        )
        .unwrap();

        let start = Instant::now();
        let report = h.handles_with(true, stuck.clone(), &BUSY).unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(3),
            "{:?}",
            start.elapsed()
        );
        assert!(report.names_incomplete);
        assert!(
            BUSY.load(Ordering::Acquire),
            "the stuck helper keeps it set"
        );
        // Meanwhile no second helper starts, and other names still come.
        let again = h.handles_with(true, stuck, &BUSY).unwrap();
        assert!(again.names_incomplete);
        // Every Windows process holds named directories (\KnownDlls).
        assert!(
            again
                .handles
                .iter()
                .any(|x| x.kind == "Directory" && x.name.is_some())
        );

        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while BUSY.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "the helper never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pipe_with_a_read_pending_is_skipped_without_waiting() {
        use std::io::{Read, Write};
        let (mut reader, mut writer) = std::io::pipe().unwrap();
        let reader_value = reader.as_raw_handle() as usize as u64;
        // A synchronous read that waits until the write below.
        let pending = std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte).map(|()| byte[0])
        });
        std::thread::sleep(Duration::from_millis(100));
        let h = ProcessHandle::open(
            std::process::id(),
            Access::QUERY_INFORMATION | Access::DUP_HANDLE,
        )
        .unwrap();
        static BUSY: AtomicBool = AtomicBool::new(false);
        let start = Instant::now();
        let report = h.handles_with(true, disk_file_namer(), &BUSY).unwrap();
        let took = start.elapsed();
        writer.write_all(&[7]).unwrap();
        assert_eq!(pending.join().unwrap().unwrap(), 7);
        assert!(took < Duration::from_secs(3), "{took:?}");
        let pipe = report
            .handles
            .iter()
            .find(|x| x.value == reader_value)
            .unwrap();
        assert_eq!((pipe.kind.as_str(), pipe.name.as_deref()), ("File", None));
    }

    #[test]
    fn a_handle_of_unknown_type_is_typed_on_its_copy_and_files_stay_guarded() {
        static BUSY: AtomicBool = AtomicBool::new(false);
        // A namer that never answers: a file named outside the helper
        // would get a name here.
        let (_release, wait) = mpsc::channel::<()>();
        let wait = std::sync::Mutex::new(wait);
        let stuck: FileNamer = Arc::new(move |_| {
            let _ = wait.lock().unwrap().recv_timeout(Duration::from_secs(2));
            None
        });
        let dir = std::env::temp_dir().join(format!("rustwtm-untyped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = std::fs::File::create(dir.join("a.txt")).unwrap();
        let wide: Vec<u16> = format!("RustWTM-untyped-{}", std::process::id())
            .encode_utf16()
            .chain(Some(0))
            .collect();
        // SAFETY: `wide` is NUL-terminated; the event is closed by the guard.
        let event = OwnedHandle::new(unsafe {
            windows::Win32::System::Threading::CreateEventW(
                None,
                true,
                false,
                windows::core::PCWSTR(wide.as_ptr()),
            )
            .unwrap()
        });
        let h = ProcessHandle::open(
            std::process::id(),
            Access::QUERY_INFORMATION | Access::DUP_HANDLE,
        )
        .unwrap();
        // As if the type map were empty.
        let mut handles = vec![
            HandleInfo {
                value: file.as_raw_handle() as usize as u64,
                ..HandleInfo::default()
            },
            HandleInfo {
                value: event.raw().0 as usize as u64,
                ..HandleInfo::default()
            },
        ];
        let incomplete = h.name_handles(&mut handles, stuck, &BUSY);
        assert!(incomplete);
        assert_eq!(
            (handles[0].kind.as_str(), handles[0].name.as_deref()),
            ("File", None)
        );
        assert_eq!(handles[1].kind, "Event");
        assert!(
            handles[1]
                .name
                .as_deref()
                .unwrap()
                .contains("RustWTM-untyped-")
        );
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn registry_paths_are_shortened() {
        assert_eq!(
            friendly_key(r"\REGISTRY\MACHINE\SOFTWARE\x"),
            r"HKLM\SOFTWARE\x"
        );
        assert_eq!(friendly_key(r"\REGISTRY\USER\S-1-5-21"), r"HKU\S-1-5-21");
        assert_eq!(friendly_key(r"\BaseNamedObjects\x"), r"\BaseNamedObjects\x");
    }

    #[test]
    fn a_short_or_odd_buffer_parses_to_nothing() {
        assert!(parse_types(&[]).is_empty());
        assert!(parse_types(&[3]).is_empty());
        assert_eq!(unicode_string_at(&[0; 1], 0), None);
        // A string pointing outside the buffer is not read.
        let mut buf = vec![0u64; 4];
        buf[0] = 8;
        buf[1] = 16;
        assert_eq!(unicode_string_at(&buf, 0), None);
    }
}
