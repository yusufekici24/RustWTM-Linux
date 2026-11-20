//! A Save As dialog through the desktop's portal helpers: `zenity` (GNOME,
//! installed on Ubuntu) or `kdialog` (KDE). Blocks until the user answers.

use std::path::PathBuf;
use std::process::Command;

use crate::{Error, Result};

pub struct FileType<'a> {
    pub name: &'a str,
    pub pattern: &'a str,
}

pub fn save_as(
    _owner: isize,
    title: &str,
    file_name: &str,
    default_extension: &str,
    types: &[FileType<'_>],
) -> Result<Option<PathBuf>> {
    let dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    let initial = dir.join(file_name);
    let zenity = Command::new("zenity")
        .arg("--file-selection")
        .arg("--save")
        .arg("--confirm-overwrite")
        .arg(format!("--title={title}"))
        .arg(format!("--filename={}", initial.display()))
        .args(
            types
                .iter()
                .map(|t| format!("--file-filter={} | {}", t.name, t.pattern.replace(';', " "))),
        )
        .output();
    let out = match zenity {
        Ok(o) => o,
        Err(_) => {
            let filter = types
                .iter()
                .map(|t| format!("{} ({})", t.name, t.pattern.replace(';', " ")))
                .collect::<Vec<_>>()
                .join("\n");
            Command::new("kdialog")
                .args([
                    "--getsavefilename",
                    &initial.to_string_lossy(),
                    &filter,
                    "--title",
                    title,
                ])
                .output()
                .map_err(|_| Error::Message("no file dialog available (install zenity)".into()))?
        }
    };
    if !out.status.success() {
        return Ok(None);
    }
    let chosen = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if chosen.is_empty() {
        return Ok(None);
    }
    let mut path = PathBuf::from(chosen);
    if path.extension().is_none() && !default_extension.is_empty() {
        path.set_extension(default_extension.trim_start_matches('.'));
    }
    Ok(Some(path))
}
