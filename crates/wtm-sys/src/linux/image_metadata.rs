//! Display names of programs, as Windows reads them from version
//! resources: the application's name from its .desktop entry, otherwise
//! the package's summary; the "company" is the package maintainer (or the
//! snap publisher), the version the package version. Cached per path.

use std::collections::HashMap;
use std::fs;
use std::sync::{Mutex, OnceLock};

use crate::linux::util::run;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Metadata {
    pub description: Option<String>,
    pub company: Option<String>,
    pub version: Option<String>,
}

/// Executable basename -> application name, from every .desktop entry.
fn desktop_names() -> &'static HashMap<String, String> {
    static NAMES: OnceLock<HashMap<String, String>> = OnceLock::new();
    NAMES.get_or_init(|| {
        let mut out = HashMap::new();
        let mut dirs = vec![
            "/usr/share/applications".to_owned(),
            "/usr/local/share/applications".to_owned(),
            "/var/lib/snapd/desktop/applications".to_owned(),
            "/var/lib/flatpak/exports/share/applications".to_owned(),
        ];
        if let Some(home) = std::env::var_os("HOME") {
            dirs.push(format!(
                "{}/.local/share/applications",
                home.to_string_lossy()
            ));
        }
        let lang = crate::linux::util::ui_language();
        for dir in dirs {
            let Ok(rd) = fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let Ok(text) = fs::read_to_string(e.path()) else {
                    continue;
                };
                let mut name = None;
                let mut local = None;
                let mut exec = None;
                let mut in_entry = false;
                for l in text.lines() {
                    if l.starts_with('[') {
                        in_entry = l == "[Desktop Entry]";
                        continue;
                    }
                    if !in_entry {
                        continue;
                    }
                    if let Some(v) = l.strip_prefix("Name=") {
                        name.get_or_insert_with(|| v.to_owned());
                    } else if let Some(v) = l.strip_prefix(&format!("Name[{lang}]=")) {
                        local = Some(v.to_owned());
                    } else if let Some(v) = l.strip_prefix("Exec=") {
                        exec.get_or_insert_with(|| v.to_owned());
                    }
                }
                let (Some(name), Some(exec)) = (local.or(name), exec) else {
                    continue;
                };
                // "env FOO=1 /usr/bin/app %U" -> "app".
                let program = exec
                    .split_whitespace()
                    .find(|w| *w != "env" && !w.contains('='))
                    .unwrap_or_default()
                    .rsplit('/')
                    .next()
                    .unwrap_or_default()
                    .to_owned();
                if !program.is_empty() {
                    out.entry(program).or_insert(name);
                }
            }
        }
        out
    })
}

fn package_fields(path: &str) -> Option<(String, String, String)> {
    let out = run("dpkg-query", &["-S", path])?;
    let pkg = out
        .lines()
        .find(|l| l.ends_with(path))?
        .split(':')
        .next()?
        .split(',')
        .next()?
        .trim()
        .to_owned();
    let fields = run(
        "dpkg-query",
        &[
            "-W",
            "-f=${Version}\t${Maintainer}\t${binary:Summary}",
            &pkg,
        ],
    )?;
    let mut f = fields.splitn(3, '\t');
    let version = f.next()?.to_owned();
    let maint = f.next()?.split('<').next()?.trim().to_owned();
    let summary = f.next().unwrap_or_default().trim().to_owned();
    Some((version, maint, summary))
}

static CACHE: Mutex<Option<HashMap<String, Metadata>>> = Mutex::new(None);

pub fn metadata(path: &str) -> Metadata {
    if let Ok(mut c) = CACHE.lock()
        && let Some(m) = c.get_or_insert_with(HashMap::new).get(path)
    {
        return m.clone();
    }
    let base = path.rsplit('/').next().unwrap_or(path);
    let mut m = Metadata {
        description: desktop_names().get(base).cloned(),
        ..Default::default()
    };
    if let Some(rest) = path.strip_prefix("/snap/") {
        let mut parts = rest.split('/');
        let name = parts.next().unwrap_or_default();
        m.version = parts.next().map(str::to_owned);
        m.company = Some(format!("Snap: {name}"));
    } else if let Some((version, maint, summary)) = package_fields(path) {
        m.version = Some(version);
        m.company = (!maint.is_empty()).then_some(maint);
        if m.description.is_none() && !summary.is_empty() {
            m.description = Some(summary);
        }
    }
    if let Ok(mut c) = CACHE.lock() {
        let map = c.get_or_insert_with(HashMap::new);
        if map.len() > 4096 {
            map.clear();
        }
        map.insert(path.to_owned(), m.clone());
    }
    m
}

pub fn description(path: &str) -> Option<String> {
    metadata(path).description
}

pub fn company(path: &str) -> Option<String> {
    metadata(path).company
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_without_panicking() {
        let _ = super::metadata("/usr/bin/env");
        let _ = super::metadata("/nonexistent");
    }
}
