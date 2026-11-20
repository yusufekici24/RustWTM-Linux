//! Container alerts: a notice when a running container stays above a CPU or
//! memory limit, and another when it comes back below it or stops.
//!
//! Containers are followed by name, so one that is restarted or re-created
//! (a new id, as Compose does) keeps its alert. Only fresh measurements
//! count: a container measured late, Docker going away or another Docker
//! context restarts the wait rather than raising or ending an alert.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, SystemTime};

use wtm_core::docker::{Container, State};

/// Quiet time per container and resource after an alert ended, so a value
/// going up and down around the limit does not keep notifying.
pub const QUIET: Duration = Duration::from_secs(5 * 60);
/// The least time a value must stay back below the limit to count as
/// recovered.
pub const RECOVERY: Duration = Duration::from_secs(30);
/// Notices kept for the Containers page.
pub const NOTICES_KEPT: usize = 50;
/// Toasts shown at most per check; the others are only listed.
pub const TOASTS_PER_CHECK: usize = 3;
/// Containers followed at most (Docker lists at most this many).
const FOLLOWED: usize = wtm_core::docker::MAX_CONTAINERS;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Off by default: on, Docker is checked in the background.
    pub enabled: bool,
    /// Percent of every CPU the container can use.
    pub cpu: f32,
    /// Percent of the container's memory limit.
    pub memory: f32,
    /// How long a value must stay at or above its limit.
    pub seconds: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: false,
            cpu: 80.0,
            memory: 90.0,
            seconds: 60,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resource {
    Cpu,
    Memory,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Kind {
    /// Over the limit. `docker` is Docker's own CPU figure when it counts
    /// 100% per core, which the Containers table shows.
    Above {
        resource: Resource,
        share: f32,
        docker: Option<f32>,
        lasted: Duration,
    },
    /// Back below the limit.
    Back(Resource),
    /// Stopped, or gone from Docker, while over a limit.
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Notice {
    pub at: SystemTime,
    pub name: String,
    pub kind: Kind,
}

impl Notice {
    /// A warning while a container is over a limit, information otherwise.
    pub fn level(&self) -> wtm_ipc::EventLevel {
        match self.kind {
            Kind::Above { .. } => wtm_ipc::EventLevel::Warn,
            _ => wtm_ipc::EventLevel::Info,
        }
    }

    /// The notice in the current language.
    pub fn text(&self) -> String {
        let name = self.name.clone();
        match &self.kind {
            Kind::Above {
                resource: Resource::Cpu,
                share,
                docker,
                lasted,
            } => {
                let mut text = crate::i18n::format(
                    "{0} has used {1} of its CPUs for {2}.",
                    &[
                        name,
                        crate::i18n::percent(format!("{share:.0}")),
                        crate::i18n::duration(*lasted),
                    ],
                );
                if let Some(docker) = docker {
                    text.push(' ');
                    text.push_str(&crate::i18n::format(
                        "Docker shows {0} (100% per core).",
                        &[crate::i18n::percent(format!("{docker:.0}"))],
                    ));
                }
                text
            }
            Kind::Above {
                resource: Resource::Memory,
                share,
                lasted,
                ..
            } => crate::i18n::format(
                "{0} has used {1} of its memory limit for {2}.",
                &[
                    name,
                    crate::i18n::percent(format!("{share:.0}")),
                    crate::i18n::duration(*lasted),
                ],
            ),
            Kind::Back(Resource::Cpu) => {
                crate::i18n::format("{0}: CPU use is back below the limit.", &[name])
            }
            Kind::Back(Resource::Memory) => {
                crate::i18n::format("{0}: memory use is back below the limit.", &[name])
            }
            Kind::Stopped => crate::i18n::format("{0} stopped while over a limit.", &[name]),
        }
    }
}

/// One container's resource, followed between checks.
#[derive(Debug, Default, Clone)]
struct Track {
    /// First fresh measurement at or above the limit, in this run of them.
    above_since: Option<Instant>,
    /// An alert was sent and has not ended.
    alerted: bool,
    /// First fresh measurement back below the limit, while alerted.
    below_since: Option<Instant>,
    quiet_until: Option<Instant>,
}

#[derive(Debug, Default)]
pub struct Watch {
    pub settings: Settings,
    tracks: HashMap<(String, Resource), Track>,
    /// The Docker context the tracks belong to.
    context: Option<String>,
    /// Newest last.
    pub notices: VecDeque<Notice>,
}

impl Watch {
    /// Follows one Docker sample; returns the notices it gave, also added to
    /// [`Self::notices`].
    pub fn update(&mut self, now: Instant, state: &State) -> Vec<Notice> {
        if !self.settings.enabled {
            self.tracks.clear();
            return Vec::new();
        }
        let Some(report) = state.report.as_ref().filter(|_| state.error.is_none()) else {
            // Docker went away: nothing is known about any container, so
            // nothing is raised or ended until it answers again.
            self.interrupt();
            return Vec::new();
        };
        if self.context.as_deref() != Some(&report.endpoint.context) {
            self.tracks.clear();
            self.context = Some(report.endpoint.context.clone());
        }
        let mut notices: Vec<(String, Kind)> = Vec::new();
        for c in &report.containers {
            if c.state == "running" {
                for resource in [Resource::Cpu, Resource::Memory] {
                    if let Some(kind) = self.follow(now, c, resource) {
                        notices.push(kind);
                    }
                }
            } else if self.stop(now, &c.name) {
                notices.push((c.name.clone(), Kind::Stopped));
            }
        }
        // A listing cut short says nothing about the containers left out.
        if !report.truncated {
            let gone: Vec<String> = self
                .tracks
                .keys()
                .map(|(name, _)| name.clone())
                .filter(|name| !report.containers.iter().any(|c| &c.name == name))
                .collect();
            for name in gone {
                if self.stop(now, &name) {
                    notices.push((name, Kind::Stopped));
                }
            }
        }
        self.tracks.retain(|_, t| {
            t.alerted || t.above_since.is_some() || t.quiet_until.is_some_and(|q| now < q)
        });
        notices.dedup();
        let at = SystemTime::now();
        let notices: Vec<Notice> = notices
            .into_iter()
            .map(|(name, kind)| Notice { at, name, kind })
            .collect();
        self.notices.extend(notices.iter().cloned());
        while self.notices.len() > NOTICES_KEPT {
            self.notices.pop_front();
        }
        notices
    }

    /// Missing measurements break every run above or below a limit.
    fn interrupt(&mut self) {
        for t in self.tracks.values_mut() {
            t.above_since = None;
            t.below_since = None;
        }
    }

    fn follow(
        &mut self,
        now: Instant,
        c: &Container,
        resource: Resource,
    ) -> Option<(String, Kind)> {
        let m = &c.metrics;
        let measured = m.measured.filter(|_| m.fresh(now));
        let share = match resource {
            Resource::Cpu => m.cpu_share(),
            Resource::Memory => m.memory_share(),
        };
        let key = (c.name.clone(), resource);
        let (Some(measured), Some(share)) = (measured, share) else {
            if let Some(t) = self.tracks.get_mut(&key) {
                t.above_since = None;
                t.below_since = None;
            }
            return None;
        };
        let limit = match resource {
            Resource::Cpu => self.settings.cpu,
            Resource::Memory => self.settings.memory,
        };
        let wait = Duration::from_secs(u64::from(self.settings.seconds));
        if !self.tracks.contains_key(&key) {
            if share < limit || self.tracks.len() >= FOLLOWED * 2 {
                return None;
            }
            self.tracks.insert(key.clone(), Track::default());
        }
        let t = self.tracks.get_mut(&key)?;
        if t.alerted {
            if share >= limit {
                t.below_since = None;
                return None;
            }
            let since = *t.below_since.get_or_insert(measured);
            if measured.saturating_duration_since(since) < wait.max(RECOVERY) {
                return None;
            }
            *t = Track {
                quiet_until: Some(now + QUIET),
                ..Track::default()
            };
            return Some((c.name.clone(), Kind::Back(resource)));
        }
        if share < limit {
            t.above_since = None;
            return None;
        }
        let since = *t.above_since.get_or_insert(measured);
        let lasted = measured.saturating_duration_since(since);
        if lasted < wait || t.quiet_until.is_some_and(|q| now < q) {
            return None;
        }
        t.alerted = true;
        t.above_since = None;
        t.below_since = None;
        let docker = match resource {
            Resource::Cpu => m.cpu.filter(|_| m.cpu_cores.is_some()),
            Resource::Memory => None,
        };
        Some((
            c.name.clone(),
            Kind::Above {
                resource,
                share,
                docker,
                lasted,
            },
        ))
    }

    /// The container stopped or is gone: `true` when it was over a limit,
    /// which then ends quietly for [`QUIET`].
    fn stop(&mut self, now: Instant, name: &str) -> bool {
        let mut alerted = false;
        for resource in [Resource::Cpu, Resource::Memory] {
            if let Some(t) = self.tracks.get_mut(&(name.to_owned(), resource)) {
                alerted |= t.alerted;
                *t = Track {
                    quiet_until: t.alerted.then_some(now + QUIET).or(t.quiet_until),
                    ..Track::default()
                };
            }
        }
        alerted
    }
}

/// The settings row, beside the CPU alert's.
pub fn settings(ui: &mut eframe::egui::Ui, s: &mut Settings) {
    use crate::i18n::PercentAffix;
    use eframe::egui;
    crate::theme::option_named_by_control(
        ui,
        crate::theme::icon::BELL,
        crate::i18n::tr("Notify when a container stays above"),
        crate::i18n::tr(
            "Docker is then checked every 10 s, also in the background. CPU is the share of all the CPUs a container can use; memory the share of its limit, or of Docker's memory when it has none.",
        ),
        |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.add_enabled(
                    s.enabled,
                    egui::Slider::new(&mut s.cpu, 10.0..=100.0)
                        .clamping(egui::SliderClamping::Always)
                        .percent_affix()
                        .fixed_decimals(0)
                        .text("CPU"),
                );
                ui.add_enabled(
                    s.enabled,
                    egui::Slider::new(&mut s.memory, 10.0..=100.0)
                        .clamping(egui::SliderClamping::Always)
                        .percent_affix()
                        .fixed_decimals(0)
                        .text(crate::i18n::tr("Memory")),
                );
                ui.label(crate::i18n::tr("for"));
                ui.add_enabled(
                    s.enabled,
                    egui::DragValue::new(&mut s.seconds)
                        .range(10..=3600)
                        .suffix(crate::i18n::tr(" s")),
                );
                ui.add_space(8.0);
                ui.label(crate::i18n::tr(if s.enabled { "On" } else { "Off" }));
                crate::theme::switch(
                    ui,
                    &mut s.enabled,
                    crate::i18n::tr("Notify when a container stays above"),
                );
            });
        },
    );
}

/// Recent notices on the Containers page, newest first, while alerts are
/// on or some are left.
pub fn notices(ui: &mut eframe::egui::Ui, watch: &mut Watch) {
    if !watch.settings.enabled && watch.notices.is_empty() {
        return;
    }
    crate::theme::card(ui, crate::i18n::tr("Container alerts"), |ui| {
        let s = watch.settings;
        ui.horizontal_wrapped(|ui| {
            if s.enabled {
                ui.weak(crate::i18n::format(
                    "On: CPU above {0} or memory above {1} for {2}.",
                    &[
                        crate::i18n::percent(format!("{:.0}", s.cpu)),
                        crate::i18n::percent(format!("{:.0}", s.memory)),
                        crate::i18n::duration(Duration::from_secs(u64::from(s.seconds))),
                    ],
                ));
            } else {
                ui.weak(crate::i18n::tr("Off. Turn them on in Settings."));
            }
            if !watch.notices.is_empty() && ui.small_button(crate::i18n::tr("Clear")).clicked() {
                watch.notices.clear();
            }
        });
        if watch.notices.is_empty() {
            ui.weak(crate::i18n::tr("No container has stayed above a limit."));
        }
        for notice in watch.notices.iter().rev() {
            ui.horizontal_wrapped(|ui| {
                ui.weak(clock(notice.at));
                let text = notice.text();
                if matches!(notice.kind, Kind::Above { .. }) {
                    crate::theme::colored(ui, crate::theme::WARN, text);
                } else {
                    ui.label(text);
                }
            });
        }
    });
}

/// `HH:MM:SS` in local time.
fn clock(at: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(at)
        .format("%H:%M:%S")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use wtm_core::docker::{Endpoint, Metrics, Report};

    const T0: u64 = 1000;

    fn container(name: &str, id: &str, running: bool, cpu: Option<f32>, at: Instant) -> Container {
        Container {
            id: id.into(),
            name: name.into(),
            state: if running { "running" } else { "exited" }.into(),
            metrics: Metrics {
                cpu,
                cpu_cores: Some(4),
                memory: Some(100),
                limit: Some(1000),
                measured: Some(at),
                ..Metrics::default()
            },
            ..Container::default()
        }
    }

    fn state(context: &str, containers: Vec<Container>, at: Instant) -> State {
        State {
            report: Some(Report {
                endpoint: Endpoint {
                    context: context.into(),
                    pipe: r"\\.\pipe\docker_engine".into(),
                },
                containers,
                detail: None,
                detail_id: None,
                sampled: at,
                truncated: false,
            }),
            error: None,
        }
    }

    fn watch() -> Watch {
        Watch {
            settings: Settings {
                enabled: true,
                seconds: 20,
                ..Settings::default()
            },
            ..Watch::default()
        }
    }

    /// `web` at `cpu` (Docker's figure, 4 cores) measured `secs` after the start.
    fn web(w: &mut Watch, start: Instant, secs: u64, cpu: f32) -> Vec<Kind> {
        let at = start + Duration::from_secs(secs);
        w.update(
            at,
            &state(
                "desktop",
                vec![container("web", "a", true, Some(cpu), at)],
                at,
            ),
        )
        .into_iter()
        .map(|n| n.kind)
        .collect()
    }

    fn above(kinds: &[Kind]) -> bool {
        matches!(
            kinds,
            [Kind::Above {
                resource: Resource::Cpu,
                ..
            }]
        )
    }

    #[test]
    fn an_alert_needs_the_whole_duration_and_ends_after_recovery() {
        let start = Instant::now() + Duration::from_secs(T0);
        let mut w = watch();
        // 360% of 4 cores is 90% of its CPUs.
        assert!(web(&mut w, start, 0, 360.0).is_empty());
        assert!(web(&mut w, start, 10, 360.0).is_empty());
        let raised = web(&mut w, start, 20, 360.0);
        assert!(above(&raised), "{raised:?}");
        let Kind::Above {
            share,
            docker,
            lasted,
            ..
        } = &raised[0]
        else {
            unreachable!()
        };
        assert_eq!(
            (*share, *docker, *lasted),
            (90.0, Some(360.0), Duration::from_secs(20))
        );
        // Once only.
        assert!(web(&mut w, start, 30, 360.0).is_empty());
        // Back below for less than the duration: still alerted.
        assert!(web(&mut w, start, 40, 40.0).is_empty());
        assert!(web(&mut w, start, 50, 360.0).is_empty());
        assert!(web(&mut w, start, 60, 40.0).is_empty());
        // At least RECOVERY (30 s), even with a shorter duration.
        assert!(web(&mut w, start, 80, 40.0).is_empty());
        assert_eq!(web(&mut w, start, 90, 40.0), [Kind::Back(Resource::Cpu)]);
        // Over again soon after: quiet.
        for secs in [100, 120, 140, 200] {
            assert!(web(&mut w, start, secs, 360.0).is_empty(), "{secs}");
        }
        // After the quiet time it can alert again.
        assert!(above(&web(&mut w, start, 90 + QUIET.as_secs() + 1, 360.0)));
        assert_eq!(w.notices.len(), 3);
    }

    #[test]
    fn a_missing_or_late_measurement_restarts_the_wait() {
        let start = Instant::now() + Duration::from_secs(T0);
        let mut w = watch();
        assert!(web(&mut w, start, 0, 360.0).is_empty());
        // Docker unreachable: neither raised nor counted.
        let at = start + Duration::from_secs(10);
        let mut offline = state("desktop", Vec::new(), at);
        offline.error = Some("pipe closed".into());
        assert!(w.update(at, &offline).is_empty());
        assert!(
            web(&mut w, start, 25, 360.0).is_empty(),
            "the wait started again"
        );
        // A measurement older than Docker's freshness window counts as none.
        let late = start + Duration::from_secs(30);
        let now = late + wtm_core::docker::STALE + Duration::from_secs(1);
        let old = state(
            "desktop",
            vec![container("web", "a", true, Some(360.0), late)],
            now,
        );
        assert!(w.update(now, &old).is_empty());
        let fresh = now + Duration::from_secs(5);
        let next = |w: &mut Watch, at: Instant| {
            w.update(
                at,
                &state(
                    "desktop",
                    vec![container("web", "a", true, Some(360.0), at)],
                    at,
                ),
            )
        };
        assert!(next(&mut w, fresh).is_empty());
        assert_eq!(next(&mut w, fresh + Duration::from_secs(20)).len(), 1);
    }

    #[test]
    fn a_recreated_container_keeps_its_alert_and_a_stopped_one_ends_it() {
        let start = Instant::now() + Duration::from_secs(T0);
        let mut w = watch();
        web(&mut w, start, 0, 360.0);
        assert!(above(&web(&mut w, start, 20, 360.0)));
        // Compose re-created it: a new id, the same name. No new alert and
        // no recovery.
        let at = start + Duration::from_secs(30);
        let again = state(
            "desktop",
            vec![container("web", "b", true, Some(360.0), at)],
            at,
        );
        assert!(w.update(at, &again).is_empty());
        // It stops: one notice, and none for a later sample of it.
        let at = start + Duration::from_secs(40);
        let stopped = state("desktop", vec![container("web", "b", false, None, at)], at);
        let notices = w.update(at, &stopped);
        assert_eq!(notices.len(), 1);
        assert_eq!(notices[0].kind, Kind::Stopped);
        assert!(w.update(at, &stopped).is_empty());
        // Removed while alerted: one notice too.
        let mut w = watch();
        web(&mut w, start, 0, 360.0);
        web(&mut w, start, 20, 360.0);
        let at = start + Duration::from_secs(30);
        let gone = w.update(at, &state("desktop", Vec::new(), at));
        assert_eq!(
            gone.iter().map(|n| &n.kind).collect::<Vec<_>>(),
            [&Kind::Stopped]
        );
    }

    #[test]
    fn a_short_listing_or_another_context_gives_no_notice() {
        let start = Instant::now() + Duration::from_secs(T0);
        let mut w = watch();
        web(&mut w, start, 0, 360.0);
        assert!(above(&web(&mut w, start, 20, 360.0)));
        let at = start + Duration::from_secs(30);
        let mut truncated = state("desktop", Vec::new(), at);
        truncated.report.as_mut().unwrap().truncated = true;
        assert!(w.update(at, &truncated).is_empty());
        // Another context: a different engine, its containers start afresh.
        let other = state(
            "remote",
            vec![container("web", "c", true, Some(40.0), at)],
            at,
        );
        assert!(w.update(at, &other).is_empty());
        assert!(w.notices.len() == 1);
    }

    #[test]
    fn memory_and_both_cpu_conventions_are_compared_as_shares() {
        let start = Instant::now() + Duration::from_secs(T0);
        let mut w = watch();
        w.settings.seconds = 0;
        let mut windows = container("win", "w", true, Some(85.0), start);
        windows.metrics.cpu_cores = None;
        // 350% of 4 cores is 87.5%: over 80% too.
        let linux = container("lin", "l", true, Some(350.0), start);
        // 300% of 4 cores is 75%: under.
        let calm = container("calm", "c", true, Some(300.0), start);
        let mut full = container("full", "f", true, Some(1.0), start);
        full.metrics.memory = Some(950);
        let notices = w.update(
            start,
            &state("desktop", vec![windows, linux, calm, full], start),
        );
        let by = |name: &str| {
            notices
                .iter()
                .find(|n| n.name == name)
                .map(|n| n.kind.clone())
        };
        assert!(matches!(by("win"), Some(Kind::Above { docker: None, .. })));
        assert!(matches!(
            by("lin"),
            Some(Kind::Above {
                docker: Some(_),
                ..
            })
        ));
        assert_eq!(by("calm"), None);
        assert!(matches!(
            by("full"),
            Some(Kind::Above {
                resource: Resource::Memory,
                ..
            })
        ));
        let text = |name: &str| notices.iter().find(|n| n.name == name).unwrap().text();
        assert_eq!(text("win"), "win has used 85% of its CPUs for 0s.");
        assert_eq!(
            text("lin"),
            "lin has used 88% of its CPUs for 0s. Docker shows 350% (100% per core)."
        );
        assert_eq!(
            text("full"),
            "full has used 95% of its memory limit for 0s."
        );
    }

    #[test]
    fn notices_and_settings_render_in_five_languages_and_clear() {
        use egui_kittest::{Harness, kittest::Queryable};
        let start = Instant::now() + Duration::from_secs(T0);
        for language in [
            crate::i18n::Language::English,
            crate::i18n::Language::Turkish,
            crate::i18n::Language::German,
            crate::i18n::Language::French,
            crate::i18n::Language::Spanish,
        ] {
            let _language = crate::i18n::scope(language);
            let mut w = watch();
            web(&mut w, start, 0, 360.0);
            web(&mut w, start, 20, 360.0);
            web(&mut w, start, 30, 40.0);
            web(&mut w, start, 70, 40.0);
            assert_eq!(w.notices.len(), 2);
            let texts: Vec<String> = w.notices.iter().map(Notice::text).collect();
            let mut h = Harness::builder()
                .allow_missing_glyphs()
                .with_size([900.0, 500.0])
                .build_ui_state(
                    |ui, (w, s): &mut (Watch, Settings)| {
                        notices(ui, w);
                        settings(ui, s);
                    },
                    (w, Settings::default()),
                );
            h.run();
            for text in &texts {
                assert!(h.query_by_label(text).is_some(), "{language:?}: {text}");
            }
            assert!(
                h.query_by_label(crate::i18n::tr("Container alerts"))
                    .is_some()
            );
            h.get_by_label(crate::i18n::tr("Clear")).click();
            h.run();
            assert!(h.state().0.notices.is_empty());
            assert!(
                h.query_by_label(crate::i18n::tr("No container has stayed above a limit."))
                    .is_some()
            );
            // The switch turns them on.
            h.get_by_label(crate::i18n::tr("Notify when a container stays above"))
                .click();
            h.run();
            assert!(h.state().1.enabled, "{language:?}");
        }
    }

    #[test]
    fn nothing_is_followed_while_alerts_are_off() {
        let start = Instant::now() + Duration::from_secs(T0);
        let mut w = Watch::default();
        assert!(!w.settings.enabled, "off unless turned on");
        for secs in [0, 100, 200] {
            assert!(web(&mut w, start, secs, 400.0).is_empty());
        }
        assert!(w.tracks.is_empty());
    }
}
