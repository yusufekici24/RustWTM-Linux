//! The desktop shell (GNOME Shell, Plasma, Xfce panel, ...) in place of
//! Explorer. GNOME Shell can restart itself in place only under X11
//! (SIGQUIT asks it to re-exec); a Wayland compositor holds every window,
//! so restarting it would end the session and is refused.

use crate::linux::util::{self, Stat};

const SHELLS: [&str; 6] = [
    "gnome-shell",
    "plasmashell",
    "xfce4-panel",
    "cinnamon",
    "mate-panel",
    "budgie-panel",
];

fn find_shell() -> Option<(u32, String)> {
    let uid = crate::linux::util::process_uid(std::process::id());
    util::numeric_entries("/proc").into_iter().find_map(|pid| {
        let s = Stat::read(pid).ok()?;
        let name = crate::process::process_name(pid, &s.comm);
        (SHELLS.contains(&name.as_str()) && (util::is_root() || util::process_uid(pid) == uid))
            .then_some((pid, name))
    })
}

pub fn desktop_pid() -> Option<u32> {
    find_shell().map(|(pid, _)| pid)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Restarted {
    Closed,
    Ended,
    RestoredByWindows,
}

pub fn restart() -> std::result::Result<Restarted, String> {
    let (pid, name) = find_shell().ok_or("No desktop shell is running")?;
    let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland")
        || std::env::var_os("WAYLAND_DISPLAY").is_some();
    match name.as_str() {
        "gnome-shell" if wayland => {
            Err("GNOME Shell cannot be restarted under Wayland without signing out".into())
        }
        "gnome-shell" => {
            // SAFETY: plain signal to a PID we just read.
            if unsafe { libc::kill(pid as i32, libc::SIGQUIT) } != 0 {
                return Err(std::io::Error::last_os_error().to_string());
            }
            Ok(Restarted::Closed)
        }
        "plasmashell" => {
            let _ = std::process::Command::new("kquitapp6")
                .arg("plasmashell")
                .status();
            std::process::Command::new("kstart")
                .arg("plasmashell")
                .spawn()
                .map(|_| Restarted::Closed)
                .map_err(|e| e.to_string())
        }
        other => {
            // Panels are restarted by their session manager when they end.
            // SAFETY: plain signal.
            if unsafe { libc::kill(pid as i32, libc::SIGTERM) } != 0 {
                return Err(format!("{other}: {}", std::io::Error::last_os_error()));
            }
            Ok(Restarted::Ended)
        }
    }
}
