//! Small helpers that let the crates above write platform-neutral code.

/// `Command::no_window()`: starts a console program without a window
/// (`CREATE_NO_WINDOW`) on Windows; nothing to do elsewhere.
pub trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

impl NoWindow for std::process::Command {
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x0800_0000)
        }
        #[cfg(not(windows))]
        {
            self
        }
    }
}

/// Per-user settings: `%APPDATA%\RustWTM` on Windows,
/// `$XDG_CONFIG_HOME/rustwtm` (`~/.config/rustwtm`) on Linux.
pub fn user_config_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("APPDATA").map(|d| std::path::PathBuf::from(d).join("RustWTM"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config"))
            })
            .map(|d| d.join("rustwtm"))
    }
}

/// Per-user data and caches: `%LOCALAPPDATA%\RustWTM` on Windows,
/// `$XDG_DATA_HOME/rustwtm` (`~/.local/share/rustwtm`) on Linux.
pub fn user_data_dir() -> Option<std::path::PathBuf> {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA").map(|d| std::path::PathBuf::from(d).join("RustWTM"))
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_DATA_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".local/share"))
            })
            .map(|d| d.join("rustwtm"))
    }
}

/// The service's folder (config, logs, history): `%ProgramData%\RustWTM`
/// on Windows, `/var/lib/rustwtm` on Linux.
pub fn machine_data_dir() -> std::path::PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("ProgramData")
            .map_or_else(
                || std::path::PathBuf::from(r"C:\ProgramData"),
                std::path::PathBuf::from,
            )
            .join("RustWTM")
    }
    #[cfg(not(windows))]
    {
        std::path::PathBuf::from("/var/lib/rustwtm")
    }
}
