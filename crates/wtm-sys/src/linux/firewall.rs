//! Cutting a program off the network on Linux: its processes are moved
//! into a systemd scope in `wtmnetblock.slice`, and an nftables table
//! (`inet rustwtm`, separate from ufw's) rejects traffic of sockets in that
//! cgroup, in and out. The blocked programs are kept in
//! /var/lib/rustwtm/firewall-blocked; [`enforce`] (called by the service
//! every few seconds) moves newly started instances in. Needs root.

use std::fs;
use std::path::Path;

use crate::linux::util::{self, run_checked};
use crate::{Error, Result};

pub const GROUP: &str = "RustWTM";

const LIST: &str = "/var/lib/rustwtm/firewall-blocked";
const SLICE: &str = "wtmnetblock.slice";

fn same_program(a: &str, b: &str) -> bool {
    let canon = |p: &str| {
        fs::canonicalize(p).map_or_else(|_| p.to_owned(), |c| c.to_string_lossy().into_owned())
    };
    a == b || canon(a) == canon(b)
}

fn read_list() -> Vec<String> {
    fs::read_to_string(LIST)
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim().is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn write_list(list: &[String]) -> Result<()> {
    if let Some(dir) = Path::new(LIST).parent() {
        fs::create_dir_all(dir).map_err(|e| Error::io("create /var/lib/rustwtm", e))?;
    }
    let mut text = list.join("\n");
    text.push('\n');
    fs::write(LIST, text).map_err(|e| Error::io("write firewall list", e))
}

/// Starts an empty-at-first scope in `slice` holding `pid`.
pub(crate) fn scope_for(pid: u32, prefix: &str, slice: &str) -> Result<()> {
    let unit = format!("{prefix}-{pid}-{}.scope", util::now_filetime() % 1_000_000);
    let pid = pid.to_string();
    run_checked(
        "busctl",
        &[
            "call",
            "org.freedesktop.systemd1",
            "/org/freedesktop/systemd1",
            "org.freedesktop.systemd1.Manager",
            "StartTransientUnit",
            "ssa(sv)a(sa(sv))",
            &unit,
            "fail",
            "2",
            "PIDs",
            "au",
            "1",
            &pid,
            "Slice",
            "s",
            slice,
            "0",
        ],
    )
    .map(drop)
}

fn in_slice(pid: u32) -> bool {
    fs::read_to_string(format!("/proc/{pid}/cgroup"))
        .is_ok_and(|c| c.contains(&format!("/{SLICE}/")))
}

fn exe(pid: u32) -> Option<String> {
    let p = fs::read_link(format!("/proc/{pid}/exe")).ok()?;
    let s = p.to_string_lossy();
    Some(s.strip_suffix(" (deleted)").unwrap_or(&s).to_owned())
}

fn rules(active: bool) -> String {
    let mut s = String::from("table inet rustwtm {}\ndelete table inet rustwtm\n");
    if active {
        s.push_str(&format!(
            "table inet rustwtm {{\n  chain out {{\n    type filter hook output priority 0; policy accept;\n    socket cgroupv2 level 1 \"{SLICE}\" counter reject\n  }}\n  chain in {{\n    type filter hook input priority 0; policy accept;\n    socket cgroupv2 level 1 \"{SLICE}\" counter drop\n  }}\n}}\n"
        ));
    }
    s
}

fn load_rules(active: bool) -> Result<()> {
    use std::io::Write;
    let mut child = std::process::Command::new("nft")
        .args(["-f", "-"])
        .stdin(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| Error::io("nft", e))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(rules(active).as_bytes())
            .map_err(|e| Error::io("nft", e))?;
    }
    let out = child.wait_with_output().map_err(|e| Error::io("nft", e))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Message(format!(
            "nft: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

/// Moves running instances of blocked programs into the blocked slice
/// and makes sure the rules are loaded. Returns how many were moved.
pub fn enforce() -> Result<usize> {
    let list = read_list();
    if list.is_empty() {
        return Ok(0);
    }
    let mut moved = 0;
    for pid in util::numeric_entries("/proc") {
        if in_slice(pid) {
            continue;
        }
        let Some(path) = exe(pid) else { continue };
        if list.iter().any(|b| same_program(b, &path))
            && scope_for(pid, "rustwtm-netblock", SLICE).is_ok()
        {
            moved += 1;
        }
    }
    // The cgroup must exist before nft can resolve it.
    if moved > 0 || run_checked("nft", &["list", "table", "inet", "rustwtm"]).is_err() {
        load_rules(true)?;
    }
    Ok(moved)
}

pub fn block(path: &str) -> Result<()> {
    if !util::is_root() {
        return Err(Error::Os {
            call: "firewall block",
            errno: libc::EPERM,
        });
    }
    let mut list = read_list();
    if !list.iter().any(|p| same_program(p, path)) {
        list.push(path.to_owned());
        write_list(&list)?;
    }
    enforce().map(drop)
}

/// Removes the block; running instances are moved out of the blocked
/// slice so they get the network back at once.
pub fn unblock(path: &str) -> Result<()> {
    if !util::is_root() {
        return Err(Error::Os {
            call: "firewall unblock",
            errno: libc::EPERM,
        });
    }
    let mut list = read_list();
    list.retain(|p| !same_program(p, path));
    write_list(&list)?;
    for pid in util::numeric_entries("/proc") {
        if in_slice(pid) && exe(pid).is_some_and(|e| same_program(&e, path)) {
            let _ = scope_for(pid, "rustwtm-netfree", "rustwtm.slice");
        }
    }
    if list.is_empty() {
        load_rules(false)?;
    }
    Ok(())
}

pub fn unblock_all() -> Result<usize> {
    let list = read_list();
    for p in &list {
        let _ = unblock(p);
    }
    let _ = fs::remove_file(LIST);
    let _ = load_rules(false);
    Ok(list.len())
}

pub fn blocked() -> Result<Vec<String>> {
    let mut list = read_list();
    list.sort_by_key(|p| p.to_lowercase());
    Ok(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_text() {
        let r = rules(true);
        assert!(r.contains("socket cgroupv2 level 1 \"wtmnetblock.slice\""));
        assert!(r.starts_with("table inet rustwtm {}\ndelete table"));
        assert!(!rules(false).contains("chain"));
        assert!(same_program("/usr/bin/env", "/usr/bin/env"));
    }
}
