//! Windows' Event Tracing has no Linux counterpart; the tracers here poll
//! kernel counters instead (see `nettrace` and `disktrace`). What remains
//! of this module keeps the names the crates above call.

/// A session name unique to this process, as on Windows.
pub fn owned_name(prefix: &str) -> String {
    format!("{prefix}-{}", std::process::id())
}

/// Nothing outlives a Linux process the way ETW sessions do.
pub fn stop_orphans(_prefix: &str) {}

pub fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

pub fn u64_at(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_and_reads() {
        assert!(super::owned_name("x").starts_with("x-"));
        assert_eq!(super::u32_at(&[1, 0, 0, 0, 9], 0), Some(1));
        assert_eq!(super::u64_at(&[1, 0, 0, 0], 0), None);
    }
}
