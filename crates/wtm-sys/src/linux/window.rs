//! Desktop windows on Linux. Under X11 (and for X11 programs under
//! XWayland) the EWMH properties are read with `xprop` and windows are
//! managed with `wmctrl` when installed; native Wayland windows are not
//! visible to other programs, so those queries return nothing there.
//! Window handles are X11 window IDs.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::process::Command;

use crate::linux::util::run;

fn x11() -> bool {
    std::env::var_os("DISPLAY").is_some()
}

fn xprop_value(args: &[&str]) -> Option<String> {
    if !x11() {
        return None;
    }
    run("xprop", args)
}

fn active_window() -> Option<isize> {
    let out = xprop_value(&["-root", "_NET_ACTIVE_WINDOW"])?;
    let id = out.rsplit(' ').next()?.trim();
    let v = isize::from_str_radix(id.trim_start_matches("0x"), 16).ok()?;
    (v != 0).then_some(v)
}

fn window_pid(id: isize) -> Option<u32> {
    let out = xprop_value(&["-id", &format!("0x{id:x}"), "_NET_WM_PID"])?;
    out.rsplit(' ').next()?.trim().parse().ok()
}

pub fn foreground_pid() -> Option<u32> {
    window_pid(active_window()?)
}

pub fn foreground_fullscreen() -> Option<(u32, bool)> {
    let id = active_window()?;
    let pid = window_pid(id)?;
    let state = xprop_value(&["-id", &format!("0x{id:x}"), "_NET_WM_STATE"]).unwrap_or_default();
    Some((pid, state.contains("_NET_WM_STATE_FULLSCREEN")))
}

pub fn find_other(title: &str) -> Option<(isize, u32)> {
    let me = std::process::id();
    windows_all()
        .into_iter()
        .find(|(_, pid, t)| *pid != me && t == title)
        .map(|(id, pid, _)| (id, pid))
}

/// A lock that one instance holds: `flock` on a file in the runtime dir.
#[derive(Debug)]
pub struct InstanceLock(Option<File>);

impl Drop for InstanceLock {
    fn drop(&mut self) {
        self.0.take();
    }
}

pub fn single_instance(name: &str) -> Option<InstanceLock> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let safe: String = name
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let file = File::create(dir.join(format!("rustwtm-{safe}.lock"))).ok()?;
    // SAFETY: valid fd.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    (rc == 0).then(|| InstanceLock(Some(file)))
}

/// Calls `on_end` when the session asks the program to quit (SIGTERM or
/// SIGHUP, which logind and the desktop send at log-out). Returns a dummy
/// handle when the watcher started.
pub fn watch_session_end(on_end: impl Fn() + Send + 'static) -> Option<isize> {
    static FIRED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    extern "C" fn handler(_: libc::c_int) {
        FIRED.store(true, std::sync::atomic::Ordering::SeqCst);
    }
    // SAFETY: installs an async-signal-safe handler that only sets a flag.
    unsafe {
        libc::signal(libc::SIGTERM, handler as *const () as libc::sighandler_t);
        libc::signal(libc::SIGHUP, handler as *const () as libc::sighandler_t);
    }
    std::thread::Builder::new()
        .name("session-end".into())
        .spawn(move || {
            while !FIRED.load(std::sync::atomic::Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            on_end();
        })
        .ok()?;
    Some(1)
}

#[derive(Clone, PartialEq, Eq, Default)]
pub struct WindowInfo {
    pub handle: isize,
    pub tid: u32,
    pub title: String,
    pub class: String,
    pub visible: bool,
    pub minimized: bool,
    pub hung: bool,
}

impl std::fmt::Debug for WindowInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowInfo")
            .field("handle", &format_args!("0x{:x}", self.handle))
            .field("title", &self.title)
            .field("class", &self.class)
            .finish_non_exhaustive()
    }
}

/// (window, pid, title) of every managed window, from `wmctrl -lpx`.
fn windows_all() -> Vec<(isize, u32, String)> {
    windows_detailed()
        .into_iter()
        .map(|(id, pid, _, t)| (id, pid, t))
        .collect()
}

fn windows_detailed() -> Vec<(isize, u32, String, String)> {
    if !x11() {
        return Vec::new();
    }
    let Some(out) = run("wmctrl", &["-lpx"]) else {
        return Vec::new();
    };
    out.lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let id = isize::from_str_radix(f.next()?.trim_start_matches("0x"), 16).ok()?;
            let _desktop = f.next()?;
            let pid = f.next()?.parse().ok()?;
            let class = f.next()?.to_owned();
            let _host = f.next()?;
            Some((id, pid, class, f.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

pub fn windows_of(pid: u32) -> Vec<WindowInfo> {
    windows_detailed()
        .into_iter()
        .filter(|w| w.1 == pid)
        .map(|(handle, _, class, title)| WindowInfo {
            handle,
            tid: pid,
            minimized: is_minimized(handle),
            title,
            class,
            visible: true,
            hung: false,
        })
        .collect()
}

pub fn process_at_pointer() -> Option<u32> {
    None
}

pub fn window_owner(raw: isize) -> Option<(u32, u32)> {
    let pid = window_pid(raw)?;
    Some((pid, pid))
}

fn wmctrl(args: &[&str]) -> bool {
    x11()
        && Command::new("wmctrl")
            .args(args)
            .status()
            .is_ok_and(|s| s.success())
}

pub fn switch_to(raw: isize) -> bool {
    wmctrl(&["-i", "-a", &format!("0x{raw:x}")])
}

pub fn bring_to_front(raw: isize) -> bool {
    switch_to(raw)
}

pub fn is_topmost(raw: isize) -> bool {
    xprop_value(&["-id", &format!("0x{raw:x}"), "_NET_WM_STATE"])
        .is_some_and(|s| s.contains("_NET_WM_STATE_ABOVE"))
}

pub fn set_topmost(raw: isize, on: bool) {
    let action = if on { "add,above" } else { "remove,above" };
    wmctrl(&["-i", "-r", &format!("0x{raw:x}"), "-b", action]);
}

pub fn release_topmost(raw: isize) {
    set_topmost(raw, false);
}

pub fn hide(raw: isize) {
    if x11() {
        let _ = Command::new("xdotool")
            .args(["windowminimize", &raw.to_string()])
            .status();
    }
}

pub fn use_exe_icon(_raw: isize) -> bool {
    false
}

pub fn is_visible(_raw: isize) -> bool {
    true
}

pub fn is_minimized(raw: isize) -> bool {
    xprop_value(&["-id", &format!("0x{raw:x}"), "_NET_WM_STATE"])
        .is_some_and(|s| s.contains("_NET_WM_STATE_HIDDEN"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_instance_at_a_time() {
        let name = format!("test-{}", std::process::id());
        let a = single_instance(&name);
        assert!(a.is_some());
        assert!(single_instance(&name).is_none());
        drop(a);
        assert!(single_instance(&name).is_some());
    }
}
