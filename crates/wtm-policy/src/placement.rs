//! Core placement: which processors each process runs on, from core pins
//! (`[[pin]]`), separation groups (`[[separate]]`) and reserved cores
//! (`[[reserve_cores]]`), with the rescue core taken out while it is
//! engaged. [`Placer::plan`] is pure; the service applies the plan with
//! CPU Sets (or affinity, for `strict` entries) and puts processes back.
//!
//! Windows does not pass a process's default CPU Sets on to the programs it
//! starts, so `tree` counts every descendant of a matched process as part
//! of the app (see `docs/core-placement.md`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use wtm_core::ProcessInfo;
use wtm_core::cpu::Topology;

use crate::rules::{Detail, Details, Matcher};
use crate::{Key, glob_match, live_parent};

/// Logical processors named in one list at most.
const MOST_LISTED: usize = 4096;
/// Ancestors looked at for `tree`.
const MOST_ANCESTORS: usize = 64;
const DEFAULT_HOLD_SECS: u64 = 5;

/// Which processors an entry asks for. In TOML: `2` (the two fastest
/// physical cores), `"performance"`, `"efficient"`, `"low_power"`, or
/// logical processor numbers such as `"0-3,8"`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "RawCores")]
pub enum CoreSpec {
    /// The fastest physical cores, with their SMT siblings.
    Count(u16),
    /// Every core of a tier.
    Tier(Tier),
    /// Logical processors, numbered across processor groups in order.
    List(Vec<u16>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Performance,
    Efficient,
    LowPower,
}

impl Tier {
    fn name(self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Efficient => "efficient",
            Self::LowPower => "low_power",
        }
    }

    /// The label `Topology::tier` gives this tier's efficiency class.
    fn label(self) -> &'static str {
        match self {
            Self::Performance => "P",
            Self::Efficient => "E",
            Self::LowPower => "LP-E",
        }
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawCores {
    Count(i64),
    Text(String),
}

impl TryFrom<RawCores> for CoreSpec {
    type Error = String;

    fn try_from(raw: RawCores) -> Result<Self, String> {
        match raw {
            RawCores::Count(n) => match u16::try_from(n) {
                Ok(n) if n >= 1 => Ok(Self::Count(n)),
                _ => Err(format!("cores = {n}: a count must be 1 or more")),
            },
            RawCores::Text(text) => text.parse(),
        }
    }
}

impl std::str::FromStr for CoreSpec {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let text = text.trim();
        match text.to_ascii_lowercase().as_str() {
            "performance" | "p" => return Ok(Self::Tier(Tier::Performance)),
            "efficient" | "e" => return Ok(Self::Tier(Tier::Efficient)),
            "low_power" | "lp" | "lp-e" => return Ok(Self::Tier(Tier::LowPower)),
            _ => {}
        }
        let bad = || {
            format!(
                "cores = \"{text}\": expected a tier (performance, efficient, low_power) \
                 or processor numbers such as \"0-3,8\""
            )
        };
        let mut cpus = BTreeSet::new();
        for part in text.split(',') {
            let part = part.trim();
            let (from, to) = match part.split_once('-') {
                Some((a, b)) => (a.trim(), b.trim()),
                None => (part, part),
            };
            let (Ok(from), Ok(to)) = (from.parse::<u16>(), to.parse::<u16>()) else {
                return Err(bad());
            };
            if from > to || usize::from(to - from) >= MOST_LISTED {
                return Err(bad());
            }
            cpus.extend(from..=to);
            if cpus.len() > MOST_LISTED {
                return Err(format!(
                    "cores = \"{text}\": at most {MOST_LISTED} processors"
                ));
            }
        }
        if cpus.is_empty() {
            return Err(bad());
        }
        Ok(Self::List(cpus.into_iter().collect()))
    }
}

impl fmt::Display for CoreSpec {
    /// As written in TOML, ranges joined: `0-3,8`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Count(n) => write!(f, "{n}"),
            Self::Tier(t) => f.write_str(t.name()),
            Self::List(cpus) => {
                let mut first = true;
                let mut i = 0;
                while i < cpus.len() {
                    let mut j = i;
                    while j + 1 < cpus.len() && cpus[j + 1] == cpus[j] + 1 {
                        j += 1;
                    }
                    if !first {
                        f.write_str(",")?;
                    }
                    first = false;
                    if i == j {
                        write!(f, "{}", cpus[i])?;
                    } else {
                        write!(f, "{}-{}", cpus[i], cpus[j])?;
                    }
                    i = j + 1;
                }
                Ok(())
            }
        }
    }
}

fn yes() -> bool {
    true
}

fn default_hold() -> u64 {
    DEFAULT_HOLD_SECS
}

/// "This app runs on these cores."
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub name: String,
    #[serde(rename = "match")]
    pub matcher: Matcher,
    pub cores: CoreSpec,
    /// Hard affinity instead of CPU Sets.
    #[serde(default)]
    pub strict: bool,
    /// The programs a matched process starts belong to it.
    #[serde(default = "yes")]
    pub tree: bool,
}

/// "While two or more of these apps run, they share no core."
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Separate {
    pub name: String,
    /// Image-name wildcards, one per app; the first gets the fastest cores.
    pub apps: Vec<String>,
    /// Shares of the cores, one per app; equal when empty.
    #[serde(default)]
    pub weights: Vec<u32>,
    #[serde(default)]
    pub strict: bool,
    #[serde(default = "yes")]
    pub tree: bool,
    /// An app counts as running after this long, and for this long after
    /// it exits, so short-lived helpers do not reshuffle the split.
    #[serde(default = "default_hold")]
    pub hold_secs: u64,
}

/// "While this app runs, these cores are its own."
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReserveCores {
    pub name: String,
    /// Image-name wildcard of the owner.
    pub app: String,
    pub cores: CoreSpec,
    /// The owner runs only on its cores; otherwise it keeps every core.
    #[serde(default = "yes")]
    pub owner_only: bool,
    #[serde(default)]
    pub strict: bool,
    #[serde(default = "yes")]
    pub tree: bool,
    #[serde(default = "default_hold")]
    pub hold_secs: u64,
}

/// What `[[pin]]`, `[[separate]]` and `[[reserve_cores]]` hold.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlacementConfig {
    pub pins: Vec<Pin>,
    pub separate: Vec<Separate>,
    pub reserve: Vec<ReserveCores>,
}

fn named(kind: &str, name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        Err(format!("{kind}: name must not be empty"))
    } else {
        Ok(())
    }
}

fn pattern(kind: &str, name: &str, pattern: &str) -> Result<(), String> {
    if pattern.trim().is_empty() {
        return Err(format!("{kind} '{name}': an app pattern is empty"));
    }
    if crate::rules::matches_every_program(pattern, &crate::rules::PROBE_NAMES) {
        return Err(format!(
            "{kind} '{name}': '{pattern}' would match every program"
        ));
    }
    Ok(())
}

const MOST_HOLD_SECS: u64 = 3600;

impl PlacementConfig {
    pub(crate) fn validate(&self) -> Result<(), String> {
        for p in &self.pins {
            named("pin", &p.name)?;
            if p.matcher.name.is_none() && p.matcher.path.is_none() {
                return Err(format!(
                    "pin '{}': match needs `name` and/or `path`",
                    p.name
                ));
            }
            if let Some(name) = &p.matcher.name {
                pattern("pin", &p.name, name)?;
            }
        }
        for s in &self.separate {
            named("separate", &s.name)?;
            if s.apps.len() < 2 {
                return Err(format!("separate '{}': needs at least two apps", s.name));
            }
            for app in &s.apps {
                pattern("separate", &s.name, app)?;
            }
            if !s.weights.is_empty() && (s.weights.len() != s.apps.len() || s.weights.contains(&0))
            {
                return Err(format!(
                    "separate '{}': weights needs one number of 1 or more per app",
                    s.name
                ));
            }
            if s.hold_secs > MOST_HOLD_SECS {
                return Err(format!(
                    "separate '{}': hold_secs must be at most {MOST_HOLD_SECS}",
                    s.name
                ));
            }
        }
        for r in &self.reserve {
            named("reserve_cores", &r.name)?;
            pattern("reserve_cores", &r.name, &r.app)?;
            if r.hold_secs > MOST_HOLD_SECS {
                return Err(format!(
                    "reserve_cores '{}': hold_secs must be at most {MOST_HOLD_SECS}",
                    r.name
                ));
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.pins.is_empty() && self.separate.is_empty() && self.reserve.is_empty()
    }
}

/// One `[[pin]]`, `[[separate]]` or `[[reserve_cores]]` entry, for the
/// editors below.
#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    Pin(Pin),
    Separate(Separate),
    Reserve(ReserveCores),
}

/// The kind of an [`Entry`]: its table name in TOML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    Pin,
    Separate,
    Reserve,
}

impl EntryKind {
    pub fn table(self) -> &'static str {
        match self {
            Self::Pin => "pin",
            Self::Separate => "separate",
            Self::Reserve => "reserve_cores",
        }
    }
}

impl Entry {
    pub fn kind(&self) -> EntryKind {
        match self {
            Self::Pin(_) => EntryKind::Pin,
            Self::Separate(_) => EntryKind::Separate,
            Self::Reserve(_) => EntryKind::Reserve,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::Pin(p) => &p.name,
            Self::Separate(s) => &s.name,
            Self::Reserve(r) => &r.name,
        }
    }

    fn table(&self) -> toml_edit::Table {
        fn cores(spec: &CoreSpec) -> toml_edit::Item {
            match spec {
                CoreSpec::Count(n) => toml_edit::value(i64::from(*n)),
                other => toml_edit::value(other.to_string()),
            }
        }
        fn flags(t: &mut toml_edit::Table, strict: bool, tree: bool) {
            if strict {
                t.insert("strict", toml_edit::value(true));
            }
            if !tree {
                t.insert("tree", toml_edit::value(false));
            }
        }
        let mut t = toml_edit::Table::new();
        match self {
            Self::Pin(p) => {
                t.insert("name", toml_edit::value(p.name.clone()));
                let mut m = toml_edit::InlineTable::new();
                for (key, value) in [
                    ("name", &p.matcher.name),
                    ("path", &p.matcher.path),
                    ("cmdline", &p.matcher.cmdline),
                ] {
                    if let Some(v) = value {
                        m.insert(key, v.as_str().into());
                    }
                }
                t.insert("match", toml_edit::value(m));
                t.insert("cores", cores(&p.cores));
                flags(&mut t, p.strict, p.tree);
            }
            Self::Separate(s) => {
                t.insert("name", toml_edit::value(s.name.clone()));
                let apps: toml_edit::Array = s.apps.iter().map(String::as_str).collect();
                t.insert("apps", toml_edit::value(apps));
                if !s.weights.is_empty() {
                    let weights: toml_edit::Array =
                        s.weights.iter().map(|&w| i64::from(w)).collect();
                    t.insert("weights", toml_edit::value(weights));
                }
                flags(&mut t, s.strict, s.tree);
                if s.hold_secs != DEFAULT_HOLD_SECS {
                    t.insert("hold_secs", toml_edit::value(s.hold_secs as i64));
                }
            }
            Self::Reserve(r) => {
                t.insert("name", toml_edit::value(r.name.clone()));
                t.insert("app", toml_edit::value(r.app.clone()));
                t.insert("cores", cores(&r.cores));
                if !r.owner_only {
                    t.insert("owner_only", toml_edit::value(false));
                }
                flags(&mut t, r.strict, r.tree);
                if r.hold_secs != DEFAULT_HOLD_SECS {
                    t.insert("hold_secs", toml_edit::value(r.hold_secs as i64));
                }
            }
        }
        t
    }
}

/// Saves `entry` into a configuration text: in place of the entry of its
/// kind named `replacing` (or its own name), else at the end. Comments and
/// formatting elsewhere are kept; the result must load.
pub fn set_entry(text: &str, entry: &Entry, replacing: Option<&str>) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let key = entry.kind().table();
    let tables = doc
        .entry(key)
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(Default::default()))
        .as_array_of_tables_mut()
        .ok_or_else(|| format!("`{key}` must be an array of tables ([[{key}]])"))?;
    let name_of = |t: &toml_edit::Table| {
        t.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned()
    };
    let old = replacing.unwrap_or(entry.name());
    if replacing.is_some_and(|r| r != entry.name())
        && tables.iter().any(|t| name_of(t) == entry.name())
    {
        return Err(format!("another {key} entry is named '{}'", entry.name()));
    }
    let found = tables.iter().position(|t| name_of(t) == old);
    match found {
        Some(i) => *tables.get_mut(i).expect("found above") = entry.table(),
        None => tables.push(entry.table()),
    }
    let text = doc.to_string();
    crate::Config::from_toml(&text).map_err(|e| e.to_string())?;
    Ok(text)
}

/// Removes the entry of `kind` named `name`; unchanged text if there is none.
pub fn remove_entry(text: &str, kind: EntryKind, name: &str) -> Result<String, String> {
    let mut doc: toml_edit::DocumentMut = text.parse().map_err(|e| format!("{e}"))?;
    let key = kind.table();
    if doc.get(key).is_some_and(|i| !i.is_array_of_tables()) {
        return Err(format!("`{key}` must be an array of tables ([[{key}]])"));
    }
    if let Some(tables) = doc.get_mut(key).and_then(|i| i.as_array_of_tables_mut()) {
        tables.retain(|t| t.get("name").and_then(|v| v.as_str()) != Some(name));
        if tables.is_empty() {
            doc.remove(key);
        }
    }
    Ok(doc.to_string())
}

/// Why a process runs on fewer than all processors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    /// Moved off the rescue core while it is engaged.
    Rescue,
    /// Runs on the cores reserved for it by this entry.
    Reserved(String),
    /// Kept off the cores this entry reserves for another app.
    KeptOff(String),
    /// Given its part of this separation group.
    Separated(String),
    /// Pinned by this entry.
    Pinned(String),
}

impl fmt::Display for Why {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rescue => f.write_str("rescue core"),
            Self::Reserved(e) => write!(f, "reserve cores '{e}'"),
            Self::KeptOff(e) => write!(f, "kept off the cores reserved by '{e}'"),
            Self::Separated(e) => write!(f, "keep apart '{e}'"),
            Self::Pinned(e) => write!(f, "keep on cores '{e}'"),
        }
    }
}

/// Where one process runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// CPU Set ids, ascending.
    pub cpus: Vec<u32>,
    /// Hard affinity instead of CPU Sets.
    pub strict: bool,
    /// In the order they were applied.
    pub why: Vec<Why>,
}

/// A separation group's split while two or more of its apps run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Split {
    pub entry: String,
    /// The app pattern of each running member and its CPU Set ids.
    pub parts: Vec<(String, Vec<u32>)>,
}

/// Cores reserved for a running owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub entry: String,
    pub cpus: Vec<u32>,
}

/// Where every process the entries reach should run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    /// Processes that run on fewer than all processors. A running process
    /// that is not here may use every processor.
    pub placements: HashMap<Key, Placement>,
    pub splits: Vec<Split>,
    pub reservations: Vec<Reservation>,
    /// Entries that could not be met, and why.
    pub notes: Vec<String>,
}

/// What a plan is made from besides the entries.
pub struct Inputs<'a> {
    pub now: Instant,
    pub topology: &'a Topology,
    pub processes: &'a [ProcessInfo],
    /// Processes that chose their own processors: left alone and not
    /// counted in a split, since they would not use the cores given.
    pub own_choice: &'a HashSet<Key>,
    /// The rescue core's CPU Set ids while it is engaged.
    pub rescue: Option<&'a [u32]>,
    /// Never placed: protected processes and RustWTM itself.
    pub untouchable: &'a dyn Fn(&ProcessInfo) -> bool,
    /// Never kept off cores for others (the rescue core's exemptions);
    /// they may still be pinned or separated by name.
    pub keeps_all: &'a dyn Fn(&ProcessInfo) -> bool,
}

/// When an app was first and last seen running, for `hold_secs`.
#[derive(Debug, Clone, Copy)]
struct Seen {
    since: Instant,
    last: Instant,
}

/// Makes plans; remembers when apps started and stopped running so splits
/// and reservations hold steady.
#[derive(Debug, Default)]
pub struct Placer {
    config: PlacementConfig,
    /// (separation entry, member) and (usize::MAX - reserve entry, 0).
    seen: HashMap<(usize, usize), Seen>,
}

/// A physical core: its CPU Set ids, fastest cores first.
struct Core {
    class: u8,
    group: u16,
    core: u8,
    ids: Vec<u32>,
}

/// The processors a plan hands out.
struct Machine<'a> {
    topology: &'a Topology,
    /// Usable CPU Set ids.
    usable: BTreeSet<u32>,
    /// Physical cores of the usable processors, fastest first.
    cores: Vec<Core>,
}

impl<'a> Machine<'a> {
    fn new(topology: &'a Topology) -> Self {
        let usable: BTreeSet<u32> = topology
            .cpus
            .iter()
            .filter(|c| !c.reserved_elsewhere)
            .map(|c| c.id)
            .collect();
        let mut by_core: HashMap<(u16, u8), Core> = HashMap::new();
        for c in topology.cpus.iter().filter(|c| !c.reserved_elsewhere) {
            by_core
                .entry((c.group, c.core))
                .or_insert_with(|| Core {
                    class: c.efficiency_class,
                    group: c.group,
                    core: c.core,
                    ids: Vec::new(),
                })
                .ids
                .push(c.id);
        }
        let mut cores: Vec<Core> = by_core.into_values().collect();
        cores.sort_by_key(|c| (std::cmp::Reverse(c.class), c.group, c.core));
        for c in &mut cores {
            c.ids.sort_unstable();
        }
        Self {
            topology,
            usable,
            cores,
        }
    }

    /// The processors of `spec` within `pool`; empty when there are none.
    fn resolve(&self, spec: &CoreSpec, pool: &BTreeSet<u32>) -> BTreeSet<u32> {
        match spec {
            CoreSpec::Count(n) => self
                .fastest(usize::from(*n), pool)
                .into_iter()
                .flatten()
                .collect(),
            CoreSpec::Tier(tier) => self
                .topology
                .cpus
                .iter()
                .filter(|c| {
                    pool.contains(&c.id) && self.topology.tier(c.efficiency_class) == tier.label()
                })
                .map(|c| c.id)
                .collect(),
            CoreSpec::List(numbers) => {
                // Numbered across groups in order, reserved processors included.
                let mut all: Vec<_> = self.topology.cpus.iter().collect();
                all.sort_by_key(|c| (c.group, c.index));
                numbers
                    .iter()
                    .filter_map(|&n| all.get(usize::from(n)))
                    .map(|c| c.id)
                    .filter(|id| pool.contains(id))
                    .collect()
            }
        }
    }

    /// Up to `n` physical cores with processors in `pool`, fastest first:
    /// each core's processors within `pool`.
    fn fastest(&self, n: usize, pool: &BTreeSet<u32>) -> Vec<Vec<u32>> {
        self.cores
            .iter()
            .map(|c| {
                c.ids
                    .iter()
                    .copied()
                    .filter(|id| pool.contains(id))
                    .collect::<Vec<_>>()
            })
            .filter(|ids| !ids.is_empty())
            .take(n)
            .collect()
    }
}

/// A separation group while two or more of its apps run: each process's
/// member index, and each member's processors.
struct ActiveSplit {
    members: Vec<Option<usize>>,
    parts: Vec<BTreeSet<u32>>,
}

/// Reserved processors of a running owner, and which processes are its.
struct ActiveReserve {
    cpus: BTreeSet<u32>,
    owners: Vec<bool>,
}

/// CPU Set ids as logical processor numbers, counted across processor
/// groups in order and with ranges joined: `0-3,8`. The numbering of
/// [`CoreSpec::List`].
pub fn processor_numbers(topology: &Topology, ids: &[u32]) -> String {
    let mut all: Vec<_> = topology.cpus.iter().collect();
    all.sort_by_key(|c| (c.group, c.index));
    let list = all
        .iter()
        .enumerate()
        .filter(|(_, c)| ids.contains(&c.id))
        .filter_map(|(i, _)| u16::try_from(i).ok())
        .collect();
    CoreSpec::List(list).to_string()
}

/// Splits `total` cores by `weights` (`total` is at least their count):
/// each its proportional share, at least one; what rounding leaves goes to
/// the largest fractions, earlier members first on a tie.
fn shares(total: usize, weights: &[u32]) -> Vec<usize> {
    let sum: u64 = weights.iter().map(|&w| u64::from(w)).sum();
    let total64 = total as u64;
    let mut out: Vec<usize> = weights
        .iter()
        .map(|&w| ((total64 * u64::from(w) / sum) as usize).max(1))
        .collect();
    // Raising small shares to one can overshoot: take back from the largest.
    while out.iter().sum::<usize>() > total {
        let largest = (0..out.len()).max_by_key(|&i| (out[i], std::cmp::Reverse(i)));
        match largest {
            Some(i) if out[i] > 1 => out[i] -= 1,
            _ => break,
        }
    }
    let mut left = total.saturating_sub(out.iter().sum::<usize>());
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse((total64 * u64::from(weights[i])) % sum));
    for i in order {
        if left == 0 {
            break;
        }
        out[i] += 1;
        left -= 1;
    }
    out
}

/// The app pattern among `patterns` a process belongs to: its own name, or
/// with `tree` the nearest ancestor's.
fn member(
    p: &ProcessInfo,
    patterns: &[String],
    tree: bool,
    by_pid: &HashMap<u32, &ProcessInfo>,
) -> Option<usize> {
    let mut at = p;
    for _ in 0..MOST_ANCESTORS {
        if let Some(i) = patterns.iter().position(|pat| glob_match(pat, &at.name)) {
            return Some(i);
        }
        if !tree {
            return None;
        }
        at = live_parent(at, by_pid)?;
    }
    None
}

impl Placer {
    pub fn new(config: PlacementConfig) -> Self {
        Self {
            config,
            seen: HashMap::new(),
        }
    }

    pub fn config(&self) -> &PlacementConfig {
        &self.config
    }

    /// Replaces the entries; what was seen running starts over.
    pub fn set_config(&mut self, config: PlacementConfig) {
        if config != self.config {
            self.config = config;
            self.seen.clear();
        }
    }

    /// Whether the app keyed `key` has run for `hold`, and not been gone for
    /// as long; records it as seen when `running`.
    fn held(&mut self, key: (usize, usize), running: bool, now: Instant, hold: Duration) -> bool {
        if running {
            let seen = self.seen.entry(key).or_insert(Seen {
                since: now,
                last: now,
            });
            seen.last = now;
            return now.duration_since(seen.since) >= hold;
        }
        match self.seen.get(&key) {
            Some(seen) if now.duration_since(seen.last) < hold => {
                now.duration_since(seen.since) >= hold
            }
            Some(_) => {
                self.seen.remove(&key);
                false
            }
            None => false,
        }
    }

    /// Where every process should run now. `lookup` resolves an image path
    /// or command line for pins that match on them.
    pub fn plan(
        &mut self,
        inputs: &Inputs<'_>,
        lookup: &mut dyn FnMut(&ProcessInfo, Detail) -> Option<String>,
    ) -> Plan {
        let mut plan = Plan::default();
        if self.config.is_empty() && inputs.rescue.is_none() {
            return plan;
        }
        let machine = Machine::new(inputs.topology);
        if machine.cores.len() < 2 {
            if !self.config.is_empty() {
                plan.notes
                    .push("fewer than two cores: nothing to place".to_owned());
            }
            return plan;
        }
        let procs = inputs.processes;
        let by_pid: HashMap<u32, &ProcessInfo> = procs.iter().map(|p| (p.pid, p)).collect();
        let index: HashMap<u32, usize> =
            procs.iter().enumerate().map(|(i, p)| (p.pid, i)).collect();
        let placeable: Vec<bool> = procs
            .iter()
            .map(|p| {
                !(inputs.untouchable)(p) && !inputs.own_choice.contains(&(p.pid, p.create_time))
            })
            .collect();
        let rescue: BTreeSet<u32> = inputs.rescue.unwrap_or_default().iter().copied().collect();
        let config = self.config.clone();

        // Reserved cores, in entry order: an earlier entry wins a core.
        let mut reserved_all = BTreeSet::new();
        let mut reserves: Vec<Option<ActiveReserve>> = Vec::new();
        for (e, r) in config.reserve.iter().enumerate() {
            let patterns = std::slice::from_ref(&r.app);
            let owners: Vec<bool> = procs
                .iter()
                .zip(&placeable)
                .map(|(p, &ok)| ok && member(p, patterns, r.tree, &by_pid).is_some())
                .collect();
            let running = owners.iter().any(|&o| o);
            let hold = Duration::from_secs(r.hold_secs);
            if !self.held((usize::MAX - e, 0), running, inputs.now, hold) {
                reserves.push(None);
                continue;
            }
            let pool: BTreeSet<u32> = machine
                .usable
                .difference(&rescue)
                .filter(|id| !reserved_all.contains(*id))
                .copied()
                .collect();
            let mut cpus = machine.resolve(&r.cores, &pool);
            // Everything else keeps at least one core.
            let left = pool.difference(&cpus).count();
            if left == 0 {
                let mut cores = machine.fastest(usize::MAX, &cpus);
                if let Some(last) = cores.pop() {
                    for id in last {
                        cpus.remove(&id);
                    }
                }
                plan.notes.push(format!(
                    "reserve cores '{}': one core left out so everything else keeps one",
                    r.name
                ));
            }
            if cpus.is_empty() {
                plan.notes.push(format!(
                    "reserve cores '{}': no core left to reserve",
                    r.name
                ));
                reserves.push(None);
                continue;
            }
            reserved_all.extend(cpus.iter().copied());
            plan.reservations.push(Reservation {
                entry: r.name.clone(),
                cpus: cpus.iter().copied().collect(),
            });
            reserves.push(Some(ActiveReserve { cpus, owners }));
        }

        // Separation groups: the running members split what is left.
        let mut splits: Vec<Option<ActiveSplit>> = Vec::new();
        for (e, s) in config.separate.iter().enumerate() {
            let members: Vec<Option<usize>> = procs
                .iter()
                .zip(&placeable)
                .map(|(p, &ok)| ok.then(|| member(p, &s.apps, s.tree, &by_pid)).flatten())
                .collect();
            let hold = Duration::from_secs(s.hold_secs);
            let active: Vec<usize> = (0..s.apps.len())
                .filter(|&m| {
                    let running = members.contains(&Some(m));
                    self.held((e, m), running, inputs.now, hold)
                })
                .collect();
            if active.len() < 2 {
                splits.push(None);
                continue;
            }
            let pool: BTreeSet<u32> = machine
                .usable
                .difference(&rescue)
                .filter(|id| !reserved_all.contains(*id))
                .copied()
                .collect();
            let cores = machine.fastest(usize::MAX, &pool);
            if cores.len() < active.len() {
                plan.notes.push(format!(
                    "keep apart '{}': {} apps running but only {} cores to split",
                    s.name,
                    active.len(),
                    cores.len()
                ));
                splits.push(None);
                continue;
            }
            let weights: Vec<u32> = active
                .iter()
                .map(|&m| s.weights.get(m).copied().unwrap_or(1))
                .collect();
            let counts = shares(cores.len(), &weights);
            let mut parts = vec![BTreeSet::new(); s.apps.len()];
            let mut next = cores.into_iter();
            let mut split = Split {
                entry: s.name.clone(),
                parts: Vec::new(),
            };
            for (&m, &n) in active.iter().zip(&counts) {
                parts[m] = next.by_ref().take(n).flatten().collect();
                split
                    .parts
                    .push((s.apps[m].clone(), parts[m].iter().copied().collect()));
            }
            plan.splits.push(split);
            splits.push(Some(ActiveSplit { members, parts }));
        }

        // Pins: the first entry matching a process (or, with `tree`, its
        // nearest matching ancestor) applies.
        let mut details = Details::new(lookup, procs.len());
        let pin_cpus: Vec<BTreeSet<u32>> = config
            .pins
            .iter()
            .map(|pin| machine.resolve(&pin.cores, &machine.usable))
            .collect();
        for (pin, cpus) in config.pins.iter().zip(&pin_cpus) {
            if cpus.is_empty() {
                plan.notes.push(format!(
                    "keep on cores '{}': cores {} name none of this machine's usable processors",
                    pin.name, pin.cores
                ));
            }
        }
        let mut pinned: Vec<Option<usize>> = vec![None; procs.len()];
        for (i, p) in procs.iter().enumerate() {
            if !placeable[i] {
                continue;
            }
            'pins: for (k, pin) in config.pins.iter().enumerate() {
                if pin_cpus[k].is_empty() {
                    continue;
                }
                let mut at = p;
                for _ in 0..MOST_ANCESTORS {
                    let j = index[&at.pid];
                    if pin.matcher.name_matches(at)
                        && pin.matcher.details_match(j, at, &mut details)
                    {
                        pinned[i] = Some(k);
                        break 'pins;
                    }
                    if !pin.tree {
                        break;
                    }
                    match live_parent(at, &by_pid) {
                        Some(parent) => at = parent,
                        None => break,
                    }
                }
            }
        }

        // Each process: the constraints in priority order; one that would
        // leave it no processor is skipped.
        let mut unmet: BTreeSet<String> = BTreeSet::new();
        for (i, p) in procs.iter().enumerate() {
            if !placeable[i] {
                continue;
            }
            let keeps_all = (inputs.keeps_all)(p);
            let mut allowed = machine.usable.clone();
            let mut why = Vec::new();
            let mut strict = false;
            let mut narrow = |allowed: &mut BTreeSet<u32>,
                              next: BTreeSet<u32>,
                              reason: Why,
                              entry_strict: bool,
                              why: &mut Vec<Why>,
                              strict: &mut bool| {
                if next == *allowed {
                    return;
                }
                if next.is_empty() {
                    unmet.insert(reason.to_string());
                    return;
                }
                *allowed = next;
                why.push(reason);
                *strict |= entry_strict;
            };
            if !rescue.is_empty() && !keeps_all {
                let next = allowed.difference(&rescue).copied().collect();
                narrow(
                    &mut allowed,
                    next,
                    Why::Rescue,
                    false,
                    &mut why,
                    &mut strict,
                );
            }
            for (r, reserve) in config.reserve.iter().zip(&reserves) {
                let Some(ActiveReserve { cpus, owners }) = reserve else {
                    continue;
                };
                if owners[i] {
                    if r.owner_only {
                        let next = allowed.intersection(cpus).copied().collect();
                        let reason = Why::Reserved(r.name.clone());
                        narrow(&mut allowed, next, reason, r.strict, &mut why, &mut strict);
                    }
                } else if !keeps_all {
                    let next = allowed.difference(cpus).copied().collect();
                    let reason = Why::KeptOff(r.name.clone());
                    narrow(&mut allowed, next, reason, false, &mut why, &mut strict);
                }
            }
            for (s, split) in config.separate.iter().zip(&splits) {
                let Some(ActiveSplit { members, parts }) = split else {
                    continue;
                };
                if let Some(m) = members[i]
                    && !parts[m].is_empty()
                {
                    let next = allowed.intersection(&parts[m]).copied().collect();
                    let reason = Why::Separated(s.name.clone());
                    narrow(&mut allowed, next, reason, s.strict, &mut why, &mut strict);
                }
            }
            if let Some(k) = pinned[i] {
                let pin = &config.pins[k];
                let next = allowed.intersection(&pin_cpus[k]).copied().collect();
                let reason = Why::Pinned(pin.name.clone());
                narrow(
                    &mut allowed,
                    next,
                    reason,
                    pin.strict,
                    &mut why,
                    &mut strict,
                );
            }
            if allowed != machine.usable {
                plan.placements.insert(
                    (p.pid, p.create_time),
                    Placement {
                        cpus: allowed.into_iter().collect(),
                        strict,
                        why,
                    },
                );
            }
        }
        for reason in unmet {
            plan.notes.push(format!(
                "{reason} would leave a process no processor; skipped for it"
            ));
        }
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    use wtm_core::cpu::LogicalCpu;

    /// Four P-cores with two threads each (ids 256-263, processors 0-7),
    /// then four E-cores (ids 264-267, processors 8-11).
    fn hybrid() -> Topology {
        let cpu = |i: u8, core: u8, class: u8| LogicalCpu {
            id: 256 + u32::from(i),
            group: 0,
            index: i,
            core,
            efficiency_class: class,
            parked: false,
            reserved_elsewhere: false,
        };
        let mut cpus: Vec<_> = (0..8).map(|i| cpu(i, i / 2, 1)).collect();
        cpus.extend((8..12).map(|i| cpu(i, i - 4, 0)));
        Topology { cpus }
    }

    fn ids(range: std::ops::Range<u32>) -> Vec<u32> {
        range.map(|i| 256 + i).collect()
    }

    fn proc(pid: u32, parent: u32, name: &str) -> ProcessInfo {
        ProcessInfo {
            io: Default::default(),
            cycle_percent: 0.0,
            pid,
            parent_pid: parent,
            name: name.into(),
            cpu_percent: 0.0,
            cpu_time: Duration::ZERO,
            working_set: 0,
            private_working_set: 0,
            private_bytes: 0,
            thread_count: 1,
            handle_count: 1,
            session_id: 1,
            base_priority: 8,
            gpu_percent: 0.0,
            gpu_memory: 0,
            io_read_rate: 0.0,
            io_write_rate: 0.0,
            net_rx_rate: 0.0,
            net_tx_rate: 0.0,
            suspended: false,
            create_time: i64::from(pid),
            memory: Default::default(),
            gpu_detail: Default::default(),
        }
    }

    fn config(toml: &str) -> PlacementConfig {
        Config::from_toml(toml).unwrap().placement()
    }

    struct World {
        topology: Topology,
        procs: Vec<ProcessInfo>,
        own: HashSet<Key>,
        rescue: Option<Vec<u32>>,
        untouchable: Vec<&'static str>,
        keeps_all: Vec<&'static str>,
    }

    impl World {
        fn new(procs: Vec<ProcessInfo>) -> Self {
            Self {
                topology: hybrid(),
                procs,
                own: HashSet::new(),
                rescue: None,
                untouchable: vec!["csrss.exe"],
                keeps_all: vec!["exempt.exe"],
            }
        }

        fn plan(&self, placer: &mut Placer, now: Instant) -> Plan {
            let untouchable = |p: &ProcessInfo| self.untouchable.contains(&p.name.as_str());
            let keeps_all = |p: &ProcessInfo| self.keeps_all.contains(&p.name.as_str());
            let inputs = Inputs {
                now,
                topology: &self.topology,
                processes: &self.procs,
                own_choice: &self.own,
                rescue: self.rescue.as_deref(),
                untouchable: &untouchable,
                keeps_all: &keeps_all,
            };
            placer.plan(&inputs, &mut |_, _| None)
        }
    }

    fn cpus_of(plan: &Plan, pid: u32) -> Option<Vec<u32>> {
        plan.placements
            .get(&(pid, i64::from(pid)))
            .map(|p| p.cpus.clone())
    }

    #[test]
    fn cores_parse_as_counts_tiers_and_lists() {
        let parse = |s: &str| s.parse::<CoreSpec>();
        assert_eq!(parse("efficient"), Ok(CoreSpec::Tier(Tier::Efficient)));
        assert_eq!(parse(" P "), Ok(CoreSpec::Tier(Tier::Performance)));
        assert_eq!(parse("0-3, 8"), Ok(CoreSpec::List(vec![0, 1, 2, 3, 8])));
        assert_eq!(parse("8,0-3,2"), Ok(CoreSpec::List(vec![0, 1, 2, 3, 8])));
        for bad in ["", "3-1", "x", "1-", "-1", "0-5000"] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(
            CoreSpec::List(vec![0, 1, 2, 3, 8, 10, 11]).to_string(),
            "0-3,8,10-11"
        );
        assert_eq!(CoreSpec::Count(2).to_string(), "2");
        assert_eq!(CoreSpec::Tier(Tier::LowPower).to_string(), "low_power");

        let c = config(
            r#"
            [[pin]]
            name = "Compilers"
            match = { name = "cl.exe" }
            cores = "efficient"

            [[pin]]
            name = "Two"
            match = { name = "two.exe" }
            cores = 2
            strict = true

            [[separate]]
            name = "Game and stream"
            apps = ["game.exe", "obs64.exe"]
            weights = [3, 1]

            [[reserve_cores]]
            name = "Audio"
            app = "reaper.exe"
            cores = "0-3"
            "#,
        );
        assert_eq!(c.pins[1].cores, CoreSpec::Count(2));
        assert!(c.pins[1].strict && c.pins[0].tree && !c.pins[0].strict);
        assert_eq!(c.separate[0].hold_secs, DEFAULT_HOLD_SECS);
        assert!(c.reserve[0].owner_only && c.reserve[0].tree);
    }

    #[test]
    fn entries_are_saved_replaced_and_removed_keeping_the_rest() {
        let text = "# my settings\ndry_run = false\n";
        let c = config(
            r#"
            [[pin]]
            name = "Builds"
            match = { name = "msbuild.exe", cmdline = "-m" }
            cores = "efficient"
            strict = true
            tree = false

            [[separate]]
            name = "Game and stream"
            apps = ["game.exe", "obs64.exe"]
            weights = [3, 1]
            hold_secs = 10

            [[reserve_cores]]
            name = "Audio"
            app = "reaper.exe"
            cores = 2
            owner_only = false
            "#,
        );
        let entries = [
            Entry::Pin(c.pins[0].clone()),
            Entry::Separate(c.separate[0].clone()),
            Entry::Reserve(c.reserve[0].clone()),
        ];
        let mut saved = text.to_owned();
        for e in &entries {
            saved = set_entry(&saved, e, None).unwrap();
        }
        assert!(saved.starts_with("# my settings"), "{saved}");
        assert_eq!(Config::from_toml(&saved).unwrap().placement(), c, "{saved}");

        // Replacing under a new name, and refusing a name already taken.
        let mut renamed = c.pins[0].clone();
        renamed.name = "MSBuild".into();
        renamed.cores = CoreSpec::List(vec![0, 1, 8]);
        let saved = set_entry(&saved, &Entry::Pin(renamed), Some("Builds")).unwrap();
        let pins = Config::from_toml(&saved).unwrap().pins;
        assert_eq!((pins.len(), pins[0].name.as_str()), (1, "MSBuild"));
        assert!(saved.contains("cores = \"0-1,8\""), "{saved}");
        let mut clash = c.separate[0].clone();
        clash.name = "Other".into();
        let with_other = set_entry(&saved, &Entry::Separate(clash.clone()), None).unwrap();
        clash.name = "Game and stream".into();
        assert!(set_entry(&with_other, &Entry::Separate(clash), Some("Other")).is_err());

        // An entry the configuration would refuse is not saved.
        let mut bad = c.separate[0].clone();
        bad.apps.truncate(1);
        bad.weights.clear();
        assert!(set_entry(&saved, &Entry::Separate(bad), None).is_err());

        let removed = remove_entry(&saved, EntryKind::Reserve, "Audio").unwrap();
        assert!(
            Config::from_toml(&removed)
                .unwrap()
                .reserve_cores
                .is_empty()
        );
        assert!(!removed.contains("reserve_cores"), "{removed}");
        assert_eq!(
            remove_entry(&removed, EntryKind::Reserve, "Audio").unwrap(),
            removed
        );
    }

    #[test]
    fn invalid_entries_are_refused() {
        for (toml, why) in [
            (
                "[[separate]]\nname = \"x\"\napps = [\"a.exe\"]",
                "at least two apps",
            ),
            (
                "[[separate]]\nname = \"x\"\napps = [\"a.exe\", \"b.exe\"]\nweights = [1]",
                "weights",
            ),
            (
                "[[separate]]\nname = \"x\"\napps = [\"*\", \"b.exe\"]",
                "every program",
            ),
            (
                "[[reserve_cores]]\nname = \"x\"\napp = \"a.exe\"\ncores = 0",
                "1 or more",
            ),
            (
                "[[pin]]\nname = \"x\"\nmatch = { cmdline = \"--x\" }\ncores = 1",
                "match needs",
            ),
            (
                "[[pin]]\nname = \" \"\nmatch = { name = \"a.exe\" }\ncores = 1",
                "name must not be empty",
            ),
        ] {
            let err = Config::from_toml(toml).unwrap_err().to_string();
            assert!(err.contains(why), "{toml}: {err}");
        }
    }

    #[test]
    fn shares_follow_the_weights_and_give_everyone_one() {
        assert_eq!(shares(8, &[1, 1]), [4, 4]);
        assert_eq!(shares(8, &[3, 1]), [6, 2]);
        assert_eq!(shares(5, &[1, 1, 1]), [2, 2, 1]);
        assert_eq!(shares(3, &[100, 1]), [2, 1]);
        assert_eq!(shares(2, &[1, 1]), [1, 1]);
        assert_eq!(shares(8, &[1, 3]), [2, 6]);
        assert_eq!(shares(3, &[1, 1, 100]), [1, 1, 1]);
        assert_eq!(shares(10, &[1, 1, 1]), [4, 3, 3]);
    }

    #[test]
    fn a_pin_places_its_app_and_with_tree_what_it_starts() {
        let mut placer = Placer::new(config(
            r#"
            [[pin]]
            name = "Builds"
            match = { name = "msbuild.exe" }
            cores = "efficient"

            [[pin]]
            name = "Fast"
            match = { name = "fast.exe" }
            cores = "0-1"
            tree = false
            "#,
        ));
        let world = World::new(vec![
            proc(10, 1, "msbuild.exe"),
            proc(11, 10, "cl.exe"),
            proc(20, 1, "fast.exe"),
            proc(21, 20, "helper.exe"),
            proc(30, 1, "other.exe"),
            proc(40, 1, "csrss.exe"),
        ]);
        let plan = world.plan(&mut placer, Instant::now());
        assert_eq!(cpus_of(&plan, 10), Some(ids(8..12)));
        assert_eq!(cpus_of(&plan, 11), Some(ids(8..12)), "started by msbuild");
        assert_eq!(cpus_of(&plan, 20), Some(ids(0..2)));
        assert_eq!(cpus_of(&plan, 21), None, "tree = false");
        assert_eq!(cpus_of(&plan, 30), None);
        let p = &plan.placements[&(10, 10)];
        assert_eq!(p.why, [Why::Pinned("Builds".into())]);
        assert!(!p.strict);
        assert!(plan.notes.is_empty(), "{:?}", plan.notes);
    }

    #[test]
    fn a_pin_naming_no_processor_of_this_machine_is_noted() {
        let mut placer = Placer::new(config(
            "[[pin]]\nname = \"Far\"\nmatch = { name = \"a.exe\" }\ncores = \"40-41\"",
        ));
        let plan = World::new(vec![proc(10, 1, "a.exe")]).plan(&mut placer, Instant::now());
        assert!(plan.placements.is_empty());
        assert!(
            plan.notes[0].contains("keep on cores 'Far'"),
            "{:?}",
            plan.notes
        );
    }

    const SEPARATE: &str = r#"
        [[separate]]
        name = "Game and stream"
        apps = ["game.exe", "obs64.exe"]
        hold_secs = 0
    "#;

    #[test]
    fn running_members_split_the_cores_fastest_first() {
        let mut placer = Placer::new(config(SEPARATE));
        let mut world = World::new(vec![
            proc(10, 1, "game.exe"),
            proc(11, 10, "gamehelper.exe"),
            proc(20, 1, "obs64.exe"),
            proc(30, 1, "other.exe"),
        ]);
        let plan = world.plan(&mut placer, Instant::now());
        // Eight physical cores, four each: the game gets the P-cores.
        assert_eq!(cpus_of(&plan, 10), Some(ids(0..8)));
        assert_eq!(cpus_of(&plan, 11), Some(ids(0..8)), "the game's helper");
        assert_eq!(cpus_of(&plan, 20), Some(ids(8..12)));
        assert_eq!(cpus_of(&plan, 30), None);
        assert_eq!(
            plan.splits,
            [Split {
                entry: "Game and stream".into(),
                parts: vec![
                    ("game.exe".into(), ids(0..8)),
                    ("obs64.exe".into(), ids(8..12))
                ],
            }]
        );

        // The same inputs give the same plan.
        assert_eq!(world.plan(&mut placer, Instant::now()), plan);

        // One left running: it gets every core back.
        world.procs.retain(|p| p.name != "obs64.exe");
        let plan = world.plan(&mut placer, Instant::now());
        assert!(plan.placements.is_empty() && plan.splits.is_empty());
    }

    #[test]
    fn weights_size_the_parts() {
        let mut placer = Placer::new(config(
            &SEPARATE.replace("hold_secs = 0", "hold_secs = 0\nweights = [1, 3]"),
        ));
        let world = World::new(vec![proc(10, 1, "game.exe"), proc(20, 1, "obs64.exe")]);
        let plan = world.plan(&mut placer, Instant::now());
        // Two cores for the game, six for the stream, fastest first.
        assert_eq!(cpus_of(&plan, 10), Some(ids(0..4)));
        assert_eq!(cpus_of(&plan, 20), Some(ids(4..12)));
    }

    #[test]
    fn a_split_waits_for_the_hold_and_outlasts_a_short_exit() {
        let mut placer = Placer::new(config(&SEPARATE.replace("hold_secs = 0", "hold_secs = 5")));
        let mut world = World::new(vec![proc(10, 1, "game.exe"), proc(20, 1, "obs64.exe")]);
        let t0 = Instant::now();
        let at = |s: u64| t0 + Duration::from_secs(s);
        assert!(
            world.plan(&mut placer, at(0)).splits.is_empty(),
            "just started"
        );
        assert!(world.plan(&mut placer, at(4)).splits.is_empty());
        assert_eq!(world.plan(&mut placer, at(5)).splits.len(), 1);
        // The stream exits briefly: its part is kept for the hold.
        world.procs.retain(|p| p.name != "obs64.exe");
        let plan = world.plan(&mut placer, at(7));
        assert_eq!(cpus_of(&plan, 10), Some(ids(0..8)));
        assert!(
            world.plan(&mut placer, at(10)).splits.is_empty(),
            "gone for 5 s"
        );
    }

    #[test]
    fn reserved_cores_belong_to_their_owner_while_it_runs() {
        let mut placer = Placer::new(config(
            r#"
            [[reserve_cores]]
            name = "Audio"
            app = "reaper.exe"
            cores = 2
            hold_secs = 0
            "#,
        ));
        let mut world = World::new(vec![
            proc(10, 1, "reaper.exe"),
            proc(11, 10, "plugin.exe"),
            proc(20, 1, "other.exe"),
            proc(30, 1, "exempt.exe"),
            proc(40, 1, "csrss.exe"),
        ]);
        let plan = world.plan(&mut placer, Instant::now());
        // The two fastest physical cores: processors 0-3.
        assert_eq!(cpus_of(&plan, 10), Some(ids(0..4)));
        assert_eq!(
            cpus_of(&plan, 11),
            Some(ids(0..4)),
            "the owner's plugin host"
        );
        assert_eq!(cpus_of(&plan, 20), Some(ids(4..12)));
        assert_eq!(
            plan.placements[&(20, 20)].why,
            [Why::KeptOff("Audio".into())]
        );
        assert_eq!(cpus_of(&plan, 30), None, "exempt keeps every core");
        assert_eq!(cpus_of(&plan, 40), None, "never touched");
        assert_eq!(plan.reservations[0].cpus, ids(0..4));

        world.procs.retain(|p| p.pid != 10 && p.pid != 11);
        assert!(
            world
                .plan(&mut placer, Instant::now())
                .placements
                .is_empty()
        );
    }

    #[test]
    fn a_reservation_always_leaves_a_core_for_everything_else() {
        let mut placer = Placer::new(config(
            "[[reserve_cores]]\nname = \"All\"\napp = \"a.exe\"\ncores = \"0-11\"\nhold_secs = 0",
        ));
        let world = World::new(vec![proc(10, 1, "a.exe"), proc(20, 1, "b.exe")]);
        let plan = world.plan(&mut placer, Instant::now());
        // The slowest core is left out of the reservation.
        assert_eq!(cpus_of(&plan, 10), Some(ids(0..11)));
        assert_eq!(cpus_of(&plan, 20), Some(vec![256 + 11]));
        assert!(
            plan.notes[0].contains("one core left out"),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn the_rescue_core_is_taken_out_of_every_placement() {
        let mut placer = Placer::new(config(SEPARATE));
        let mut world = World::new(vec![
            proc(10, 1, "game.exe"),
            proc(20, 1, "obs64.exe"),
            proc(30, 1, "other.exe"),
            proc(40, 1, "exempt.exe"),
        ]);
        // The last P-core, both threads.
        world.rescue = Some(ids(6..8));
        let plan = world.plan(&mut placer, Instant::now());
        // Seven cores left: four for the game, three for the stream.
        assert_eq!(cpus_of(&plan, 10), Some([ids(0..6), ids(8..9)].concat()));
        assert_eq!(cpus_of(&plan, 20), Some(ids(9..12)));
        assert_eq!(cpus_of(&plan, 30), Some([ids(0..6), ids(8..12)].concat()));
        assert_eq!(plan.placements[&(30, 30)].why, [Why::Rescue]);
        assert_eq!(cpus_of(&plan, 40), None, "exempt from the rescue core");
    }

    #[test]
    fn a_constraint_that_would_leave_no_processor_is_skipped() {
        let mut placer = Placer::new(config(&format!(
            "{SEPARATE}\n[[pin]]\nname = \"Slow\"\nmatch = {{ name = \"game.exe\" }}\ncores = \"efficient\""
        )));
        let world = World::new(vec![proc(10, 1, "game.exe"), proc(20, 1, "obs64.exe")]);
        let plan = world.plan(&mut placer, Instant::now());
        // The split (higher priority) gives the game the P-cores; the pin
        // to E-cores would leave it nothing and is skipped.
        assert_eq!(cpus_of(&plan, 10), Some(ids(0..8)));
        assert_eq!(
            plan.placements[&(10, 10)].why,
            [Why::Separated("Game and stream".into())]
        );
        assert!(
            plan.notes[0].contains("keep on cores 'Slow'"),
            "{:?}",
            plan.notes
        );
    }

    #[test]
    fn a_process_that_chose_its_own_cores_is_left_out() {
        let mut placer = Placer::new(config(SEPARATE));
        let mut world = World::new(vec![proc(10, 1, "game.exe"), proc(20, 1, "obs64.exe")]);
        world.own.insert((20, 20));
        let plan = world.plan(&mut placer, Instant::now());
        // Only the game counts as running: nothing to split.
        assert!(plan.placements.is_empty() && plan.splits.is_empty());
    }

    #[test]
    fn strict_entries_mark_their_placements() {
        let mut placer = Placer::new(config(
            "[[pin]]\nname = \"Hard\"\nmatch = { name = \"a.exe\" }\ncores = 1\nstrict = true",
        ));
        let plan = World::new(vec![proc(10, 1, "a.exe")]).plan(&mut placer, Instant::now());
        assert!(plan.placements[&(10, 10)].strict);
        assert_eq!(
            cpus_of(&plan, 10),
            Some(ids(0..2)),
            "one core, both threads"
        );
    }

    #[test]
    fn a_uniform_machine_has_no_tiers_to_pin_to() {
        let mut placer = Placer::new(config(
            "[[pin]]\nname = \"E\"\nmatch = { name = \"a.exe\" }\ncores = \"efficient\"",
        ));
        let mut world = World::new(vec![proc(10, 1, "a.exe")]);
        for c in &mut world.topology.cpus {
            c.efficiency_class = 0;
        }
        let plan = world.plan(&mut placer, Instant::now());
        assert!(plan.placements.is_empty());
        assert!(
            plan.notes[0].contains("keep on cores 'E'"),
            "{:?}",
            plan.notes
        );
    }
}
