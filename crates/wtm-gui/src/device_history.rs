//! A disk's, network adapter's or GPU's history as the service recorded it:
//! a minute at a time, kept for a week (see the service's `history.rs`).
//! Shown under the device's live graphs on its Performance page.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use eframe::egui::{self, Color32, Ui};

use crate::backend::Data;

/// The ranges offered, in hours.
const RANGES: [u32; 3] = [1, 24, 168];
/// A shown history is asked for again this often.
pub const REFRESH: Duration = Duration::from_secs(60);

/// The range chosen and what was last asked for.
#[derive(Debug)]
pub struct View {
    pub hours: u32,
    /// The device and range asked for, and when.
    requested: Option<(String, u32, Instant)>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            hours: 24,
            requested: None,
        }
    }
}

impl View {
    /// The device and range to ask the service for now, if any: a new one,
    /// or the shown one once a minute.
    pub fn due(&mut self, device: Option<&str>, now: Instant) -> Option<(String, u32)> {
        let device = device?;
        let fresh = self.requested.as_ref().is_some_and(|(d, h, at)| {
            d == device && *h == self.hours && now.saturating_duration_since(*at) < REFRESH
        });
        if fresh {
            return None;
        }
        self.requested = Some((device.to_owned(), self.hours, now));
        Some((device.to_owned(), self.hours))
    }
}

/// The service's answer for a device and range.
#[derive(Debug, Clone)]
pub struct Loaded {
    pub device: String,
    pub hours: u32,
    /// `Ok(None)`: the service has not recorded the device.
    pub result: Result<Option<wtm_ipc::DeviceHistory>, String>,
    /// When it was answered: the right end of the charts.
    pub at: SystemTime,
}

/// The device a Performance page shows whose history the service keeps.
pub fn wanted(selection: &crate::performance_view::State) -> Option<&str> {
    use crate::performance_view::Page;
    let device = selection.device.as_deref()?;
    let prefix = match selection.page {
        Page::Disk => "disk:",
        Page::Network => "net:",
        Page::Gpu => "gpu:",
        _ => return None,
    };
    device.starts_with(prefix).then_some(device)
}

#[derive(Clone, Copy)]
enum Unit {
    Percent,
    Rate,
    Bytes,
}

impl Unit {
    fn text(self, v: f32) -> String {
        match self {
            Self::Percent => crate::i18n::percent(format!("{v:.1}")),
            Self::Rate => format!("{}/s", crate::i18n::bytes(f64::from(v))),
            Self::Bytes => crate::i18n::bytes(f64::from(v)),
        }
    }
}

/// The three measures of a device kind, as the service records them.
fn measures(device: &str) -> Vec<(&'static str, Unit, Color32)> {
    use crate::theme::{DISK_READ, DISK_WRITE, GPU, NET_RX, NET_TX};
    if device.starts_with("disk:") {
        vec![
            (crate::i18n::tr("Active time"), Unit::Percent, DISK_READ),
            (crate::i18n::tr("Read speed"), Unit::Rate, DISK_READ),
            (crate::i18n::tr("Write speed"), Unit::Rate, DISK_WRITE),
        ]
    } else if device.starts_with("net:") {
        vec![
            (crate::i18n::tr("Receive"), Unit::Rate, NET_RX),
            (crate::i18n::tr("Send"), Unit::Rate, NET_TX),
        ]
    } else {
        vec![
            (crate::i18n::tr("Utilization"), Unit::Percent, GPU),
            (crate::i18n::tr("Dedicated GPU memory"), Unit::Bytes, GPU),
            (crate::i18n::tr("Shared GPU memory"), Unit::Bytes, GPU),
        ]
    }
}

/// The card with the device's recorded history.
pub fn show(ui: &mut Ui, data: &Data, device: &str, view: &mut View) {
    crate::theme::card(ui, crate::i18n::tr("Recorded history"), |ui| {
        ui.horizontal_wrapped(|ui| {
            for hours in RANGES {
                crate::theme::choice(ui, &mut view.hours, hours, crate::i18n::period(hours));
            }
        });
        let loaded = data
            .device_history
            .as_ref()
            .filter(|l| l.device == device && l.hours == view.hours);
        let Some(loaded) = loaded else {
            ui.weak(crate::i18n::tr("Loading…"));
            return;
        };
        let history = match &loaded.result {
            Err(e) => {
                ui.weak(crate::i18n::message(e).as_ref());
                return;
            }
            Ok(None) => {
                ui.add(egui::Label::new(egui::RichText::new(crate::i18n::tr(
                    "The service has no history of this device. It records a device once it has read its model or name, and only while it runs.",
                )).weak()).wrap());
                return;
            }
            Ok(Some(history)) => history,
        };
        let caption = if history.minutes.is_empty() {
            crate::i18n::tr(
                "Nothing recorded in this period. The service keeps each device's last week, a minute at a time.",
            )
            .to_owned()
        } else {
            crate::i18n::format(
                "Recorded {0} in this period. The service keeps each device's last week, a minute at a time; minutes it did not run stay empty.",
                &[crate::i18n::duration(Duration::from_secs(
                    history.minutes.len() as u64 * 60,
                ))],
            )
        };
        ui.add(egui::Label::new(egui::RichText::new(caption).weak()).wrap());
        if history.minutes.is_empty() {
            return;
        }
        let now = loaded
            .at
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64 / 60);
        let span = i64::from(loaded.hours) * 60;
        let measures = measures(device);
        let charts = |ui: &mut Ui, i: usize| {
            let (title, unit, color) = measures[i];
            let points: Vec<(i64, f32, f32)> = history
                .minutes
                .iter()
                .filter_map(|m| m.values[i].map(|(avg, peak)| (m.minute, avg, peak)))
                .collect();
            crate::container_history::minute_chart(ui, title, &points, now, span, color, &|v| {
                unit.text(v)
            });
        };
        let columns = if ui.available_width() >= 240.0 * measures.len() as f32 {
            measures.len()
        } else {
            1
        };
        if columns == 1 {
            for i in 0..measures.len() {
                charts(ui, i);
            }
        } else {
            ui.columns(columns, |cols| {
                for (i, col) in cols.iter_mut().enumerate() {
                    charts(col, i);
                }
            });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    use wtm_ipc::{DeviceHistory, DeviceMinute};

    #[test]
    fn the_history_is_asked_for_once_a_minute_and_when_the_choice_changes() {
        let mut view = View::default();
        let t = Instant::now();
        assert_eq!(view.due(None, t), None);
        assert_eq!(view.due(Some("disk:0"), t), Some(("disk:0".into(), 24)));
        assert_eq!(view.due(Some("disk:0"), t + Duration::from_secs(30)), None);
        assert_eq!(
            view.due(Some("disk:1"), t + Duration::from_secs(31)),
            Some(("disk:1".into(), 24))
        );
        view.hours = 168;
        assert_eq!(
            view.due(Some("disk:1"), t + Duration::from_secs(32)),
            Some(("disk:1".into(), 168))
        );
        assert_eq!(
            view.due(Some("disk:1"), t + Duration::from_secs(92)),
            Some(("disk:1".into(), 168))
        );
    }

    #[test]
    fn only_a_device_page_wants_a_history() {
        use crate::performance_view::{Page, State};
        let mut s = State::default();
        s.page = Page::Disk;
        s.device = Some("disk:0".into());
        assert_eq!(wanted(&s), Some("disk:0"));
        s.page = Page::Network;
        assert_eq!(wanted(&s), None, "a disk is not a network page's device");
        s.device = Some("net:0000000000000001".into());
        assert_eq!(wanted(&s), Some("net:0000000000000001"));
        s.page = Page::All;
        assert_eq!(wanted(&s), None);
    }

    fn page(loaded: Option<Loaded>) -> Harness<'static, (Data, View)> {
        let data = Data {
            device_history: loaded,
            ..Data::default()
        };
        Harness::builder().with_size([900.0, 500.0]).build_ui_state(
            |ui, (data, view): &mut (Data, View)| show(ui, data, "disk:0", view),
            (data, View::default()),
        )
    }

    fn loaded(result: Result<Option<DeviceHistory>, String>) -> Option<Loaded> {
        Some(Loaded {
            device: "disk:0".into(),
            hours: 24,
            result,
            at: UNIX_EPOCH + Duration::from_secs(1_000_000 * 60),
        })
    }

    #[test]
    fn a_recorded_day_is_drawn_with_its_coverage() {
        let minute = |m: i64| DeviceMinute {
            minute: m,
            values: [Some((10.0, 40.0)), Some((1e6, 5e6)), Some((2e6, 3e6))],
        };
        // Two runs with a gap between them.
        let minutes = (1_000_000 - 120..1_000_000 - 60)
            .chain(1_000_000 - 30..1_000_000)
            .map(minute)
            .collect();
        let mut h = page(loaded(Ok(Some(DeviceHistory {
            label: "SSD A".into(),
            minutes,
        }))));
        h.run();
        assert!(
            h.query_by_label_contains("Recorded 1h 30m in this period")
                .is_some()
        );
        for title in ["Active time", "Read speed", "Write speed"] {
            assert!(h.query_by_label(title).is_some(), "{title}");
        }
        // Another range: asked for, shown as loading meanwhile.
        h.get_by_label("7 days").click();
        h.run();
        assert_eq!(h.state().1.hours, 168);
        assert!(h.query_by_label("Loading…").is_some());
        h.state_mut().1.hours = 24;
        h.run();
        assert!(h.query_by_label("Loading…").is_none());
    }

    #[test]
    fn an_unrecorded_device_and_a_missing_service_are_explained() {
        let mut h = page(loaded(Ok(None)));
        h.run();
        assert!(
            h.query_by_label_contains("has no history of this device")
                .is_some()
        );
        let mut h = page(loaded(Err(
            "Usage history is recorded by the RustWTM service".into(),
        )));
        h.run();
        assert!(
            h.query_by_label_contains("recorded by the RustWTM service")
                .is_some()
        );
        let mut h = page(loaded(Ok(Some(DeviceHistory {
            label: "SSD A".into(),
            minutes: Vec::new(),
        }))));
        h.run();
        assert!(
            h.query_by_label_contains("Nothing recorded in this period")
                .is_some()
        );
        assert!(h.query_by_label("Active time").is_none());
    }
}
