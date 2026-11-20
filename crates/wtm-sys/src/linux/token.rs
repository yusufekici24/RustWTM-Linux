//! A process's credentials in the shape of a Windows token: the user, its
//! groups (the primary group first), and its capabilities as privileges
//! (enabled when effective, enabled by default when permitted). "App
//! container" means a sandbox: seccomp filtering or a confined snap.

use crate::Result;
use crate::control::ProcessHandle;
use crate::linux::util::{self, kv_text};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    pub sid: String,
    pub name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenGroup {
    pub account: Account,
    pub attributes: u32,
}

impl TokenGroup {
    const MANDATORY: u32 = 0x1;
    const ENABLED: u32 = 0x4;
    const OWNER: u32 = 0x8;
    const DENY_ONLY: u32 = 0x10;
    const INTEGRITY: u32 = 0x20;
    const RESOURCE: u32 = 0x2000_0000;
    const LOGON_ID: u32 = 0xC000_0000;

    pub fn enabled(&self) -> bool {
        self.attributes & Self::ENABLED != 0
    }
    pub fn deny_only(&self) -> bool {
        self.attributes & Self::DENY_ONLY != 0
    }
    pub fn mandatory(&self) -> bool {
        self.attributes & Self::MANDATORY != 0
    }
    pub fn owner(&self) -> bool {
        self.attributes & Self::OWNER != 0
    }
    pub fn integrity(&self) -> bool {
        self.attributes & Self::INTEGRITY != 0
    }
    pub fn logon_id(&self) -> bool {
        self.attributes & Self::LOGON_ID == Self::LOGON_ID
    }
    pub fn resource(&self) -> bool {
        self.attributes & Self::RESOURCE != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenPrivilege {
    pub name: String,
    pub description: Option<String>,
    pub attributes: u32,
}

impl TokenPrivilege {
    const ENABLED_BY_DEFAULT: u32 = 0x1;
    const ENABLED: u32 = 0x2;

    pub fn enabled(&self) -> bool {
        self.attributes & Self::ENABLED != 0
    }
    pub fn enabled_by_default(&self) -> bool {
        self.attributes & Self::ENABLED_BY_DEFAULT != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenReport {
    pub user: Account,
    pub session: u32,
    pub app_container: bool,
    pub groups: Vec<TokenGroup>,
    pub privileges: Vec<TokenPrivilege>,
}

const CAPS: [(&str, &str); 41] = [
    ("CAP_CHOWN", "Change file owners"),
    ("CAP_DAC_OVERRIDE", "Bypass file permission checks"),
    ("CAP_DAC_READ_SEARCH", "Bypass file read permission checks"),
    ("CAP_FOWNER", "Bypass owner checks on files"),
    ("CAP_FSETID", "Keep set-user-ID bits when files change"),
    ("CAP_KILL", "Send signals to any process"),
    ("CAP_SETGID", "Change group IDs"),
    ("CAP_SETUID", "Change user IDs"),
    ("CAP_SETPCAP", "Change capabilities"),
    (
        "CAP_LINUX_IMMUTABLE",
        "Set immutable and append-only file flags",
    ),
    ("CAP_NET_BIND_SERVICE", "Bind ports below 1024"),
    ("CAP_NET_BROADCAST", "Broadcast and listen to multicast"),
    ("CAP_NET_ADMIN", "Administer the network"),
    ("CAP_NET_RAW", "Use raw sockets"),
    ("CAP_IPC_LOCK", "Lock memory"),
    ("CAP_IPC_OWNER", "Bypass IPC permission checks"),
    ("CAP_SYS_MODULE", "Load and unload kernel modules"),
    ("CAP_SYS_RAWIO", "Perform raw I/O"),
    ("CAP_SYS_CHROOT", "Change the root directory"),
    ("CAP_SYS_PTRACE", "Debug any process"),
    ("CAP_SYS_PACCT", "Configure process accounting"),
    ("CAP_SYS_ADMIN", "Administer the system"),
    ("CAP_SYS_BOOT", "Reboot the system"),
    (
        "CAP_SYS_NICE",
        "Raise priorities and set scheduling of any process",
    ),
    ("CAP_SYS_RESOURCE", "Override resource limits"),
    ("CAP_SYS_TIME", "Change the system clock"),
    ("CAP_SYS_TTY_CONFIG", "Configure terminals"),
    ("CAP_MKNOD", "Create device files"),
    ("CAP_LEASE", "Take file leases"),
    ("CAP_AUDIT_WRITE", "Write to the audit log"),
    ("CAP_AUDIT_CONTROL", "Configure auditing"),
    ("CAP_SETFCAP", "Set file capabilities"),
    ("CAP_MAC_OVERRIDE", "Override mandatory access control"),
    ("CAP_MAC_ADMIN", "Configure mandatory access control"),
    ("CAP_SYSLOG", "Use privileged syslog operations"),
    ("CAP_WAKE_ALARM", "Wake the system from sleep"),
    ("CAP_BLOCK_SUSPEND", "Block system suspend"),
    ("CAP_AUDIT_READ", "Read the audit log"),
    ("CAP_PERFMON", "Use performance monitoring"),
    ("CAP_BPF", "Use BPF"),
    ("CAP_CHECKPOINT_RESTORE", "Checkpoint and restore processes"),
];

fn caps(status: &str, key: &str) -> u64 {
    kv_text(status, key)
        .and_then(|v| u64::from_str_radix(v, 16).ok())
        .unwrap_or(0)
}

fn group_account(gid: u32) -> Account {
    Account {
        sid: format!("S-1-22-2-{gid}"),
        name: crate::pipe::group_name(gid),
    }
}

impl ProcessHandle {
    pub fn token_report(&self) -> Result<TokenReport> {
        self.check()?;
        let status = std::fs::read_to_string(self.proc_path("status"))
            .map_err(|e| crate::Error::io("status", e))?;
        let uid = util::process_uid(self.pid()).unwrap_or(u32::MAX);
        let gid: u32 = kv_text(&status, "Gid")
            .and_then(|g| g.split_whitespace().next()?.parse().ok())
            .unwrap_or(0);
        let mut groups = vec![TokenGroup {
            account: group_account(gid),
            attributes: TokenGroup::ENABLED | TokenGroup::MANDATORY | TokenGroup::OWNER,
        }];
        for g in kv_text(&status, "Groups")
            .unwrap_or("")
            .split_whitespace()
            .filter_map(|g| g.parse::<u32>().ok())
        {
            if g != gid {
                groups.push(TokenGroup {
                    account: group_account(g),
                    attributes: TokenGroup::ENABLED | TokenGroup::MANDATORY,
                });
            }
        }
        let (eff, prm) = (caps(&status, "CapEff"), caps(&status, "CapPrm"));
        let privileges = CAPS
            .iter()
            .enumerate()
            .filter(|(i, _)| (eff | prm) & (1u64 << i) != 0)
            .map(|(i, (name, what))| TokenPrivilege {
                name: (*name).to_owned(),
                description: Some((*what).to_owned()),
                attributes: (if eff & (1 << i) != 0 {
                    TokenPrivilege::ENABLED
                } else {
                    0
                }) | (if prm & (1 << i) != 0 {
                    TokenPrivilege::ENABLED_BY_DEFAULT
                } else {
                    0
                }),
            })
            .collect();
        let seccomp = kv_text(&status, "Seccomp").is_some_and(|s| s != "0");
        let package = self.package_full_name().ok().flatten();
        Ok(TokenReport {
            user: Account {
                sid: crate::pipe::uid_sid(uid),
                name: util::user_name(uid),
            },
            session: crate::process::session_of(self.pid()),
            app_container: seccomp || package.is_some(),
            groups,
            privileges,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Access;

    #[test]
    fn reads_self() {
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        let t = h.token_report().unwrap();
        assert!(t.user.sid.starts_with("S-1-22-1-"));
        assert!(!t.groups.is_empty() && t.groups[0].enabled());
        let _ = TokenGroup::DENY_ONLY
            | TokenGroup::INTEGRITY
            | TokenGroup::RESOURCE
            | TokenGroup::LOGON_ID;
    }
}
