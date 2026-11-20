//! Ctrl+Shift+Esc opening RustWTM on Linux: a GNOME custom keybinding
//! owned by RustWTM (`.../custom-keybindings/rustwtm/`), set per user with
//! gsettings. For every user at once, the package ships a dconf default
//! (see packaging). "Windows" here means the desktop's own behaviour.

use std::path::{Path, PathBuf};

pub const LAUNCH_ARG: &str = "--task-manager";
pub const NATIVE_ARG: &str = "--open-windows-task-manager";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub enum State {
    #[default]
    Windows,
    RustWTM,
    Other,
}

const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/rustwtm/";
const BINDING: &str = "<Control><Shift>Escape";

fn gsettings(args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new("gsettings")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|_| "gsettings is not available (GNOME desktop required)".to_owned())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// `['/a/', '/b/']` or `@as []` -> paths.
pub(crate) fn parse_list(text: &str) -> Vec<String> {
    let inner = text
        .trim()
        .trim_start_matches("@as")
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']');
    inner
        .split(',')
        .map(|s| s.trim().trim_matches('\'').to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

pub(crate) fn format_list(paths: &[String]) -> String {
    if paths.is_empty() {
        return "@as []".into();
    }
    format!(
        "[{}]",
        paths
            .iter()
            .map(|p| format!("'{p}'"))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn gui_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let gui = exe
        .parent()
        .ok_or("Missing program directory")?
        .join("rustwtm");
    Ok(if gui.is_file() {
        gui
    } else {
        PathBuf::from("/usr/bin/rustwtm")
    })
}

fn binding_of(path: &str) -> String {
    gsettings(&["get", &format!("{CUSTOM}:{path}"), "binding"])
        .unwrap_or_default()
        .trim_matches('\'')
        .to_owned()
}

pub fn status() -> Result<State, String> {
    let list = parse_list(&gsettings(&["get", SCHEMA, "custom-keybindings"])?);
    if list.iter().any(|p| p == PATH) && binding_of(PATH) == BINDING {
        return Ok(State::RustWTM);
    }
    if list.iter().any(|p| p != PATH && binding_of(p) == BINDING) {
        return Ok(State::Other);
    }
    Ok(State::Windows)
}

pub fn configure(enabled: bool) -> Result<State, String> {
    let current = status()?;
    if enabled && current == State::Other {
        return Err("Another application already uses Ctrl+Shift+Esc".into());
    }
    let mut list = parse_list(&gsettings(&["get", SCHEMA, "custom-keybindings"])?);
    let key = format!("{CUSTOM}:{PATH}");
    if enabled {
        let exe = gui_path()?;
        gsettings(&["set", &key, "name", "RustWTM"])?;
        gsettings(&[
            "set",
            &key,
            "command",
            &format!("\"{}\" {LAUNCH_ARG}", exe.display()),
        ])?;
        gsettings(&["set", &key, "binding", BINDING])?;
        if !list.iter().any(|p| p == PATH) {
            list.push(PATH.to_owned());
            gsettings(&["set", SCHEMA, "custom-keybindings", &format_list(&list)])?;
        }
    } else {
        list.retain(|p| p != PATH);
        gsettings(&["set", SCHEMA, "custom-keybindings", &format_list(&list)])?;
        let _ = gsettings(&["reset", &key, "binding"]);
    }
    status()
}

/// Opens the desktop's own system monitor.
pub fn open_windows_task_manager() -> Result<(), String> {
    open_native().map_err(|e| e.to_string())
}

pub fn open_native() -> Result<(), NativeError> {
    for app in [
        "gnome-system-monitor",
        "plasma-systemmonitor",
        "xfce4-taskmanager",
        "mate-system-monitor",
    ] {
        if wtm_sys::shell::launch(app, "", false).is_ok() {
            return Ok(());
        }
    }
    Err(NativeError(wtm_sys::Error::Message(
        "No system monitor is installed".into(),
    )))
}

#[derive(Debug)]
pub struct NativeError(wtm_sys::Error);

impl std::fmt::Display for NativeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

pub fn cleanup(_exe: &Path) -> Result<(), String> {
    if status().ok() == Some(State::RustWTM) {
        configure(false).map(drop)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_round_trip() {
        assert_eq!(parse_list("@as []"), Vec::<String>::new());
        let l = parse_list("['/a/', '/b/']");
        assert_eq!(l, ["/a/", "/b/"]);
        assert_eq!(format_list(&l), "['/a/', '/b/']");
        assert_eq!(format_list(&[]), "@as []");
    }
}
