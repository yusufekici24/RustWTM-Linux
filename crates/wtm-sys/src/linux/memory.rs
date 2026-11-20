//! Where physical memory goes, from /proc/meminfo, mapped onto the Windows
//! page lists: dirty and writeback pages are "modified", clean page cache
//! "standby", MemFree "free"; reclaimable slab is the paged pool and
//! unreclaimable slab, kernel stacks and page tables the non-paged pool.

use std::fs;

use crate::linux::util::kv_bytes;
use crate::{Error, Result};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Composition {
    pub total: u64,
    pub available: u64,
    pub installed: Option<u64>,
    pub paged_pool: u64,
    pub nonpaged_pool: u64,
    pub modified: Option<u64>,
    pub standby: Option<u64>,
    pub free: Option<u64>,
}

impl Composition {
    pub fn in_use(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }

    pub fn hardware_reserved(&self) -> Option<u64> {
        self.installed
            .filter(|installed| *installed >= self.total)
            .map(|installed| installed - self.total)
    }

    pub fn cached(&self) -> Option<u64> {
        Some(self.standby? + self.modified?)
    }
}

/// Installed RAM: the DIMMs in the SMBIOS tables when readable (root),
/// otherwise the memory blocks the kernel knows, rounded up to a GiB.
pub(crate) fn installed() -> Option<u64> {
    if let Ok(modules) = crate::smbios::memory_modules() {
        let sum: u64 = modules.iter().map(|m| m.size).sum();
        if sum > 0 {
            return Some(sum);
        }
    }
    let block = u64::from_str_radix(
        crate::linux::util::read_trim("/sys/devices/system/memory/block_size_bytes")?.as_str(),
        16,
    )
    .ok()?;
    let blocks = fs::read_dir("/sys/devices/system/memory")
        .ok()?
        .filter(|e| {
            e.as_ref()
                .is_ok_and(|e| e.file_name().to_string_lossy().starts_with("memory"))
        })
        .count() as u64;
    let bytes = block * blocks;
    const GIB: u64 = 1 << 30;
    (bytes > 0).then(|| bytes.div_ceil(GIB) * GIB)
}

pub fn composition() -> Result<Composition> {
    let m = fs::read_to_string("/proc/meminfo").map_err(|e| Error::io("/proc/meminfo", e))?;
    Ok(from_meminfo(&m, installed()))
}

pub(crate) fn from_meminfo(m: &str, installed: Option<u64>) -> Composition {
    let g = |k| kv_bytes(m, k).unwrap_or(0);
    let total = g("MemTotal");
    let available = kv_bytes(m, "MemAvailable").unwrap_or_else(|| g("MemFree") + g("Cached"));
    let modified = g("Dirty") + g("Writeback");
    let cache = (g("Cached") + g("Buffers")).saturating_sub(g("Shmem"));
    Composition {
        total,
        available,
        installed,
        paged_pool: g("SReclaimable"),
        nonpaged_pool: g("SUnreclaim") + g("KernelStack") + g("PageTables"),
        modified: Some(modified),
        standby: Some(cache.saturating_sub(modified)),
        free: Some(g("MemFree")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_meminfo() {
        let m = "MemTotal: 1000 kB\nMemFree: 100 kB\nMemAvailable: 600 kB\nBuffers: 50 kB\nCached: 500 kB\nShmem: 50 kB\nDirty: 20 kB\nWriteback: 0 kB\nSReclaimable: 30 kB\nSUnreclaim: 10 kB\nKernelStack: 5 kB\nPageTables: 5 kB\n";
        let c = from_meminfo(m, Some(1024 * 1024));
        assert_eq!(c.total, 1000 * 1024);
        assert_eq!(c.in_use(), 400 * 1024);
        assert_eq!(c.modified, Some(20 * 1024));
        assert_eq!(c.standby, Some(480 * 1024));
        assert_eq!(c.nonpaged_pool, 20 * 1024);
        assert_eq!(c.hardware_reserved(), Some(24 * 1024));
    }

    #[test]
    fn reads_this_machine() {
        let c = composition().unwrap();
        assert!(c.total > 0 && c.available <= c.total);
    }
}
