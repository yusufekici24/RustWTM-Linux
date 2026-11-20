//! A process's address space as VMMap and System Informer's Memory tab
//! show it: each allocation with its kind, reserved and committed size,
//! protection and, for images and mapped files, the file.
//!
//! The layout is read with `VirtualQueryEx`, which needs only
//! [`Access::QUERY`](crate::control::Access::QUERY) on Windows 8.1 and
//! later. Naming heaps, stacks, the PEB and TEBs
//! ([`name_usage`](ProcessHandle::name_usage)) also reads the pointers the
//! process keeps to them and the signature word of each private
//! allocation's header, which needs the right to read its memory; nothing
//! else in that memory is read.
//! Naming a mapped file asks the file system, which can stall on a network
//! share that stopped answering; names are budgeted, and callers read the
//! map off the UI thread.

use std::time::{Duration, Instant};

use windows::Win32::System::Memory::{MEM_FREE, MEMORY_BASIC_INFORMATION, VirtualQueryEx};
use windows::Win32::System::ProcessStatus::GetMappedFileNameW;
use windows::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};

use crate::Result;
use crate::control::ProcessHandle;

/// Regions walked at most, and the time spent; a process can hold
/// hundreds of thousands, and the map is read while it changes.
const MOST_REGIONS: usize = 200_000;
const BUDGET: Duration = Duration::from_secs(3);
/// File names looked up at most per map.
const MOST_NAMES: usize = 20_000;

const MEM_COMMIT: u32 = 0x1000;
const MEM_IMAGE: u32 = 0x100_0000;
const MEM_MAPPED: u32 = 0x4_0000;

/// What backs an allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum MemoryKind {
    /// An executable or DLL, mapped from its file.
    Image,
    /// A view of a file.
    Mapped,
    /// Shared memory without a file (paging-file backed sections, and the
    /// huge reservation of Control Flow Guard's bitmap).
    Shareable,
    /// The process's own memory: heaps, stacks and everything it allocates.
    #[default]
    Private,
}

/// What the process itself keeps in a private allocation, from the
/// pointers in its PEB and its threads' TEBs and from heap headers. `wow64` marks the 32-bit
/// copies a 32-bit process on 64-bit Windows has besides the native ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Usage {
    /// The process environment block.
    Peb { wow64: bool },
    /// A thread's environment block.
    Teb { tid: u32, wow64: bool },
    /// A thread's stack.
    Stack { tid: u32, wow64: bool },
    /// A heap, or a segment a heap grew by, known by the signature in its
    /// header; `default` is the process's default heap. Low-fragmentation
    /// and large blocks a heap allocates on its own are not named.
    Heap { default: bool, wow64: bool },
}

/// One allocation: the regions reserved together at one base address.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Allocation {
    pub base: u64,
    pub kind: MemoryKind,
    /// Address space it spans, committed or not.
    pub reserved: u64,
    /// Of that, backed by memory or the paging file.
    pub committed: u64,
    pub regions: u32,
    /// The protection it was reserved with (`PAGE_*`).
    pub protection: u32,
    /// Some committed region of it may be executed. In private memory this
    /// is what JIT compilers (.NET, JavaScript, Java) produce, and also
    /// what injected code needs.
    pub executable: bool,
    /// The file of an image or mapped view, as a drive path.
    pub name: Option<String>,
    /// What the process keeps in it, in order; empty when unknown.
    pub usage: Vec<Usage>,
}

/// The allocations of a process, in address order.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryMap {
    pub allocations: Vec<Allocation>,
    /// The walk or the names stopped at a cap or the time budget.
    pub incomplete: bool,
    /// Heaps, stacks, the PEB and TEBs could not be named: the process
    /// refused this program the right to read its memory.
    pub usage_unread: bool,
}

impl MemoryMap {
    /// Labels each allocation that contains one of `marks`' addresses.
    pub(crate) fn label(&mut self, marks: &[(u64, Usage)]) {
        for &(address, usage) in marks {
            // Allocations are in address order and never overlap.
            let i = self.allocations.partition_point(|a| a.base <= address);
            let Some(a) = i.checked_sub(1).map(|i| &mut self.allocations[i]) else {
                continue;
            };
            if address - a.base < a.reserved && !a.usage.contains(&usage) {
                a.usage.push(usage);
                a.usage.sort_unstable();
            }
        }
    }
}

/// Threads named at most.
const MOST_THREADS: usize = 4096;

// Offsets in the native (64-bit) and WOW64 (32-bit) PEB and TEB. The PEB's
// heap list starts with the default heap but is not complete on every
// build (on 26300 it held 1 of a test process's 4 heaps), so only that
// entry is read.
const PEB_PROCESS_HEAPS: usize = 0xF0;
const PEB32_PROCESS_HEAPS: usize = 0x90;
/// Where a heap's header holds its signature: `SegmentSignature` of an NT
/// heap segment, `Signature` of a segment heap. In a 32-bit process only
/// NT heaps were found (no segment heap to measure), so only they are
/// recognized there.
const HEAP_SIGNATURE: usize = 0x10;
const HEAP32_SIGNATURE: usize = 0x8;
const NT_HEAP_SEGMENT: u32 = 0xFFEE_FFEE;
const SEGMENT_HEAP: u32 = 0xDDEE_DDEE;
/// `NT_TIB.StackBase` and `NT_TIB.Self`.
const TEB_STACK_BASE: usize = 0x8;
const TEB_SELF: usize = 0x30;
const TEB32_STACK_BASE: usize = 0x4;
const TEB32_SELF: usize = 0x18;
/// A WOW64 thread's 32-bit TEB follows its native one.
const TEB32_AFTER_TEB: usize = 0x2000;

/// How far a walk may go; tests make it short.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub regions: usize,
    pub budget: Duration,
    pub names: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            regions: MOST_REGIONS,
            budget: BUDGET,
            names: MOST_NAMES,
        }
    }
}

impl ProcessHandle {
    /// The process's allocations.
    pub fn memory_map(&self) -> Result<MemoryMap> {
        self.memory_map_with(Limits::default())
    }

    pub(crate) fn memory_map_with(&self, limits: Limits) -> Result<MemoryMap> {
        let start = Instant::now();
        let mut info = SYSTEM_INFO::default();
        // SAFETY: `info` is a writable SYSTEM_INFO.
        unsafe { GetSystemInfo(&mut info) };
        let highest = info.lpMaximumApplicationAddress as usize;

        let mut map = MemoryMap::default();
        let mut address = 0usize;
        let mut regions = 0usize;
        let mut first = true;
        loop {
            if address > highest {
                break;
            }
            if regions >= limits.regions || start.elapsed() > limits.budget {
                map.incomplete = true;
                break;
            }
            let mut mbi = MEMORY_BASIC_INFORMATION::default();
            // SAFETY: `mbi` is writable for its full size; the address is
            // only probed by the system call.
            let written = unsafe {
                VirtualQueryEx(
                    self.raw(),
                    Some(address as *const core::ffi::c_void),
                    &mut mbi,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            if written == 0 {
                // The first query failing is a missing right, not the end.
                if first {
                    return Err(windows::core::Error::from_thread().into());
                }
                break;
            }
            first = false;
            regions += 1;
            let base = mbi.BaseAddress as usize;
            let Some(next) = base.checked_add(mbi.RegionSize) else {
                break;
            };
            // The process changes its address space while this walks it:
            // never step backwards or stand still.
            if mbi.RegionSize == 0 || next <= address {
                break;
            }
            if mbi.State != MEM_FREE {
                add_region(&mut map.allocations, &mbi);
            }
            address = next;
        }

        let devices = crate::disktrace::dos_devices();
        let mut named = 0usize;
        for a in &mut map.allocations {
            if !matches!(a.kind, MemoryKind::Image | MemoryKind::Mapped) {
                continue;
            }
            if named >= limits.names || start.elapsed() > limits.budget {
                map.incomplete = true;
                break;
            }
            named += 1;
            a.name = self
                .mapped_file(a.base)
                .map(|n| crate::disktrace::dos_path(&n, &devices));
            if a.kind == MemoryKind::Mapped && a.name.is_none() {
                a.kind = MemoryKind::Shareable;
            }
        }
        Ok(map)
    }

    /// Names `map`'s heaps (by the signature in each private allocation's
    /// header, the default heap by the PEB), the PEB and, for each of
    /// `threads` (thread IDs; at most 4,096), its TEB and stack. Needs the
    /// right to read the process's memory: without it this fails and
    /// names nothing. A process still starting may have no default heap
    /// yet, which is then left unnamed. A thread that exits or refuses to
    /// be opened meanwhile
    /// is left out; at `deadline` naming stops and the map is marked
    /// incomplete.
    pub fn name_usage(
        &self,
        map: &mut MemoryMap,
        threads: &[u32],
        deadline: Instant,
    ) -> Result<()> {
        let mut marks = Vec::new();
        let peb = self.peb_address()?;
        marks.push((peb as u64, Usage::Peb { wow64: false }));
        // Reading the PEB fails without the right to read the process. A
        // process still starting, before its loader ran, has no heap list
        // yet: its PEB and threads are still named.
        let heaps = self.remote_word(peb + PEB_PROCESS_HEAPS)?;
        let default = (heaps != 0)
            .then(|| self.remote_word(heaps).ok())
            .flatten()
            .filter(|&heap| heap != 0)
            .map(|heap| heap as u64);
        if let Some(heap) = default {
            marks.push((
                heap,
                Usage::Heap {
                    default: true,
                    wow64: false,
                },
            ));
        }
        let peb32 = self.peb32_address().ok().flatten();
        let mut default32 = None;
        if let Some(peb32) = peb32 {
            marks.push((peb32 as u64, Usage::Peb { wow64: true }));
            // A 32-bit process still starting may not have its heap yet.
            default32 = self
                .remote_u32(peb32 + PEB32_PROCESS_HEAPS)
                .and_then(|list| self.remote_u32(list as usize))
                .ok()
                .filter(|&heap| heap != 0)
                .map(u64::from);
            if let Some(heap) = default32 {
                marks.push((
                    heap,
                    Usage::Heap {
                        default: true,
                        wow64: true,
                    },
                ));
            }
        }

        let mut late = false;
        for a in &map.allocations {
            // Heaps Windows shares between processes (such as the one for
            // talking to csrss) live in shared memory without a file.
            let heap_kind = matches!(a.kind, MemoryKind::Private | MemoryKind::Shareable);
            if !heap_kind || a.committed == 0 || Some(a.base) == default {
                continue;
            }
            if Instant::now() > deadline {
                late = true;
                break;
            }
            let base = a.base as usize;
            let signature = |at: usize| self.remote_u32(base + at).ok();
            let wow64 = if matches!(
                signature(HEAP_SIGNATURE),
                Some(NT_HEAP_SEGMENT | SEGMENT_HEAP)
            ) {
                false
            } else if peb32.is_some()
                && Some(a.base) != default32
                && signature(HEAP32_SIGNATURE) == Some(NT_HEAP_SEGMENT)
            {
                true
            } else {
                continue;
            };
            marks.push((
                a.base,
                Usage::Heap {
                    default: false,
                    wow64,
                },
            ));
        }

        let pid = self.pid();
        late |= threads.len() > MOST_THREADS;
        for &tid in threads.iter().take(MOST_THREADS) {
            if late || Instant::now() > deadline {
                late = true;
                break;
            }
            let Some(teb) = crate::threads::teb_address(tid, pid) else {
                continue;
            };
            let teb = teb as usize;
            // The TEB names itself; anything else is not one (the thread
            // exited and its memory was reused).
            if self.remote_word(teb + TEB_SELF).ok() != Some(teb) {
                continue;
            }
            marks.push((teb as u64, Usage::Teb { tid, wow64: false }));
            // The stack grows down from its base: the byte below is in it.
            if let Ok(base) = self.remote_word(teb + TEB_STACK_BASE)
                && base != 0
            {
                marks.push((base as u64 - 1, Usage::Stack { tid, wow64: false }));
            }
            if peb32.is_some() {
                let teb32 = teb + TEB32_AFTER_TEB;
                if self.remote_u32(teb32 + TEB32_SELF).ok() == Some(teb32 as u32) {
                    marks.push((teb32 as u64, Usage::Teb { tid, wow64: true }));
                    if let Ok(base) = self.remote_u32(teb32 + TEB32_STACK_BASE)
                        && base != 0
                    {
                        marks.push((u64::from(base) - 1, Usage::Stack { tid, wow64: true }));
                    }
                }
            }
        }
        map.label(&marks);
        map.incomplete |= late;
        Ok(())
    }

    /// The NT path of the file mapped at `base`, if any.
    fn mapped_file(&self, base: u64) -> Option<String> {
        let mut name = vec![0u16; 1024];
        // SAFETY: `name` is writable for its length.
        let len = unsafe {
            GetMappedFileNameW(
                self.raw(),
                base as usize as *const core::ffi::c_void,
                &mut name,
            )
        } as usize;
        (len > 0).then(|| String::from_utf16_lossy(&name[..len.min(name.len())]))
    }
}

/// Adds a region to the allocation it belongs to; an allocation's regions
/// are contiguous, so it is the last one or a new one.
fn add_region(allocations: &mut Vec<Allocation>, mbi: &MEMORY_BASIC_INFORMATION) {
    let base = mbi.AllocationBase as usize as u64;
    let size = mbi.RegionSize as u64;
    let committed = mbi.State.0 == MEM_COMMIT;
    let executable = committed && is_executable(mbi.Protect.0);
    match allocations.last_mut() {
        Some(a) if a.base == base => {
            a.reserved += size;
            a.regions += 1;
        }
        _ => allocations.push(Allocation {
            base,
            kind: kind_of(mbi.Type.0),
            reserved: size,
            regions: 1,
            protection: mbi.AllocationProtect.0,
            ..Allocation::default()
        }),
    }
    let a = allocations.last_mut().expect("just added");
    if committed {
        a.committed += size;
    }
    a.executable |= executable;
}

fn kind_of(page_type: u32) -> MemoryKind {
    match page_type {
        MEM_IMAGE => MemoryKind::Image,
        MEM_MAPPED => MemoryKind::Mapped,
        _ => MemoryKind::Private,
    }
}

/// Whether a `PAGE_*` protection allows execution; the guard, no-cache and
/// write-combine modifiers are ignored.
pub fn is_executable(protect: u32) -> bool {
    protect & 0xF0 != 0
}

/// A `PAGE_*` protection as VMMap abbreviates it ("RW", "RX", "RWX", "WC"
/// for copy-on-write); empty for none (reserved memory reports 0).
pub fn protection_code(protect: u32) -> &'static str {
    match protect & 0xFF {
        0x01 => "NA",
        0x02 => "R",
        0x04 => "RW",
        0x08 => "WC",
        0x10 => "X",
        0x20 => "RX",
        0x40 => "RWX",
        0x80 => "RWXC",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Access;
    use windows::Win32::System::Memory::{
        CreateFileMappingW, FILE_MAP_READ, MEM_COMMIT as COMMIT, MEM_RELEASE, MEM_RESERVE,
        MapViewOfFile, PAGE_EXECUTE_READ, PAGE_READONLY, PAGE_READWRITE, UnmapViewOfFile,
        VirtualAlloc, VirtualFree,
    };

    #[test]
    fn maps_its_own_private_executable_and_file_backed_memory() {
        // Small allocations, freed at the end.
        // SAFETY: plain allocations, released below.
        let data = unsafe { VirtualAlloc(None, 64 * 1024, MEM_RESERVE | COMMIT, PAGE_READWRITE) };
        // SAFETY: as above; never executed.
        let code = unsafe { VirtualAlloc(None, 4096, MEM_RESERVE | COMMIT, PAGE_EXECUTE_READ) };
        // Reserved only: counted as address space, not as committed.
        // SAFETY: as above.
        let held = unsafe { VirtualAlloc(None, 256 * 1024, MEM_RESERVE, PAGE_READWRITE) };
        assert!(!data.is_null() && !code.is_null() && !held.is_null());

        let dir = std::env::temp_dir().join(format!("rustwtm-map-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("view.bin");
        std::fs::write(&path, vec![7u8; 8192]).unwrap();
        let file = std::fs::File::open(&path).unwrap();
        use std::os::windows::io::AsRawHandle;
        // SAFETY: the file handle is live; the mapping is closed below.
        let section = crate::handle::OwnedHandle::new(unsafe {
            CreateFileMappingW(
                windows::Win32::Foundation::HANDLE(file.as_raw_handle()),
                None,
                PAGE_READONLY,
                0,
                0,
                windows::core::PCWSTR::null(),
            )
            .unwrap()
        });
        // SAFETY: maps the whole small file; unmapped below.
        let view = unsafe { MapViewOfFile(section.raw(), FILE_MAP_READ, 0, 0, 0) };
        assert!(!view.Value.is_null());

        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        let map = h.memory_map().unwrap();
        let at = |p: *const core::ffi::c_void| {
            map.allocations
                .iter()
                .find(|a| a.base == p as usize as u64)
                .cloned()
                .unwrap()
        };
        let d = at(data);
        assert_eq!(
            (d.kind, d.committed, d.executable),
            (MemoryKind::Private, 64 * 1024, false)
        );
        let c = at(code);
        assert_eq!((c.kind, c.executable), (MemoryKind::Private, true));
        let r = at(held);
        assert_eq!((r.reserved, r.committed, r.regions), (256 * 1024, 0, 1));
        let v = at(view.Value);
        assert_eq!(v.kind, MemoryKind::Mapped);
        let name = v.name.clone().unwrap().to_lowercase();
        assert!(
            name.ends_with(r"\view.bin") && !name.starts_with(r"\device"),
            "{v:?}"
        );

        let exe = std::env::current_exe().unwrap();
        let exe_name = exe.file_name().unwrap().to_string_lossy().to_lowercase();
        assert!(map.allocations.iter().any(|a| {
            a.kind == MemoryKind::Image
                && a.name
                    .as_deref()
                    .is_some_and(|n| n.to_lowercase().ends_with(&exe_name))
        }));
        assert!(!map.incomplete);
        // In address order, never overlapping.
        assert!(
            map.allocations
                .windows(2)
                .all(|w| w[0].base + w[0].reserved <= w[1].base)
        );

        // SAFETY: each was allocated or mapped above and is released once.
        unsafe {
            UnmapViewOfFile(view).unwrap();
            VirtualFree(data, 0, MEM_RELEASE).unwrap();
            VirtualFree(code, 0, MEM_RELEASE).unwrap();
            VirtualFree(held, 0, MEM_RELEASE).unwrap();
        }
        drop(section);
        drop(file);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_walk_stops_at_its_caps() {
        let h = ProcessHandle::open(std::process::id(), Access::QUERY_INFORMATION).unwrap();
        let map = h
            .memory_map_with(Limits {
                regions: 3,
                ..Limits::default()
            })
            .unwrap();
        assert!(map.incomplete && map.allocations.len() <= 3);
        let map = h
            .memory_map_with(Limits {
                names: 0,
                ..Limits::default()
            })
            .unwrap();
        assert!(map.incomplete);
        assert!(map.allocations.iter().all(|a| a.name.is_none()));
    }

    #[test]
    fn limited_query_rights_are_enough() {
        // Windows 8.1 and later answer layout and file names with limited
        // query rights, which more processes grant than full ones.
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        let map = h.memory_map().unwrap();
        assert!(map.allocations.iter().any(|a| a.name.is_some()));
    }

    fn thread_ids(pid: u32) -> Vec<u32> {
        crate::process::ProcessSnapshotter::new()
            .threads(pid)
            .unwrap()
            .iter()
            .map(|t| t.tid)
            .collect()
    }

    fn containing(map: &MemoryMap, address: u64) -> &Allocation {
        map.allocations
            .iter()
            .find(|a| a.base <= address && address - a.base < a.reserved)
            .unwrap()
    }

    fn named(h: &ProcessHandle, pid: u32) -> MemoryMap {
        let mut map = h.memory_map().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        h.name_usage(&mut map, &thread_ids(pid), deadline).unwrap();
        map
    }

    fn all_usage(map: &MemoryMap) -> impl Iterator<Item = (&Allocation, Usage)> {
        map.allocations
            .iter()
            .flat_map(|a| a.usage.iter().map(move |&u| (a, u)))
    }

    #[test]
    fn names_its_own_heaps_stacks_peb_and_tebs() {
        // A second thread, parked until the map is read; normal stack size.
        let (stop, parked) = std::sync::mpsc::channel::<()>();
        let (ready, started) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            let local = 7u64;
            ready
                .send((crate::threads::current_thread_id(), &raw const local as u64))
                .unwrap();
            let _ = parked.recv();
            std::hint::black_box(local);
        });
        let (worker_tid, worker_local) = started.recv().unwrap();

        // Other tests in this process create and destroy heaps meanwhile:
        // only those there before and after the map was read are checked.
        let process_heaps = || {
            let mut heaps = vec![windows::Win32::Foundation::HANDLE::default(); 1024];
            // SAFETY: `heaps` is writable for its length.
            let count = unsafe { windows::Win32::System::Memory::GetProcessHeaps(&mut heaps) };
            heaps.truncate(count as usize);
            heaps
        };
        let before = process_heaps();
        let pid = std::process::id();
        let h = ProcessHandle::open(pid, Access::READ_PARAMETERS).unwrap();
        let map = named(&h, pid);
        assert!(!map.incomplete);
        let after = process_heaps();
        let heaps: Vec<_> = before.iter().filter(|h| after.contains(h)).collect();
        assert!(!heaps.is_empty());

        // Every heap Windows reports is named, the default one as such.
        // SAFETY: plain call.
        let default = unsafe { windows::Win32::System::Memory::GetProcessHeap() }.unwrap();
        for heap in heaps {
            let a = containing(&map, heap.0 as usize as u64);
            let usage = Usage::Heap {
                default: *heap == default,
                wow64: false,
            };
            assert_eq!(a.usage, vec![usage], "{a:?}");
        }
        assert_eq!(
            all_usage(&map)
                .filter(|(_, u)| matches!(u, Usage::Heap { default: true, .. }))
                .count(),
            1
        );

        let me = crate::threads::current_thread_id();
        let local = 7u64;
        for (address, tid) in [(&raw const local as u64, me), (worker_local, worker_tid)] {
            let a = containing(&map, address);
            assert!(
                a.usage.contains(&Usage::Stack { tid, wow64: false }),
                "{a:?}"
            );
        }
        for usage in [
            Usage::Peb { wow64: false },
            Usage::Teb {
                tid: me,
                wow64: false,
            },
        ] {
            assert!(all_usage(&map).any(|(_, u)| u == usage), "{usage:?}");
        }
        // Only private memory is named, besides heaps Windows shares, and a
        // native process has no 32-bit copies.
        for (a, u) in all_usage(&map) {
            let heap = matches!(u, Usage::Heap { .. }) && a.kind == MemoryKind::Shareable;
            assert!(a.kind == MemoryKind::Private || heap, "{u:?} {a:?}");
            assert!(
                !matches!(
                    u,
                    Usage::Peb { wow64: true }
                        | Usage::Teb { wow64: true, .. }
                        | Usage::Stack { wow64: true, .. }
                        | Usage::Heap { wow64: true, .. }
                ),
                "{u:?}"
            );
        }

        stop.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn query_rights_alone_name_nothing() {
        let pid = std::process::id();
        let h = ProcessHandle::open(pid, Access::QUERY).unwrap();
        let mut map = h.memory_map().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        assert!(h.name_usage(&mut map, &thread_ids(pid), deadline).is_err());
        assert!(map.allocations.iter().all(|a| a.usage.is_empty()));
    }

    #[test]
    fn a_passed_deadline_names_only_the_peb_and_default_heap() {
        let pid = std::process::id();
        let h = ProcessHandle::open(pid, Access::READ_PARAMETERS).unwrap();
        let mut map = h.memory_map().unwrap();
        h.name_usage(&mut map, &thread_ids(pid), Instant::now())
            .unwrap();
        assert!(map.incomplete);
        let mut usage: Vec<Usage> = all_usage(&map).map(|(_, u)| u).collect();
        usage.sort_unstable();
        assert_eq!(
            usage,
            vec![
                Usage::Peb { wow64: false },
                Usage::Heap {
                    default: true,
                    wow64: false
                }
            ]
        );
    }

    #[test]
    fn a_process_still_starting_has_its_peb_named_without_a_heap() {
        // Started suspended, before its loader ran: the PEB exists but its
        // heap list does not yet. One small child, ended below.
        use std::os::windows::process::CommandExt;
        const CREATE_SUSPENDED: u32 = 0x4;
        let mut child = std::process::Command::new(r"C:\Windows\System32\cmd.exe")
            .args(["/d", "/c", "exit"])
            .creation_flags(CREATE_SUSPENDED)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let h = ProcessHandle::open(pid, Access::READ_PARAMETERS).unwrap();
        let mut map = h.memory_map().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let named = h.name_usage(&mut map, &thread_ids(pid), deadline);
        let _ = child.kill();
        let _ = child.wait();
        named.unwrap();
        assert!(all_usage(&map).any(|(_, u)| u == Usage::Peb { wow64: false }));
        assert!(
            !all_usage(&map).any(|(_, u)| matches!(u, Usage::Heap { default: true, .. })),
            "{map:?}"
        );
    }

    #[test]
    fn names_a_32_bit_childs_own_heaps_and_stacks() {
        let program = r"C:\Windows\SysWOW64\cmd.exe";
        if !std::path::Path::new(program).exists() {
            return;
        }
        // `pause` waits on the open pipe until the child is ended.
        let mut child = std::process::Command::new(program)
            .args(["/d", "/c", "pause"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let h = ProcessHandle::open(pid, Access::READ_PARAMETERS).unwrap();
        // The child sets up its 32-bit side as it starts.
        let until = Instant::now() + Duration::from_secs(10);
        let map = loop {
            let map = named(&h, pid);
            let has = |f: fn(&Usage) -> bool| all_usage(&map).any(|(_, u)| f(&u));
            if has(|u| matches!(u, Usage::Stack { wow64: true, .. }))
                && has(|u| {
                    matches!(
                        u,
                        Usage::Heap {
                            default: true,
                            wow64: true
                        }
                    )
                })
            {
                break map;
            }
            assert!(Instant::now() < until, "{map:?}");
            std::thread::sleep(Duration::from_millis(20));
        };
        let _ = child.kill();
        let _ = child.wait();
        assert!(all_usage(&map).any(|(_, u)| u == Usage::Peb { wow64: true }));
        assert!(all_usage(&map).any(|(_, u)| u
            == Usage::Heap {
                default: true,
                wow64: false
            }));
        // 32-bit stacks and heaps lie below 4 GiB, a thread's two stacks
        // are separate allocations, and only private memory is named
        // besides shared heaps.
        for (a, u) in all_usage(&map) {
            let heap = matches!(u, Usage::Heap { .. }) && a.kind == MemoryKind::Shareable;
            assert!(a.kind == MemoryKind::Private || heap, "{u:?} {a:?}");
            if let Usage::Stack { wow64: true, .. } | Usage::Heap { wow64: true, .. } = u {
                assert!(a.base < 1 << 32, "{u:?}");
            }
            if let Usage::Stack { tid, wow64: true } = u {
                assert!(!a.usage.contains(&Usage::Stack { tid, wow64: false }));
            }
        }
    }

    #[test]
    fn reading_heap_signatures_leaves_guard_pages_armed() {
        use windows::Win32::System::Memory::{PAGE_GUARD, VirtualQuery};
        // A committed guard page at an allocation's base, where heap
        // signatures are looked for: the read is refused and the guard,
        // which the process may rely on, stays.
        // SAFETY: one small allocation, freed below.
        let guarded = unsafe {
            VirtualAlloc(
                None,
                4096,
                MEM_RESERVE | COMMIT,
                PAGE_READWRITE | PAGE_GUARD,
            )
        };
        assert!(!guarded.is_null());
        let protect = || {
            let mut mbi = MEMORY_BASIC_INFORMATION::default();
            // SAFETY: `mbi` is writable for its full size.
            unsafe {
                VirtualQuery(
                    Some(guarded),
                    &mut mbi,
                    size_of::<MEMORY_BASIC_INFORMATION>(),
                )
            };
            mbi.Protect.0
        };
        assert_eq!(protect(), 0x104);
        let pid = std::process::id();
        let h = ProcessHandle::open(pid, Access::READ_PARAMETERS).unwrap();
        let map = named(&h, pid);
        let a = containing(&map, guarded as usize as u64);
        assert!(a.usage.is_empty(), "{a:?}");
        assert_eq!(protect(), 0x104);
        // SAFETY: allocated above and released once.
        unsafe { VirtualFree(guarded, 0, MEM_RELEASE).unwrap() };
    }

    #[test]
    fn labels_go_to_the_allocation_holding_the_address() {
        let alloc = |base: u64, reserved: u64| Allocation {
            base,
            reserved,
            ..Allocation::default()
        };
        let mut map = MemoryMap {
            allocations: vec![alloc(0x1000, 0x1000), alloc(0x4000, 0x2000)],
            ..MemoryMap::default()
        };
        let stack = Usage::Stack {
            tid: 4,
            wow64: false,
        };
        let peb = Usage::Peb { wow64: false };
        map.label(&[
            (0x5fff, stack),
            (0x4000, peb),
            (0x5000, stack),
            // Below, between and above the allocations: nothing to label.
            (0x10, peb),
            (0x2000, peb),
            (0x9000, peb),
        ]);
        assert!(map.allocations[0].usage.is_empty());
        assert_eq!(map.allocations[1].usage, vec![peb, stack]);
    }

    #[test]
    fn protections_decode_without_their_modifiers() {
        assert_eq!(protection_code(0x04), "RW");
        assert_eq!(protection_code(0x104), "RW", "guard page");
        assert_eq!(protection_code(0x40), "RWX");
        assert_eq!(protection_code(0), "");
        assert!(is_executable(0x20) && is_executable(0x220) && !is_executable(0x104));
    }
}
