//! CPU topology from sysfs. Efficiency classes are relative tiers: on
//! Intel hybrid parts `cpu_core` CPUs are P (class 2), `cpu_atom` CPUs with
//! an L3 are E (1) and those without (Meteor Lake's SoC tile) LP-E (0). On
//! ARM big.LITTLE `cpu_capacity` ranks the tiers. CPU "IDs" are the kernel
//! CPU numbers, which is also what affinity takes.

use std::collections::BTreeMap;
use std::fs;

use crate::Result;
use crate::linux::util::{read_trim, read_u64};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalCpu {
    pub id: u32,
    pub group: u16,
    pub index: u8,
    pub core: u8,
    pub efficiency_class: u8,
    pub parked: bool,
    pub reserved_elsewhere: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Topology {
    pub cpus: Vec<LogicalCpu>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoreSelection {
    Performance,
    Efficient,
    LowPower,
    All,
}

impl CoreSelection {
    pub fn label(self) -> &'static str {
        match self {
            Self::Performance => "P-cores",
            Self::Efficient => "E-cores",
            Self::LowPower => "LP-E cores",
            Self::All => "All available cores",
        }
    }
}

impl Topology {
    pub fn classes(&self) -> Vec<u8> {
        let mut classes: Vec<_> = self.cpus.iter().map(|c| c.efficiency_class).collect();
        classes.sort_unstable();
        classes.dedup();
        classes
    }
    pub fn hybrid(&self) -> bool {
        self.classes().len() > 1
    }
    pub fn groups(&self) -> Vec<u16> {
        let mut groups: Vec<_> = self.cpus.iter().map(|c| c.group).collect();
        groups.sort_unstable();
        groups.dedup();
        groups
    }
    pub fn tier(&self, class: u8) -> String {
        let classes = self.classes();
        if classes.len() < 2 {
            return "Uniform".into();
        }
        if classes.len() > 3 {
            return format!("Class {class}");
        }
        if Some(&class) == classes.last() {
            "P".into()
        } else if classes.len() == 3 && Some(&class) == classes.first() {
            "LP-E".into()
        } else {
            "E".into()
        }
    }
    pub fn mask(
        &self,
        selection: CoreSelection,
        group: u16,
        allowed: u64,
    ) -> std::result::Result<u64, String> {
        let classes = self.classes();
        let class = match selection {
            CoreSelection::All => None,
            _ if classes.len() < 2 => {
                return Err(
                    "The kernel reports one CPU class; P/E selection is unavailable".into(),
                );
            }
            CoreSelection::Performance => classes.last().copied(),
            CoreSelection::Efficient if classes.len() <= 3 => Some(classes[classes.len() - 2]),
            CoreSelection::LowPower if classes.len() == 3 => classes.first().copied(),
            _ => return Err("This topology has no unambiguous matching core tier".into()),
        };
        let mask = self
            .cpus
            .iter()
            .filter(|c| {
                c.group == group
                    && c.index < 64
                    && !c.reserved_elsewhere
                    && class.is_none_or(|v| v == c.efficiency_class)
            })
            .fold(0u64, |mask, c| mask | (1u64 << c.index))
            & allowed;
        if mask == 0 {
            Err("No matching available processors".into())
        } else {
            Ok(mask)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RescueSplit {
    pub reserved: Vec<u32>,
    pub others: Vec<u32>,
    pub reserved_indices: Vec<u8>,
}

impl Topology {
    /// The last physical core of the fastest tier (all its SMT siblings on
    /// machines with at least 8 logical processors). `None` when fewer than
    /// two processors are usable.
    pub fn rescue_split(&self) -> Option<RescueSplit> {
        let usable: Vec<_> = self.cpus.iter().filter(|c| !c.reserved_elsewhere).collect();
        if usable.len() < 2 {
            return None;
        }
        let fastest = usable.iter().map(|c| c.efficiency_class).max()?;
        let pick = usable
            .iter()
            .filter(|c| c.group == 0 && c.efficiency_class == fastest)
            .max_by_key(|c| c.index)?;
        let whole_core = usable.len() >= 8;
        let (reserved, others): (Vec<&&LogicalCpu>, Vec<_>) = usable.iter().partition(|c| {
            c.group == pick.group
                && if whole_core {
                    c.core == pick.core
                } else {
                    c.index == pick.index
                }
        });
        if others.is_empty() {
            return None;
        }
        Some(RescueSplit {
            reserved: reserved.iter().map(|c| c.id).collect(),
            others: others.iter().map(|c| c.id).collect(),
            reserved_indices: reserved.iter().map(|c| c.index).collect(),
        })
    }
}

/// Parses a kernel CPU list such as `0-3,8,10-11`.
pub(crate) fn parse_list(text: &str) -> Vec<u32> {
    let mut out = Vec::new();
    for part in text.trim().split(',').filter(|p| !p.is_empty()) {
        match part.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.parse::<u32>(), b.parse::<u32>()) {
                    out.extend(a..=b.min(a + 4096));
                }
            }
            None => {
                if let Ok(v) = part.parse() {
                    out.push(v);
                }
            }
        }
    }
    out
}

pub(crate) fn online_cpus() -> Vec<u32> {
    read_trim("/sys/devices/system/cpu/online")
        .map(|s| parse_list(&s))
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| (0..crate::system::logical_cpu_count()).collect())
}

pub(crate) fn online_mask() -> u64 {
    online_cpus()
        .into_iter()
        .filter(|&c| c < 64)
        .fold(0, |m, c| m | (1 << c))
}

fn has_l3(cpu: u32) -> bool {
    let base = format!("/sys/devices/system/cpu/cpu{cpu}/cache");
    let Ok(rd) = fs::read_dir(&base) else {
        return true;
    };
    rd.filter_map(|e| e.ok())
        .any(|e| read_trim(e.path().join("level")).as_deref() == Some("3"))
}

/// The efficiency class of every online CPU.
fn classes(cpus: &[u32]) -> BTreeMap<u32, u8> {
    let mut out = BTreeMap::new();
    let core = read_trim("/sys/devices/cpu_core/cpus").map(|s| parse_list(&s));
    let atom = read_trim("/sys/devices/cpu_atom/cpus").map(|s| parse_list(&s));
    if let (Some(core), Some(atom)) = (core, atom) {
        for &c in cpus {
            let class = if core.contains(&c) {
                2
            } else if atom.contains(&c) {
                u8::from(has_l3(c))
            } else {
                2
            };
            out.insert(c, class);
        }
        return out;
    }
    // ARM and others: rank distinct capacities.
    let caps: Vec<(u32, u64)> = cpus
        .iter()
        .map(|&c| {
            let cap = read_u64(format!("/sys/devices/system/cpu/cpu{c}/cpu_capacity"))
                .or_else(|| {
                    read_u64(format!(
                        "/sys/devices/system/cpu/cpu{c}/cpufreq/cpuinfo_max_freq"
                    ))
                })
                .unwrap_or(0);
            (c, cap)
        })
        .collect();
    let mut distinct: Vec<u64> = caps.iter().map(|(_, v)| *v).collect();
    distinct.sort_unstable();
    distinct.dedup();
    // Frequency differences below 15% are boost bins, not core types.
    let mut tiers: Vec<u64> = Vec::new();
    for v in distinct {
        if tiers.last().is_none_or(|&t| v as f64 > t as f64 * 1.15) {
            tiers.push(v);
        }
    }
    for (c, v) in caps {
        let class = tiers.iter().rposition(|&t| v >= t).unwrap_or(0);
        out.insert(c, class as u8);
    }
    out
}

pub fn topology() -> Result<Topology> {
    let cpus = online_cpus();
    let classes = classes(&cpus);
    let mut cores: BTreeMap<(u64, u64), u8> = BTreeMap::new();
    let mut out = Vec::with_capacity(cpus.len());
    for &c in &cpus {
        let base = format!("/sys/devices/system/cpu/cpu{c}/topology");
        let package = read_u64(format!("{base}/physical_package_id")).unwrap_or(0);
        let core_id = read_u64(format!("{base}/core_id")).unwrap_or(u64::from(c));
        let next = cores.len() as u8;
        let core = *cores.entry((package, core_id)).or_insert(next);
        out.push(LogicalCpu {
            id: c,
            group: (c / 64) as u16,
            index: (c % 64) as u8,
            core,
            efficiency_class: classes.get(&c).copied().unwrap_or(0),
            parked: false,
            reserved_elsewhere: false,
        });
    }
    out.sort_by_key(|c| (c.group, c.index));
    Ok(Topology { cpus: out })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cpu_lists() {
        assert_eq!(parse_list("0-3,8,10-11\n"), vec![0, 1, 2, 3, 8, 10, 11]);
        assert_eq!(parse_list(""), Vec::<u32>::new());
    }

    #[test]
    fn reads_this_machine() {
        let t = topology().unwrap();
        assert!(!t.cpus.is_empty());
        assert_eq!(t.cpus.len(), online_cpus().len());
        assert!(t.mask(CoreSelection::All, 0, u64::MAX).is_ok());
    }

    #[test]
    fn hybrid_tiers() {
        let cpu = |id, class, core| LogicalCpu {
            id,
            group: 0,
            index: id as u8,
            core,
            efficiency_class: class,
            parked: false,
            reserved_elsewhere: false,
        };
        let t = Topology {
            cpus: vec![cpu(0, 2, 0), cpu(1, 2, 0), cpu(2, 1, 1), cpu(3, 0, 2)],
        };
        assert_eq!(t.tier(2), "P");
        assert_eq!(t.tier(1), "E");
        assert_eq!(t.tier(0), "LP-E");
        assert_eq!(
            t.mask(CoreSelection::Performance, 0, u64::MAX).unwrap(),
            0b11
        );
        assert_eq!(
            t.mask(CoreSelection::LowPower, 0, u64::MAX).unwrap(),
            0b1000
        );
        let split = t.rescue_split().unwrap();
        assert_eq!(split.reserved, vec![1]);
    }
}
