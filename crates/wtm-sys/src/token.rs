//! A process's access token as Process Explorer's Security tab shows it:
//! the account, the groups with their flags and the privileges with their
//! state.
//!
//! Needs only `TOKEN_QUERY` on the token, which query rights on the
//! process usually grant for the user's own processes. Account names are
//! looked up once per SID and cached; a lookup can take seconds when a
//! domain controller must answer, so callers read a token off the UI thread.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use windows::Win32::Foundation::{HANDLE, HLOCAL, LUID, LocalFree};
use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
use windows::Win32::Security::{
    GetTokenInformation, LookupAccountSidW, LookupPrivilegeDisplayNameW, LookupPrivilegeNameW,
    PSID, SID_AND_ATTRIBUTES, SID_NAME_USE, TOKEN_GROUPS, TOKEN_INFORMATION_CLASS,
    TOKEN_PRIVILEGES, TOKEN_QUERY, TOKEN_USER, TokenGroups, TokenIsAppContainer, TokenPrivileges,
    TokenSessionId, TokenUser,
};
use windows::Win32::System::Threading::OpenProcessToken;
use windows::core::{PCWSTR, PWSTR};

use crate::Result;
use crate::control::ProcessHandle;
use crate::handle::OwnedHandle;

/// A SID and, when Windows can name it, its account name.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Account {
    /// Such as "S-1-5-32-544".
    pub sid: String,
    /// Such as "BUILTIN\Administrators"; `None` for SIDs without a name,
    /// such as logon sessions and app capabilities.
    pub name: Option<String>,
}

/// A group in the token and how the token uses it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenGroup {
    pub account: Account,
    /// `SE_GROUP_*` flags.
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

    /// Grants access; a group that is only used to deny access, or is
    /// switched off, does not.
    pub fn enabled(&self) -> bool {
        self.attributes & Self::ENABLED != 0
    }
    /// Only checked against deny entries: how UAC filters Administrators
    /// out of a standard token.
    pub fn deny_only(&self) -> bool {
        self.attributes & Self::DENY_ONLY != 0
    }
    /// Cannot be switched off.
    pub fn mandatory(&self) -> bool {
        self.attributes & Self::MANDATORY != 0
    }
    /// May be set as the owner of new objects.
    pub fn owner(&self) -> bool {
        self.attributes & Self::OWNER != 0
    }
    /// The token's integrity level.
    pub fn integrity(&self) -> bool {
        self.attributes & Self::INTEGRITY != 0
    }
    /// Identifies the sign-in session.
    pub fn logon_id(&self) -> bool {
        self.attributes & Self::LOGON_ID == Self::LOGON_ID
    }
    /// A domain-local group.
    pub fn resource(&self) -> bool {
        self.attributes & Self::RESOURCE != 0
    }
}

/// A privilege in the token and whether it is switched on.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenPrivilege {
    /// Such as "SeShutdownPrivilege".
    pub name: String,
    /// What it allows, in the language of Windows, such as "Shut down the
    /// system".
    pub description: Option<String>,
    /// `SE_PRIVILEGE_*` flags.
    pub attributes: u32,
}

impl TokenPrivilege {
    const ENABLED_BY_DEFAULT: u32 = 0x1;
    const ENABLED: u32 = 0x2;

    /// In effect now. A disabled privilege is held but must be switched on
    /// before it counts.
    pub fn enabled(&self) -> bool {
        self.attributes & Self::ENABLED != 0
    }
    pub fn enabled_by_default(&self) -> bool {
        self.attributes & Self::ENABLED_BY_DEFAULT != 0
    }
}

/// What a process's token holds.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TokenReport {
    pub user: Account,
    /// The Remote Desktop session the token belongs to.
    pub session: u32,
    /// Runs in an app container (a sandbox for packaged apps and browser
    /// renderers).
    pub app_container: bool,
    /// In the token's order.
    pub groups: Vec<TokenGroup>,
    pub privileges: Vec<TokenPrivilege>,
}

impl ProcessHandle {
    /// The process's token: account, groups and privileges. Needs
    /// [`crate::control::Access::QUERY`].
    pub fn token_report(&self) -> Result<TokenReport> {
        let mut raw = HANDLE::default();
        // SAFETY: `raw` receives a token handle that OwnedHandle closes.
        unsafe { OpenProcessToken(self.raw(), TOKEN_QUERY, &mut raw)? };
        let token = OwnedHandle::new(raw);

        let buf = token_info(&token, TokenUser)?;
        // SAFETY: the buffer holds a TOKEN_USER whose SID points into it.
        let user = account(unsafe { (*buf.as_ptr().cast::<TOKEN_USER>()).User.Sid })?;

        let buf = token_info(&token, TokenGroups)?;
        // SAFETY: the buffer holds a TOKEN_GROUPS whose GroupCount entries
        // and their SIDs lie inside it.
        let groups = unsafe {
            let header = buf.as_ptr().cast::<TOKEN_GROUPS>();
            let first = std::ptr::addr_of!((*header).Groups).cast::<SID_AND_ATTRIBUTES>();
            std::slice::from_raw_parts(first, (*header).GroupCount as usize)
                .iter()
                .map(|g| {
                    Ok(TokenGroup {
                        account: account(g.Sid)?,
                        attributes: g.Attributes,
                    })
                })
                .collect::<Result<Vec<_>>>()?
        };

        let buf = token_info(&token, TokenPrivileges)?;
        // SAFETY: the buffer holds a TOKEN_PRIVILEGES whose PrivilegeCount
        // entries lie inside it.
        let privileges = unsafe {
            let header = buf.as_ptr().cast::<TOKEN_PRIVILEGES>();
            let first = std::ptr::addr_of!((*header).Privileges)
                .cast::<windows::Win32::Security::LUID_AND_ATTRIBUTES>();
            std::slice::from_raw_parts(first, (*header).PrivilegeCount as usize)
                .iter()
                .filter_map(|p| {
                    let name = privilege_name(&p.Luid)?;
                    Some(TokenPrivilege {
                        description: privilege_description(&name),
                        name,
                        attributes: p.Attributes.0,
                    })
                })
                .collect()
        };

        Ok(TokenReport {
            user,
            session: token_u32(&token, TokenSessionId)?,
            app_container: token_u32(&token, TokenIsAppContainer).is_ok_and(|v| v != 0),
            groups,
            privileges,
        })
    }
}

/// A variable-length token information class; u64 elements keep the
/// structures aligned.
fn token_info(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<u64>> {
    // Groups of a domain account with many memberships are the largest;
    // a megabyte is far beyond any real token.
    const MOST: u32 = 1 << 20;
    let mut len = 0u32;
    // SAFETY: a null buffer only reports the required size.
    let _ = unsafe { GetTokenInformation(token.raw(), class, None, 0, &mut len) };
    if len == 0 || len > MOST {
        return Err(windows::core::Error::from_thread().into());
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: `buf` is writable for `len` bytes and aligned.
    unsafe {
        GetTokenInformation(
            token.raw(),
            class,
            Some(buf.as_mut_ptr().cast()),
            len,
            &mut len,
        )?
    };
    Ok(buf)
}

fn token_u32(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> Result<u32> {
    let mut value = 0u32;
    let mut len = 0u32;
    // SAFETY: `value` is writable for four bytes.
    unsafe {
        GetTokenInformation(
            token.raw(),
            class,
            Some(std::ptr::from_mut(&mut value).cast()),
            4,
            &mut len,
        )?
    };
    Ok(value)
}

/// SID strings to account names; `None` remembers a SID Windows cannot
/// name, so it is not asked again.
static NAMES: Mutex<Option<HashMap<String, Option<String>>>> = Mutex::new(None);

/// Names kept at most; a PC sees a few hundred distinct SIDs.
const NAMES_KEPT: usize = 4096;

fn account(sid: PSID) -> Result<Account> {
    let text = sid_string(sid)?;
    let cached = NAMES
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .and_then(|names| names.get(&text).cloned());
    let name = match cached {
        Some(name) => name,
        None => {
            // Looked up without the lock held: it may wait on the network.
            let name = account_name(sid);
            let mut names = NAMES.lock().unwrap_or_else(PoisonError::into_inner);
            let names = names.get_or_insert_with(HashMap::new);
            if names.len() >= NAMES_KEPT {
                names.clear();
            }
            names.insert(text.clone(), name.clone());
            name
        }
    };
    Ok(Account { sid: text, name })
}

fn sid_string(sid: PSID) -> Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: `sid` points into a live token buffer; `text` is freed below.
    unsafe {
        ConvertSidToStringSidW(sid, &mut text)?;
        let out = text.to_string();
        LocalFree(Some(HLOCAL(text.0.cast())));
        out.map_err(|_| windows::core::Error::from_thread().into())
    }
}

/// `DOMAIN\name`, or just the name for well-known groups such as
/// "Everyone" that have no domain.
fn account_name(sid: PSID) -> Option<String> {
    let mut name = [0u16; 256];
    let mut domain = [0u16; 256];
    let (mut name_len, mut domain_len) = (name.len() as u32, domain.len() as u32);
    let mut kind = SID_NAME_USE::default();
    // SAFETY: both buffers are writable for the lengths passed; `sid`
    // points into a live token buffer.
    unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut kind,
        )
        .ok()?
    };
    let name = String::from_utf16_lossy(&name[..name_len as usize]);
    Some(match domain_len {
        0 => name,
        n => format!(
            "{}\\{name}",
            String::from_utf16_lossy(&domain[..n as usize])
        ),
    })
}

fn privilege_name(luid: &LUID) -> Option<String> {
    let mut name = [0u16; 128];
    let mut len = name.len() as u32;
    // SAFETY: `name` is writable for `len` characters.
    unsafe {
        LookupPrivilegeNameW(
            PCWSTR::null(),
            luid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut len,
        )
        .ok()?
    };
    Some(String::from_utf16_lossy(&name[..len as usize]))
}

/// Windows' own description of a privilege, in the user's language.
fn privilege_description(name: &str) -> Option<String> {
    let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
    let mut text = [0u16; 256];
    let mut len = text.len() as u32;
    let mut language = 0u32;
    // SAFETY: `wide` is NUL-terminated; `text` is writable for `len`
    // characters.
    unsafe {
        LookupPrivilegeDisplayNameW(
            PCWSTR::null(),
            PCWSTR(wide.as_ptr()),
            Some(PWSTR(text.as_mut_ptr())),
            &mut len,
            &mut language,
        )
        .ok()?
    };
    Some(String::from_utf16_lossy(&text[..len as usize])).filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::Access;

    #[test]
    fn own_token_names_its_user_groups_and_privileges() {
        let h = ProcessHandle::open(std::process::id(), Access::QUERY).unwrap();
        let t = h.token_report().unwrap();
        assert_eq!(t.user.sid, crate::security::current_user_sid().unwrap());
        assert!(t.user.name.as_deref().is_some_and(|n| n.contains('\\')));

        let everyone = t
            .groups
            .iter()
            .find(|g| g.account.sid == "S-1-1-0")
            .unwrap();
        assert!(everyone.account.name.is_some());
        assert!(everyone.enabled() && everyone.mandatory() && !everyone.deny_only());
        // Exactly one group carries the integrity level.
        let labels: Vec<_> = t.groups.iter().filter(|g| g.integrity()).collect();
        assert_eq!(labels.len(), 1);
        assert!(labels[0].account.sid.starts_with("S-1-16-"));
        // How the test was started decides whether there is a logon SID;
        // if there is one, it is enabled and cannot be switched off.
        for logon in t.groups.iter().filter(|g| g.logon_id()) {
            assert!(logon.enabled() && logon.mandatory());
        }

        let notify = t
            .privileges
            .iter()
            .find(|p| p.name == "SeChangeNotifyPrivilege")
            .unwrap();
        assert!(notify.enabled() && notify.enabled_by_default());
        assert!(notify.description.as_deref().is_some_and(|d| !d.is_empty()));

        let mut session = 0u32;
        // SAFETY: `session` is a writable u32.
        unsafe {
            windows::Win32::System::RemoteDesktop::ProcessIdToSessionId(
                std::process::id(),
                &mut session,
            )
            .unwrap()
        };
        assert_eq!(t.session, session);
        assert!(!t.app_container);

        // A second read is answered from the name cache, unchanged.
        assert_eq!(h.token_report().unwrap().groups, t.groups);
    }

    #[test]
    fn group_and_privilege_flags_decode() {
        let group = |attributes| TokenGroup {
            attributes,
            ..TokenGroup::default()
        };
        // UAC's filtered Administrators: deny-only, not enabled.
        let admins = group(0x10);
        assert!(admins.deny_only() && !admins.enabled() && !admins.mandatory());
        let logon = group(0xC000_0007);
        assert!(logon.logon_id() && logon.enabled() && logon.mandatory());
        // The resource bit alone is not a logon ID.
        let resource = group(0x2000_0004);
        assert!(resource.resource() && !resource.logon_id());
        let owner = group(0xF);
        assert!(owner.owner() && !owner.integrity());
        assert!(group(0x60).integrity());

        let p = |attributes| TokenPrivilege {
            attributes,
            ..TokenPrivilege::default()
        };
        assert!(p(3).enabled() && p(3).enabled_by_default());
        assert!(!p(0).enabled() && !p(1).enabled() && p(1).enabled_by_default());
    }
}
