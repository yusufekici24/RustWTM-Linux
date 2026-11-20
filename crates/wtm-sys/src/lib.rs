//! Safe wrappers over the operating-system APIs used by RustWTM: Win32 / NT
//! on Windows, procfs / sysfs / cgroup v2 / systemd on Linux. Both back ends
//! expose the same public API, so the crates above build for either.
//!
//! This is the only crate in the workspace that contains `unsafe` code.

#![cfg(any(windows, target_os = "linux"))]

#[cfg(windows)]
pub mod collate;
#[cfg(target_os = "linux")]
#[path = "linux/collate.rs"]
pub mod collate;
#[cfg(windows)]
pub mod control;
#[cfg(target_os = "linux")]
#[path = "linux/control.rs"]
pub mod control;
#[cfg(windows)]
pub mod cpu;
#[cfg(target_os = "linux")]
#[path = "linux/cpu.rs"]
pub mod cpu;
#[cfg(windows)]
pub mod cpu_info;
#[cfg(target_os = "linux")]
#[path = "linux/cpu_info.rs"]
pub mod cpu_info;
#[cfg(windows)]
pub mod disk;
#[cfg(target_os = "linux")]
#[path = "linux/disk.rs"]
pub mod disk;
#[cfg(windows)]
pub mod disktrace;
#[cfg(target_os = "linux")]
#[path = "linux/disktrace.rs"]
pub mod disktrace;
#[cfg(windows)]
pub mod dpapi;
#[cfg(target_os = "linux")]
#[path = "linux/dpapi.rs"]
pub mod dpapi;
#[cfg(windows)]
pub mod ending;
#[cfg(target_os = "linux")]
#[path = "linux/ending.rs"]
pub mod ending;
#[cfg(windows)]
pub mod environment;
#[cfg(target_os = "linux")]
#[path = "linux/environment.rs"]
pub mod environment;
#[cfg(windows)]
mod error;
#[cfg(target_os = "linux")]
#[path = "linux/error.rs"]
mod error;
#[cfg(windows)]
pub mod etw;
#[cfg(target_os = "linux")]
#[path = "linux/etw.rs"]
pub mod etw;
#[cfg(windows)]
pub mod explorer;
#[cfg(target_os = "linux")]
#[path = "linux/explorer.rs"]
pub mod explorer;
#[cfg(windows)]
mod fasthash;
#[cfg(windows)]
pub mod file_dialog;
#[cfg(target_os = "linux")]
#[path = "linux/file_dialog.rs"]
pub mod file_dialog;
#[cfg(windows)]
pub mod file_locks;
#[cfg(target_os = "linux")]
#[path = "linux/file_locks.rs"]
pub mod file_locks;
#[cfg(windows)]
pub mod firewall;
#[cfg(target_os = "linux")]
#[path = "linux/firewall.rs"]
pub mod firewall;
#[cfg(windows)]
pub mod gpu;
#[cfg(target_os = "linux")]
#[path = "linux/gpu.rs"]
pub mod gpu;
#[cfg(windows)]
pub mod gpu_perf;
#[cfg(target_os = "linux")]
#[path = "linux/gpu_perf.rs"]
pub mod gpu_perf;
#[cfg(windows)]
mod handle;
#[cfg(target_os = "linux")]
#[path = "linux/handle.rs"]
mod handle;
#[cfg(windows)]
pub mod handles;
#[cfg(target_os = "linux")]
#[path = "linux/handles.rs"]
pub mod handles;
#[cfg(windows)]
pub mod hotkey;
#[cfg(target_os = "linux")]
#[path = "linux/hotkey.rs"]
pub mod hotkey;
#[cfg(windows)]
pub mod http;
#[cfg(target_os = "linux")]
#[path = "linux/http.rs"]
pub mod http;
#[cfg(windows)]
pub mod icon;
#[cfg(target_os = "linux")]
#[path = "linux/icon.rs"]
pub mod icon;
#[cfg(windows)]
pub mod image_metadata;
#[cfg(target_os = "linux")]
#[path = "linux/image_metadata.rs"]
pub mod image_metadata;
#[cfg(windows)]
pub mod job;
#[cfg(target_os = "linux")]
#[path = "linux/job.rs"]
pub mod job;
#[cfg(windows)]
pub mod memory;
#[cfg(target_os = "linux")]
#[path = "linux/memory.rs"]
pub mod memory;
#[cfg(windows)]
pub mod memory_map;
#[cfg(target_os = "linux")]
#[path = "linux/memory_map.rs"]
pub mod memory_map;
#[cfg(windows)]
pub mod mitigation;
#[cfg(target_os = "linux")]
#[path = "linux/mitigation.rs"]
pub mod mitigation;
#[cfg(windows)]
pub mod modules;
#[cfg(target_os = "linux")]
#[path = "linux/modules.rs"]
pub mod modules;
#[cfg(windows)]
pub mod net;
#[cfg(target_os = "linux")]
#[path = "linux/net.rs"]
pub mod net;
#[cfg(windows)]
pub mod net_info;
#[cfg(target_os = "linux")]
#[path = "linux/net_info.rs"]
pub mod net_info;
#[cfg(windows)]
pub mod nettrace;
#[cfg(target_os = "linux")]
#[path = "linux/nettrace.rs"]
pub mod nettrace;
#[cfg(windows)]
mod ntapi;
#[cfg(target_os = "linux")]
#[path = "linux/ntapi.rs"]
mod ntapi;
#[cfg(windows)]
pub mod nvme_health;
#[cfg(target_os = "linux")]
#[path = "linux/nvme_health.rs"]
pub mod nvme_health;
#[cfg(windows)]
pub mod physical_disk;
#[cfg(target_os = "linux")]
#[path = "linux/physical_disk.rs"]
pub mod physical_disk;
#[cfg(windows)]
pub mod pipe;
#[cfg(target_os = "linux")]
#[path = "linux/pipe.rs"]
pub mod pipe;
#[cfg(windows)]
pub mod pipe_http;
#[cfg(target_os = "linux")]
#[path = "linux/pipe_http.rs"]
pub mod pipe_http;
#[cfg(windows)]
pub mod power;
#[cfg(target_os = "linux")]
#[path = "linux/power.rs"]
pub mod power;
#[cfg(windows)]
pub mod privilege;
#[cfg(target_os = "linux")]
#[path = "linux/privilege.rs"]
pub mod privilege;
#[cfg(windows)]
pub mod process;
#[cfg(target_os = "linux")]
#[path = "linux/process.rs"]
pub mod process;
#[cfg(windows)]
pub mod registry;
#[cfg(target_os = "linux")]
#[path = "linux/registry.rs"]
pub mod registry;
#[cfg(windows)]
pub mod security;
#[cfg(target_os = "linux")]
#[path = "linux/security.rs"]
pub mod security;
#[cfg(windows)]
pub mod services;
#[cfg(target_os = "linux")]
#[path = "linux/services.rs"]
pub mod services;
#[cfg(windows)]
pub mod sessions;
#[cfg(target_os = "linux")]
#[path = "linux/sessions.rs"]
pub mod sessions;
#[cfg(windows)]
pub mod shell;
#[cfg(target_os = "linux")]
#[path = "linux/shell.rs"]
pub mod shell;
#[cfg(windows)]
pub mod smbios;
#[cfg(target_os = "linux")]
#[path = "linux/smbios.rs"]
pub mod smbios;
#[cfg(windows)]
pub mod spawn;
#[cfg(target_os = "linux")]
#[path = "linux/spawn.rs"]
pub mod spawn;
#[cfg(windows)]
pub mod system;
#[cfg(target_os = "linux")]
#[path = "linux/system.rs"]
pub mod system;
#[cfg(windows)]
pub mod threads;
#[cfg(target_os = "linux")]
#[path = "linux/threads.rs"]
pub mod threads;
#[cfg(windows)]
pub mod token;
#[cfg(target_os = "linux")]
#[path = "linux/token.rs"]
pub mod token;
#[cfg(windows)]
pub mod volumes;
#[cfg(target_os = "linux")]
#[path = "linux/volumes.rs"]
pub mod volumes;
#[cfg(windows)]
pub mod waits;
#[cfg(target_os = "linux")]
#[path = "linux/waits.rs"]
pub mod waits;
#[cfg(windows)]
pub mod window;
#[cfg(target_os = "linux")]
#[path = "linux/window.rs"]
pub mod window;

pub use error::{Error, Result};

pub mod compat;

/// `String::from_utf16_lossy` with room for the common all-ASCII case
/// reserved up front: that one grows from half the length, so most short
/// strings (process names, paths) were allocated twice.
#[cfg(windows)]
pub(crate) fn utf16_lossy(units: &[u16]) -> String {
    let mut s = String::with_capacity(units.len());
    s.extend(
        char::decode_utf16(units.iter().copied()).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)),
    );
    s
}

/// The user's Windows UI language, independent of regional number formats.
#[cfg(windows)]
pub fn system_ui_language() -> String {
    // SAFETY: GetUserDefaultUILanguage takes no pointers or owned resources.
    let language = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
    match language & 0x03ff {
        0x1f => "tr",
        0x07 => "de",
        0x0c => "fr",
        0x0a => "es",
        _ => "en",
    }
    .to_owned()
}

/// The user's UI language from the POSIX locale (`LC_ALL`, `LC_MESSAGES`,
/// `LANG`, `LANGUAGE`), narrowed to the languages RustWTM ships.
#[cfg(target_os = "linux")]
pub fn system_ui_language() -> String {
    linux::util::ui_language()
}

/// Refusing to execute listed programs (fanotify); Linux only.
#[cfg(target_os = "linux")]
#[path = "linux/exec_guard.rs"]
pub mod exec_guard;

#[cfg(target_os = "linux")]
pub(crate) mod linux {
    #[path = "util.rs"]
    pub(crate) mod util;
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn utf16_lossy_matches_std_in_one_allocation() {
        let cases: [&[u16]; 4] = [
            &[],
            &[0x61, 0x62, 0x63],
            // Non-ASCII, a surrogate pair and a lone surrogate.
            &[0x00e7, 0xd83d, 0xde00, 0xd800, 0x41],
            &[0xdc00],
        ];
        for units in cases {
            assert_eq!(super::utf16_lossy(units), String::from_utf16_lossy(units));
        }
        let ascii: Vec<u16> = "explorer.exe".encode_utf16().collect();
        assert_eq!(super::utf16_lossy(&ascii).capacity(), ascii.len());
    }
}
