//! Signed-in users from systemd-logind. A "session" is a user (its UID),
//! matching the session of each process (the user slice it runs in); the
//! state is that of the user's most present logind session.

use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::linux::util::{run, run_checked};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionState {
    Active,
    Connected,
    ConnectQuery,
    Shadow,
    Disconnected,
    Idle,
    Listen,
    Reset,
    Down,
    Init,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: u32,
    pub station: String,
    pub user: String,
    pub domain: String,
    pub state: SessionState,
}

fn rank(s: SessionState) -> u8 {
    match s {
        SessionState::Active => 4,
        SessionState::Idle => 3,
        SessionState::Connected => 2,
        SessionState::Disconnected => 1,
        _ => 0,
    }
}

pub fn sessions() -> Result<Vec<Session>> {
    let list = run("loginctl", &["list-sessions", "--no-legend", "--no-pager"])
        .ok_or(Error::Unsupported("loginctl"))?;
    let ids: Vec<String> = list
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(str::to_owned))
        .collect();
    let mut out: Vec<Session> = Vec::new();
    for id in ids {
        let Some(text) = run(
            "loginctl",
            &[
                "show-session",
                &id,
                "-p",
                "User",
                "-p",
                "Name",
                "-p",
                "State",
                "-p",
                "IdleHint",
                "-p",
                "Remote",
                "-p",
                "Seat",
                "-p",
                "TTY",
                "-p",
                "Class",
                "-p",
                "Service",
            ],
        ) else {
            continue;
        };
        let get = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(k)?.strip_prefix('=').map(str::to_owned))
                .unwrap_or_default()
        };
        if get("Class") == "greeter" || get("Class") == "manager" {
            continue;
        }
        let Ok(uid) = get("User").parse::<u32>() else {
            continue;
        };
        let state = match (get("State").as_str(), get("IdleHint") == "yes") {
            ("active", true) => SessionState::Idle,
            ("active", false) => SessionState::Active,
            ("online", _) => SessionState::Connected,
            ("closing", _) => SessionState::Down,
            _ => SessionState::Unknown,
        };
        let station = if get("Remote") == "yes" {
            format!("{}#{id}", get("Service"))
        } else if !get("Seat").is_empty() {
            get("Seat")
        } else {
            get("TTY")
        };
        let name = get("Name");
        let (user, domain) = match name.split_once('@') {
            Some((u, d)) => (u.to_owned(), d.to_ascii_uppercase()),
            None => match name.split_once('\\') {
                Some((d, u)) => (u.to_owned(), d.to_owned()),
                None => (name.clone(), String::new()),
            },
        };
        match out.iter_mut().find(|s| s.id == uid) {
            Some(s) if rank(state) > rank(s.state) => {
                s.state = state;
                s.station = station;
            }
            Some(_) => {}
            None => out.push(Session {
                id: uid,
                station,
                user,
                domain,
                state,
            }),
        }
    }
    Ok(out)
}

/// Locks the user's sessions: the closest Linux has to disconnecting
/// while staying signed in.
pub fn disconnect(session: u32) -> Result<()> {
    for id in logind_sessions(session) {
        run_checked("loginctl", &["lock-session", &id])?;
    }
    Ok(())
}

fn logind_sessions(uid: u32) -> Vec<String> {
    run("loginctl", &["list-sessions", "--no-legend", "--no-pager"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let id = f.next()?;
            (f.next()?.parse::<u32>().ok()? == uid).then(|| id.to_owned())
        })
        .collect()
}

pub const MESSAGE_TITLE_MAX: usize = 256;
pub const MESSAGE_TEXT_MAX: usize = 4096;

/// Shows a desktop notification in the user's session.
pub fn send_message(session: u32, title: &str, text: &str) -> Result<()> {
    let title: String = title.chars().take(MESSAGE_TITLE_MAX).collect();
    let text: String = text.chars().take(MESSAGE_TEXT_MAX).collect();
    let mut cmd = Command::new("notify-send");
    cmd.args(["--app-name=RustWTM", "--icon=rustwtm", &title, &text]);
    let mut cmd = if crate::linux::util::is_root() {
        let name = crate::linux::util::user_name(session).ok_or(Error::Os {
            call: "getpwuid",
            errno: libc::ESRCH,
        })?;
        let runtime = format!("/run/user/{session}");
        let mut c = Command::new("runuser");
        c.args(["-u", &name, "--", "env"])
            .arg(format!("XDG_RUNTIME_DIR={runtime}"))
            .arg(format!("DBUS_SESSION_BUS_ADDRESS=unix:path={runtime}/bus"))
            .arg("notify-send")
            .args(cmd.get_args());
        c
    } else {
        cmd
    };
    let out = cmd.output().map_err(|e| Error::io("notify-send", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Message(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ))
    }
}

pub fn log_off(session: u32) -> Result<()> {
    run_checked("loginctl", &["terminate-user", &session.to_string()]).map(drop)
}

#[cfg(test)]
mod tests {
    #[test]
    fn lists_without_panicking() {
        let _ = super::sessions();
    }
}
