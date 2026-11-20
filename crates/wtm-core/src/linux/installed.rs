//! Installed desktop applications on Linux: Debian packages that ship a
//! .desktop entry (what a person thinks of as "an app"), snaps and
//! Flatpaks. Removal is handed to the package manager through polkit
//! (`pkexec apt-get remove`, `pkexec snap remove`, `flatpak uninstall`),
//! interactive, never silent.

use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    User,
    Machine,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppId {
    pub scope: Scope,
    /// Unused on Linux (no 32-bit registry view).
    pub view32: bool,
    /// `deb:<package>`, `snap:<name>` or `flatpak:<app id>`.
    pub key: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledApp {
    pub id: AppId,
    pub name: String,
    pub publisher: String,
    pub version: String,
    pub estimated_bytes: Option<u64>,
    pub location: String,
    pub uninstall: String,
    pub msi: bool,
    pub no_remove: bool,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub apps: Vec<InstalledApp>,
    pub partial: bool,
}

const MAX_ENTRIES: usize = 4096;

fn run(program: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Packages essential to the system are never offered for removal.
const PROTECTED: &[&str] = &[
    "ubuntu-desktop",
    "gnome-shell",
    "gnome-control-center",
    "nautilus",
    "gnome-terminal",
    "rustwtm",
    "snapd",
    "apt",
    "dpkg",
    "systemd",
];

fn desktop_apps() -> BTreeMap<String, String> {
    // .desktop file -> Name, for the visible (not NoDisplay) entries.
    let mut out = BTreeMap::new();
    let Ok(rd) = std::fs::read_dir("/usr/share/applications") else {
        return out;
    };
    for e in rd.flatten() {
        let path = e.path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        if text.contains("NoDisplay=true") {
            continue;
        }
        if let Some(name) = text.lines().find_map(|l| l.strip_prefix("Name=")) {
            out.insert(path.to_string_lossy().into_owned(), name.trim().to_owned());
        }
    }
    out
}

fn debs(apps: &mut Vec<InstalledApp>, partial: &mut bool) {
    let desktops = desktop_apps();
    if desktops.is_empty() {
        return;
    }
    let paths: Vec<&str> = desktops.keys().map(String::as_str).collect();
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    for chunk in paths.chunks(200) {
        let mut args = vec!["-S"];
        args.extend(chunk);
        let Some(out) = run("dpkg-query", &args) else {
            *partial = true;
            continue;
        };
        for l in out.lines() {
            if let Some((pkgs, path)) = l.split_once(": ") {
                let pkg = pkgs
                    .split(',')
                    .next()
                    .unwrap_or(pkgs)
                    .trim()
                    .split(':')
                    .next()
                    .unwrap_or("")
                    .to_owned();
                if let Some(name) = desktops.get(path.trim()) {
                    owner.entry(pkg).or_insert_with(|| name.clone());
                }
            }
        }
    }
    if owner.is_empty() {
        return;
    }
    let mut args = vec![
        "-W".to_owned(),
        "-f=${Package}\t${Version}\t${Maintainer}\t${Installed-Size}\t${Priority}\n".to_owned(),
    ];
    args.extend(owner.keys().cloned());
    let refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let Some(out) = run("dpkg-query", &refs) else {
        *partial = true;
        return;
    };
    for l in out.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 5 {
            continue;
        }
        let pkg = f[0].to_owned();
        let Some(name) = owner.get(&pkg) else {
            continue;
        };
        apps.push(InstalledApp {
            id: AppId {
                scope: Scope::Machine,
                view32: false,
                key: format!("deb:{pkg}"),
            },
            name: name.clone(),
            publisher: f[2].split('<').next().unwrap_or("").trim().to_owned(),
            version: f[1].to_owned(),
            estimated_bytes: f[3].parse::<u64>().ok().map(|kb| kb * 1024),
            location: String::new(),
            uninstall: format!("apt-get remove {pkg}"),
            msi: false,
            no_remove: PROTECTED.contains(&pkg.as_str())
                || matches!(f[4], "required" | "important"),
        });
    }
}

fn snaps(apps: &mut Vec<InstalledApp>) {
    let Some(out) = run("snap", &["list"]) else {
        return;
    };
    for l in out.lines().skip(1) {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let notes = f.get(5).copied().unwrap_or("");
        // Bases, the snapd snap and kernels are not apps.
        if notes.contains("base")
            || notes.contains("snapd")
            || notes.contains("kernel")
            || notes.contains("gadget")
        {
            continue;
        }
        apps.push(InstalledApp {
            id: AppId {
                scope: Scope::Machine,
                view32: false,
                key: format!("snap:{}", f[0]),
            },
            name: f[0].to_owned(),
            publisher: f[4].trim_end_matches('✓').trim_end_matches('*').to_owned(),
            version: f[1].to_owned(),
            estimated_bytes: std::fs::metadata(format!(
                "/var/lib/snapd/snaps/{}_{}.snap",
                f[0], f[2]
            ))
            .ok()
            .map(|m| m.len()),
            location: format!("/snap/{}", f[0]),
            uninstall: format!("snap remove {}", f[0]),
            msi: false,
            no_remove: false,
        });
    }
}

fn flatpaks(apps: &mut Vec<InstalledApp>) {
    let Some(out) = run(
        "flatpak",
        &[
            "list",
            "--app",
            "--columns=application,name,version,origin,installation,size",
        ],
    ) else {
        return;
    };
    for l in out.lines() {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 5 {
            continue;
        }
        let user = f[4] == "user";
        apps.push(InstalledApp {
            id: AppId {
                scope: if user { Scope::User } else { Scope::Machine },
                view32: false,
                key: format!("flatpak:{}", f[0]),
            },
            name: f[1].to_owned(),
            publisher: f[3].to_owned(),
            version: f[2].to_owned(),
            estimated_bytes: None,
            location: String::new(),
            uninstall: format!("flatpak uninstall {}", f[0]),
            msi: false,
            no_remove: false,
        });
    }
}

pub fn list() -> Report {
    let mut r = Report::default();
    debs(&mut r.apps, &mut r.partial);
    snaps(&mut r.apps);
    flatpaks(&mut r.apps);
    r.apps.sort_by_key(|a| a.name.to_lowercase());
    if r.apps.len() > MAX_ENTRIES {
        r.apps.truncate(MAX_ENTRIES);
        r.partial = true;
    }
    r
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchPlan {
    pub executable: String,
    pub arguments: String,
}

fn safe_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.as_bytes()[0].is_ascii_alphanumeric()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
}

/// The package manager command that removes `app`, asking through polkit.
pub fn plan(app: &InstalledApp) -> Result<LaunchPlan, &'static str> {
    if app.no_remove {
        return Err("Removal is disabled for this system component.");
    }
    let (kind, name) = app
        .id
        .key
        .split_once(':')
        .ok_or("Unknown application source.")?;
    if !safe_name(name) {
        return Err("Unexpected application identifier.");
    }
    Ok(match kind {
        "deb" => LaunchPlan {
            executable: "/usr/bin/pkexec".into(),
            arguments: format!("/usr/bin/apt-get remove --yes {name}"),
        },
        "snap" => LaunchPlan {
            executable: "/usr/bin/pkexec".into(),
            arguments: format!("/usr/bin/snap remove {name}"),
        },
        "flatpak" if app.id.scope == Scope::User => LaunchPlan {
            executable: "/usr/bin/flatpak".into(),
            arguments: format!("uninstall --user --noninteractive {name}"),
        },
        "flatpak" => LaunchPlan {
            executable: "/usr/bin/flatpak".into(),
            arguments: format!("uninstall --system --noninteractive {name}"),
        },
        _ => return Err("Unknown application source."),
    })
}

/// Starts the removal; success means started, not removed.
pub fn uninstall(app: &InstalledApp) -> Result<(), String> {
    let fresh = list();
    if !fresh
        .apps
        .iter()
        .any(|a| a.id == app.id && a.version == app.version)
    {
        return Err("Application registration changed. Refresh the list.".into());
    }
    let p = plan(app).map_err(str::to_owned)?;
    wtm_sys::shell::launch(&p.executable, &p.arguments, false).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(key: &str) -> InstalledApp {
        InstalledApp {
            id: AppId {
                scope: Scope::Machine,
                view32: false,
                key: key.into(),
            },
            name: "X".into(),
            publisher: String::new(),
            version: "1".into(),
            estimated_bytes: None,
            location: String::new(),
            uninstall: String::new(),
            msi: false,
            no_remove: false,
        }
    }

    #[test]
    fn plans_are_fixed_commands() {
        assert_eq!(
            plan(&app("deb:vlc")).unwrap().arguments,
            "/usr/bin/apt-get remove --yes vlc"
        );
        assert!(plan(&app("deb:--purge")).is_err());
        assert!(plan(&app("deb:a;rm -rf")).is_err());
        assert!(plan(&app("other:x")).is_err());
        let _ = list();
    }
}
