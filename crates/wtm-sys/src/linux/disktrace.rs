//! Disk bytes per process and file, by polling. Each take reads every
//! process's storage counters (`read_bytes` / `write_bytes` in
//! /proc/PID/io); for the processes that moved bytes it reads the file
//! positions of their open regular files (/proc/PID/fdinfo) and gives each
//! file the bytes its position advanced. What no position explains
//! (memory-mapped I/O, writeback of earlier writes) is reported without a
//! path. Needs root for other users' processes.

use std::collections::HashMap;
use std::fs;
use std::sync::Mutex;

use crate::linux::util;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileIo {
    pub pid: u32,
    pub path: Option<String>,
    pub read: u64,
    pub write: u64,
}

#[derive(Debug, Default)]
struct State {
    /// pid -> (start ticks, read_bytes, write_bytes).
    io: HashMap<u32, (u64, u64, u64)>,
    /// (pid, fd) -> (path, position).
    positions: HashMap<(u32, u32), (String, u64)>,
}

#[derive(Debug)]
pub struct DiskTracer {
    state: Mutex<State>,
    names_session: String,
}

fn storage_io(pid: u32) -> Option<(u64, u64)> {
    let text = fs::read_to_string(format!("/proc/{pid}/io")).ok()?;
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k)?.trim().parse::<u64>().ok())
    };
    Some((get("read_bytes:")?, get("write_bytes:")?))
}

/// Open regular files: fd, path, position, access mode (0 r, 1 w, 2 rw).
fn open_files(pid: u32) -> Vec<(u32, String, u64, u32)> {
    let Ok(rd) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in rd.flatten() {
        let Some(fd) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let Ok(target) = fs::read_link(e.path()) else {
            continue;
        };
        let path = target.to_string_lossy();
        if !path.starts_with('/') || path.starts_with("/dev/") || path.starts_with("/proc/") {
            continue;
        }
        let Ok(info) = fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")) else {
            continue;
        };
        let pos = info
            .lines()
            .find_map(|l| l.strip_prefix("pos:")?.trim().parse().ok())
            .unwrap_or(0);
        let flags = info
            .lines()
            .find_map(|l| u32::from_str_radix(l.strip_prefix("flags:")?.trim(), 8).ok())
            .unwrap_or(0);
        out.push((
            fd,
            path.trim_end_matches(" (deleted)").to_owned(),
            pos,
            flags & 3,
        ));
    }
    out
}

impl DiskTracer {
    pub fn start(name: &str) -> crate::Result<Self> {
        let t = Self {
            state: Mutex::new(State::default()),
            names_session: format!("{name}-names"),
        };
        t.take();
        Ok(t)
    }

    /// Bytes per process and file since the previous call, largest first.
    pub fn take(&self) -> Vec<FileIo> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let first = state.io.is_empty();
        let mut io = HashMap::new();
        let mut out = Vec::new();
        let mut positions = HashMap::new();
        for pid in util::numeric_entries("/proc") {
            let Ok(stat) = util::Stat::read(pid) else {
                continue;
            };
            if stat.kernel_thread() {
                continue;
            }
            let Some((r, w)) = storage_io(pid) else {
                continue;
            };
            io.insert(pid, (stat.starttime, r, w));
            let (dr, dw) = match state.io.get(&pid) {
                Some(&(start, pr, pw)) if start == stat.starttime => {
                    (r.saturating_sub(pr), w.saturating_sub(pw))
                }
                Some(_) | None if first => (0, 0),
                _ => (r, w),
            };
            // Keep positions of every process with open files that moved
            // bytes, so the next take can diff them.
            if dr == 0 && dw == 0 && !state.positions.keys().any(|k| k.0 == pid) {
                continue;
            }
            let files = open_files(pid);
            let (mut left_r, mut left_w) = (dr, dw);
            for (fd, path, pos, mode) in files {
                if let Some((prev_path, prev_pos)) = state.positions.get(&(pid, fd))
                    && *prev_path == path
                    && pos > *prev_pos
                {
                    let moved = pos - prev_pos;
                    let (fr, fw) = match mode {
                        0 => (moved.min(left_r), 0),
                        1 => (0, moved.min(left_w)),
                        _ => {
                            let total = (left_r + left_w).max(1);
                            let r = (moved as u128 * left_r as u128 / total as u128) as u64;
                            (r.min(left_r), moved.saturating_sub(r).min(left_w))
                        }
                    };
                    left_r -= fr;
                    left_w -= fw;
                    if fr + fw > 0 {
                        out.push(FileIo {
                            pid,
                            path: Some(path.clone()),
                            read: fr,
                            write: fw,
                        });
                    }
                }
                positions.insert((pid, fd), (path, pos));
            }
            if left_r + left_w > 0 {
                out.push(FileIo {
                    pid,
                    path: None,
                    read: left_r,
                    write: left_w,
                });
            }
        }
        state.io = io;
        state.positions = positions;
        out.sort_by_key(|t| std::cmp::Reverse(t.read + t.write));
        out
    }

    pub fn names_session(&self) -> &str {
        &self.names_session
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn takes_without_panicking() {
        let t = DiskTracer::start("t").unwrap();
        let _ = t.take();
        assert!(t.names_session().ends_with("-names"));
    }
}
