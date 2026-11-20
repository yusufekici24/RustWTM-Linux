//! The memory map of a process from /proc/PID/smaps, in the shape of
//! VMMap's: file mappings of one executable or library are one "image"
//! allocation, other file mappings "mapped", shared memory "shareable",
//! the rest "private". Protections are written as Windows `PAGE_*` values.

use std::time::{Duration, Instant};

use crate::Result;
use crate::control::ProcessHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
pub enum MemoryKind {
    Image,
    Mapped,
    Shareable,
    #[default]
    Private,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Usage {
    Peb { wow64: bool },
    Teb { tid: u32, wow64: bool },
    Stack { tid: u32, wow64: bool },
    Heap { default: bool, wow64: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Allocation {
    pub base: u64,
    pub kind: MemoryKind,
    pub reserved: u64,
    pub committed: u64,
    pub regions: u32,
    pub protection: u32,
    pub executable: bool,
    pub name: Option<String>,
    pub usage: Vec<Usage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoryMap {
    pub allocations: Vec<Allocation>,
    pub incomplete: bool,
    pub usage_unread: bool,
}

const MOST_REGIONS: usize = 200_000;
const BUDGET: Duration = Duration::from_secs(5);

pub(crate) fn page_protection(perms: &str) -> u32 {
    let b = perms.as_bytes();
    let (r, w, x) = (
        b.first() == Some(&b'r'),
        b.get(1) == Some(&b'w'),
        b.get(2) == Some(&b'x'),
    );
    match (r, w, x) {
        (false, false, false) => 0x01,
        (_, false, false) => 0x02,
        (_, true, false) => 0x04,
        (false | true, false, true) => 0x20,
        (_, true, true) => 0x40,
    }
}

impl ProcessHandle {
    pub fn memory_map(&self) -> Result<MemoryMap> {
        let started = Instant::now();
        let text = std::fs::read_to_string(self.proc_path("smaps"))
            .map_err(|e| crate::Error::io("smaps", e))?;
        self.check()?;
        let exe = self.image_path().ok();
        let mut map = MemoryMap::default();
        let mut cur: Option<Allocation> = None;
        let mut regions = 0usize;
        let push = |map: &mut MemoryMap, a: Allocation| {
            if let Some(last) = map.allocations.last_mut()
                && last.kind == MemoryKind::Image
                && a.kind == MemoryKind::Image
                && last.name == a.name
                && last.base + last.reserved == a.base
            {
                last.reserved += a.reserved;
                last.committed += a.committed;
                last.regions += a.regions;
                last.executable |= a.executable;
                return;
            }
            map.allocations.push(a);
        };
        for line in text.lines() {
            // Header lines start with an address range.
            let first = line.split_whitespace().next().unwrap_or("");
            if let Some((a, b)) = first.split_once('-')
                && let (Ok(a), Ok(b)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16))
            {
                if let Some(done) = cur.take() {
                    push(&mut map, done);
                }
                regions += 1;
                if regions > MOST_REGIONS || started.elapsed() > BUDGET {
                    map.incomplete = true;
                    break;
                }
                let f: Vec<&str> = line.split_whitespace().collect();
                let perms = f.get(1).copied().unwrap_or("----");
                let inode = f.get(4).copied().unwrap_or("0");
                let path = if f.len() > 5 {
                    Some(f[5..].join(" "))
                } else {
                    None
                };
                let shared = perms.ends_with('s');
                let kind = match &path {
                    Some(p) if inode != "0" && p.starts_with('/') && !p.starts_with("/dev/") => {
                        let code = p.contains(".so")
                            || exe.as_deref() == Some(p.trim_end_matches(" (deleted)"));
                        if code {
                            MemoryKind::Image
                        } else if shared {
                            MemoryKind::Shareable
                        } else {
                            MemoryKind::Mapped
                        }
                    }
                    Some(p)
                        if p.starts_with("/memfd:")
                            || p.starts_with("/dev/shm")
                            || p.starts_with("/SYSV") =>
                    {
                        MemoryKind::Shareable
                    }
                    _ if shared => MemoryKind::Shareable,
                    _ => MemoryKind::Private,
                };
                let mut usage = Vec::new();
                match path.as_deref() {
                    Some("[stack]") => usage.push(Usage::Stack {
                        tid: self.pid(),
                        wow64: false,
                    }),
                    Some("[heap]") => usage.push(Usage::Heap {
                        default: true,
                        wow64: false,
                    }),
                    _ => {}
                }
                let protection = page_protection(perms);
                cur = Some(Allocation {
                    base: a,
                    kind,
                    reserved: b.saturating_sub(a),
                    committed: 0,
                    regions: 1,
                    protection,
                    executable: is_executable(protection),
                    name: path
                        .filter(|p| p.starts_with('/'))
                        .map(|p| p.trim_end_matches(" (deleted)").to_owned()),
                    usage,
                });
                continue;
            }
            if let Some(a) = cur.as_mut() {
                let kb = |key: &str| {
                    line.strip_prefix(key)
                        .and_then(|r| r.split_whitespace().next()?.parse::<u64>().ok())
                };
                if let Some(v) = kb("Rss:") {
                    a.committed += v * 1024;
                } else if let Some(v) = kb("Swap:") {
                    a.committed += v * 1024;
                }
            }
        }
        if let Some(done) = cur.take() {
            push(&mut map, done);
        }
        Ok(map)
    }

    /// Names each thread's stack: the stack pointer of a thread waiting
    /// in a system call is in /proc/PID/task/TID/syscall (needs ptrace
    /// access: one's own processes, or root). The main thread's stack and
    /// the heap were named while the map was read; there is no PEB or TEB.
    pub fn name_usage(
        &self,
        map: &mut MemoryMap,
        threads: &[u32],
        deadline: Instant,
    ) -> Result<()> {
        self.check()?;
        let mut marks = Vec::new();
        for &tid in threads.iter().take(4096) {
            if Instant::now() >= deadline {
                map.incomplete = true;
                break;
            }
            let Ok(text) = std::fs::read_to_string(self.proc_path(&format!("task/{tid}/syscall")))
            else {
                map.usage_unread = true;
                continue;
            };
            // "nr arg1..arg6 sp pc", or "running" / "-1 sp pc".
            let f: Vec<&str> = text.split_whitespace().collect();
            if f.len() < 3 || f[0] == "running" {
                continue;
            }
            let Some(sp) = u64::from_str_radix(f[f.len() - 2].trim_start_matches("0x"), 16).ok()
            else {
                continue;
            };
            marks.push((sp, Usage::Stack { tid, wow64: false }));
        }
        for (address, usage) in marks {
            let i = map.allocations.partition_point(|a| a.base <= address);
            let Some(a) = i.checked_sub(1).map(|i| &mut map.allocations[i]) else {
                continue;
            };
            if address - a.base < a.reserved && !a.usage.contains(&usage) {
                a.usage.push(usage);
                a.usage.sort_unstable();
            }
        }
        Ok(())
    }
}

pub fn is_executable(protect: u32) -> bool {
    protect & 0xF0 != 0
}

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

    #[test]
    fn names_the_stack_of_a_waiting_thread() {
        let (tx, rx) = std::sync::mpsc::channel::<()>();
        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let t = std::thread::spawn(move || {
            tid_tx.send(crate::threads::current_thread_id()).unwrap();
            let _ = rx.recv();
        });
        let tid = tid_rx.recv().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        let mut m = h.memory_map().unwrap();
        h.name_usage(
            &mut m,
            &[tid],
            Instant::now() + std::time::Duration::from_secs(2),
        )
        .unwrap();
        assert!(
            m.allocations
                .iter()
                .any(|a| a.usage.contains(&Usage::Stack { tid, wow64: false }))
        );
        tx.send(()).unwrap();
        t.join().unwrap();
    }

    #[test]
    fn maps_itself() {
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        let m = h.memory_map().unwrap();
        assert!(
            m.allocations
                .iter()
                .any(|a| a.kind == MemoryKind::Image && a.executable)
        );
        assert!(
            m.allocations
                .iter()
                .any(|a| a.usage.iter().any(|u| matches!(u, Usage::Stack { .. })))
        );
        assert!(m.allocations.windows(2).all(|w| w[0].base < w[1].base));
        assert_eq!(protection_code(page_protection("r-xp")), "RX");
        assert_eq!(protection_code(page_protection("rw-p")), "RW");
    }
}
