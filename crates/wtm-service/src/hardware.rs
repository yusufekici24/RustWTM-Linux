//! Dedicated sensor/control worker. No hardware call runs on the governor or
//! IPC threads. Profiles are session-only; startup always keeps hardware defaults.
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::time::{Duration, Instant};
use wtm_core::hardware::{HardwareReader, HardwareReport, unix_ms};
use wtm_policy::{CoolingMode, CoolingProfile, smooth_duty};

use crate::events::EventLog;
use wtm_ipc::EventLevel;

/// Readings a change must last before it is logged (about six seconds), so
/// a sensor missing from one reading does not fill the event log.
const SETTLE: u8 = 3;

enum Message {
    Set(String, CoolingProfile, Instant, Sender<Result<(), String>>),
    Stop(Sender<()>),
}

#[derive(Debug)]
pub struct HardwareHub {
    report: Arc<RwLock<HardwareReport>>,
    dry_run: Arc<AtomicBool>,
    worker: Mutex<Option<Sender<Message>>>,
    /// When a client last asked for sensors or a fan setting.
    last_request: Mutex<Instant>,
    /// The worker is holding a fan profile, which must keep being renewed.
    controlling: Arc<AtomicBool>,
    /// Where lasting sensor and cooling changes are logged.
    events: OnceLock<EventLog>,
}
impl Default for HardwareHub {
    fn default() -> Self {
        Self {
            report: Arc::new(RwLock::new(HardwareReport::default())),
            dry_run: Arc::new(AtomicBool::new(true)),
            worker: Mutex::new(None),
            last_request: Mutex::new(Instant::now()),
            controlling: Arc::new(AtomicBool::new(false)),
            events: OnceLock::new(),
        }
    }
}

/// How a controlled fan channel is driven, as far as the event log cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cooling {
    Software,
    /// Left to the hardware: the profile's temperature sensor is missing or
    /// stale.
    Automatic,
    Failed,
}

/// A state that is logged when it changes and the change lasts.
#[derive(Debug)]
struct Track<T> {
    /// The state last settled on; `None` until one has.
    settled: Option<T>,
    /// A different state and the readings it has lasted.
    pending: Option<(T, u8)>,
}

impl<T> Default for Track<T> {
    fn default() -> Self {
        Self {
            settled: None,
            pending: None,
        }
    }
}

impl<T: Copy + PartialEq> Track<T> {
    /// The previous and new state once `now` has lasted `SETTLE` readings.
    fn observe(&mut self, now: T) -> Option<(Option<T>, T)> {
        if self.settled == Some(now) {
            self.pending = None;
            return None;
        }
        let count = match self.pending {
            Some((state, count)) if state == now => count + 1,
            _ => 1,
        };
        if count < SETTLE {
            self.pending = Some((now, count));
            return None;
        }
        self.pending = None;
        let before = self.settled.replace(now);
        Some((before, now))
    }
}

/// Turns the worker's readings into log lines: sensor reading failing and
/// resuming, and a controlled fan falling back to the hardware, failing or
/// returning to software control. Kept by the worker only, so a restarted
/// helper starts from "unknown" and logs nothing for a normal first reading.
#[derive(Debug, Default)]
struct Changes {
    reading: Track<bool>,
    channels: HashMap<String, Track<Cooling>>,
}

impl Changes {
    /// A reading failed with `error`.
    fn failed(&mut self, error: &str) -> Vec<(EventLevel, String)> {
        match self.reading.observe(false) {
            Some((_, false)) => vec![(
                EventLevel::Warn,
                format!("Sensors: reading failed: {error}"),
            )],
            _ => Vec::new(),
        }
    }

    /// A reading succeeded and each controlled channel (`id`, `name`) is in
    /// the given state. Channels no longer controlled are forgotten.
    fn read(
        &mut self,
        channels: &[(String, String, Cooling, String)],
    ) -> Vec<(EventLevel, String)> {
        let mut lines = Vec::new();
        if let Some((Some(false), true)) = self.reading.observe(true) {
            lines.push((EventLevel::Info, "Sensors: readings resumed".to_owned()));
        }
        self.channels
            .retain(|id, _| channels.iter().any(|(c, ..)| c == id));
        for (id, name, state, error) in channels {
            let change = self.channels.entry(id.clone()).or_default().observe(*state);
            match change {
                Some((_, Cooling::Automatic)) => lines.push((
                    EventLevel::Warn,
                    format!(
                        "Cooling {name}: back on hardware automatic, its temperature sensor \
                         is missing or stale"
                    ),
                )),
                Some((_, Cooling::Failed)) => lines.push((
                    EventLevel::Warn,
                    format!("Cooling {name}: control failed: {error}"),
                )),
                Some((Some(_), Cooling::Software)) => lines.push((
                    EventLevel::Info,
                    format!("Cooling {name}: under software control again"),
                )),
                _ => {}
            }
        }
        lines
    }
}

fn log(events: Option<&EventLog>, lines: Vec<(EventLevel, String)>) {
    if let Some(events) = events {
        for (level, message) in lines {
            events.push(level, None, message);
        }
    }
}

/// The helper may stop: nobody asked for `after` and no fan is controlled.
fn idle(last_request: Instant, controlling: bool, after: Duration) -> bool {
    !controlling && last_request.elapsed() >= after
}

impl HardwareHub {
    pub fn dry_run(&self, dry: bool) {
        self.dry_run.store(dry, Ordering::Relaxed);
    }
    /// Logs lasting sensor and cooling changes to `events` from the next
    /// start of the helper on.
    pub fn log_to(&self, events: EventLog) {
        let _ = self.events.set(events);
    }
    fn sender(&self) -> Sender<Message> {
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        worker.get_or_insert_with(|| {
            let (tx, rx) = mpsc::channel();
            let (report, dry_run) = (self.report.clone(), self.dry_run.clone());
            let controlling = self.controlling.clone();
            let events = self.events.get().cloned();
            std::thread::spawn(move || {
                let mut reader = HardwareReader::new(true);
                let mut changes = Changes::default();
                let mut profiles: HashMap<String, CoolingProfile> = HashMap::new();
                let mut applied: HashMap<String, (f32, Instant)> = HashMap::new();
                loop {
                    match rx.recv_timeout(Duration::from_secs(2)) {
                        Ok(Message::Stop(reply)) => { for id in profiles.keys() {let _=reader.set(id,None);} drop(reader); let _=reply.send(()); return; }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Ok(Message::Set(id, profile, deadline, reply)) => {
                            if Instant::now() >= deadline {let _=reply.send(Err("Cooling request expired before execution".into()));continue;}
                            let r = if profile.mode == CoolingMode::Automatic {
                                // Remove the request even when restoring fails; future leases
                                // must not re-apply a setting the user just disabled.
                                profiles.remove(&id); applied.remove(&id);
                                reader.set(&id, None)
                            } else if dry_run.load(Ordering::Relaxed) {
                                Err("Switch the service from monitoring to enforcing before controlling fans".into())
                            } else {
                                let snapshot = report.read().unwrap_or_else(PoisonError::into_inner).clone();
                                match snapshot.channels.iter().find(|c| c.id == id) {
                                    None => Err("Cooling channel is no longer available".into()),
                                    Some(channel) => profile.validate(channel).and_then(|()| {
                                        let duty = profile.duty(&snapshot, unix_ms()).ok_or("Temperature sensor is missing or stale")?;
                                        reader.set(&id, Some(duty))?;
                                        applied.insert(id.clone(), (duty, Instant::now()));
                                        profiles.insert(id.clone(), profile);
                                        Ok(())
                                    }),
                                }
                            };
                            if reply.send(r).is_err() {
                                // Do not keep renewing an operation whose caller timed out.
                                profiles.remove(&id); applied.remove(&id);
                                let _ = reader.set(&id, None);
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    controlling.store(!profiles.is_empty(), Ordering::Relaxed);
                    match reader.sample() {
                        Ok(mut snapshot) => {
                            let now = unix_ms();
                            let mut states = Vec::new();
                            for (id, profile) in &profiles {
                                let requested = if dry_run.load(Ordering::Relaxed) { None } else { profile.duty(&snapshot, now) };
                                let duty = requested.map(|v| applied.get(id).map_or(v, |(old, at)| smooth_duty(*old, v, at.elapsed().as_secs_f32())));
                                let result = reader.set(id, duty);
                                let (state, error) = match &result { Ok(()) if duty.is_some() => (Cooling::Software, String::new()), Ok(()) => (Cooling::Automatic, String::new()), Err(e) => (Cooling::Failed, e.clone()) };
                                let label = match (result, duty) {
                                    (Ok(()), Some(v)) => { applied.insert(id.clone(), (v, Instant::now())); format!("Software · {v:.0}%") },
                                    (Ok(()), None) => { applied.remove(id); "Hardware automatic · sensor unavailable or monitoring only".into() },
                                    (Err(e), _) => format!("Control failed: {e}"),
                                };
                                snapshot.cooling.push((id.clone(),label));
                                let name = snapshot.channels.iter().find(|c| &c.id == id).map_or_else(|| id.clone(), |c| c.name.clone());
                                states.push((id.clone(), name, state, error));
                            }
                            // Monitoring mode releases every fan: not a fault.
                            if dry_run.load(Ordering::Relaxed) {profiles.clear();applied.clear();states.clear();}
                            log(events.as_ref(), changes.read(&states));
                            controlling.store(!profiles.is_empty(), Ordering::Relaxed);
                            *report.write().unwrap_or_else(PoisonError::into_inner) = snapshot;
                        }
                        Err(e) => {
                            // Restore immediately on failed collection, and allow the helper's
                            // lease deadline to retry if communication itself is broken.
                            for id in profiles.keys() { let _ = reader.set(id,None); }
                            applied.clear();
                            log(events.as_ref(), changes.failed(&e.to_string()));
                            report.write().unwrap_or_else(PoisonError::into_inner).notice = format!("Sensor collection failed: {e}");
                        }
                    }
                }
                for id in profiles.keys() { let _ = reader.set(id,None); }
            });
            tx
        }).clone()
    }
    fn requested(&self) {
        *self
            .last_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }
    pub fn snapshot(&self) -> HardwareReport {
        self.requested();
        self.sender();
        self.report
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
    pub fn set(&self, id: String, profile: CoolingProfile) -> Result<(), String> {
        if profile.mode != CoolingMode::Automatic && self.dry_run.load(Ordering::Relaxed) {
            return Err(
                "Switch the service from monitoring to enforcing before controlling fans".into(),
            );
        }
        self.requested();
        let (reply, rx) = mpsc::channel();
        self.sender()
            .send(Message::Set(
                id,
                profile,
                Instant::now() + Duration::from_secs(6),
                reply,
            ))
            .map_err(|e| e.to_string())?;
        // Six seconds to start plus the provider's four-second control timeout.
        rx.recv_timeout(Duration::from_secs(12))
            .map_err(|e| format!("Hardware control did not acknowledge: {e}"))?
    }
    /// Stops the helper (about 100 MB and a steady trickle of CPU) once no
    /// client asked for `after` and no fan profile is active. The next
    /// request starts it again. Does not wait for the helper to exit.
    pub fn stop_if_idle(&self, after: Duration) {
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        let last = *self
            .last_request
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if worker.is_none() || !idle(last, self.controlling.load(Ordering::Relaxed), after) {
            return;
        }
        if let Some(tx) = worker.take() {
            let (reply, _) = mpsc::channel();
            let _ = tx.send(Message::Stop(reply));
        }
        // A reading kept from before the pause would look current.
        *self.report.write().unwrap_or_else(PoisonError::into_inner) = HardwareReport::default();
    }
    pub fn stop(&self) {
        if let Some(tx) = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            let (reply, rx) = mpsc::channel();
            let _ = tx.send(Message::Stop(reply));
            let _ = rx.recv_timeout(Duration::from_secs(5));
        }
    }
}
impl Drop for HardwareHub {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn monitoring_mode_rejects_writes_without_even_starting_a_provider() {
        let hub = HardwareHub::default();
        let profile = CoolingProfile {
            mode: CoolingMode::Manual(50.0),
            sensor_id: "temp".into(),
            ..Default::default()
        };
        assert!(
            hub.set("fan".into(), profile)
                .unwrap_err()
                .contains("monitoring")
        );
        assert!(hub.worker.lock().unwrap().is_none());
    }

    fn channel(state: Cooling) -> Vec<(String, String, Cooling, String)> {
        vec![(
            "/lpc/0/control/1".into(),
            "CPU Fan".into(),
            state,
            "denied".into(),
        )]
    }

    fn messages(lines: Vec<(EventLevel, String)>) -> Vec<String> {
        lines.into_iter().map(|(_, m)| m).collect()
    }

    #[test]
    fn a_first_normal_reading_logs_nothing() {
        let mut changes = Changes::default();
        for _ in 0..10 {
            assert!(changes.read(&channel(Cooling::Software)).is_empty());
        }
    }

    #[test]
    fn a_sensor_missing_from_a_few_readings_is_not_logged() {
        let mut changes = Changes::default();
        for _ in 0..5 {
            changes.read(&channel(Cooling::Software));
        }
        // Gone for two readings, back, gone for two again: never three.
        for state in [
            Cooling::Automatic,
            Cooling::Automatic,
            Cooling::Software,
            Cooling::Automatic,
            Cooling::Automatic,
            Cooling::Software,
        ] {
            assert!(changes.read(&channel(state)).is_empty(), "{state:?}");
        }
    }

    #[test]
    fn a_lasting_fallback_and_its_recovery_are_logged_once() {
        let mut changes = Changes::default();
        for _ in 0..3 {
            changes.read(&channel(Cooling::Software));
        }
        let mut logged = Vec::new();
        for _ in 0..6 {
            logged.extend(changes.read(&channel(Cooling::Automatic)));
        }
        assert_eq!(
            messages(logged),
            [
                "Cooling CPU Fan: back on hardware automatic, its temperature sensor is missing or stale"
            ]
        );
        let mut logged = Vec::new();
        for _ in 0..6 {
            logged.extend(changes.read(&channel(Cooling::Software)));
        }
        assert_eq!(
            logged,
            [(
                EventLevel::Info,
                "Cooling CPU Fan: under software control again".to_owned()
            )]
        );
        let mut logged = Vec::new();
        for _ in 0..3 {
            logged.extend(changes.read(&channel(Cooling::Failed)));
        }
        assert_eq!(
            messages(logged),
            ["Cooling CPU Fan: control failed: denied"]
        );
    }

    #[test]
    fn a_fallback_from_the_start_is_logged_and_released_channels_are_forgotten() {
        let mut changes = Changes::default();
        let mut logged = Vec::new();
        for _ in 0..3 {
            logged.extend(changes.read(&channel(Cooling::Automatic)));
        }
        assert_eq!(logged.len(), 1);
        // Released, then controlled again: a fresh start, not a recovery.
        changes.read(&[]);
        for _ in 0..5 {
            assert!(changes.read(&channel(Cooling::Software)).is_empty());
        }
    }

    #[test]
    fn failed_readings_are_logged_once_and_their_end_too() {
        let mut changes = Changes::default();
        // A helper that never read: the failure is still worth a line.
        let mut logged = Vec::new();
        for _ in 0..5 {
            logged.extend(changes.failed("pipe closed"));
        }
        assert_eq!(
            logged,
            [(
                EventLevel::Warn,
                "Sensors: reading failed: pipe closed".to_owned()
            )]
        );
        // One good reading between failures is not a recovery.
        assert!(changes.read(&[]).is_empty());
        assert!(changes.failed("pipe closed").is_empty());
        let mut logged = Vec::new();
        for _ in 0..4 {
            logged.extend(changes.read(&[]));
        }
        assert_eq!(messages(logged), ["Sensors: readings resumed"]);
        // A new helper starts unknown: its first readings are silent.
        let mut changes = Changes::default();
        for _ in 0..4 {
            assert!(changes.read(&[]).is_empty());
        }
    }

    #[test]
    fn idle_needs_quiet_clients_and_no_fan_control() {
        let long_ago = Instant::now() - Duration::from_secs(60);
        let after = Duration::from_secs(30);
        assert!(idle(long_ago, false, after));
        assert!(!idle(long_ago, true, after), "a fan profile keeps it");
        assert!(!idle(Instant::now(), false, after));
    }

    #[test]
    fn the_helper_stops_when_idle_and_restarts_on_request() {
        let hub = HardwareHub::default();
        // Never started: nothing to stop.
        hub.stop_if_idle(Duration::ZERO);
        assert!(hub.worker.lock().unwrap().is_none());

        let _ = hub.snapshot();
        assert!(hub.worker.lock().unwrap().is_some());
        // Asked just now.
        hub.stop_if_idle(Duration::from_secs(30));
        assert!(hub.worker.lock().unwrap().is_some());
        // Driving a fan keeps it running however long nobody asks.
        hub.controlling.store(true, Ordering::Relaxed);
        hub.stop_if_idle(Duration::ZERO);
        assert!(hub.worker.lock().unwrap().is_some());
        hub.controlling.store(false, Ordering::Relaxed);
        hub.stop_if_idle(Duration::ZERO);
        assert!(hub.worker.lock().unwrap().is_none());
        let _ = hub.snapshot();
        assert!(hub.worker.lock().unwrap().is_some(), "started again");
        hub.stop();
    }
}
