//! The Windows Save As dialog.

use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_CANCELLED, HWND, RPC_E_CHANGED_MODE};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoTaskMemFree, CoUninitialize,
};
use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
use windows::Win32::UI::Shell::{FileSaveDialog, IFileSaveDialog, SIGDN_FILESYSPATH};
use windows::core::{HRESULT, HSTRING, PCWSTR};

use crate::Result;

/// A file type offered in the dialog, e.g. `("Text file", "*.txt")`.
pub struct FileType<'a> {
    pub name: &'a str,
    pub pattern: &'a str,
}

/// Asks where to save a file. `owner` is the window the dialog belongs to
/// (0 for none); it blocks the calling thread until the user answers, so
/// call it off the UI thread. `Ok(None)` when the user cancels. Windows
/// asks before overwriting an existing file.
pub fn save_as(
    owner: isize,
    title: &str,
    file_name: &str,
    default_extension: &str,
    types: &[FileType<'_>],
) -> Result<Option<PathBuf>> {
    let _com = Apartment::enter();
    let prepared = Prepared::new(title, file_name, default_extension, types)?;
    // SAFETY: COM is initialized on this thread by `_com`; `prepared` keeps
    // every string the dialog was given alive, and the result is freed below.
    unsafe {
        if let Err(e) = prepared.dialog.Show(Some(HWND(owner as *mut _))) {
            if e.code() == HRESULT::from_win32(ERROR_CANCELLED.0) {
                return Ok(None);
            }
            return Err(e.into());
        }
        let item = prepared.dialog.GetResult()?;
        let raw = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = raw.to_string();
        CoTaskMemFree(Some(raw.0 as *const _));
        Ok(Some(PathBuf::from(path.map_err(|e| {
            windows::core::Error::new(windows::Win32::Foundation::E_UNEXPECTED, e.to_string())
        })?)))
    }
}

/// A Save As dialog set up but not shown, with the strings it was given:
/// the file types are read from them when it is shown.
struct Prepared {
    dialog: IFileSaveDialog,
    _strings: Vec<HSTRING>,
}

impl Prepared {
    /// Needs COM on the calling thread.
    fn new(
        title: &str,
        file_name: &str,
        default_extension: &str,
        types: &[FileType<'_>],
    ) -> Result<Self> {
        let mut strings: Vec<HSTRING> = Vec::with_capacity(types.len() * 2 + 3);
        for t in types {
            strings.push(HSTRING::from(t.name));
            strings.push(HSTRING::from(t.pattern));
        }
        let specs: Vec<COMDLG_FILTERSPEC> = strings
            .chunks(2)
            .map(|pair| COMDLG_FILTERSPEC {
                pszName: PCWSTR(pair[0].as_ptr()),
                pszSpec: PCWSTR(pair[1].as_ptr()),
            })
            .collect();
        let (title, file_name, extension) = (
            HSTRING::from(title),
            HSTRING::from(file_name),
            HSTRING::from(default_extension),
        );
        // SAFETY: the caller entered COM; the strings live in `Self`.
        let dialog = unsafe {
            let dialog: IFileSaveDialog =
                CoCreateInstance(&FileSaveDialog, None, CLSCTX_INPROC_SERVER)?;
            if !specs.is_empty() {
                dialog.SetFileTypes(&specs)?;
            }
            dialog.SetTitle(&title)?;
            dialog.SetFileName(&file_name)?;
            dialog.SetDefaultExtension(&extension)?;
            dialog
        };
        strings.extend([title, file_name, extension]);
        Ok(Self {
            dialog,
            _strings: strings,
        })
    }
}

/// A single-threaded apartment for the dialog, left again on drop.
struct Apartment {
    owned: bool,
}

impl Apartment {
    fn enter() -> Self {
        // SAFETY: balanced by `Drop` when it succeeds. A thread already in
        // another apartment reports RPC_E_CHANGED_MODE; the dialog still
        // works from a multithreaded one, so that is not an error.
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        Self {
            owned: hr.is_ok() && hr != RPC_E_CHANGED_MODE,
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.owned {
            // SAFETY: pairs with the successful CoInitializeEx in `enter`.
            unsafe { CoUninitialize() };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Everything up to showing it: the dialog is created and takes its
    /// title, name, extension and file types. Nothing is displayed.
    #[test]
    fn the_dialog_is_set_up_without_being_shown() {
        let _com = Apartment::enter();
        let prepared = Prepared::new(
            "Save support bundle",
            "RustWTM-support-20261117-1200.txt",
            "txt",
            &[FileType {
                name: "Text file",
                pattern: "*.txt",
            }],
        )
        .unwrap();
        // SAFETY: COM is entered above; the dialog is alive, and the name
        // it returns is freed after reading.
        let name = unsafe {
            let raw = prepared.dialog.GetFileName().unwrap();
            let name = raw.to_string().unwrap();
            CoTaskMemFree(Some(raw.0 as *const _));
            name
        };
        assert_eq!(name, "RustWTM-support-20261117-1200.txt");
        drop(prepared);
    }
}
