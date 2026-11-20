//! Package updates on Linux: APT (upgradable packages), snaps and Flatpaks,
//! in the JSON shape packages.ps1 gives on Windows. Updates go through the
//! package managers; APT and snap ask polkit when not run as root.

use serde_json::{Value, json};

use super::Package;

fn run(program: &str, args: &[&str]) -> Result<String, String> {
    let out = std::process::Command::new(program)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// `name/suite version arch [upgradable from: old]`.
fn parse_apt(text: &str) -> Vec<Value> {
    text.lines()
        .filter_map(|l| {
            let (name, rest) = l.split_once('/')?;
            let mut f = rest.split_whitespace();
            let _suite = f.next()?;
            let target = f.next()?;
            let from = l.split("upgradable from: ").nth(1)?.trim_end_matches(']').trim();
            Some(json!({"id": name, "source": "apt", "name": name, "installed": from, "target": target}))
        })
        .take(1024)
        .collect()
}

/// `snap refresh --list`: Name Version Rev Size Publisher Notes.
fn parse_snap_refresh(text: &str) -> Vec<(String, String)> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            Some((f.next()?.to_owned(), f.next()?.to_owned()))
        })
        .collect()
}

pub(super) fn inventory() -> Result<Value, String> {
    let mut packages = Vec::new();
    let mut store = Vec::new();
    let mut warnings = Vec::new();
    match run("apt", &["list", "--upgradable"]) {
        Ok(t) => packages.extend(parse_apt(&t)),
        Err(e) => warnings.push(format!("APT: {e}")),
    }
    if let Ok(list) = run("snap", &["list"]) {
        let updates = run("snap", &["refresh", "--list"])
            .map(|t| parse_snap_refresh(&t))
            .unwrap_or_default();
        for l in list.lines().skip(1) {
            let f: Vec<&str> = l.split_whitespace().collect();
            if f.len() < 5 {
                continue;
            }
            if let Some((_, target)) = updates.iter().find(|(n, _)| n == f[0]) {
                packages.push(json!({"id": f[0], "source": "snap", "name": f[0], "installed": f[1], "target": target}));
            }
            let notes = f.get(5).copied().unwrap_or("");
            store.push(json!({
                "id": format!("snap:{}", f[0]), "family": f[3], "name": f[0],
                "publisher": f[4].trim_end_matches('✓').trim_end_matches('*'), "version": f[1],
                "location": format!("/snap/{}", f[0]),
                "removable": !(notes.contains("base") || notes.contains("snapd") || notes.contains("kernel")),
            }));
        }
    }
    if let Ok(list) = run(
        "flatpak",
        &["list", "--app", "--columns=application,name,version,origin"],
    ) {
        let updates = run(
            "flatpak",
            &["remote-ls", "--updates", "--columns=application,version"],
        )
        .unwrap_or_default();
        for l in list.lines() {
            let f: Vec<&str> = l.split('\t').collect();
            if f.len() < 4 {
                continue;
            }
            if let Some(target) = updates.lines().find_map(|u| {
                u.split_once('\t')
                    .filter(|(a, _)| *a == f[0])
                    .map(|(_, v)| v.to_owned())
            }) && target != f[2]
                && !target.is_empty()
            {
                packages.push(json!({"id": f[0], "source": "flatpak", "name": f[1], "installed": f[2], "target": target}));
            }
            store.push(json!({
                "id": format!("flatpak:{}", f[0]), "family": f[0], "name": f[1], "publisher": f[3],
                "version": f[2], "location": "", "removable": true,
            }));
        }
    }
    packages.truncate(1024);
    store.truncate(1024);
    Ok(json!({"packages": packages, "store": store, "warnings": warnings}))
}

pub(super) fn update(packages: &[Package]) -> Result<Value, String> {
    let root = wtm_sys::shell::is_elevated().unwrap_or(false);
    let elevate = |args: Vec<String>| -> Vec<String> {
        if root {
            args
        } else {
            std::iter::once("/usr/bin/pkexec".to_owned())
                .chain(args)
                .collect()
        }
    };
    let mut results = Vec::new();
    for p in packages {
        let cmd: Vec<String> = match p.source.as_str() {
            "apt" => elevate(vec![
                "/usr/bin/apt-get".into(),
                "install".into(),
                "--only-upgrade".into(),
                "-y".into(),
                p.id.clone(),
            ]),
            "snap" => elevate(vec!["/usr/bin/snap".into(), "refresh".into(), p.id.clone()]),
            "flatpak" => vec![
                "/usr/bin/flatpak".into(),
                "update".into(),
                "-y".into(),
                "--noninteractive".into(),
                p.id.clone(),
            ],
            _ => {
                results.push(json!({"id": p.id, "source": p.source, "status": "failed", "message": "unknown source"}));
                continue;
            }
        };
        let refs: Vec<&str> = cmd[1..].iter().map(String::as_str).collect();
        let (status, message) = match run(&cmd[0], &refs) {
            Ok(_) => ("updated", String::new()),
            Err(e) => ("failed", e.chars().take(4000).collect()),
        };
        results.push(json!({"id": p.id, "source": p.source, "status": status, "message": message}));
    }
    Ok(Value::Array(results))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_apt_and_snap() {
        let a =
            parse_apt("Listing...\nfirefox/noble-updates 125.0 amd64 [upgradable from: 124.0]\n");
        assert_eq!(a.len(), 1);
        assert_eq!(a[0]["installed"], "124.0");
        assert_eq!(a[0]["target"], "125.0");
        let s = parse_snap_refresh(
            "Name Version Rev Size Publisher Notes\nfirefox 126.0 4000 280MB mozilla✓ -\n",
        );
        assert_eq!(s, [("firefox".to_owned(), "126.0".to_owned())]);
    }
}
