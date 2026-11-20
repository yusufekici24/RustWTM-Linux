//! One lazy Docker reader, independent of the process sampler and UI thread.
//! It reads every 2 s while a page shows Docker, and every [`BACKGROUND`]
//! while container alerts are on, which it checks after each reading.
use crate::backend::Data;
use crate::container_alerts::Notice;
use crate::container_history::History;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::SystemTime;
use std::time::{Duration, Instant};

/// Time between readings while only container alerts need them.
pub const BACKGROUND: Duration = Duration::from_secs(10);
/// Time between readings while a page shows Docker.
const SHOWN: Duration = Duration::from_secs(2);

struct Request {
    selected: Option<String>,
    until: Instant,
}
struct Session {
    signal: SyncSender<()>,
    latest: Arc<Mutex<Request>>,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}
struct Control {
    session: Option<Session>,
    retry: Instant,
}
pub struct DockerWorker {
    data: Weak<Mutex<Data>>,
    ctx: eframe::egui::Context,
    control: Mutex<Control>,
    /// Shows a container notice as a Windows notification.
    toast: Arc<dyn Fn(&Notice) + Send + Sync>,
}
impl DockerWorker {
    pub fn new(
        data: &Arc<Mutex<Data>>,
        ctx: eframe::egui::Context,
        toast: impl Fn(&Notice) + Send + Sync + 'static,
    ) -> Self {
        Self {
            data: Arc::downgrade(data),
            ctx,
            control: Mutex::new(Control {
                session: None,
                retry: Instant::now(),
            }),
            toast: Arc::new(toast),
        }
    }
    #[cfg(test)]
    pub fn offline() -> Self {
        Self {
            data: Weak::new(),
            ctx: Default::default(),
            control: Mutex::new(Control {
                session: None,
                retry: Instant::now(),
            }),
            toast: Arc::new(|_: &Notice| {}),
        }
    }
    /// A page shows Docker: read every 2 s for the next 6 s, with the
    /// details of `selected`.
    pub fn request(&self, selected: Option<&str>) {
        let control = self.start();
        if let Some(session) = &control.session {
            let mut request = session
                .latest
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            request.selected = selected.map(str::to_owned);
            request.until = Instant::now() + Duration::from_secs(6);
            drop(request);
            let _ = session.signal.try_send(());
        }
    }
    /// Container alerts are on: make sure the reader runs. It keeps reading
    /// in the background for as long as they stay on.
    pub fn watch(&self) {
        drop(self.start());
    }
    fn start(&self) -> std::sync::MutexGuard<'_, Control> {
        let mut control = self.control.lock().unwrap_or_else(PoisonError::into_inner);
        if self.data.strong_count() == 0 {
            return control;
        }
        if control.session.is_none() && Instant::now() >= control.retry {
            let (signal, receiver) = mpsc::sync_channel(1);
            let latest = Arc::new(Mutex::new(Request {
                selected: None,
                until: Instant::now(),
            }));
            let stop = Arc::new(AtomicBool::new(false));
            let (requests, stopping, data, ctx) = (
                latest.clone(),
                stop.clone(),
                self.data.clone(),
                self.ctx.clone(),
            );
            let toast = self.toast.clone();
            match std::thread::Builder::new()
                .name("docker-monitor".into())
                .spawn(move || {
                    let mut sampler = wtm_core::docker::Sampler::default();
                    run(
                        receiver,
                        requests,
                        stopping,
                        data,
                        ctx,
                        move |selected, stop| sampler.sample(selected, stop),
                        move |notice| toast(notice),
                        crate::container_history::path(),
                    );
                }) {
                Ok(worker) => {
                    control.session = Some(Session {
                        signal,
                        latest,
                        stop,
                        worker,
                    })
                }
                Err(error) => {
                    control.retry = Instant::now() + Duration::from_secs(30);
                    if let Some(data) = self.data.upgrade() {
                        data.lock().unwrap_or_else(PoisonError::into_inner).docker =
                            Arc::new(wtm_core::docker::State {
                                report: None,
                                error: Some(format!("Docker monitor could not start: {error}")),
                            });
                    }
                }
            }
        }
        control
    }
}
// The reader's inputs and outputs, each a seam the tests replace.
#[allow(clippy::too_many_arguments)]
fn run(
    receiver: mpsc::Receiver<()>,
    requests: Arc<Mutex<Request>>,
    stopping: Arc<AtomicBool>,
    data: Weak<Mutex<Data>>,
    ctx: eframe::egui::Context,
    mut sample: impl FnMut(Option<&str>, &AtomicBool) -> Result<wtm_core::docker::Report, String>,
    toast: impl Fn(&Notice),
    store: Option<PathBuf>,
) {
    let mut next = Instant::now();
    let mut selected = None;
    let mut was_shown = false;
    let mut saved = Instant::now();
    if let (Some(path), Some(data)) = (&store, data.upgrade()) {
        let loaded = History::load(path, SystemTime::now());
        data.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .container_history = loaded;
    }
    while !stopping.load(Ordering::Relaxed) {
        if matches!(
            receiver.recv_timeout(Duration::from_millis(200)),
            Err(mpsc::RecvTimeoutError::Disconnected)
        ) {
            break;
        }
        let (requested, until) = {
            let r = requests.lock().unwrap_or_else(PoisonError::into_inner);
            (r.selected.clone(), r.until)
        };
        let shown = Instant::now() < until;
        if requested != selected || (shown && !was_shown) {
            selected = requested;
            next = Instant::now();
        }
        was_shown = shown;
        let Some(watching) = data.upgrade().map(|data| {
            data.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .container_alerts
                .settings
                .enabled
        }) else {
            break;
        };
        if Instant::now() < next || !(shown || watching) {
            continue;
        }
        // Details and logs only for a page that shows them.
        let report = sample(selected.as_deref().filter(|_| shown), &stopping);
        if stopping.load(Ordering::Relaxed) {
            break;
        }
        let Some(data) = data.upgrade() else {
            break;
        };
        let notices = {
            let mut data = data.lock().unwrap_or_else(PoisonError::into_inner);
            let mut state = (*data.docker).clone();
            let read = report.is_ok();
            match report {
                Ok(report) => {
                    state.report = Some(report);
                    state.error = None;
                }
                Err(error) => state.error = Some(error),
            }
            if let Some(report) = state.report.as_ref().filter(|_| read) {
                data.container_history
                    .record(Instant::now(), SystemTime::now(), report);
            }
            let notices = data.container_alerts.update(Instant::now(), &state);
            data.docker = Arc::new(state);
            notices
        };
        // Many containers crossing a limit together: the first few as
        // notifications, all of them on the Containers page.
        notices
            .iter()
            .take(crate::container_alerts::TOASTS_PER_CHECK)
            .for_each(&toast);
        ctx.request_repaint();
        next = Instant::now() + if shown { SHOWN } else { BACKGROUND };
        if saved.elapsed() >= crate::container_history::SAVE_EVERY {
            save(&store, &data, false);
            saved = Instant::now();
        } else {
            // A cleared history is forgotten on disk right away.
            save(&store, &data, true);
        }
    }
    // Minutes recorded since the last save are lost if the window process
    // ends before this runs.
    if let Some(data) = data.upgrade() {
        save(&store, &data, false);
    }
}

/// Writes the history when it changed (only once it was emptied, with
/// `cleared_only`). It is copied under the lock and written after it, so a
/// slow disk never holds up the window.
fn save(store: &Option<PathBuf>, data: &Mutex<Data>, cleared_only: bool) {
    let Some(path) = store else {
        return;
    };
    let copy = {
        let mut data = data.lock().unwrap_or_else(PoisonError::into_inner);
        let history = &mut data.container_history;
        if !history.dirty || (cleared_only && !history.cleared) {
            return;
        }
        history.dirty = false;
        history.cleared = false;
        history.clone()
    };
    if copy.save(path).is_err() {
        // Tried again at the next save.
        data.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .container_history
            .dirty = true;
    }
}

impl Drop for DockerWorker {
    fn drop(&mut self) {
        let control = self
            .control
            .get_mut()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(session) = control.session.take() {
            session.stop.store(true, Ordering::Relaxed);
            let _ = session.signal.try_send(());
            let until = Instant::now() + Duration::from_secs(2);
            while !session.worker.is_finished() && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            if session.worker.is_finished() {
                let _ = session.worker.join();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    /// Runs the reader with no page asking for Docker, alerts `on` or off
    /// with a CPU limit of `cpu`, until it has read once or `wait` has
    /// passed. Returns the readings, the notifications and the notices kept.
    fn background(
        on: bool,
        cpu: f32,
        wait: Duration,
        store: Option<PathBuf>,
    ) -> (Vec<Option<String>>, Vec<String>, usize) {
        let data = Arc::new(Mutex::new(Data::default()));
        {
            let mut d = data.lock().unwrap();
            d.container_alerts.settings = crate::container_alerts::Settings {
                enabled: on,
                cpu,
                seconds: 0,
                ..Default::default()
            };
        }
        let requests = Arc::new(Mutex::new(Request {
            selected: Some("details".into()),
            // No page has asked for Docker recently.
            until: Instant::now(),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (_signal, receiver) = mpsc::sync_channel(1);
        let (read, reads) = mpsc::channel();
        let (shown, toasts) = mpsc::channel();
        let (weak, stopping) = (Arc::downgrade(&data), stop.clone());
        let worker = std::thread::spawn(move || {
            run(
                receiver,
                requests,
                stopping,
                weak,
                Default::default(),
                move |id, _| {
                    read.send(id.map(str::to_owned)).unwrap();
                    Ok(crate::containers::fixture().report.unwrap())
                },
                move |notice| shown.send(notice.text()).unwrap(),
                store,
            )
        });
        let first = reads.recv_timeout(wait).ok();
        // With alerts on, the reading is followed by its notice.
        let notified = if on {
            toasts.recv_timeout(wait).ok()
        } else {
            None
        };
        stop.store(true, Ordering::Relaxed);
        worker.join().unwrap();
        let toasts: Vec<String> = notified.into_iter().chain(toasts.try_iter()).collect();
        let d = data.lock().unwrap();
        assert_eq!(d.docker.report.is_some(), on, "the reading is published");
        (
            first.into_iter().collect(),
            toasts,
            d.container_alerts.notices.len(),
        )
    }

    #[test]
    fn alerts_keep_docker_read_in_the_background_and_notify() {
        // Of the fixture's running containers only database-3 is above 15%:
        // 72.5% in Docker's count, 18% of its 4 cores.
        let (reads, toasts, kept) = background(true, 15.0, Duration::from_secs(3), None);
        // Read without a page asking, and without the details only a page shows.
        assert_eq!(reads, [None]);
        // database-4 is busier but has exited: it is not followed.
        assert_eq!((toasts.len(), kept), (1, 1), "{toasts:?}");
        assert!(
            toasts[0].starts_with("database-3 has used 18% of its CPUs for 0s. Docker shows 7"),
            "{toasts:?}"
        );
        let (reads, toasts, _) = background(false, 15.0, Duration::from_millis(600), None);
        assert!(
            reads.is_empty(),
            "alerts off and no page: Docker is not read"
        );
        assert!(toasts.is_empty());
    }

    #[test]
    fn many_containers_crossing_together_give_three_notifications() {
        // All four running fixture containers are above 1%.
        let (_, toasts, kept) = background(true, 1.0, Duration::from_secs(3), None);
        assert_eq!(kept, 4, "every notice is listed");
        assert_eq!(
            toasts.len(),
            crate::container_alerts::TOASTS_PER_CHECK,
            "{toasts:?}"
        );
    }

    #[test]
    fn readings_are_recorded_and_saved_when_the_reader_stops() {
        let dir = std::env::temp_dir().join(format!("wtm-docker-history-{}", std::process::id()));
        let path = dir.join("container-history.json");
        let (_, _, kept) = background(true, 1.0, Duration::from_secs(3), Some(path.clone()));
        assert_eq!(kept, 4);
        let saved = History::load(&path, SystemTime::now());
        for name in ["web-0", "web-1", "web-2", "database-3"] {
            assert_eq!(
                saved.series("desktop-linux", name).map(|s| s.minutes.len()),
                Some(1),
                "{name}"
            );
        }
        assert!(
            saved.series("desktop-linux", "database-4").is_none(),
            "an exited container is not recorded"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn clearing_the_history_reaches_the_file_before_the_next_reading_is_kept() {
        use wtm_core::docker::{Container, Endpoint, Metrics, Report};
        let dir = std::env::temp_dir().join(format!("wtm-docker-clear-{}", std::process::id()));
        let path = dir.join("container-history.json");
        let store = Some(path.clone());
        let report = |name: &str| Report {
            endpoint: Endpoint {
                context: "desktop".into(),
                pipe: r"\\.\pipe\docker_engine".into(),
            },
            containers: vec![Container {
                name: name.into(),
                state: "running".into(),
                metrics: Metrics {
                    cpu: Some(5.0),
                    measured: Some(Instant::now()),
                    ..Metrics::default()
                },
                ..Container::default()
            }],
            detail: None,
            detail_id: None,
            sampled: Instant::now(),
            truncated: false,
        };
        let data = Mutex::new(Data::default());
        let record = |name: &str| {
            data.lock().unwrap().container_history.record(
                Instant::now(),
                SystemTime::now(),
                &report(name),
            )
        };
        record("old");
        save(&store, &data, false);
        assert!(
            History::load(&path, SystemTime::now())
                .series("desktop", "old")
                .is_some()
        );
        data.lock().unwrap().container_history.clear();
        // The next reading comes in before the save that follows it.
        record("new");
        save(&store, &data, true);
        let saved = History::load(&path, SystemTime::now());
        assert!(
            saved.series("desktop", "old").is_none(),
            "forgotten on disk"
        );
        assert!(saved.series("desktop", "new").is_some());
        // Nothing cleared since: no early save.
        record("later");
        save(&store, &data, true);
        assert!(
            History::load(&path, SystemTime::now())
                .series("desktop", "later")
                .is_none()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn slow_docker_does_not_lock_process_data_and_shutdown_discards_its_reply() {
        let data = Arc::new(Mutex::new(Data::default()));
        let requests = Arc::new(Mutex::new(Request {
            selected: Some("original".into()),
            until: Instant::now() + Duration::from_secs(10),
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let (signal, receiver) = mpsc::sync_channel(1);
        let (entered, arrived) = mpsc::channel();
        let (release, gate) = mpsc::channel();
        let (weak, latest, stopping) = (Arc::downgrade(&data), requests.clone(), stop.clone());
        let worker = std::thread::spawn(move || {
            run(
                receiver,
                latest,
                stopping,
                weak,
                Default::default(),
                move |id, _| {
                    entered.send(id.map(str::to_owned)).unwrap();
                    gate.recv_timeout(Duration::from_secs(3)).unwrap();
                    Ok(crate::containers::fixture().report.unwrap())
                },
                |_| {},
                None,
            )
        });
        signal.send(()).unwrap();
        assert_eq!(
            arrived
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .as_deref(),
            Some("original")
        );
        assert!(
            data.try_lock().is_ok(),
            "process data remains accessible while Docker is blocked"
        );
        requests.lock().unwrap().selected = Some("replacement".into());
        stop.store(true, Ordering::Relaxed);
        release.send(()).unwrap();
        worker.join().unwrap();
        assert!(
            data.lock().unwrap().docker.report.is_none(),
            "a reply after shutdown is not published"
        );
    }
}
