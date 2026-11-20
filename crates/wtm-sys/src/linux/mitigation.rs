//! Exploit mitigations as Task Manager names them, from what Linux knows:
//! no UAC virtualization; "DEP" holds unless the process runs with the
//! READ_IMPLIES_EXEC personality; hardware stack protection is the x86
//! user shadow stack (`x86_Thread_features: shstk`).

use crate::Result;
use crate::control::ProcessHandle;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum UacVirtualization {
    NotAllowed,
    Disabled,
    Enabled,
}

impl UacVirtualization {
    pub fn decode(allowed: u32, enabled: u32) -> Self {
        match (allowed != 0, enabled != 0) {
            (false, _) => Self::NotAllowed,
            (true, false) => Self::Disabled,
            (true, true) => Self::Enabled,
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum StackProtection {
    Disabled,
    CompatibleModules { audit: bool },
    AllModules { audit: bool },
}

impl StackProtection {
    pub fn decode(flags: u32) -> Self {
        let enabled = flags & 1 != 0;
        let audit = flags & 2 != 0;
        let strict = flags & 0x10 != 0;
        if !enabled && !audit {
            return Self::Disabled;
        }
        let audit = !enabled;
        if strict {
            Self::AllModules { audit }
        } else {
            Self::CompatibleModules { audit }
        }
    }
}

pub fn dep_enabled_from(flags: u32) -> bool {
    flags & 1 != 0
}

/// `READ_IMPLIES_EXEC` in personality(2).
const READ_IMPLIES_EXEC: u32 = 0x0040_0000;

impl ProcessHandle {
    pub fn uac_virtualization(&self) -> Result<UacVirtualization> {
        Ok(UacVirtualization::NotAllowed)
    }

    pub fn dep_enabled(&self) -> Result<bool> {
        let text = std::fs::read_to_string(self.proc_path("personality"))
            .map_err(|e| crate::Error::io("personality", e))?;
        let p = u32::from_str_radix(text.trim(), 16).unwrap_or(0);
        Ok(p & READ_IMPLIES_EXEC == 0)
    }

    pub fn stack_protection(&self) -> Result<StackProtection> {
        let status = std::fs::read_to_string(self.proc_path("status"))
            .map_err(|e| crate::Error::io("status", e))?;
        let on = status
            .lines()
            .find_map(|l| l.strip_prefix("x86_Thread_features:"))
            .is_some_and(|f| f.split_whitespace().any(|w| w == "shstk"));
        Ok(if on {
            StackProtection::CompatibleModules { audit: false }
        } else {
            StackProtection::Disabled
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
        assert!(h.dep_enabled().unwrap());
        h.stack_protection().unwrap();
        assert_eq!(
            h.uac_virtualization().unwrap(),
            UacVirtualization::NotAllowed
        );
    }
}
