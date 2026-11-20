//! File permissions, the Linux counterpart of the Windows ACL helpers.
//!
//! The SDDL strings the crates above pass are mapped onto modes: a
//! descriptor that grants users (BU, AU, IU, WD) read access gives 0755 /
//! 0644, one that grants them write access 0777 / 0666 (never used for
//! anything trusted), anything else 0700 / 0600. "Administrators" are
//! root: trusted files and folders are root-owned and writable by root
//! only, through every folder above them.

use std::fs;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Users {
    None,
    Read,
    Write,
}

fn users(sddl: &str) -> Users {
    let mut best = Users::None;
    for ace in sddl.split('(').skip(1) {
        let ace = ace.trim_end_matches(')');
        let f: Vec<&str> = ace.split(';').collect();
        if f.len() < 6 || f[0] != "A" {
            continue;
        }
        if !matches!(f[5], "BU" | "AU" | "IU" | "WD") {
            continue;
        }
        let rights = f[2].to_ascii_uppercase();
        let write = rights.contains("GA")
            || rights.contains("FA")
            || rights.contains("GW")
            || rights.contains("FW");
        let level = if write { Users::Write } else { Users::Read };
        if level as u8 > best as u8 {
            best = level;
        }
    }
    best
}

fn mode(sddl: &str, dir: bool) -> u32 {
    match (users(sddl), dir) {
        (Users::None, true) => 0o700,
        (Users::None, false) => 0o600,
        (Users::Read, true) => 0o755,
        (Users::Read, false) => 0o644,
        (Users::Write, true) => 0o1777,
        (Users::Write, false) => 0o666,
    }
}

pub fn set_dacl(path: &Path, sddl: &str) -> Result<()> {
    let dir = fs::metadata(path)
        .map_err(|e| Error::io("stat", e))?
        .is_dir();
    fs::set_permissions(path, fs::Permissions::from_mode(mode(sddl, dir)))
        .map_err(|e| Error::io("chmod", e))
}

/// Creates `path` anew (replacing what is there) with the mode from the
/// start, so it is never readable by others, not even for a moment.
pub fn create_private(path: &Path, sddl: &str) -> Result<fs::File> {
    match fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(Error::io("remove", e)),
        _ => {}
    }
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode(sddl, false))
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| Error::io("create", e))
}

#[derive(Debug, PartialEq, Eq)]
pub enum Secured {
    Created,
    Kept,
    Replaced(PathBuf),
    Contested,
}

/// Root-owned, no link, nobody else may write (sticky directories count
/// as guarded only when also root-owned).
fn trusted_entry(meta: &fs::Metadata) -> bool {
    meta.uid() == 0
        && !meta.file_type().is_symlink()
        && meta.mode() & 0o022 == 0
        && (meta.is_dir() || meta.nlink() <= 1)
}

fn tree_trusted(path: &Path, depth: u32) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !trusted_entry(&meta) {
        return false;
    }
    if meta.is_dir() && depth < 16 {
        let Ok(rd) = fs::read_dir(path) else {
            return false;
        };
        for e in rd.flatten() {
            if !tree_trusted(&e.path(), depth + 1) {
                return false;
            }
        }
    }
    true
}

fn secure_dir_with(path: &Path, sddl: &str) -> Result<Secured> {
    let wanted = mode(sddl, true);
    let mut moved = None;
    for _ in 0..3 {
        match fs::symlink_metadata(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut b = fs::DirBuilder::new();
                std::os::unix::fs::DirBuilderExt::mode(&mut b, wanted);
                match b.create(path) {
                    Ok(()) => {
                        // The umask may have narrowed it.
                        fs::set_permissions(path, fs::Permissions::from_mode(wanted))
                            .map_err(|e| Error::io("chmod", e))?;
                        return Ok(moved.map_or(Secured::Created, Secured::Replaced));
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(e) => return Err(Error::io("mkdir", e)),
                }
            }
            Err(e) => return Err(Error::io("stat", e)),
            Ok(_) if tree_trusted(path, 0) || !crate::linux::util::is_root() => {
                fs::set_permissions(path, fs::Permissions::from_mode(wanted))
                    .map_err(|e| Error::io("chmod", e))?;
                return Ok(moved.map_or(Secured::Kept, Secured::Replaced));
            }
            Ok(_) => {
                let stamp = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let aside = path.with_file_name(format!(
                    "{}.untrusted-{stamp}",
                    path.file_name()
                        .map(|n| n.to_string_lossy())
                        .unwrap_or_default()
                ));
                fs::rename(path, &aside).map_err(|e| Error::io("rename", e))?;
                moved = Some(aside);
            }
        }
    }
    Ok(Secured::Contested)
}

pub fn secure_dir(path: &Path, sddl: &str) -> Result<Secured> {
    secure_dir_with(path, sddl)
}

pub fn secure_program_dir(path: &Path, sddl: &str) -> Result<Secured> {
    secure_dir_with(path, sddl)
}

/// Only root can change the program at `path` or put another in its place.
pub fn only_admins_control(path: &Path) -> Result<bool> {
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io("stat", e))?;
    if meta.is_dir() || !trusted_entry(&meta) {
        return Ok(false);
    }
    let Some(folder) = path.parent() else {
        return Ok(false);
    };
    let fm = fs::symlink_metadata(folder).map_err(|e| Error::io("stat", e))?;
    if !trusted_entry(&fm) {
        return Ok(false);
    }
    only_admins_move(folder)
}

/// Nobody but root can move, remove or replace any folder `path` lies in.
pub fn only_admins_move(path: &Path) -> Result<bool> {
    use std::path::Component;
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, Component::CurDir | Component::ParentDir))
    {
        return Ok(false);
    }
    for above in path.ancestors().skip(1) {
        let meta = fs::symlink_metadata(above).map_err(|e| Error::io("stat", e))?;
        if !trusted_entry(&meta) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The account this thread acts for (its file-system UID).
pub fn thread_user_sid() -> Result<String> {
    // SAFETY: setfsuid(-1) only reads the current value.
    let fsuid = unsafe { libc::setfsuid(u32::MAX) };
    Ok(crate::pipe::uid_sid(fsuid as u32))
}

pub fn current_user_sid() -> Result<String> {
    // SAFETY: getuid takes no arguments.
    Ok(crate::pipe::uid_sid(unsafe { libc::getuid() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sddl() {
        assert_eq!(mode("D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)", true), 0o700);
        assert_eq!(
            mode("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;0x12008b;;;IU)", false),
            0o644
        );
        assert_eq!(mode("D:P(A;OICI;FA;;;SY)(A;OICI;GRGX;;;BU)", true), 0o755);
    }

    #[test]
    fn private_files_start_private() {
        let p = std::env::temp_dir().join(format!("wtm-private-{}", std::process::id()));
        let f = create_private(&p, "D:P(A;;FA;;;SY)").unwrap();
        drop(f);
        assert_eq!(fs::metadata(&p).unwrap().mode() & 0o777, 0o600);
        let _ = fs::remove_file(&p);
        assert!(only_admins_move(Path::new("/usr/bin/env")).unwrap());
        assert!(!only_admins_move(Path::new("relative")).unwrap());
    }
}
