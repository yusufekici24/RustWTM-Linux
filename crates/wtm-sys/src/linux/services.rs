//! systemd system services, through `systemctl`.
//!
//! A unit is named without its `.service` suffix, as Windows services are
//! named; either form is accepted. Enabled units start "Automatic", static
//! and indirect ones "Manual", disabled and masked ones "Disabled".

use std::collections::HashMap;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::linux::util::{run, run_checked};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    Unknown(u32),
}

impl ServiceState {
    fn from_systemd(active: &str, sub: &str) -> Self {
        match active {
            "active" if sub == "exited" => Self::Stopped,
            "active" => Self::Running,
            "reloading" => Self::ContinuePending,
            "activating" => Self::StartPending,
            "deactivating" => Self::StopPending,
            "inactive" | "failed" => Self::Stopped,
            _ => Self::Unknown(0),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StartType {
    Boot,
    System,
    Automatic,
    Manual,
    Disabled,
}

impl StartType {
    fn from_systemd(state: &str) -> Option<Self> {
        Some(match state {
            "enabled" | "enabled-runtime" | "alias" => Self::Automatic,
            "static" | "indirect" | "generated" | "transient" | "linked" | "linked-runtime" => {
                Self::Manual
            }
            "disabled" | "masked" | "masked-runtime" => Self::Disabled,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServiceEntry {
    pub name: String,
    pub display_name: String,
    pub state: ServiceState,
    pub pid: u32,
    pub start_type: Option<StartType>,
    pub binary_path: Option<String>,
    pub account: Option<String>,
}

impl ServiceEntry {
    /// The slice the service runs in, as Task Manager's Group column shows
    /// the svchost group; `None` for the default system.slice.
    pub fn group(&self) -> Option<&str> {
        None
    }
}

fn unit(name: &str) -> String {
    // RustWTM's own service is named as on Windows by the crates above.
    if name.eq_ignore_ascii_case("RustWTM") {
        return "rustwtm.service".into();
    }
    if name.contains('.') {
        name.to_owned()
    } else {
        format!("{name}.service")
    }
}

fn short(unit: &str) -> &str {
    unit.strip_suffix(".service").unwrap_or(unit)
}

/// `systemctl show` blocks: blank-line separated `Key=Value` lines.
fn show(units: &[String], props: &str) -> Vec<HashMap<String, String>> {
    let mut out = Vec::new();
    for chunk in units.chunks(200) {
        let mut args = vec!["show", "--no-pager", "-p", props];
        args.extend(chunk.iter().map(String::as_str));
        let Some(text) = run("systemctl", &args) else {
            continue;
        };
        for block in text.split("\n\n") {
            let map: HashMap<String, String> = block
                .lines()
                .filter_map(|l| l.split_once('=').map(|(k, v)| (k.to_owned(), v.to_owned())))
                .collect();
            if !map.is_empty() {
                out.push(map);
            }
        }
    }
    out
}

/// `{ path=/usr/sbin/cron ; argv[]=/usr/sbin/cron -f -P ; ... }` -> argv.
fn exec_argv(exec: &str) -> Option<String> {
    let start = exec.find("argv[]=")? + 7;
    let rest = &exec[start..];
    Some(rest.split(" ;").next()?.trim().to_owned())
}

pub fn list_services() -> Result<Vec<ServiceEntry>> {
    let files = run(
        "systemctl",
        &[
            "list-unit-files",
            "--type=service",
            "--no-legend",
            "--no-pager",
            "--plain",
        ],
    )
    .ok_or(Error::Unsupported("systemctl"))?;
    let mut units: Vec<String> = files
        .lines()
        .filter_map(|l| l.split_whitespace().next())
        // Templates (foo@.service) are not services by themselves.
        .filter(|u| !u.ends_with("@.service"))
        .map(str::to_owned)
        .collect();
    let loaded = run(
        "systemctl",
        &[
            "list-units",
            "--type=service",
            "--all",
            "--no-legend",
            "--no-pager",
            "--plain",
        ],
    )
    .unwrap_or_default();
    for u in loaded.lines().filter_map(|l| l.split_whitespace().next()) {
        if !units.iter().any(|x| x == u) && u.ends_with(".service") {
            units.push(u.to_owned());
        }
    }
    let props =
        "Id,Description,ActiveState,SubState,MainPID,UnitFileState,User,ExecStart,LoadState";
    let mut out: Vec<ServiceEntry> = show(&units, props)
        .into_iter()
        .filter(|m| m.get("LoadState").map(String::as_str) != Some("not-found"))
        .filter_map(|m| {
            let id = m.get("Id")?.clone();
            let get = |k: &str| m.get(k).cloned().unwrap_or_default();
            Some(ServiceEntry {
                name: short(&id).to_owned(),
                display_name: get("Description"),
                state: ServiceState::from_systemd(&get("ActiveState"), &get("SubState")),
                pid: get("MainPID").parse().unwrap_or(0),
                start_type: StartType::from_systemd(&get("UnitFileState")),
                binary_path: exec_argv(&get("ExecStart")),
                account: Some(
                    m.get("User")
                        .filter(|u| !u.is_empty())
                        .cloned()
                        .unwrap_or_else(|| "root".into()),
                ),
            })
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out.dedup_by(|a, b| a.name == b.name);
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    Missing,
    NotRunning,
    Running(u32),
}

pub fn presence(name: &str) -> Presence {
    let m = show(&[unit(name)], "LoadState,MainPID,ActiveState");
    // No answer: no systemd (a container) or no such unit.
    let Some(m) = m.first() else {
        return Presence::Missing;
    };
    if m.get("LoadState").map(String::as_str) == Some("not-found") {
        return Presence::Missing;
    }
    match m.get("MainPID").and_then(|p| p.parse::<u32>().ok()) {
        Some(pid) if pid != 0 => Presence::Running(pid),
        _ => Presence::NotRunning,
    }
}

pub fn service_pid(name: &str) -> Option<u32> {
    match presence(name) {
        Presence::Running(pid) => Some(pid),
        _ => None,
    }
}

fn state(name: &str) -> Option<ServiceState> {
    let m = show(&[unit(name)], "ActiveState,SubState");
    let m = m.first()?;
    Some(ServiceState::from_systemd(
        m.get("ActiveState")?,
        m.get("SubState")?,
    ))
}

pub fn restart_service(name: &str, timeout: Duration) -> Result<()> {
    let u = unit(name);
    run_checked("systemctl", &["stop", "--no-block", &u])?;
    let deadline = Instant::now() + timeout;
    while state(name) != Some(ServiceState::Stopped) {
        if Instant::now() >= deadline {
            return Err(Error::Message(format!("{u} did not stop in time")));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    run_checked("systemctl", &["start", "--no-block", &u])?;
    Ok(())
}

pub fn start_service(name: &str) -> Result<()> {
    run_checked("systemctl", &["start", "--no-block", &unit(name)]).map(drop)
}

pub fn stop_service(name: &str) -> Result<()> {
    run_checked("systemctl", &["stop", "--no-block", &unit(name)]).map(drop)
}

pub fn set_start_type(name: &str, start_type: StartType) -> Result<()> {
    let u = unit(name);
    let verb = match start_type {
        StartType::Boot | StartType::System | StartType::Automatic => "enable",
        StartType::Manual => "disable",
        StartType::Disabled => "mask",
    };
    if verb != "mask" {
        // Undo a previous mask first; harmless when not masked.
        let _ = run("systemctl", &["unmask", &u]);
    }
    run_checked("systemctl", &[verb, &u]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_states() {
        assert_eq!(
            ServiceState::from_systemd("active", "running"),
            ServiceState::Running
        );
        assert_eq!(
            ServiceState::from_systemd("failed", "failed"),
            ServiceState::Stopped
        );
        assert_eq!(
            StartType::from_systemd("enabled"),
            Some(StartType::Automatic)
        );
        assert_eq!(StartType::from_systemd("masked"), Some(StartType::Disabled));
        assert_eq!(
            exec_argv("{ path=/usr/sbin/cron ; argv[]=/usr/sbin/cron -f -P ; ignore_errors=no }")
                .as_deref(),
            Some("/usr/sbin/cron -f -P")
        );
        assert_eq!(unit("cron"), "cron.service");
        assert_eq!(short("cron.service"), "cron");
    }
}
