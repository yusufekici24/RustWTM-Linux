//! Processes RustWTM must never kill, suspend or throttle.

/// Core OS processes. Matched case-insensitively against the image name.
/// Name matching errs on the side of safety: an impostor `csrss.exe` is also
/// left alone until path verification lands with the governor.
#[cfg_attr(not(windows), allow(dead_code))]
const PROTECTED_NAMES: &[&str] = &[
    "System Idle Process",
    "System",
    "Secure System",
    "Registry",
    "Memory Compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "LsaIso.exe",
    "dwm.exe",
    "fontdrvhost.exe",
];

/// Core Linux processes: init, the session and display stack, D-Bus,
/// logind and udev. Matched exactly (Linux names are case-sensitive).
#[cfg(target_os = "linux")]
const PROTECTED_LINUX: &[&str] = &[
    "systemd",
    "init",
    "kthreadd",
    "systemd-journald",
    "systemd-logind",
    "systemd-udevd",
    "systemd-resolved",
    "systemd-networkd",
    "systemd-oomd",
    "dbus-daemon",
    "dbus-broker",
    "dbus-broker-launch",
    "polkitd",
    "gdm3",
    "gdm-session-worker",
    "gnome-shell",
    "gnome-session-binary",
    "Xorg",
    "Xwayland",
    "sddm",
    "lightdm",
    "kwin_wayland",
    "kwin_x11",
    "plasmashell",
    "sshd",
    "login",
    "agetty",
];

/// Why a policy acting on its own (kill list, rules, governor, game mode,
/// insights) must never end or suspend a process called `name`, which a
/// person may still do by hand: a service host takes Windows services with
/// it (DcomLaunch's forces a restart), and RustWTM's own programs are the
/// way back.
pub fn automatic_end_blocked(name: &str) -> Option<&'static str> {
    const SERVICE_HOSTS: [&str; 2] = ["svchost.exe", "systemd"];
    const RUSTWTM: [&str; 6] = [
        "rustwtm.exe",
        "wtm.exe",
        "wtm-service.exe",
        "rustwtm",
        "wtm",
        "wtm-service",
    ];
    if SERVICE_HOSTS.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        Some("it hosts system services")
    } else if RUSTWTM.iter().any(|n| n.eq_ignore_ascii_case(name)) {
        Some("it is part of RustWTM")
    } else {
        None
    }
}

/// Returns why `pid`/`name` is protected, or `None` if it may be controlled.
#[cfg(target_os = "linux")]
pub fn protection_reason(pid: u32, name: &str) -> Option<&'static str> {
    // PID 1 (init), kthreadd (which stands for every kernel thread) and the
    // kernel workers started with it at boot (3, 4).
    if pid <= 4 {
        return Some("kernel process");
    }
    if pid == std::process::id() {
        return Some("RustWTM itself");
    }
    PROTECTED_LINUX
        .contains(&name)
        .then_some("core system process")
}

/// Returns why `pid`/`name` is protected, or `None` if it may be controlled.
#[cfg(windows)]
pub fn protection_reason(pid: u32, name: &str) -> Option<&'static str> {
    if pid == 0 || pid == 4 {
        return Some("kernel process");
    }
    if pid == std::process::id() {
        return Some("RustWTM itself");
    }
    PROTECTED_NAMES
        .iter()
        .any(|p| p.eq_ignore_ascii_case(name))
        .then_some("core Windows process")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policies_never_end_service_hosts_or_rustwtm() {
        assert!(automatic_end_blocked("SVCHOST.EXE").is_some());
        assert!(automatic_end_blocked("wtm-service.exe").is_some());
        assert!(automatic_end_blocked("chrome.exe").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn protects_core_linux_processes() {
        assert!(protection_reason(1, "anything").is_some());
        assert!(protection_reason(2, "kthreadd").is_some());
        assert!(protection_reason(999_999, "gnome-shell").is_some());
        assert!(protection_reason(999_999, "firefox").is_none());
        assert!(automatic_end_blocked("wtm-service").is_some());
    }

    #[cfg(windows)]
    #[test]
    fn protects_core_processes() {
        assert!(protection_reason(0, "").is_some());
        assert!(protection_reason(4, "System").is_some());
        assert!(protection_reason(999_999, "CSRSS.EXE").is_some());
        assert!(protection_reason(std::process::id(), "x").is_some());
        assert!(protection_reason(999_999, "notepad.exe").is_none());
    }
}
