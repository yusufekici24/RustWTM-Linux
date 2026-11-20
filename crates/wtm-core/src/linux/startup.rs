//! Startup programs on Linux: XDG autostart entries, the user's
//! (~/.config/autostart) and the system's (/etc/xdg/autostart). Disabling
//! keeps the entry, as Windows' StartupApproved does: a user's own entry
//! gets `Hidden=true`; a system entry is overridden for this user with a
//! copy carrying `Hidden=true` (the XDG rule), or edited in place by root.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartupScope {
    CurrentUser,
    AllUsers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartupSource {
    Run,
    Run32,
    Folder,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartupEntry {
    pub name: String,
    pub command: String,
    pub scope: StartupScope,
    pub source: StartupSource,
    pub enabled: bool,
}

fn user_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .map(|c| c.join("autostart"))
}

const SYSTEM_DIR: &str = "/etc/xdg/autostart";

#[derive(Debug, Default)]
struct Desktop {
    name: Option<String>,
    exec: Option<String>,
    hidden: bool,
    disabled: bool,
    only_in: Option<String>,
    not_in: Option<String>,
}

fn parse(text: &str) -> Desktop {
    let mut d = Desktop::default();
    let mut in_entry = false;
    for l in text.lines() {
        if l.starts_with('[') {
            in_entry = l.trim() == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            continue;
        };
        let v = v.trim().to_owned();
        match k.trim() {
            "Name" => d.name = Some(v),
            "Exec" => d.exec = Some(v),
            "Hidden" => d.hidden = v == "true",
            "X-GNOME-Autostart-enabled" => d.disabled = v == "false",
            "OnlyShowIn" => d.only_in = Some(v),
            "NotShowIn" => d.not_in = Some(v),
            _ => {}
        }
    }
    d
}

/// Whether the entry applies to this desktop (OnlyShowIn / NotShowIn).
fn for_this_desktop(d: &Desktop) -> bool {
    let current = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    let mine: Vec<&str> = current.split(':').filter(|s| !s.is_empty()).collect();
    if mine.is_empty() {
        return true;
    }
    if let Some(only) = &d.only_in
        && !only.split(';').any(|s| mine.contains(&s))
    {
        return false;
    }
    if let Some(not) = &d.not_in
        && not.split(';').any(|s| mine.contains(&s))
    {
        return false;
    }
    true
}

fn files(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".desktop"))
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect()
}

/// Exec with field codes (%U, %f, ...) removed.
fn command_of(exec: &str) -> String {
    exec.split_whitespace()
        .filter(|w| !(w.len() == 2 && w.starts_with('%')))
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn list_startup() -> Vec<StartupEntry> {
    let mut out = Vec::new();
    let user = user_dir();
    let user_files: Vec<(String, PathBuf)> = user.as_deref().map(files).unwrap_or_default();
    for (file, path) in files(Path::new(SYSTEM_DIR)) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let system = parse(&text);
        if !for_this_desktop(&system) {
            continue;
        }
        // A user file of the same name overrides it.
        let over = user_files
            .iter()
            .find(|(f, _)| *f == file)
            .and_then(|(_, p)| std::fs::read_to_string(p).ok())
            .map(|t| parse(&t));
        let effective = over.as_ref().unwrap_or(&system);
        out.push(StartupEntry {
            name: system
                .name
                .clone()
                .unwrap_or_else(|| file.trim_end_matches(".desktop").to_owned()),
            command: command_of(
                effective
                    .exec
                    .as_deref()
                    .or(system.exec.as_deref())
                    .unwrap_or(""),
            ),
            scope: StartupScope::AllUsers,
            source: StartupSource::Folder,
            enabled: !(effective.hidden || effective.disabled),
        });
    }
    let system_names: Vec<String> = files(Path::new(SYSTEM_DIR))
        .into_iter()
        .map(|(f, _)| f)
        .collect();
    for (file, path) in user_files {
        if system_names.contains(&file) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let d = parse(&text);
        out.push(StartupEntry {
            name: d
                .name
                .clone()
                .unwrap_or_else(|| file.trim_end_matches(".desktop").to_owned()),
            command: command_of(d.exec.as_deref().unwrap_or("")),
            scope: StartupScope::CurrentUser,
            source: StartupSource::Folder,
            enabled: !(d.hidden || d.disabled),
        });
    }
    out.sort_by_key(|e| e.name.to_lowercase());
    out
}

/// The program a startup command runs, as an absolute path: the first
/// word (quotes and an `env VAR=value` prefix allowed), looked up in PATH
/// when it has no slash. `var` expands `$VAR` / `${VAR}`.
pub fn executable(command: &str, var: impl Fn(&str) -> Option<String>) -> Option<String> {
    let words = wtm_sys::shell::split_args(command.trim());
    let mut it = words
        .into_iter()
        .skip_while(|w| w == "env" || (w.contains('=') && !w.starts_with('/')));
    let first = it.next()?;
    let expanded = if let Some(rest) = first.strip_prefix('$') {
        let (name, tail) = match rest.strip_prefix('{') {
            Some(r) => {
                let end = r.find('}')?;
                (&r[..end], &r[end + 1..])
            }
            None => {
                let end = rest.find('/').unwrap_or(rest.len());
                (&rest[..end], &rest[end..])
            }
        };
        format!("{}{tail}", var(name)?)
    } else {
        first
    };
    if expanded.ends_with(".desktop") || expanded.is_empty() || expanded.len() > 1024 {
        return None;
    }
    if expanded.starts_with('/') {
        return Some(expanded);
    }
    let path = var("PATH").unwrap_or_else(|| "/usr/local/bin:/usr/bin:/bin".into());
    std::env::split_paths(&path)
        .map(|d| d.join(&expanded))
        .find(|p| p.is_file())
        .map(|p| p.to_string_lossy().into_owned())
}

fn set_key(text: &str, key: &str, value: &str) -> String {
    let mut out = String::new();
    let mut done = false;
    let mut in_entry = false;
    for l in text.lines() {
        if l.starts_with('[') {
            if in_entry && !done {
                out.push_str(&format!("{key}={value}\n"));
                done = true;
            }
            in_entry = l.trim() == "[Desktop Entry]";
        } else if in_entry && l.split_once('=').is_some_and(|(k, _)| k.trim() == key) {
            if !done {
                out.push_str(&format!("{key}={value}\n"));
                done = true;
            }
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    if !done {
        out.push_str(&format!("{key}={value}\n"));
    }
    out
}

fn find_file(entry: &StartupEntry) -> Option<(String, PathBuf)> {
    let dir = match entry.scope {
        StartupScope::AllUsers => PathBuf::from(SYSTEM_DIR),
        StartupScope::CurrentUser => user_dir()?,
    };
    files(&dir).into_iter().find(|(file, path)| {
        let d = std::fs::read_to_string(path)
            .map(|t| parse(&t))
            .unwrap_or_default();
        d.name.as_deref() == Some(entry.name.as_str())
            || file.trim_end_matches(".desktop") == entry.name
    })
}

pub fn set_startup_enabled(entry: &StartupEntry, enabled: bool) -> io::Result<()> {
    let (file, path) = find_file(entry).ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    let hidden = if enabled { "false" } else { "true" };
    let target = match entry.scope {
        StartupScope::CurrentUser => path.clone(),
        // Root may change it for everyone; others override it for themselves.
        StartupScope::AllUsers if wtm_sys::shell::is_elevated().unwrap_or(false) => path.clone(),
        StartupScope::AllUsers => {
            let dir = user_dir().ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
            std::fs::create_dir_all(&dir)?;
            dir.join(&file)
        }
    };
    let base = std::fs::read_to_string(if target.exists() { &target } else { &path })?;
    let text = set_key(
        &set_key(&base, "Hidden", hidden),
        "X-GNOME-Autostart-enabled",
        if enabled { "true" } else { "false" },
    );
    std::fs::write(&target, text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_commands_name_their_program() {
        let var = |name: &str| match name {
            "HOME" => Some("/home/me".to_owned()),
            "PATH" => Some("/usr/bin:/bin".to_owned()),
            _ => None,
        };
        assert_eq!(
            executable("/usr/bin/foo --tray", var).as_deref(),
            Some("/usr/bin/foo")
        );
        assert_eq!(
            executable("env A=1 /opt/x/app %U", var).as_deref(),
            Some("/opt/x/app")
        );
        assert_eq!(
            executable("$HOME/bin/tool", var).as_deref(),
            Some("/home/me/bin/tool")
        );
        assert_eq!(
            executable("sh -c true", var)
                .as_deref()
                .map(|p| p.ends_with("/sh")),
            Some(true)
        );
        assert_eq!(executable("", var), None);
    }

    #[test]
    fn hides_and_shows() {
        let t = "[Desktop Entry]\nName=X\nExec=x\n[Desktop Action a]\nName=A\n";
        let h = set_key(t, "Hidden", "true");
        assert!(h.contains("Hidden=true\n[Desktop Action a]"));
        assert_eq!(set_key(&h, "Hidden", "false").matches("Hidden=").count(), 1);
        assert_eq!(command_of("app --x %U"), "app --x");
        let d = parse(&h);
        assert!(d.hidden && d.name.as_deref() == Some("X"));
    }
}
