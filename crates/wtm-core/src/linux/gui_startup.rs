//! Starting the RustWTM window at sign-in on Linux: an XDG autostart entry
//! (~/.config/autostart/rustwtm.desktop). The window never runs as root:
//! privileged work is the service's, and a root GUI under Wayland is not
//! possible; "administrator" startup is therefore always off.

use std::path::{Path, PathBuf};

pub const SCHEDULED_OPEN: &str = "--scheduled-open";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Settings {
    pub administrator: bool,
    pub autostart: bool,
}

#[derive(Debug, Clone, Copy)]
pub enum Change {
    Initialize { first_run: bool },
    Set(Settings),
}

const FILE: &str = "rustwtm.desktop";

fn autostart_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("autostart"))
}

fn gui_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("Missing program directory")?;
    let gui = dir.join("rustwtm");
    Ok(if gui.is_file() {
        gui
    } else {
        PathBuf::from("/usr/bin/rustwtm")
    })
}

pub(crate) fn entry_text(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=RustWTM\nComment=Task manager and resource governor\nExec=\"{}\" --background\nIcon=rustwtm\nTerminal=false\nX-GNOME-Autostart-enabled=true\nX-GNOME-Autostart-Delay=5\nNoDisplay=true\n",
        exe.display()
    )
}

pub fn settings() -> Settings {
    let on = autostart_dir()
        .and_then(|d| std::fs::read_to_string(d.join(FILE)).ok())
        .is_some_and(|t| {
            !t.contains("Hidden=true") && !t.contains("X-GNOME-Autostart-enabled=false")
        });
    Settings {
        administrator: false,
        autostart: on,
    }
}

fn set_autostart(on: bool) -> Result<(), String> {
    let dir = autostart_dir().ok_or("No home directory")?;
    let path = dir.join(FILE);
    if on {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(&path, entry_text(&gui_path()?)).map_err(|e| e.to_string())
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
            _ => Ok(()),
        }
    }
}

pub fn configure(change: Change) -> Result<Settings, String> {
    let old = settings();
    let desired = match change {
        Change::Initialize { first_run } => Settings {
            administrator: false,
            autostart: old.autostart || first_run,
        },
        Change::Set(value) => value,
    };
    if desired.administrator {
        return Err("On Linux the window runs as the signed-in user; the RustWTM service does the privileged work".into());
    }
    if desired.autostart != old.autostart {
        set_autostart(desired.autostart)?;
    }
    Ok(settings())
}

pub fn launch_task(_background: bool) -> Result<(), String> {
    Err("Administrator preference is off".into())
}

pub fn wait_launcher() {}

/// Nothing system-wide is registered for the window.
pub fn cleanup_tasks(_exe: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_is_quoted() {
        let t = entry_text(Path::new("/opt/rust wtm/rustwtm"));
        assert!(t.contains("Exec=\"/opt/rust wtm/rustwtm\" --background"));
        assert!(
            configure(Change::Set(Settings {
                administrator: true,
                autostart: false
            }))
            .is_err()
        );
    }
}
