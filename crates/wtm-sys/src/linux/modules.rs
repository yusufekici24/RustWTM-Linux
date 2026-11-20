//! Shared objects mapped into a process (/proc/PID/maps), and where a
//! program comes from: ELF files carry no signatures, so the Linux
//! counterpart of "signed by" is "installed by": a file a Debian package
//! owns counts as signed through that package (its archive is signed), a
//! file inside a snap as signed by the Snap Store.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::linux::util::run;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Module {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub base: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Signature {
    Signed {
        signer: Option<String>,
        /// Vouched for by a package database rather than inside the file.
        catalog: bool,
    },
    Unsigned,
    Invalid(String),
}

/// File-backed executable mappings, the main program first.
pub fn process_modules(pid: u32, create_time: i64) -> crate::Result<Vec<Module>> {
    use crate::control::{Access, ProcessHandle};
    let h = ProcessHandle::open(pid, Access::QUERY)?;
    if create_time != 0 && h.create_time()? != create_time {
        return Err(crate::Error::Reused { pid });
    }
    let exe = h.image_path().ok();
    let maps =
        std::fs::read_to_string(h.proc_path("maps")).map_err(|e| crate::Error::io("maps", e))?;
    h.check()?;
    let mut by_path: Vec<Module> = Vec::new();
    for line in maps.lines() {
        let mut f = line.split_whitespace();
        let (Some(range), Some(_perms), Some(_off), Some(_dev), Some(inode)) =
            (f.next(), f.next(), f.next(), f.next(), f.next())
        else {
            continue;
        };
        let path = f.collect::<Vec<_>>().join(" ");
        if inode == "0" || !path.starts_with('/') {
            continue;
        }
        let Some((a, b)) = range.split_once('-') else {
            continue;
        };
        let (Ok(a), Ok(b)) = (u64::from_str_radix(a, 16), u64::from_str_radix(b, 16)) else {
            continue;
        };
        let path = path.trim_end_matches(" (deleted)").to_owned();
        let is_code =
            path.contains(".so") || Some(&path) == exe.as_ref() || path.contains("/ld-linux");
        if !is_code {
            continue;
        }
        match by_path.iter_mut().find(|m| m.path == path) {
            Some(m) => {
                let end = (m.base + m.size).max(b);
                m.base = m.base.min(a);
                m.size = end - m.base;
            }
            None => by_path.push(Module {
                name: path.rsplit('/').next().unwrap_or(&path).to_owned(),
                path,
                size: b - a,
                base: a,
            }),
        }
    }
    if let Some(exe) = exe
        && let Some(i) = by_path.iter().position(|m| m.path == exe)
    {
        let main = by_path.remove(i);
        by_path.insert(0, main);
    }
    Ok(by_path)
}

static CACHE: Mutex<Option<HashMap<String, Signature>>> = Mutex::new(None);

fn package_of(path: &str) -> Option<String> {
    let out = run("dpkg-query", &["-S", path])?;
    // "coreutils: /usr/bin/ls" (diversions print extra lines).
    out.lines()
        .find(|l| l.ends_with(path) && !l.starts_with("diversion"))
        .and_then(|l| l.split(':').next())
        .map(|p| p.split(',').next().unwrap_or(p).trim().to_owned())
}

fn origin_of(package: &str) -> Option<String> {
    let maint = run("dpkg-query", &["-W", "-f=${Maintainer}", package])?;
    let name = maint.split('<').next().unwrap_or(&maint).trim();
    (!name.is_empty()).then(|| name.to_owned())
}

pub fn signature(path: &str) -> Signature {
    if let Ok(mut cache) = CACHE.lock()
        && let Some(s) = cache.get_or_insert_with(HashMap::new).get(path)
    {
        return s.clone();
    }
    let result = if let Some(rest) = path.strip_prefix("/snap/") {
        let name = rest.split('/').next().unwrap_or(rest);
        Signature::Signed {
            signer: Some(format!("Snap Store ({name})")),
            catalog: true,
        }
    } else if let Some(pkg) = package_of(path) {
        let who = origin_of(&pkg).unwrap_or_else(|| "Debian package".into());
        Signature::Signed {
            signer: Some(format!("{who} ({pkg})")),
            catalog: true,
        }
    } else {
        Signature::Unsigned
    };
    if let Ok(mut cache) = CACHE.lock() {
        let map = cache.get_or_insert_with(HashMap::new);
        if map.len() > 4096 {
            map.clear();
        }
        map.insert(path.to_owned(), result.clone());
    }
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    pub name: String,
    pub subject: String,
}

/// ELF files have no embedded signature.
pub fn embedded_signer(_path: &str) -> Result<Option<Signer>, String> {
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_modules_start_with_the_executable() {
        let me = std::process::id();
        let mods = process_modules(me, 0).unwrap();
        let exe = std::fs::read_link("/proc/self/exe").unwrap();
        assert_eq!(mods[0].path, exe.to_string_lossy());
        assert!(mods.iter().any(|m| m.name.starts_with("libc")));
        let _ = signature("/usr/bin/env");
        assert_eq!(embedded_signer("/usr/bin/env"), Ok(None));
    }
}
