//! The one owner of the power plan, and of keeping the computer awake.
//!
//! Game mode and `[[while_running]]` entries only ask
//! ([`wtm_policy::choose_plan`]); this switches. The plan in use before the
//! first switch is written to disk before switching, so a crash never
//! leaves the machine on another plan ([`crate::engine::restore_saved_plan`]
//! puts it back at the next start). When nobody asks any more the plan goes
//! back, unless the user chose another one meanwhile: that choice stays.

use std::path::PathBuf;

use wtm_policy::{KeepAwake, PlanRequest, resolve_plan};

use crate::events::EventLog;

/// What the keeper changes on the machine; a fake in tests, so they never
/// switch the real plan or keep the developer's machine awake.
pub trait PowerSystem: Send {
    /// `(GUID, name)` of every plan.
    fn plans(&mut self) -> Result<Vec<(String, String)>, String>;
    fn active_plan(&mut self) -> Result<String, String>;
    fn set_plan(&mut self, guid: &str) -> Result<(), String>;
    /// Keeps the computer from sleeping, with `reason` shown by
    /// `powercfg /requests`; `None` lets it sleep again.
    fn keep_awake(&mut self, reason: Option<&str>) -> Result<(), String>;
}

/// The machine itself.
#[derive(Default)]
pub struct System {
    request: Option<(String, wtm_sys::power::PowerRequest)>,
}

impl PowerSystem for System {
    fn plans(&mut self) -> Result<Vec<(String, String)>, String> {
        wtm_sys::power::plans().map_err(|e| e.to_string())
    }

    fn active_plan(&mut self) -> Result<String, String> {
        wtm_sys::power::active_scheme().map_err(|e| e.to_string())
    }

    fn set_plan(&mut self, guid: &str) -> Result<(), String> {
        wtm_sys::power::set_active_scheme(guid).map_err(|e| e.to_string())
    }

    fn keep_awake(&mut self, reason: Option<&str>) -> Result<(), String> {
        use wtm_sys::power::{Awake, PowerRequest};
        let Some(reason) = reason else {
            // Closing the request ends it.
            self.request = None;
            return Ok(());
        };
        if self.request.as_ref().is_some_and(|(r, _)| r == reason) {
            return Ok(());
        }
        // The new one is held before the old one goes.
        let request = PowerRequest::new(reason).map_err(|e| e.to_string())?;
        request.set(Awake::System).map_err(|e| e.to_string())?;
        self.request = Some((reason.to_owned(), request));
        Ok(())
    }
}

fn same(a: &str, b: &str) -> bool {
    let trim = |s: &str| s.trim().trim_matches(['{', '}']).to_ascii_uppercase();
    trim(a) == trim(b)
}

/// A plan as configured, for messages: game mode's GUID by its name.
fn wanted(request: &PlanRequest) -> String {
    if same(&request.plan, wtm_sys::power::HIGH_PERFORMANCE) {
        "High performance".to_owned()
    } else {
        request.plan.clone()
    }
}

fn who(request: &PlanRequest) -> String {
    request
        .entry
        .as_ref()
        .map_or_else(|| "game mode".to_owned(), |e| format!("'{e}'"))
}

/// The plan switched to, for `wtm_ipc::PowerStatus`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Switched {
    pub name: String,
    /// `None`: game mode.
    pub entry: Option<String>,
}

pub struct PowerKeeper {
    system: Box<dyn PowerSystem>,
    /// Where the plan to go back to is kept while it is replaced.
    file: Option<PathBuf>,
    dry_run: bool,
    events: EventLog,
    /// The plan before RustWTM switched, while it is switched.
    original: Option<String>,
    /// The plan RustWTM switched to (GUID, name).
    applied: Option<(String, String)>,
    /// The request served now.
    request: Option<PlanRequest>,
    /// A request the user overrode by choosing another plan: left alone
    /// until another request comes.
    overridden: Option<PlanRequest>,
    /// Why the request is not served (a plan that is not on the machine).
    note: Option<String>,
    /// The reason the computer is kept awake.
    awake: Option<String>,
    /// What was last reported for the keep-awake, so a failure or a
    /// dry-run preview is reported once.
    awake_reported: Option<String>,
    /// Giving the plan back failed; reported once.
    restore_failed: bool,
}

impl PowerKeeper {
    pub fn new(
        system: Box<dyn PowerSystem>,
        file: Option<PathBuf>,
        dry_run: bool,
        events: EventLog,
    ) -> Self {
        Self {
            system,
            file,
            dry_run,
            events,
            original: None,
            applied: None,
            request: None,
            overridden: None,
            note: None,
            awake: None,
            awake_reported: None,
            restore_failed: false,
        }
    }

    /// Monitoring only gives everything back and then only reports.
    pub fn set_dry_run(&mut self, dry_run: bool) {
        if dry_run == self.dry_run {
            return;
        }
        if dry_run {
            self.release("monitoring only");
        }
        self.dry_run = dry_run;
        self.request = None;
        self.awake_reported = None;
    }

    /// What the window shows: the plan and keep-awake actually in force
    /// (nothing in monitoring only), and the entries active now.
    pub fn status(
        &self,
        active: Vec<wtm_policy::ActiveEntry>,
        awake: Option<(KeepAwake, Vec<String>)>,
    ) -> wtm_ipc::PowerStatus {
        let switched = self.switched();
        wtm_ipc::PowerStatus {
            active,
            plan: switched.as_ref().map(|s| s.name.clone()),
            plan_for: switched.and_then(|s| s.entry),
            awake: awake.filter(|_| self.awake.is_some()),
            note: self.note.clone(),
        }
    }

    pub fn switched(&self) -> Option<Switched> {
        let (_, name) = self.applied.as_ref()?;
        Some(Switched {
            name: name.clone(),
            entry: self.request.as_ref().and_then(|r| r.entry.clone()),
        })
    }

    #[cfg(test)]
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }

    /// Serves the plan `request` and keeps the computer awake for
    /// `awake` (a reason), each sample.
    pub fn tick(&mut self, request: Option<PlanRequest>, awake: Option<String>) {
        self.tick_plan(request);
        self.tick_awake(awake);
    }

    fn tick_plan(&mut self, request: Option<PlanRequest>) {
        let changed = request != self.request;
        if changed {
            self.note = None;
            if self.overridden.is_some() && self.overridden != request {
                self.overridden = None;
            }
        }
        self.request.clone_from(&request);
        let Some(request) = request else {
            self.give_back();
            return;
        };
        if self.overridden.as_ref() == Some(&request) {
            return;
        }
        let plans = match self.system.plans() {
            Ok(p) => p,
            Err(e) => {
                if changed {
                    self.events
                        .warn(None, format!("power plans could not be read: {e}"));
                }
                return;
            }
        };
        let Some(guid) = resolve_plan(&request.plan, &plans) else {
            if changed {
                let note = format!(
                    "power plan '{}' for {} is not on this machine",
                    wanted(&request),
                    who(&request)
                );
                self.events.warn(None, note.clone());
                self.note = Some(note);
            }
            self.give_back();
            return;
        };
        let name = plans
            .iter()
            .find(|(g, _)| same(g, &guid))
            .map_or_else(|| guid.clone(), |(_, n)| n.clone());
        if self.dry_run {
            if changed {
                self.events.info(
                    None,
                    format!("power plan {name} for {} (dry run)", who(&request)),
                );
            }
            return;
        }
        let Ok(active) = self.system.active_plan() else {
            return;
        };
        if let Some((applied, _)) = &self.applied {
            if !same(&active, applied) {
                // Chosen by the user meanwhile: theirs to keep.
                let kept = plans
                    .iter()
                    .find(|(g, _)| same(g, &active))
                    .map_or_else(|| active.clone(), |(_, n)| n.clone());
                self.events
                    .info(None, format!("power plan {kept} chosen meanwhile is kept"));
                self.forget_original();
                self.overridden = Some(request);
                return;
            }
            if same(applied, &guid) {
                return;
            }
        }
        if self.original.is_none() {
            // Written first: a crash right after switching still finds it.
            if let Some(file) = &self.file
                && let Err(e) = std::fs::write(file, active.trim_matches(['{', '}']))
            {
                self.events.warn(
                    None,
                    format!("power plan unchanged: the current one cannot be remembered: {e}"),
                );
                return;
            }
            self.original = Some(active);
        }
        match self.system.set_plan(&guid) {
            Ok(()) => {
                self.events
                    .info(None, format!("power plan {name} for {}", who(&request)));
                self.applied = Some((guid, name));
            }
            Err(e) => {
                if changed {
                    self.events
                        .warn(None, format!("power plan {name} could not be set: {e}"));
                }
                if self.applied.is_none() {
                    self.forget_original();
                }
            }
        }
    }

    /// Back to the plan before the switch, unless the user chose another.
    fn give_back(&mut self) {
        let Some((applied, applied_name)) = self.applied.take() else {
            self.forget_original();
            return;
        };
        let Some(original) = self.original.clone() else {
            return;
        };
        match self.system.active_plan() {
            Ok(active) if !same(&active, &applied) => {
                self.events
                    .info(None, "power plan chosen meanwhile is kept".to_owned());
                self.forget_original();
            }
            _ => match self.system.set_plan(&original) {
                Ok(()) => {
                    let name = self
                        .system
                        .plans()
                        .ok()
                        .and_then(|p| p.into_iter().find(|(g, _)| same(g, &original)))
                        .map_or(original.clone(), |(_, n)| n);
                    self.events.info(None, format!("power plan {name} again"));
                    self.forget_original();
                }
                // Tried again next time, and left on disk for the next
                // start; reported once.
                Err(e) => {
                    if !self.restore_failed {
                        self.events.warn(
                            None,
                            format!("could not restore the power plan {original}: {e}"),
                        );
                    }
                    self.restore_failed = true;
                    self.applied = Some((applied, applied_name));
                }
            },
        }
    }

    fn forget_original(&mut self) {
        self.restore_failed = false;
        if self.original.take().is_some()
            && let Some(file) = &self.file
        {
            let _ = std::fs::remove_file(file);
        }
    }

    fn tick_awake(&mut self, reason: Option<String>) {
        if self.dry_run {
            if reason != self.awake_reported {
                if let Some(r) = &reason {
                    self.events
                        .info(None, format!("{r} would keep the computer awake (dry run)"));
                }
                self.awake_reported.clone_from(&reason);
            }
            return;
        }
        if reason == self.awake {
            return;
        }
        match self.system.keep_awake(reason.as_deref()) {
            Ok(()) => {
                match &reason {
                    Some(r) => self
                        .events
                        .info(None, format!("{r} keeps the computer awake")),
                    None => self.events.info(None, "the computer may sleep again"),
                }
                self.awake = reason.clone();
                self.awake_reported = reason;
            }
            Err(e) => {
                if reason != self.awake_reported {
                    self.events
                        .warn(None, format!("the computer could not be kept awake: {e}"));
                    self.awake_reported = reason;
                }
            }
        }
    }

    /// Gives the plan back and lets the computer sleep (the service stops,
    /// or monitoring only).
    pub fn release(&mut self, reason: &str) {
        if self.applied.is_some() {
            self.events
                .info(None, format!("{reason}: power plan given back"));
        }
        self.give_back();
        self.request = None;
        self.overridden = None;
        if self.awake.is_some() {
            let _ = self.system.keep_awake(None);
            self.awake = None;
        }
        self.awake_reported = None;
    }
}

/// The reason shown for keeping awake: the entries asking.
pub fn awake_reason(awake: Option<&(KeepAwake, Vec<String>)>) -> Option<String> {
    let (_, who) = awake?;
    let names: Vec<String> = who.iter().map(|w| format!("'{w}'")).collect();
    Some(format!("RustWTM: {}", names.join(", ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const BALANCED: &str = "381B4222-F694-41F0-9685-FF5BB260DF2E";
    const HIGH: &str = "8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C";
    const SAVER: &str = "A1841308-3541-4FAB-BC81-F71556F20B4A";

    #[derive(Default)]
    struct Machine {
        active: String,
        awake: Option<String>,
        sets: Vec<String>,
        fail_set: bool,
    }

    /// A machine in memory: the real plan is never touched.
    #[derive(Clone)]
    struct Fake(Arc<Mutex<Machine>>);

    impl Fake {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Machine {
                active: BALANCED.into(),
                ..Default::default()
            })))
        }
        fn active(&self) -> String {
            self.0.lock().unwrap().active.clone()
        }
        fn user_chooses(&self, plan: &str) {
            self.0.lock().unwrap().active = plan.into();
        }
    }

    impl PowerSystem for Fake {
        fn plans(&mut self) -> Result<Vec<(String, String)>, String> {
            Ok(vec![
                (BALANCED.into(), "Balanced".into()),
                (HIGH.into(), "High performance".into()),
                (SAVER.into(), "Power saver".into()),
            ])
        }
        fn active_plan(&mut self) -> Result<String, String> {
            Ok(self.active())
        }
        fn set_plan(&mut self, guid: &str) -> Result<(), String> {
            let mut m = self.0.lock().unwrap();
            if m.fail_set {
                return Err("denied".into());
            }
            m.sets.push(guid.into());
            m.active = guid.into();
            Ok(())
        }
        fn keep_awake(&mut self, reason: Option<&str>) -> Result<(), String> {
            self.0.lock().unwrap().awake = reason.map(str::to_owned);
            Ok(())
        }
    }

    fn file(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("wtm-power-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("power-plan.txt")
    }

    fn keeper(fake: &Fake, file: &std::path::Path, dry_run: bool) -> (PowerKeeper, EventLog) {
        let events = EventLog::default();
        let k = PowerKeeper::new(
            Box::new(fake.clone()),
            Some(file.to_path_buf()),
            dry_run,
            events.clone(),
        );
        (k, events)
    }

    fn ask(entry: Option<&str>, plan: &str) -> Option<PlanRequest> {
        Some(PlanRequest {
            entry: entry.map(str::to_owned),
            plan: plan.into(),
        })
    }

    fn log(events: &EventLog) -> Vec<String> {
        events.after(0).into_iter().map(|e| e.message).collect()
    }

    #[test]
    fn switches_for_a_request_and_back_when_none_is_left() {
        let (fake, path) = (Fake::new(), file("back"));
        let (mut k, events) = keeper(&fake, &path, false);
        k.tick(ask(Some("Render"), "high performance"), None);
        assert_eq!(fake.active(), HIGH);
        // Remembered on disk before the switch, for a crash.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BALANCED);
        assert_eq!(
            k.switched(),
            Some(Switched {
                name: "High performance".into(),
                entry: Some("Render".into())
            })
        );
        // Game mode taking over the same plan changes nothing on the machine.
        k.tick(ask(None, HIGH), None);
        k.tick(ask(Some("Saver"), "Power saver"), None);
        assert_eq!(fake.active(), SAVER);
        k.tick(None, None);
        assert_eq!(fake.active(), BALANCED);
        assert!(!path.exists());
        assert_eq!(fake.0.lock().unwrap().sets, [HIGH, SAVER, BALANCED]);
        assert_eq!(
            log(&events),
            [
                "power plan High performance for 'Render'",
                "power plan Power saver for 'Saver'",
                "power plan Balanced again",
            ]
        );
    }

    #[test]
    fn a_plan_the_user_chooses_meanwhile_is_kept() {
        let (fake, path) = (Fake::new(), file("user"));
        let (mut k, events) = keeper(&fake, &path, false);
        let render = ask(Some("Render"), HIGH);
        k.tick(render.clone(), None);
        fake.user_chooses(SAVER);
        k.tick(render.clone(), None);
        // Not switched back while the same request lasts, nor after it.
        k.tick(render, None);
        assert_eq!(fake.active(), SAVER);
        assert!(!path.exists());
        k.tick(None, None);
        assert_eq!(fake.active(), SAVER);
        // A later request is served again.
        k.tick(ask(Some("Render"), HIGH), None);
        assert_eq!(fake.active(), HIGH);
        assert!(log(&events).contains(&"power plan Power saver chosen meanwhile is kept".into()));
        // Chosen meanwhile, noticed only when giving back.
        fake.user_chooses(BALANCED);
        k.tick(None, None);
        assert_eq!(fake.active(), BALANCED);
        assert_eq!(
            log(&events).last().unwrap(),
            "power plan chosen meanwhile is kept"
        );
    }

    #[test]
    fn a_failed_restore_keeps_the_saved_plan_and_tries_again() {
        let (fake, path) = (Fake::new(), file("restore"));
        let (mut k, events) = keeper(&fake, &path, false);
        k.tick(ask(Some("Render"), HIGH), None);
        fake.0.lock().unwrap().fail_set = true;
        for _ in 0..3 {
            k.tick(None, None);
        }
        // Still remembered for the next start, reported once.
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BALANCED);
        assert_eq!(
            log(&events)
                .iter()
                .filter(|l| l.starts_with("could not restore"))
                .count(),
            1
        );
        fake.0.lock().unwrap().fail_set = false;
        k.tick(None, None);
        assert_eq!(fake.active(), BALANCED);
        assert!(!path.exists());
    }

    #[test]
    fn game_mode_asks_by_guid_and_is_named_by_the_plan() {
        let (fake, path) = (Fake::new(), file("game"));
        let (mut k, events) = keeper(&fake, &path, true);
        k.tick(ask(None, HIGH), None);
        assert_eq!(
            log(&events),
            ["power plan High performance for game mode (dry run)"]
        );
    }

    #[test]
    fn a_missing_plan_is_noted_once_and_nothing_is_switched() {
        let (fake, path) = (Fake::new(), file("missing"));
        let (mut k, events) = keeper(&fake, &path, false);
        for _ in 0..3 {
            k.tick(ask(Some("Render"), "Ultimate Performance"), None);
        }
        assert_eq!(fake.active(), BALANCED);
        assert!(!path.exists());
        assert_eq!(
            k.note(),
            Some("power plan 'Ultimate Performance' for 'Render' is not on this machine")
        );
        assert_eq!(log(&events).len(), 1);
    }

    #[test]
    fn a_refused_switch_leaves_nothing_behind() {
        let (fake, path) = (Fake::new(), file("refused"));
        fake.0.lock().unwrap().fail_set = true;
        let (mut k, events) = keeper(&fake, &path, false);
        k.tick(ask(Some("Render"), HIGH), None);
        k.tick(ask(Some("Render"), HIGH), None);
        assert_eq!(fake.active(), BALANCED);
        assert!(!path.exists());
        assert!(k.switched().is_none());
        assert_eq!(
            log(&events),
            ["power plan High performance could not be set: denied"]
        );
    }

    #[test]
    fn monitoring_only_switches_nothing_and_keeps_nothing_awake() {
        let (fake, path) = (Fake::new(), file("dry"));
        let (mut k, events) = keeper(&fake, &path, true);
        let awake = Some("RustWTM: 'Render'".to_owned());
        for _ in 0..2 {
            k.tick(ask(Some("Render"), HIGH), awake.clone());
        }
        assert_eq!(fake.active(), BALANCED);
        assert_eq!(fake.0.lock().unwrap().awake, None);
        assert_eq!(
            log(&events),
            [
                "power plan High performance for 'Render' (dry run)",
                "RustWTM: 'Render' would keep the computer awake (dry run)",
            ]
        );
        // Nothing is shown as in force.
        let asked = Some((KeepAwake::System, vec!["Render".to_owned()]));
        let shown = k.status(Vec::new(), asked.clone());
        assert_eq!((shown.plan, shown.awake), (None, None));
        // The same mode again (any configuration save) reports nothing new.
        k.set_dry_run(true);
        k.tick(ask(Some("Render"), HIGH), awake.clone());
        assert_eq!(log(&events).len(), 2);
        // A missing plan is noted in monitoring only too.
        k.tick(ask(None, "Ultimate Performance"), None);
        assert!(k.note().is_some());
        k.tick(ask(Some("Render"), HIGH), awake.clone());
        // Enforcing: served at once, and shown.
        k.set_dry_run(false);
        k.tick(ask(Some("Render"), HIGH), awake.clone());
        assert_eq!(fake.active(), HIGH);
        assert_eq!(fake.0.lock().unwrap().awake, awake);
        let shown = k.status(Vec::new(), asked.clone());
        assert_eq!(shown.plan.as_deref(), Some("High performance"));
        assert_eq!(shown.awake, asked);
        // Back to monitoring only: everything given back.
        k.set_dry_run(true);
        assert_eq!(fake.active(), BALANCED);
        assert_eq!(fake.0.lock().unwrap().awake, None);
    }

    #[test]
    fn keeps_awake_while_asked_and_releases_on_stop() {
        let (fake, path) = (Fake::new(), file("awake"));
        let (mut k, events) = keeper(&fake, &path, false);
        let who = (
            KeepAwake::System,
            vec!["Render".to_owned(), "Movie".to_owned()],
        );
        let reason = awake_reason(Some(&who));
        assert_eq!(reason.as_deref(), Some("RustWTM: 'Render', 'Movie'"));
        k.tick(ask(Some("Render"), HIGH), reason.clone());
        k.tick(ask(Some("Render"), HIGH), reason.clone());
        assert_eq!(fake.0.lock().unwrap().awake, reason);
        k.release("service stopping");
        assert_eq!(fake.0.lock().unwrap().awake, None);
        assert_eq!(fake.active(), BALANCED);
        assert_eq!(
            log(&events),
            [
                "power plan High performance for 'Render'",
                "RustWTM: 'Render', 'Movie' keeps the computer awake",
                "service stopping: power plan given back",
                "power plan Balanced again",
            ]
        );
    }
}
