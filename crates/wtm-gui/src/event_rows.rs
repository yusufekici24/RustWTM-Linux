//! Every source's events in one list, oldest first: the service's log
//! (governor decisions, sensor and cooling changes), actions taken from this
//! window, and the window's container alerts. Shown in Events and as markers
//! on the Performance charts.

use std::borrow::Cow;
use std::hash::{Hash, Hasher};
use std::time::UNIX_EPOCH;

use wtm_ipc::EventLevel;

use crate::backend::Data;

/// Who recorded an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Source {
    /// The RustWTM service's event log.
    Service,
    /// An action taken from this window without the service.
    Window,
    /// A container alert from this window's Docker reader.
    Docker,
}

impl Source {
    pub const ALL: [Source; 3] = [Source::Service, Source::Window, Source::Docker];

    pub fn label(self) -> &'static str {
        match self {
            Self::Service => crate::i18n::tr("Service"),
            Self::Window => crate::i18n::tr("This window"),
            Self::Docker => "Docker",
        }
    }

    /// Where the event was recorded, how exact its time is and how long it
    /// is kept.
    pub fn explanation(self) -> &'static str {
        match self {
            Self::Service => crate::i18n::tr(
                "Logged by the RustWTM service at the time shown, and kept by the service while it runs.",
            ),
            Self::Window => crate::i18n::tr(
                "Done from this window without the service; kept only until the window closes.",
            ),
            Self::Docker => crate::i18n::tr(
                "Noticed by this window's Docker reader, which reads every 10 s (every 2 s while Containers or Overview is open), so the change can be up to one reading earlier. Kept only until the window closes; not in recorded incidents.",
            ),
        }
    }
}

/// One event, whatever recorded it.
#[derive(Debug, Clone)]
pub struct Row<'a> {
    pub source: Source,
    /// Tells rows apart: the service's id, or a hash of a notice.
    pub key: (Source, u64, u64),
    pub unix_ms: u64,
    pub level: EventLevel,
    pub pid: Option<u32>,
    pub name: Option<&'a str>,
    /// As recorded: the service's English text, or a container notice
    /// already in the current language.
    raw: Cow<'a, str>,
}

impl Row<'_> {
    /// The message in the current language. Translated when asked, never
    /// for every row at once.
    pub fn text(&self) -> Cow<'_, str> {
        match self.source {
            Source::Docker => Cow::Borrowed(self.raw.as_ref()),
            _ => crate::i18n::message(&self.raw),
        }
    }

    /// The message as recorded.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// A sensor or fan cooling change logged by the service.
    pub fn about_sensors(&self) -> bool {
        self.source == Source::Service
            && (self.raw.starts_with("Sensors: ") || self.raw.starts_with("Cooling "))
    }
}

/// All events, oldest first; equal times keep the service's order.
pub fn rows(data: &Data) -> Vec<Row<'_>> {
    let mut rows: Vec<Row<'_>> = data
        .events
        .iter()
        .map(|e| {
            let source = if e.id == 0 {
                Source::Window
            } else {
                Source::Service
            };
            Row {
                source,
                key: (source, e.id, e.unix_ms),
                unix_ms: e.unix_ms,
                level: e.level,
                pid: e.pid,
                name: e.name.as_deref(),
                raw: Cow::Borrowed(e.message.as_str()),
            }
        })
        .collect();
    rows.extend(data.container_alerts.notices.iter().map(|n| {
        let unix_ms =
            n.at.duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as u64);
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        n.name.hash(&mut hash);
        format!("{:?}", n.kind).hash(&mut hash);
        Row {
            source: Source::Docker,
            key: (Source::Docker, hash.finish(), unix_ms),
            unix_ms,
            level: n.level(),
            pid: None,
            name: Some(n.name.as_str()),
            raw: Cow::Owned(n.text()),
        }
    }));
    rows.sort_by_key(|r| r.unix_ms);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::container_alerts::{Kind, Notice, Resource};
    use std::time::{Duration, SystemTime};
    use wtm_ipc::Event;

    fn event(id: u64, unix_ms: u64, message: &str) -> Event {
        Event {
            id,
            unix_ms,
            level: EventLevel::Info,
            pid: None,
            name: None,
            message: message.into(),
            notify: false,
        }
    }

    #[test]
    fn every_source_is_merged_by_time() {
        let mut data = Data::default();
        data.events.push_back(event(7, 1_000, "Governor: first"));
        data.events.push_back(event(0, 5_000, "Local: Ended"));
        data.events
            .push_back(event(8, 6_000, "Sensors: readings resumed"));
        data.container_alerts.notices.push_back(Notice {
            at: UNIX_EPOCH + Duration::from_millis(3_000),
            name: "web-0".into(),
            kind: Kind::Above {
                resource: Resource::Memory,
                share: 95.0,
                docker: None,
                lasted: Duration::from_secs(60),
            },
        });
        data.container_alerts.notices.push_back(Notice {
            at: UNIX_EPOCH + Duration::from_millis(3_000),
            name: "db-0".into(),
            kind: Kind::Stopped,
        });
        let rows = rows(&data);
        let order: Vec<_> = rows.iter().map(|r| (r.source, r.unix_ms)).collect();
        assert_eq!(
            order,
            [
                (Source::Service, 1_000),
                (Source::Docker, 3_000),
                (Source::Docker, 3_000),
                (Source::Window, 5_000),
                (Source::Service, 6_000),
            ]
        );
        assert_eq!(rows[1].level, EventLevel::Warn);
        assert_eq!(rows[1].name, Some("web-0"));
        assert_eq!(rows[2].level, EventLevel::Info);
        assert_ne!(rows[1].key, rows[2].key, "two notices of one reading");
        assert!(rows[4].about_sensors());
        assert!(!rows[0].about_sensors());
        assert!(rows.iter().all(|r| !r.text().is_empty()));
    }

    #[test]
    fn a_notice_keeps_its_key_while_others_arrive() {
        let mut data = Data::default();
        let notice = Notice {
            at: SystemTime::now(),
            name: "web-0".into(),
            kind: Kind::Back(Resource::Cpu),
        };
        data.container_alerts.notices.push_back(notice.clone());
        let before = rows(&data)[0].key;
        data.container_alerts.notices.push_back(Notice {
            name: "web-1".into(),
            ..notice
        });
        assert!(rows(&data).iter().any(|r| r.key == before));
    }
}
