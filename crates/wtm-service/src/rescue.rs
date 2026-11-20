//! Applies core placement: the rescue core, which keeps a core free for
//! RustWTM and Windows while the machine is overloaded, and the `[[pin]]`,
//! `[[separate]]` and `[[reserve_cores]]` entries. One plan
//! ([`wtm_policy::Placer`]) says where each process runs; this writes it
//! with CPU Sets (hard affinity for `strict` entries), gives processes every
//! core back when the plan lets them go, and records each write so that a
//! crashed service's confinements are undone when it starts again.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};
use wtm_core::control::{self, Placed, ProcessRef};
use wtm_core::cpu::{RescueSplit, Topology};
use wtm_core::{ProcessInfo, Snapshot};
use wtm_ipc::{PlacedProcess, PlacementStatus, RescueStatus};
use wtm_policy::{
    Detail, Inputs, Placement, PlacementConfig, Placer, RescueCore, RescueStep, Reservation,
    ReserveConfig, Split, Why,
};

use crate::events::EventLog;

/// Processes placed per tick, so a huge process count cannot stall the
/// engine; the rest follow on the next ticks.
const PER_TICK: usize = 500;

type Key = (u32, i64);

/// What the service gave a process, to compare and to undo.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Written {
    pub target: ProcessRef,
    /// CPU Set ids, ascending.
    pub cpus: Vec<u32>,
    /// Hard affinity: the mask written and the one to put back.
    pub affinity: Option<(u64, u64)>,
    /// Hard affinity was asked for; the masks are known once written.
    pub strict: bool,
    /// Moved off the rescue core (among other reasons, perhaps).
    pub rescue: bool,
    /// Placed for a reason besides the rescue core.
    pub entries: bool,
    /// The entries that placed it, highest priority first.
    #[serde(default)]
    pub why: Vec<String>,
}

/// How a placement is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum How {
    CpuSets,
    /// Hard affinity in one processor group.
    Affinity {
        group: u16,
        mask: u64,
    },
}

/// Writes placements; tests pass one that records instead.
pub trait CpuWriter: Send {
    /// Confines `target` to `cpus`; `ours` is what was last written to it.
    fn place(
        &mut self,
        target: &ProcessRef,
        cpus: &[u32],
        how: How,
        ours: Option<&Written>,
    ) -> Result<Placed, String>;
    /// Gives `target` back what `written` took, unless it has changed
    /// its processors itself since.
    fn release(&mut self, written: &Written) -> Result<(), String>;
}

/// The real thing.
struct System;

impl CpuWriter for System {
    fn place(
        &mut self,
        target: &ProcessRef,
        cpus: &[u32],
        how: How,
        ours: Option<&Written>,
    ) -> Result<Placed, String> {
        match how {
            How::CpuSets => control::place_cpu_sets(
                target,
                cpus,
                ours.filter(|w| !w.strict).map(|w| w.cpus.as_slice()),
            ),
            How::Affinity { group, mask } => {
                control::place_affinity(target, group, mask, ours.and_then(|w| w.affinity))
            }
        }
        .map_err(|e| e.to_string())
    }

    fn release(&mut self, written: &Written) -> Result<(), String> {
        match (written.strict, written.affinity) {
            (true, Some((ours, original))) => {
                control::release_affinity(&written.target, ours, original)
            }
            // Recorded before a write that never completed.
            (true, None) => Ok(()),
            (false, _) => control::release_cpu_sets(&written.target, &written.cpus),
        }
        .map_err(|e| e.to_string())
    }
}

/// What was last said about the plan, so each change is said once.
#[derive(Debug, Default)]
struct Reported {
    splits: Vec<Split>,
    reservations: Vec<Reservation>,
    notes: Vec<String>,
}

pub struct Rescue {
    policy: RescueCore,
    topology: Option<Topology>,
    split: Option<RescueSplit>,
    placer: Placer,
    dry_run: bool,
    events: EventLog,
    writer: Box<dyn CpuWriter>,
    written: HashMap<Key, Written>,
    /// Refused, or confined by their own choice: left alone, and why.
    own: HashMap<Key, (ProcessRef, String)>,
    /// Where writes are recorded; `None` in tests that need no record.
    record: Option<PathBuf>,
    reported: Reported,
}

impl Rescue {
    pub fn new(cfg: ReserveConfig, dry_run: bool, events: EventLog) -> Self {
        Self::with(
            cfg,
            dry_run,
            events,
            wtm_core::cpu::topology().ok(),
            Box::new(System),
        )
    }

    fn with(
        cfg: ReserveConfig,
        dry_run: bool,
        events: EventLog,
        topology: Option<Topology>,
        writer: Box<dyn CpuWriter>,
    ) -> Self {
        let split = topology.as_ref().and_then(Topology::rescue_split);
        if split.is_none() && cfg.rescue_core {
            events.warn(None, "rescue core unavailable on this processor layout");
        }
        Self {
            policy: RescueCore::new(cfg),
            topology,
            split,
            placer: Placer::default(),
            dry_run,
            events,
            writer,
            written: HashMap::new(),
            own: HashMap::new(),
            record: None,
            reported: Reported::default(),
        }
    }

    /// Gives back what a previous run recorded and did not undo (it
    /// crashed or was killed), then records this run's writes at `path`.
    pub fn recover(&mut self, path: &Path) {
        let left: Vec<Written> = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        let mut given_back = 0;
        for w in &left {
            // The creation time in `target` makes a reused PID fail.
            if self.writer.release(w).is_ok() {
                given_back += 1;
            }
        }
        if given_back > 0 {
            self.events.info(
                None,
                format!("every core given back to {given_back} apps placed before a restart"),
            );
        }
        self.record = Some(path.to_owned());
        self.save();
    }

    /// Writes the record of what is placed now.
    fn save(&self) {
        let Some(path) = &self.record else {
            return;
        };
        let written: Vec<&Written> = self.written.values().collect();
        let result = serde_json::to_vec(&written)
            .map_err(std::io::Error::other)
            .and_then(|bytes| {
                let temp = path.with_extension("json.tmp");
                std::fs::write(&temp, bytes)?;
                std::fs::rename(&temp, path)
            });
        if let Err(e) = result {
            tracing::warn!(error = %e, "placement record not written");
        }
    }

    pub fn configure(&mut self, cfg: ReserveConfig, dry_run: bool) {
        if dry_run != self.dry_run {
            if self.policy.engaged() {
                // Hand the core back under the old mode, engage afresh under the new.
                self.policy.set_config(ReserveConfig {
                    rescue_core: false,
                    ..cfg.clone()
                });
                self.policy.tick(Instant::now(), 0.0);
            }
            self.release_all("configuration changed");
        }
        self.dry_run = dry_run;
        self.policy.set_config(cfg);
        // Programs the new settings leave alone (newly exempt) get every
        // core back now, not when the load recovers.
        let cfg = self.policy.config().clone();
        let freed: Vec<Key> = self
            .written
            .iter()
            .filter(|(_, w)| w.rescue && !w.entries && !cfg.confines(w.target.pid, &w.target.name))
            .map(|(k, _)| *k)
            .collect();
        for key in freed {
            if let Some(w) = self.written.remove(&key) {
                let _ = self.writer.release(&w);
            }
        }
        self.save();
    }

    /// Replaces the `[[pin]]`, `[[separate]]` and `[[reserve_cores]]`
    /// entries; the next tick applies them.
    pub fn set_placement(&mut self, config: PlacementConfig) {
        self.placer.set_config(config);
    }

    /// The processors and where each placed process runs, for clients.
    pub fn placement_status(&self) -> Option<PlacementStatus> {
        let topology = self.topology.clone()?;
        let mut placed: Vec<PlacedProcess> = self
            .written
            .values()
            .map(|w| PlacedProcess {
                target: w.target.clone(),
                cpus: w.cpus.clone(),
                strict: w.strict,
                why: w.why.clone(),
            })
            .collect();
        placed.sort_by_key(|p| (p.target.pid, p.target.create_time));
        let mut left_alone: Vec<(ProcessRef, String)> = self.own.values().cloned().collect();
        left_alone.sort_by_key(|(t, _)| (t.pid, t.create_time));
        Some(PlacementStatus {
            topology,
            placed,
            splits: self.reported.splits.clone(),
            reservations: self.reported.reservations.clone(),
            notes: self.reported.notes.clone(),
            left_alone,
            dry_run: self.dry_run,
        })
    }

    /// What clients show while the rescue core is engaged.
    pub fn summary(&self) -> Option<RescueStatus> {
        let split = self.split.as_ref().filter(|_| self.policy.engaged())?;
        Some(RescueStatus {
            cpus: split.reserved_indices.clone(),
            moved: self.written.values().filter(|w| w.rescue).count(),
        })
    }

    /// `live` holds the processes running in `snap`.
    #[cfg(test)]
    pub fn tick(&mut self, now: Instant, snap: &Snapshot, live: &HashSet<Key>) {
        self.tick_with(now, snap, live, &mut |_, _| None);
    }

    /// Updates placement for the live processes in the snapshot.
    /// `lookup` resolves image paths and command lines for matching pins.
    pub fn tick_with(
        &mut self,
        now: Instant,
        snap: &Snapshot,
        live: &HashSet<Key>,
        lookup: &mut dyn FnMut(&ProcessInfo, Detail) -> Option<String>,
    ) {
        let Some(topology) = self.topology.clone() else {
            return;
        };
        let step = match &self.split {
            Some(split) => {
                let step = self.policy.tick(now, snap.system.cpu_percent);
                if step == Some(RescueStep::Engage) {
                    self.events.alert(
                        None,
                        format!(
                            "CPU overloaded: {} {} free for RustWTM and Windows",
                            cpu_names(split),
                            if self.dry_run {
                                "would be kept (dry run)"
                            } else {
                                "kept"
                            }
                        ),
                    );
                }
                step
            }
            None => None,
        };
        let before = self.written.len();
        self.written.retain(|k, _| live.contains(k));
        self.own.retain(|k, _| live.contains(k));
        let mut changed = self.written.len() != before;

        let engaged = self.policy.engaged() && !self.dry_run;
        let rescue: Option<Vec<u32>> = self
            .split
            .as_ref()
            .filter(|_| engaged)
            .map(|s| s.reserved.clone());
        let cfg = self.policy.config().clone();
        let untouchable = |p: &ProcessInfo| ReserveConfig::untouchable(p.pid, &p.name);
        let keeps_all = |p: &ProcessInfo| cfg.exempts(&p.name);
        let own: HashSet<Key> = self.own.keys().copied().collect();
        let inputs = Inputs {
            now,
            topology: &topology,
            processes: &snap.processes,
            own_choice: &own,
            rescue: rescue.as_deref(),
            untouchable: &untouchable,
            keeps_all: &keeps_all,
        };
        let plan = self.placer.plan(&inputs, lookup);
        self.report(&topology, &plan.splits, &plan.reservations, &plan.notes);
        if self.dry_run {
            changed |= !self.written.is_empty();
            self.release_all("dry run");
            if changed {
                self.save();
            }
            return;
        }

        // Let go of what the plan no longer places.
        let mut handed_back = 0;
        let gone: Vec<Key> = self
            .written
            .keys()
            .filter(|k| !plan.placements.contains_key(k))
            .copied()
            .collect();
        for key in gone {
            if let Some(w) = self.written.remove(&key) {
                handed_back += usize::from(w.rescue);
                let _ = self.writer.release(&w);
                changed = true;
            }
        }

        let by_key: HashMap<Key, &ProcessInfo> = snap
            .processes
            .iter()
            .map(|p| ((p.pid, p.create_time), p))
            .collect();
        let mut keys: Vec<&Key> = plan.placements.keys().collect();
        keys.sort_unstable();
        let mut writes = 0;
        for key in keys {
            let placement = &plan.placements[key];
            if self.own.contains_key(key) {
                continue;
            }
            let how = how(&topology, placement);
            let wanted = written(by_key[key], placement, how);
            // The same processors for other reasons: only the record changes.
            if let Some(w) = self.written.get_mut(key)
                && w.cpus == wanted.cpus
                && w.strict == wanted.strict
            {
                if (w.rescue, w.entries, &w.why) != (wanted.rescue, wanted.entries, &wanted.why) {
                    (w.rescue, w.entries) = (wanted.rescue, wanted.entries);
                    w.why = wanted.why;
                    changed = true;
                }
                continue;
            }
            let ours = self.written.get(key);
            if writes >= PER_TICK {
                break;
            }
            writes += 1;
            changed = true;
            // Switching between CPU Sets and affinity: undo the other first.
            let mut ours = ours.cloned();
            if ours.as_ref().is_some_and(|w| w.strict != wanted.strict) {
                if let Some(w) = self.written.remove(key) {
                    let _ = self.writer.release(&w);
                }
                ours = None;
            }
            // Recorded before it is written, so a crash in between still
            // gives it back.
            self.written.insert(
                *key,
                Written {
                    affinity: ours.as_ref().and_then(|w| w.affinity),
                    ..wanted.clone()
                },
            );
            self.save();
            match self
                .writer
                .place(&wanted.target, &wanted.cpus, how, ours.as_ref())
            {
                Ok(Placed::Done { affinity }) => {
                    self.written.insert(*key, Written { affinity, ..wanted });
                }
                Ok(Placed::OwnChoice) => {
                    self.written.remove(key);
                    let why = "chose its own processors".to_owned();
                    self.own.insert(*key, (wanted.target, why));
                }
                Err(e) => {
                    self.written.remove(key);
                    self.own.insert(*key, (wanted.target, e));
                }
            }
        }
        if changed {
            self.save();
        }
        if step == Some(RescueStep::Release)
            && let Some(split) = &self.split
        {
            self.events.info(
                None,
                format!(
                    "load recovered: {} handed back to {handed_back} apps",
                    cpu_names(split)
                ),
            );
        }
    }

    /// Says what changed in the plan: splits and reservations that start,
    /// change or end, and entries that cannot be met.
    fn report(
        &mut self,
        topology: &Topology,
        splits: &[Split],
        reservations: &[Reservation],
        notes: &[String],
    ) {
        let would = if self.dry_run { " (dry run)" } else { "" };
        for s in splits {
            if !self.reported.splits.contains(s) {
                // Which processors each got is on the Cores page.
                let apps: Vec<&str> = s.parts.iter().map(|(app, _)| app.as_str()).collect();
                self.events.info(
                    None,
                    format!(
                        "keep apart '{}'{would}: cores split between {}",
                        s.entry,
                        apps.join(", ")
                    ),
                );
            }
        }
        for s in &self.reported.splits {
            if !splits.iter().any(|n| n.entry == s.entry) {
                self.events.info(
                    None,
                    format!(
                        "keep apart '{}' ended: every app has every core back",
                        s.entry
                    ),
                );
            }
        }
        for r in reservations {
            if !self.reported.reservations.contains(r) {
                self.events.info(
                    None,
                    format!(
                        "reserve cores '{}'{would}: CPUs {} kept for its app",
                        r.entry,
                        wtm_policy::processor_numbers(topology, &r.cpus)
                    ),
                );
            }
        }
        for r in &self.reported.reservations {
            if !reservations.iter().any(|n| n.entry == r.entry) {
                self.events
                    .info(None, format!("reserve cores '{}' released", r.entry));
            }
        }
        for note in notes {
            if !self.reported.notes.contains(note) {
                self.events.warn(None, note.clone());
            }
        }
        self.reported = Reported {
            splits: splits.to_vec(),
            reservations: reservations.to_vec(),
            notes: notes.to_vec(),
        };
    }

    /// Gives every placed process every core back.
    pub fn release_all(&mut self, reason: &str) {
        self.own.clear();
        if self.written.is_empty() {
            return;
        }
        let mut rescued = 0;
        let count = self.written.len();
        for (_, w) in self.written.drain() {
            rescued += usize::from(w.rescue);
            let _ = self.writer.release(&w);
        }
        self.save();
        match &self.split {
            Some(split) if rescued == count => self.events.info(
                None,
                format!("{reason}: {} handed back to {count} apps", cpu_names(split)),
            ),
            _ => self.events.info(
                None,
                format!("{reason}: every core given back to {count} apps"),
            ),
        }
    }
}

/// How `placement` is written: hard affinity when it asks for it and its
/// processors share a group, CPU Sets otherwise.
fn how(topology: &Topology, placement: &Placement) -> How {
    if !placement.strict {
        return How::CpuSets;
    }
    let cpus: Vec<_> = topology
        .cpus
        .iter()
        .filter(|c| placement.cpus.contains(&c.id) && c.index < 64)
        .collect();
    match cpus.first() {
        Some(first) if cpus.iter().all(|c| c.group == first.group) => How::Affinity {
            group: first.group,
            mask: cpus.iter().fold(0u64, |m, c| m | (1u64 << c.index)),
        },
        _ => How::CpuSets,
    }
}

/// What writing `placement` to `p` records.
fn written(p: &ProcessInfo, placement: &Placement, how: How) -> Written {
    Written {
        target: ProcessRef::from(p),
        cpus: placement.cpus.clone(),
        affinity: None,
        strict: matches!(how, How::Affinity { .. }),
        rescue: placement.why.contains(&Why::Rescue),
        entries: placement.why.iter().any(|w| *w != Why::Rescue),
        why: placement.why.iter().map(Why::to_string).collect(),
    }
}

fn cpu_names(split: &RescueSplit) -> String {
    let list: Vec<_> = split.reserved_indices.iter().map(u8::to_string).collect();
    format!(
        "{} {}",
        if list.len() == 1 { "CPU" } else { "CPUs" },
        list.join(", ")
    )
}

#[cfg(test)]
#[cfg_attr(not(windows), allow(unused_imports, dead_code))]
mod tests {
    use super::*;
    use std::time::Duration;

    fn tick(r: &mut Rescue, now: Instant, snap: &Snapshot) {
        let live = snap
            .processes
            .iter()
            .map(|p| (p.pid, p.create_time))
            .collect();
        r.tick(now, snap, &live);
    }

    fn snapshot(cpu: f32, procs: Vec<wtm_core::ProcessInfo>) -> Snapshot {
        let mut s = wtm_core::Sampler::new().sample().unwrap();
        s.system.cpu_percent = cpu;
        s.processes = procs;
        s
    }

    #[cfg(windows)]
    #[test]
    fn engages_confines_new_processes_and_releases() {
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let live = wtm_core::Sampler::new().sample().unwrap();
        let ping = live
            .processes
            .iter()
            .find(|p| p.pid == pid)
            .unwrap()
            .clone();
        let events = EventLog::default();
        let cfg = ReserveConfig {
            sustain_secs: 0,
            release_secs: 0,
            ..ReserveConfig::default()
        };
        let mut r = Rescue::new(cfg.clone(), false, events.clone());
        if r.split.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return; // single-processor runner
        }
        let others = r.split.clone().unwrap().others;
        let sets = |pid| {
            let h = wtm_sys::control::ProcessHandle::open(pid, wtm_sys::control::Access::QUERY)
                .unwrap();
            h.default_cpu_sets().unwrap()
        };
        let t = Instant::now();
        tick(&mut r, t, &snapshot(99.0, vec![ping.clone()]));
        assert_eq!(sets(pid).len(), others.len());
        assert_eq!(r.summary().unwrap().moved, 1);

        tick(
            &mut r,
            t + Duration::from_secs(1),
            &snapshot(10.0, vec![ping.clone()]),
        );
        assert!(sets(pid).is_empty());
        assert!(r.summary().is_none());

        // Dry run only reports.
        r.configure(cfg, true);
        tick(
            &mut r,
            t + Duration::from_secs(2),
            &snapshot(99.0, vec![ping]),
        );
        assert!(sets(pid).is_empty());
        assert_eq!(r.summary().unwrap().moved, 0);
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn names_reserved_processors() {
        let one = RescueSplit {
            reserved: vec![7],
            others: vec![],
            reserved_indices: vec![7],
        };
        assert_eq!(cpu_names(&one), "CPU 7");
        let two = RescueSplit {
            reserved_indices: vec![14, 15],
            ..one
        };
        assert_eq!(cpu_names(&two), "CPUs 14, 15");
    }

    #[cfg(windows)]
    #[test]
    fn a_program_exempted_while_engaged_gets_every_core_back_at_once() {
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let ping = crate::test_util::wait_for("ping sampled", || {
            wtm_core::Sampler::new()
                .sample()
                .unwrap()
                .processes
                .into_iter()
                .find(|p| p.pid == pid)
        });
        let cfg = |exempt: &[&str]| ReserveConfig {
            sustain_secs: 0,
            release_secs: 0,
            exempt: exempt.iter().map(|s| (*s).to_owned()).collect(),
            ..ReserveConfig::default()
        };
        let mut r = Rescue::new(cfg(&[]), false, EventLog::default());
        if r.split.is_some() {
            let sets = || {
                wtm_sys::control::ProcessHandle::open(pid, wtm_sys::control::Access::QUERY)
                    .unwrap()
                    .default_cpu_sets()
                    .unwrap()
                    .len()
            };
            let t = Instant::now();
            tick(&mut r, t, &snapshot(99.0, vec![ping.clone()]));
            assert!(sets() > 0);
            r.configure(cfg(&["PING.EXE"]), false);
            assert_eq!(sets(), 0);
            tick(
                &mut r,
                t + Duration::from_secs(1),
                &snapshot(99.0, vec![ping]),
            );
            assert_eq!(sets(), 0);
            assert_eq!(r.summary().unwrap().moved, 0);
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use wtm_core::cpu::LogicalCpu;

    /// Records what would be written; refuses the PIDs in `own`.
    #[derive(Clone, Default)]
    struct Fake {
        log: Arc<Mutex<Vec<String>>>,
        own: Arc<Mutex<HashSet<u32>>>,
    }

    impl Fake {
        fn take(&self) -> Vec<String> {
            std::mem::take(&mut *self.log.lock().unwrap())
        }
    }

    impl CpuWriter for Fake {
        fn place(
            &mut self,
            target: &ProcessRef,
            cpus: &[u32],
            how: How,
            ours: Option<&Written>,
        ) -> Result<Placed, String> {
            if self.own.lock().unwrap().contains(&target.pid) {
                return Ok(Placed::OwnChoice);
            }
            let again = if ours.is_some() { " again" } else { "" };
            self.log
                .lock()
                .unwrap()
                .push(format!("place {}{again} {cpus:?} {how:?}", target.pid));
            Ok(Placed::Done {
                affinity: match how {
                    How::Affinity { mask, .. } => Some((mask, 0xfff)),
                    How::CpuSets => None,
                },
            })
        }

        fn release(&mut self, written: &Written) -> Result<(), String> {
            self.log
                .lock()
                .unwrap()
                .push(format!("release {}", written.target.pid));
            Ok(())
        }
    }

    /// Four P-cores with two threads each (ids 256-263), four E-cores
    /// (ids 264-267).
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

    fn proc(pid: u32, name: &str) -> ProcessInfo {
        ProcessInfo {
            io: Default::default(),
            cycle_percent: 0.0,
            pid,
            parent_pid: 1,
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

    fn snapshot(cpu: f32, procs: Vec<ProcessInfo>) -> Snapshot {
        let mut s = wtm_core::Sampler::new().sample().unwrap();
        s.system.cpu_percent = cpu;
        s.processes = procs;
        s
    }

    fn tick(r: &mut Rescue, now: Instant, snap: &Snapshot) {
        let live = snap
            .processes
            .iter()
            .map(|p| (p.pid, p.create_time))
            .collect();
        r.tick(now, snap, &live);
    }

    fn placement(toml: &str) -> PlacementConfig {
        wtm_policy::Config::from_toml(toml).unwrap().placement()
    }

    const SEPARATE: &str = r#"
        [[separate]]
        name = "Game and stream"
        apps = ["game.exe", "obs64.exe"]
        hold_secs = 0
    "#;

    fn rescue(fake: &Fake, events: &EventLog) -> Rescue {
        let cfg = ReserveConfig {
            rescue_core: false,
            ..ReserveConfig::default()
        };
        Rescue::with(
            cfg,
            false,
            events.clone(),
            Some(hybrid()),
            Box::new(fake.clone()),
        )
    }

    fn messages(events: &EventLog) -> Vec<String> {
        events.after(0).into_iter().map(|e| e.message).collect()
    }

    #[test]
    fn a_split_is_written_once_and_undone_when_it_ends() {
        let fake = Fake::default();
        let events = EventLog::default();
        let mut r = rescue(&fake, &events);
        r.set_placement(placement(SEPARATE));
        let both = snapshot(10.0, vec![proc(10, "game.exe"), proc(20, "obs64.exe")]);
        let t = Instant::now();
        tick(&mut r, t, &both);
        assert_eq!(
            fake.take(),
            [
                "place 10 [256, 257, 258, 259, 260, 261, 262, 263] CpuSets",
                "place 20 [264, 265, 266, 267] CpuSets",
            ]
        );
        assert!(
            messages(&events)
                .iter()
                .any(|m| m
                    == "keep apart 'Game and stream': cores split between game.exe, obs64.exe"),
            "{:?}",
            messages(&events)
        );
        // Nothing changed: nothing written.
        tick(&mut r, t + Duration::from_secs(1), &both);
        assert!(fake.take().is_empty());

        // The stream exits: the game gets every core back.
        tick(
            &mut r,
            t + Duration::from_secs(2),
            &snapshot(10.0, vec![proc(10, "game.exe")]),
        );
        assert_eq!(fake.take(), ["release 10"]);
        assert!(
            messages(&events)
                .iter()
                .any(|m| m.contains("keep apart 'Game and stream' ended")),
        );
        assert!(r.written.is_empty());
    }

    #[test]
    fn a_process_that_chose_its_own_cores_is_not_asked_again() {
        let fake = Fake::default();
        fake.own.lock().unwrap().insert(20);
        let mut r = rescue(&fake, &EventLog::default());
        r.set_placement(placement(SEPARATE));
        let both = snapshot(10.0, vec![proc(10, "game.exe"), proc(20, "obs64.exe")]);
        let t = Instant::now();
        tick(&mut r, t, &both);
        assert_eq!(fake.take().len(), 1, "the game placed, the stream refused");
        // Next tick the stream is left out of the split: the game alone
        // keeps every core.
        tick(&mut r, t + Duration::from_secs(1), &both);
        assert_eq!(fake.take(), ["release 10"]);
    }

    #[test]
    fn the_status_says_where_each_process_runs_and_why() {
        let fake = Fake::default();
        fake.own.lock().unwrap().insert(30);
        let mut r = rescue(&fake, &EventLog::default());
        r.set_placement(placement(&format!(
            "{SEPARATE}
[[pin]]
name = \"Other\"
match = {{ name = \"other.exe\" }}
cores = 1"
        )));
        tick(
            &mut r,
            Instant::now(),
            &snapshot(
                10.0,
                vec![
                    proc(10, "game.exe"),
                    proc(20, "obs64.exe"),
                    proc(30, "other.exe"),
                ],
            ),
        );
        let status = r.placement_status().unwrap();
        assert_eq!(status.topology, hybrid());
        let game = &status.placed[0];
        assert_eq!(
            (game.target.pid, game.why.clone(), game.cpus.len()),
            (10, vec!["keep apart 'Game and stream'".to_owned()], 8)
        );
        assert_eq!(status.splits.len(), 1);
        assert_eq!(
            status.left_alone,
            [(
                ProcessRef::from(&proc(30, "other.exe")),
                "chose its own processors".to_owned()
            )]
        );
        assert!(!status.dry_run);
    }

    #[test]
    fn strict_entries_write_hard_affinity_in_one_group() {
        let fake = Fake::default();
        let mut r = rescue(&fake, &EventLog::default());
        r.set_placement(placement(
            "[[pin]]\nname = \"Hard\"\nmatch = { name = \"a.exe\" }\ncores = \"8-9\"\nstrict = true",
        ));
        tick(
            &mut r,
            Instant::now(),
            &snapshot(10.0, vec![proc(10, "a.exe")]),
        );
        assert_eq!(
            fake.take(),
            ["place 10 [264, 265] Affinity { group: 0, mask: 768 }"]
        );
        assert_eq!(r.written[&(10, 10)].affinity, Some((768, 0xfff)));
    }

    #[test]
    fn dry_run_writes_nothing_and_says_what_it_would_do() {
        let fake = Fake::default();
        let events = EventLog::default();
        let mut r = rescue(&fake, &events);
        r.configure(
            ReserveConfig {
                rescue_core: false,
                ..ReserveConfig::default()
            },
            true,
        );
        r.set_placement(placement(SEPARATE));
        tick(
            &mut r,
            Instant::now(),
            &snapshot(10.0, vec![proc(10, "game.exe"), proc(20, "obs64.exe")]),
        );
        assert!(fake.take().is_empty());
        assert!(
            messages(&events).iter().any(|m| m
                == "keep apart 'Game and stream' (dry run): cores split between game.exe, obs64.exe"),
            "{:?}",
            messages(&events)
        );
    }

    #[test]
    fn a_huge_process_count_is_placed_over_several_ticks() {
        let fake = Fake::default();
        let mut r = rescue(&fake, &EventLog::default());
        r.set_placement(placement(
            "[[reserve_cores]]\nname = \"Audio\"\napp = \"reaper.exe\"\ncores = 1\nhold_secs = 0",
        ));
        // PIDs low enough to be Windows' own are protected: start higher.
        let mut procs = vec![proc(1000, "reaper.exe")];
        procs.extend((1001..1700).map(|pid| proc(pid, "worker.exe")));
        let snap = snapshot(10.0, procs);
        let t = Instant::now();
        tick(&mut r, t, &snap);
        assert_eq!(fake.take().len(), PER_TICK);
        tick(&mut r, t + Duration::from_secs(1), &snap);
        assert_eq!(fake.take().len(), 700 - PER_TICK);
    }

    #[test]
    fn writes_are_recorded_and_given_back_after_a_crash() {
        let dir = std::env::temp_dir().join(format!("rustwtm-placement-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("placement.json");
        let _ = std::fs::remove_file(&path);

        let fake = Fake::default();
        let mut r = rescue(&fake, &EventLog::default());
        r.recover(&path);
        r.set_placement(placement(SEPARATE));
        tick(
            &mut r,
            Instant::now(),
            &snapshot(10.0, vec![proc(10, "game.exe"), proc(20, "obs64.exe")]),
        );
        fake.take();
        // The service dies without giving anything back.
        drop(r);

        let events = EventLog::default();
        let mut next = rescue(&fake, &events);
        next.recover(&path);
        let mut given: Vec<String> = fake.take();
        given.sort();
        assert_eq!(given, ["release 10", "release 20"]);
        assert!(
            messages(&events)
                .iter()
                .any(|m| m == "every core given back to 2 apps placed before a restart")
        );
        // And the record is empty again.
        let left: Vec<Written> = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(left.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn a_pin_confines_a_real_process_and_lets_it_go() {
        let mut child = std::process::Command::new("ping")
            .args(["-n", "30", "127.0.0.1"])
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let ping = crate::test_util::wait_for("ping sampled", || {
            wtm_core::Sampler::new()
                .sample()
                .unwrap()
                .processes
                .into_iter()
                .find(|p| p.pid == pid)
        });
        let sets = || {
            wtm_sys::control::ProcessHandle::open(pid, wtm_sys::control::Access::QUERY)
                .unwrap()
                .default_cpu_sets()
                .unwrap()
        };
        let cfg = ReserveConfig {
            rescue_core: false,
            ..ReserveConfig::default()
        };
        let mut r = Rescue::new(cfg, false, EventLog::default());
        let usable = r.topology.as_ref().map_or(0, |t| {
            t.cpus.iter().filter(|c| !c.reserved_elsewhere).count()
        });
        if usable >= 2 {
            r.set_placement(placement(&format!(
                "[[pin]]
name = \"Ping\"
match = {{ name = \"{}\" }}
cores = 1",
                ping.name
            )));
            let snap = snapshot(10.0, vec![ping.clone()]);
            let t = Instant::now();
            tick(&mut r, t, &snap);
            let pinned = sets();
            assert!(!pinned.is_empty() && pinned.len() < usable, "{pinned:?}");
            assert_eq!(pinned, r.written[&(pid, ping.create_time)].cpus);

            r.set_placement(PlacementConfig::default());
            tick(&mut r, t + Duration::from_secs(1), &snap);
            assert!(sets().is_empty());
            assert!(r.written.is_empty());
        }
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_rescue_core_and_entries_share_one_plan() {
        let fake = Fake::default();
        let cfg = ReserveConfig {
            sustain_secs: 0,
            release_secs: 0,
            ..ReserveConfig::default()
        };
        let mut r = Rescue::with(
            cfg,
            false,
            EventLog::default(),
            Some(hybrid()),
            Box::new(fake.clone()),
        );
        r.set_placement(placement(SEPARATE));
        let procs = vec![
            proc(10, "game.exe"),
            proc(20, "obs64.exe"),
            proc(30, "other.exe"),
        ];
        let t = Instant::now();
        tick(&mut r, t, &snapshot(99.0, procs.clone()));
        let reserved = r.split.clone().unwrap().reserved;
        for w in r.written.values() {
            assert!(
                !w.cpus.iter().any(|id| reserved.contains(id)),
                "{w:?} kept off the rescue core"
            );
        }
        assert_eq!(r.summary().unwrap().moved, 3);
        // Load recovers: the split stays, the rescue core is handed back.
        fake.take();
        tick(&mut r, t + Duration::from_secs(1), &snapshot(10.0, procs));
        let log = fake.take();
        assert!(
            log.iter().any(|l| l.starts_with("place 10 again")),
            "{log:?}"
        );
        assert!(log.contains(&"release 30".to_owned()), "{log:?}");
        assert!(r.summary().is_none());
    }
}
