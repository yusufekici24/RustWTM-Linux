use std::fmt;

use serde::{Deserialize, Serialize};
use wtm_core::control::{IoPriority, MemoryPriority, PriorityClass, ProcessRef};

/// Ordered from most to least restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriorityLevel {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
}

impl From<PriorityLevel> for PriorityClass {
    fn from(p: PriorityLevel) -> Self {
        match p {
            PriorityLevel::Idle => PriorityClass::Idle,
            PriorityLevel::BelowNormal => PriorityClass::BelowNormal,
            PriorityLevel::Normal => PriorityClass::Normal,
            PriorityLevel::AboveNormal => PriorityClass::AboveNormal,
        }
    }
}

/// An I/O priority below normal; ordered from most to least restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoLevel {
    /// What Windows gives background-mode processes.
    VeryLow,
    Low,
}

impl From<IoLevel> for IoPriority {
    fn from(level: IoLevel) -> Self {
        match level {
            IoLevel::VeryLow => IoPriority::Background,
            IoLevel::Low => IoPriority::Low,
        }
    }
}

impl fmt::Display for IoLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            IoLevel::VeryLow => "very low",
            IoLevel::Low => "low",
        })
    }
}

/// A memory priority below normal; ordered from most to least restrictive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryLevel {
    VeryLow,
    Low,
    Medium,
    BelowNormal,
}

impl From<MemoryLevel> for MemoryPriority {
    fn from(level: MemoryLevel) -> Self {
        match level {
            MemoryLevel::VeryLow => MemoryPriority::VeryLow,
            MemoryLevel::Low => MemoryPriority::Low,
            MemoryLevel::Medium => MemoryPriority::Medium,
            MemoryLevel::BelowNormal => MemoryPriority::BelowNormal,
        }
    }
}

impl fmt::Display for MemoryLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            MemoryLevel::VeryLow => "very low",
            MemoryLevel::Low => "low",
            MemoryLevel::Medium => "medium",
            MemoryLevel::BelowNormal => "below normal",
        })
    }
}

/// One enforcement step. In TOML: `"eco_qos"`, `{ cpu_cap = 25 }`,
/// `{ priority = "idle" }`, ...
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Priority(PriorityLevel),
    EcoQos,
    /// The default priority of the process's disk and network I/O.
    IoPriority(IoLevel),
    /// How soon Windows repurposes the process's standby pages.
    MemoryPriority(MemoryLevel),
    /// Hard cap as a share of total machine CPU (0 < x <= 100).
    CpuCap(f32),
    /// Hard cap as a fraction (0 < x <= 1) of the CPU usage observed when
    /// the process was first acted on. Resolved to `CpuCap` before execution.
    CpuCapRelative(f32),
    MemoryCapMb(u64),
    Suspend,
    Kill,
    /// One-shot: move the working set to the standby list.
    TrimMemory,
}

impl Action {
    /// Turns relative caps into absolute ones using `baseline_cpu`.
    pub(crate) fn resolve(self, baseline_cpu: f32) -> Self {
        match self {
            Action::CpuCapRelative(f) => Action::CpuCap((baseline_cpu * f).clamp(0.01, 100.0)),
            other => other,
        }
    }

    pub(crate) fn validate(&self) -> Result<(), String> {
        match *self {
            Action::CpuCap(p) if !(p > 0.0 && p <= 100.0) => {
                Err(format!("cpu_cap must be in (0, 100], got {p}"))
            }
            Action::CpuCapRelative(f) if !(f > 0.0 && f <= 1.0) => {
                Err(format!("cpu_cap_relative must be in (0, 1], got {f}"))
            }
            Action::MemoryCapMb(0) => Err("memory_cap_mb must be positive".into()),
            _ => Ok(()),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Action::Priority(p) => write!(f, "priority {p:?}"),
            Action::EcoQos => f.write_str("efficiency mode"),
            Action::IoPriority(l) => write!(f, "I/O priority {l}"),
            Action::MemoryPriority(l) => write!(f, "memory priority {l}"),
            Action::CpuCap(p) => write!(f, "CPU cap {p:.2}%"),
            Action::CpuCapRelative(r) => write!(f, "CPU cap {:.0}% of baseline", r * 100.0),
            Action::MemoryCapMb(mb) => write!(f, "memory cap {mb} MiB"),
            Action::Suspend => f.write_str("suspend"),
            Action::Kill => f.write_str("kill"),
            Action::TrimMemory => f.write_str("trim memory"),
        }
    }
}

/// Who asked for an action. Each source's actions are tracked separately so
/// one source rolling back never undoes another source's limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Source {
    Governor,
    Rule,
    /// Long-term history analysis.
    Insight,
    Manual,
    /// Relieves memory pressure.
    MemoryGuard,
    /// Calms background apps while a full-screen app is in front.
    GameMode,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Apply an (already resolved) action to a process.
    Apply {
        source: Source,
        target: ProcessRef,
        action: Action,
        reason: String,
    },
    /// Undo everything this source applied to a process.
    Restore {
        source: Source,
        target: ProcessRef,
        reason: String,
    },
}

impl Decision {
    pub fn target(&self) -> &ProcessRef {
        match self {
            Decision::Apply { target, .. } | Decision::Restore { target, .. } => target,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn priority_levels_map_to_classes() {
        for (level, class) in [
            (PriorityLevel::Idle, PriorityClass::Idle),
            (PriorityLevel::BelowNormal, PriorityClass::BelowNormal),
            (PriorityLevel::Normal, PriorityClass::Normal),
            (PriorityLevel::AboveNormal, PriorityClass::AboveNormal),
        ] {
            assert_eq!(PriorityClass::from(level), class);
        }
    }

    #[test]
    fn resolve_only_touches_relative_caps_and_clamps() {
        assert_eq!(
            Action::CpuCapRelative(0.5).resolve(40.0),
            Action::CpuCap(20.0)
        );
        // A process idle at baseline still gets a tiny but valid cap.
        assert_eq!(
            Action::CpuCapRelative(0.5).resolve(0.0),
            Action::CpuCap(0.01)
        );
        assert_eq!(Action::Kill.resolve(40.0), Action::Kill);
    }

    #[test]
    fn validate_rejects_out_of_range_values() {
        assert!(Action::CpuCap(100.0).validate().is_ok());
        assert!(Action::CpuCap(f32::NAN).validate().is_err());
        assert!(Action::CpuCap(101.0).validate().is_err());
        assert!(Action::CpuCapRelative(1.0).validate().is_ok());
        assert!(Action::CpuCapRelative(0.0).validate().is_err());
        assert!(Action::MemoryCapMb(1).validate().is_ok());
        assert_eq!(
            Action::MemoryCapMb(0).validate().unwrap_err(),
            "memory_cap_mb must be positive"
        );
    }

    #[test]
    fn io_and_memory_priorities_parse_from_toml_and_only_lower() {
        #[derive(Deserialize)]
        struct Doc {
            actions: Vec<Action>,
        }
        let doc: Doc = toml::from_str(
            r#"actions = [{ io_priority = "very_low" }, { memory_priority = "low" }]"#,
        )
        .unwrap();
        assert_eq!(
            doc.actions,
            [
                Action::IoPriority(IoLevel::VeryLow),
                Action::MemoryPriority(MemoryLevel::Low)
            ]
        );
        for raised in [
            r#"actions = [{ io_priority = "normal" }]"#,
            r#"actions = [{ io_priority = "high" }]"#,
            r#"actions = [{ memory_priority = "normal" }]"#,
        ] {
            assert!(toml::from_str::<Doc>(raised).is_err(), "{raised}");
        }
        assert_eq!(IoPriority::from(IoLevel::VeryLow), IoPriority::Background);
        assert_eq!(
            MemoryPriority::from(MemoryLevel::BelowNormal),
            MemoryPriority::BelowNormal
        );
        // The most restrictive first, as the executor folds them.
        assert!(IoLevel::VeryLow < IoLevel::Low);
        assert!(MemoryLevel::VeryLow < MemoryLevel::BelowNormal);
    }

    #[test]
    fn display_is_human_readable() {
        let shown: Vec<String> = [
            Action::Priority(PriorityLevel::Idle),
            Action::EcoQos,
            Action::IoPriority(IoLevel::VeryLow),
            Action::MemoryPriority(MemoryLevel::BelowNormal),
            Action::CpuCap(12.5),
            Action::CpuCapRelative(0.25),
            Action::MemoryCapMb(512),
            Action::Suspend,
            Action::Kill,
        ]
        .iter()
        .map(ToString::to_string)
        .collect();
        assert_eq!(
            shown,
            [
                "priority Idle",
                "efficiency mode",
                "I/O priority very low",
                "memory priority below normal",
                "CPU cap 12.50%",
                "CPU cap 25% of baseline",
                "memory cap 512 MiB",
                "suspend",
                "kill",
            ]
        );
    }
}
