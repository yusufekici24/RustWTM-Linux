//! Starting programs, documents and URLs as the desktop would: programs
//! are run directly, anything else through `xdg-open`; elevation goes
//! through polkit (`pkexec`). A root process (the service) starts things
//! for the signed-in desktop user with that user's session environment.

use std::process::{Command, Stdio};

use crate::{Error, Result};

pub fn system_directory() -> Result<std::path::PathBuf> {
    Ok(std::path::PathBuf::from("/usr/bin"))
}

pub fn is_elevated() -> Result<bool> {
    Ok(crate::linux::util::is_root())
}

/// Splits a command-line string the way a POSIX shell would for plain
/// words, single and double quotes and backslash escapes (no expansion).
pub fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut any = false;
    let mut chars = s.chars();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => {
                single = !single;
                any = true;
            }
            '"' if !single => {
                double = !double;
                any = true;
            }
            '\\' if !single => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    any = true;
                }
            }
            c if c.is_whitespace() && !single && !double => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A program to run, as opposed to a document, folder or URL to open.
fn is_program(file: &str) -> bool {
    if file.contains("://") {
        return false;
    }
    let path = std::path::Path::new(file);
    if file.contains('/') {
        use std::os::unix::fs::PermissionsExt;
        return path.is_file()
            && std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0);
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|d| d.join(file).is_file()))
}

fn command_for(file: &str, parameters: &str) -> Command {
    if is_program(file) {
        let mut c = Command::new(file);
        c.args(split_args(parameters));
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(file);
        c
    }
}

fn spawn_detached(mut cmd: Command) -> Result<()> {
    use std::os::unix::process::CommandExt;
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A session of its own so it outlives RustWTM and gets no signals
    // meant for it.
    // SAFETY: setsid is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let child = cmd.spawn().map_err(|e| Error::io("spawn", e))?;
    // Reap it in the background so it never lingers as a zombie.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

pub fn launch(file: &str, parameters: &str, elevated: bool) -> Result<()> {
    if elevated && !crate::linux::util::is_root() {
        let mut c = Command::new("pkexec");
        c.arg(file).args(split_args(parameters));
        let out = c
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Error::io("pkexec", e))?;
        // pkexec answers 126 when the prompt is dismissed and 127 when not
        // authorized; wait briefly for either, then let it run.
        let mut child = out;
        for _ in 0..600 {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return match status.code() {
                        Some(126) => Err(Error::Message(
                            "the authentication dialog was dismissed".into(),
                        )),
                        Some(127) => Err(Error::Message("Not authorized".into())),
                        _ => Ok(()),
                    };
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
                Err(e) => return Err(Error::io("pkexec", e)),
            }
            // A program that started keeps running past the prompt.
            if crate::linux::util::numeric_entries(&format!("/proc/{}/task", child.id())).len() > 1
            {
                break;
            }
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        return Ok(());
    }
    spawn_detached(command_for(file, parameters))
}

pub fn launch_hidden(file: &str, parameters: &str) -> Result<()> {
    spawn_detached(command_for(file, parameters))
}

/// Environment variables, as name / value pairs.
pub(crate) type Env = Vec<(String, String)>;

/// The graphical session of the active user: UID, name and the variables
/// a desktop program needs, read from that session's shell.
pub(crate) fn desktop_user() -> Option<(u32, String, Env)> {
    let sessions = crate::sessions::sessions().ok()?;
    let active = sessions
        .iter()
        .find(|s| s.state == crate::sessions::SessionState::Active && s.id != 0)
        .or_else(|| sessions.iter().find(|s| s.id != 0))?;
    let uid = active.id;
    let wanted = [
        "DISPLAY",
        "WAYLAND_DISPLAY",
        "DBUS_SESSION_BUS_ADDRESS",
        "XDG_RUNTIME_DIR",
        "XAUTHORITY",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_TYPE",
        "HOME",
        "LANG",
    ];
    // A process of the user's graphical session holds its environment.
    for pid in crate::linux::util::numeric_entries("/proc") {
        if crate::linux::util::process_uid(pid) != Some(uid) {
            continue;
        }
        let Ok(raw) = std::fs::read(format!("/proc/{pid}/environ")) else {
            continue;
        };
        let env: Vec<(String, String)> = raw
            .split(|b| *b == 0)
            .filter_map(|kv| {
                let kv = String::from_utf8_lossy(kv);
                let (k, v) = kv.split_once('=')?;
                wanted.contains(&k).then(|| (k.to_owned(), v.to_owned()))
            })
            .collect();
        if env
            .iter()
            .any(|(k, _)| k == "DISPLAY" || k == "WAYLAND_DISPLAY")
        {
            return Some((uid, active.user.clone(), env));
        }
    }
    let runtime = format!("/run/user/{uid}");
    Some((
        uid,
        active.user.clone(),
        vec![
            ("XDG_RUNTIME_DIR".into(), runtime.clone()),
            (
                "DBUS_SESSION_BUS_ADDRESS".into(),
                format!("unix:path={runtime}/bus"),
            ),
        ],
    ))
}

/// Runs `cmd` as the desktop user when this process is root; as is
/// otherwise.
pub(crate) fn as_desktop_user(cmd: Command) -> Result<Command> {
    if !crate::linux::util::is_root() {
        return Ok(cmd);
    }
    let (_, user, env) = desktop_user()
        .ok_or_else(|| Error::Message("no signed-in desktop user (timed out)".into()))?;
    let mut c = Command::new("runuser");
    c.args(["-u", &user, "--", "env"]);
    for (k, v) in env {
        c.arg(format!("{k}={v}"));
    }
    c.arg(cmd.get_program());
    c.args(cmd.get_args());
    Ok(c)
}

pub fn launch_as_desktop_user(file: &str, parameters: &str) -> Result<()> {
    spawn_detached(as_desktop_user(command_for(file, parameters))?)
}

pub const DESKTOP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

pub fn is_timeout(error: &crate::Error) -> bool {
    matches!(error, Error::Message(m) if m.contains("timed out"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_like_a_shell() {
        assert_eq!(
            split_args(r#"-a "b c" 'd e' f\ g """#),
            ["-a", "b c", "d e", "f g", ""]
        );
        assert!(split_args("   ").is_empty());
        assert!(is_program("sh"));
        assert!(!is_program("https://example.com"));
    }
}
