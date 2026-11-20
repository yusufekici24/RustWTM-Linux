//! Names each disk, network adapter and GPU of a sample the same way after a
//! restart, for its per-minute history. Disk numbers and GPU LUIDs can change
//! when Windows starts, so disks go by model and GPUs by name (each with an
//! ordinal among equals); network adapters by their NET_LUID, which Windows
//! keeps. A device whose model or name cannot be read is not recorded,
//! rather than under a name that could later mean another device.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use wtm_core::SystemStats;

/// Most devices recorded per sample; further ones are left out.
pub const MAX_DEVICES: usize = 32;
/// A disk number not seen for this long is read again when it returns: it
/// may be another disk by then.
const DISK_FORGET: Duration = Duration::from_secs(60);
/// A disk or GPU whose name could not be read is tried again after this.
const RETRY: Duration = Duration::from_secs(600);

/// What kind of device a history belongs to; it says what its three
/// measures are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Active time (%), read and write (bytes per second).
    Disk,
    /// Received and sent (bytes per second).
    Network,
    /// Load (%), dedicated and shared memory in use (bytes).
    Gpu,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Network => "network",
            Self::Gpu => "gpu",
        }
    }
}

/// One device in one sample.
#[derive(Debug, Clone, PartialEq)]
pub struct Reading {
    /// The name kept across restarts, such as `disk:Samsung SSD 990 PRO 2TB#0`.
    pub key: String,
    pub kind: Kind,
    /// What the device is called, for the history's caption.
    pub label: String,
    pub values: [Option<f64>; 3],
}

/// Reads device names from Windows, on the [`Worker`]'s thread; a seam so
/// tests never touch hardware.
pub trait Names {
    /// The model of `\\.\PhysicalDrive{number}`, without reading SMART data
    /// or anything else that could wake the disk.
    fn disk_model(&mut self, number: u32) -> Option<String>;
    /// Every hardware GPU as `(luid, name)`, in DXGI order. Enumerates the
    /// adapters only; no Direct3D device is created.
    fn gpus(&mut self) -> Vec<(String, String)>;
}

/// The names Windows gives.
pub struct Windows;

impl Names for Windows {
    fn disk_model(&mut self, number: u32) -> Option<String> {
        wtm_sys::physical_disk::read(number)
            .ok()
            .map(|d| d.model.trim().to_owned())
            .filter(|m| !m.is_empty())
    }

    fn gpus(&mut self) -> Vec<(String, String)> {
        wtm_sys::gpu::adapters()
            .unwrap_or_default()
            .into_iter()
            .map(|a| (a.luid, a.name.trim().to_owned()))
            .collect()
    }
}

/// A name to look up.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ask {
    Disk(u32),
    Gpus,
}

/// A looked-up name.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// The disk's model; `None` when it could not be read.
    Disk(u32, Option<String>),
    /// Every hardware GPU as `(luid, name)`, in DXGI order.
    Gpus(Vec<(String, String)>),
}

/// Where names are asked for and answered later; never waits.
pub trait Lookup {
    fn ask(&mut self, ask: Ask);
    /// The answers that arrived since the last call.
    fn answers(&mut self) -> Vec<Answer>;
}

/// Looks names up on its own thread: opening a disk can hang (a failing
/// drive, a stuck USB bridge), and the service's sample loop, which also
/// protects the machine, must never wait for it. The thread is started on
/// the first question and ends when this is dropped, without being waited
/// for, so a lookup stuck in the driver cannot hold up the service's stop.
pub struct Worker<N> {
    names: Option<N>,
    asks: Option<std::sync::mpsc::Sender<Ask>>,
    answers: Option<std::sync::mpsc::Receiver<Answer>>,
    /// Asked and not answered yet: never asked twice.
    open: std::collections::HashSet<Ask>,
}

impl<N: Names + Send + 'static> Worker<N> {
    pub fn new(names: N) -> Self {
        Self {
            names: Some(names),
            asks: None,
            answers: None,
            open: std::collections::HashSet::new(),
        }
    }
}

impl<N: Names + Send + 'static> Lookup for Worker<N> {
    fn ask(&mut self, ask: Ask) {
        if !self.open.insert(ask.clone()) {
            return;
        }
        if self.asks.is_none()
            && let Some(mut names) = self.names.take()
        {
            let (asks, questions) = std::sync::mpsc::channel::<Ask>();
            let (replies, answers) = std::sync::mpsc::channel();
            let started = std::thread::Builder::new()
                .name("device-names".into())
                .spawn(move || {
                    for ask in questions {
                        let answer = match ask {
                            Ask::Disk(number) => Answer::Disk(number, names.disk_model(number)),
                            Ask::Gpus => Answer::Gpus(names.gpus()),
                        };
                        if replies.send(answer).is_err() {
                            break;
                        }
                    }
                });
            match started {
                Ok(_) => {
                    self.asks = Some(asks);
                    self.answers = Some(answers);
                }
                // Devices then stay unnamed and are not recorded.
                Err(e) => tracing::warn!(error = %e, "device names cannot be looked up"),
            }
        }
        if let Some(asks) = &self.asks {
            let _ = asks.send(ask);
        }
    }

    fn answers(&mut self) -> Vec<Answer> {
        let Some(answers) = &self.answers else {
            return Vec::new();
        };
        let got: Vec<Answer> = answers.try_iter().collect();
        for answer in &got {
            self.open.remove(&match answer {
                Answer::Disk(number, _) => Ask::Disk(*number),
                Answer::Gpus(_) => Ask::Gpus,
            });
        }
        got
    }
}

/// A disk's model as far as it is known.
#[derive(Debug, Clone, PartialEq)]
enum Model {
    /// Asked for, not answered yet.
    Pending,
    /// Answered at the instant given; `None`: it could not be read.
    Known(Option<String>, Instant),
}

/// Remembers the names it was told, so a device is looked up once, not on
/// every sample. A device is recorded from the first sample after its name
/// arrived.
pub struct Keys<L> {
    lookup: L,
    /// Disk number to its model and when the number was last seen.
    disks: HashMap<u32, (Model, Instant)>,
    /// `(luid, name)` in DXGI order, and when they were answered.
    gpus: Option<(Vec<(String, String)>, Instant)>,
    gpus_asked: bool,
}

impl<L: Lookup> Keys<L> {
    pub fn new(lookup: L) -> Self {
        Self {
            lookup,
            disks: HashMap::new(),
            gpus: None,
            gpus_asked: false,
        }
    }

    /// The devices of `stats` with their lasting names, and for each the
    /// id the window knows it by now (`disk:0`, `net:…`, `gpu:…`) mapped to
    /// that name. Never waits for a name: it is asked for and used once it
    /// has arrived.
    pub fn readings(
        &mut self,
        stats: &SystemStats,
        now: Instant,
    ) -> (Vec<Reading>, HashMap<String, String>) {
        for answer in self.lookup.answers() {
            match answer {
                Answer::Disk(number, model) => {
                    if let Some(entry) = self.disks.get_mut(&number) {
                        entry.0 = Model::Known(model, now);
                    }
                }
                Answer::Gpus(list) => {
                    self.gpus = Some((list, now));
                    self.gpus_asked = false;
                }
            }
        }

        let mut readings = Vec::new();
        let mut ids = HashMap::new();
        let mut add = |id: String, reading: Reading| {
            if readings.len() < MAX_DEVICES {
                ids.insert(id, reading.key.clone());
                readings.push(reading);
            }
        };

        self.disks
            .retain(|_, (_, seen)| now.saturating_duration_since(*seen) < DISK_FORGET);
        let mut disks: Vec<_> = stats
            .disks
            .iter()
            .filter_map(|d| Some((d.number()?, d)))
            .collect();
        disks.sort_by_key(|(n, _)| *n);
        let mut models: Vec<String> = Vec::new();
        for (number, disk) in disks {
            let new = !self.disks.contains_key(&number);
            let entry = self.disks.entry(number).or_insert((Model::Pending, now));
            let ask = new
                || matches!(&entry.0, Model::Known(None, read)
                    if now.saturating_duration_since(*read) >= RETRY);
            if ask {
                entry.0 = Model::Pending;
                self.lookup.ask(Ask::Disk(number));
            }
            entry.1 = now;
            let Model::Known(Some(model), _) = &entry.0 else {
                continue;
            };
            let model = model.clone();
            let nth = models.iter().filter(|m| **m == model).count();
            models.push(model.clone());
            add(
                format!("disk:{number}"),
                Reading {
                    key: format!("disk:{model}#{nth}"),
                    kind: Kind::Disk,
                    label: model,
                    values: [
                        Some(f64::from(disk.active_percent)),
                        Some(disk.read_rate),
                        Some(disk.write_rate),
                    ],
                },
            );
        }

        let known = |gpus: &Option<(Vec<(String, String)>, Instant)>, luid: &str| {
            gpus.as_ref()
                .is_some_and(|(list, _)| list.iter().any(|(l, _)| l == luid))
        };
        let unknown = stats.gpus.iter().any(|g| !known(&self.gpus, &g.luid));
        let due = self
            .gpus
            .as_ref()
            .is_none_or(|(_, read)| now.saturating_duration_since(*read) >= RETRY);
        // The GPUs are listed when the service starts and, while one stays
        // unknown (added later, or not a hardware GPU), every ten minutes.
        if unknown && due && !self.gpus_asked {
            self.gpus_asked = true;
            self.lookup.ask(Ask::Gpus);
        }
        if let Some((list, _)) = &self.gpus {
            let mut names: Vec<&str> = Vec::new();
            let mut ordinals: HashMap<&str, usize> = HashMap::new();
            for (luid, name) in list {
                let nth = names.iter().filter(|n| **n == name).count();
                names.push(name);
                ordinals.insert(luid, nth);
            }
            let mut gpus: Vec<_> = stats
                .gpus
                .iter()
                .filter_map(|g| Some((list.iter().position(|(l, _)| *l == g.luid)?, g)))
                .collect();
            gpus.sort_by_key(|(i, _)| *i);
            for (index, g) in gpus {
                let name = &list[index].1;
                if name.is_empty() {
                    continue;
                }
                let nth = ordinals.get(g.luid.as_str()).copied().unwrap_or(0);
                add(
                    format!("gpu:{}", g.luid),
                    Reading {
                        key: format!("gpu:{name}#{nth}"),
                        kind: Kind::Gpu,
                        label: name.clone(),
                        values: [
                            Some(f64::from(g.percent)),
                            g.dedicated_used.map(|b| b as f64),
                            g.shared_used.map(|b| b as f64),
                        ],
                    },
                );
            }
        }

        // Last, so many virtual adapters cannot crowd out disks and GPUs.
        for a in &stats.net_adapters {
            add(
                format!("net:{:016x}", a.luid),
                Reading {
                    key: format!("net:{:016x}", a.luid),
                    kind: Kind::Network,
                    label: a.name.clone(),
                    values: [Some(a.rx_rate), Some(a.tx_rate), None],
                },
            );
        }
        (readings, ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use wtm_core::NetAdapterUsage;
    use wtm_sys::disk::DiskUsage;
    use wtm_sys::gpu::GpuAdapterUsage;

    /// Answers after `delay` calls of `answers`, from fixed names.
    #[derive(Default)]
    struct Fake {
        models: HashMap<u32, String>,
        gpus: Vec<(String, String)>,
        delay: usize,
        asked: Vec<Ask>,
        queue: VecDeque<(usize, Ask)>,
    }

    impl Lookup for &mut Fake {
        fn ask(&mut self, ask: Ask) {
            self.asked.push(ask.clone());
            self.queue.push_back((self.delay, ask));
        }
        fn answers(&mut self) -> Vec<Answer> {
            let mut out = Vec::new();
            for (wait, ask) in std::mem::take(&mut self.queue) {
                if wait > 0 {
                    self.queue.push_back((wait - 1, ask));
                    continue;
                }
                out.push(match ask {
                    Ask::Disk(n) => Answer::Disk(n, self.models.get(&n).cloned()),
                    Ask::Gpus => Answer::Gpus(self.gpus.clone()),
                });
            }
            out
        }
    }

    fn disk(name: &str, active: f32) -> DiskUsage {
        DiskUsage {
            name: name.into(),
            active_percent: active,
            read_rate: 1000.0,
            write_rate: 2000.0,
            ..DiskUsage::default()
        }
    }

    fn gpu(luid: &str, percent: f32) -> GpuAdapterUsage {
        GpuAdapterUsage {
            luid: luid.into(),
            percent,
            dedicated_used: Some(512),
            ..GpuAdapterUsage::default()
        }
    }

    fn stats(disks: Vec<DiskUsage>, gpus: Vec<GpuAdapterUsage>) -> SystemStats {
        SystemStats {
            disks,
            gpus,
            net_adapters: vec![NetAdapterUsage {
                luid: 0x0006_0000_0100_0000,
                name: "Ethernet".into(),
                rx_rate: 10.0,
                tx_rate: 20.0,
                ..NetAdapterUsage::default()
            }],
            ..SystemStats::default()
        }
    }

    /// Two samples a second apart: the first asks, the second has answers.
    fn settled<L: Lookup>(
        keys: &mut Keys<L>,
        s: &SystemStats,
        t: Instant,
    ) -> (Vec<Reading>, HashMap<String, String>) {
        keys.readings(s, t);
        keys.readings(s, t + Duration::from_secs(1))
    }

    #[test]
    fn devices_get_names_that_survive_a_restart() {
        let mut fake = Fake {
            models: HashMap::from([
                (0, "SSD A".into()),
                (1, "HDD B".into()),
                (2, "SSD A".into()),
            ]),
            gpus: vec![
                ("0000_0001".into(), "GPU X".into()),
                ("0000_0002".into(), "GPU X".into()),
            ],
            ..Fake::default()
        };
        let t = Instant::now();
        let s = stats(
            vec![disk("2 E:", 3.0), disk("0 C:", 1.0), disk("1 D:", 2.0)],
            vec![gpu("0000_0002", 7.0), gpu("0000_0001", 5.0)],
        );
        let (readings, ids) = settled(&mut Keys::new(&mut fake), &s, t);
        let keys: Vec<_> = readings.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(
            keys,
            [
                "disk:SSD A#0",
                "disk:HDD B#0",
                "disk:SSD A#1",
                "gpu:GPU X#0",
                "gpu:GPU X#1",
                "net:0006000001000000",
            ]
        );
        assert_eq!(ids["disk:2"], "disk:SSD A#1");
        assert_eq!(ids["gpu:0000_0002"], "gpu:GPU X#1");
        assert_eq!(ids["net:0006000001000000"], "net:0006000001000000");
        assert_eq!(readings[0].values, [Some(1.0), Some(1000.0), Some(2000.0)]);
        assert_eq!(readings[3].values, [Some(5.0), Some(512.0), None]);

        // After a restart the GPUs have other LUIDs: same names.
        fake.gpus = vec![
            ("0000_0009".into(), "GPU X".into()),
            ("0000_0008".into(), "GPU X".into()),
        ];
        let s = stats(vec![], vec![gpu("0000_0008", 1.0)]);
        let (readings, ids) = settled(&mut Keys::new(&mut fake), &s, t);
        assert_eq!(readings[0].key, "gpu:GPU X#1");
        assert_eq!(ids["gpu:0000_0008"], "gpu:GPU X#1");
    }

    #[test]
    fn a_slow_name_is_waited_for_by_nobody_and_recorded_once_it_arrives() {
        let mut fake = Fake {
            models: HashMap::from([(0, "SSD A".into())]),
            gpus: vec![("0000_0001".into(), "GPU X".into())],
            delay: 3,
            ..Fake::default()
        };
        let t = Instant::now();
        let s = stats(vec![disk("0 C:", 1.0)], vec![gpu("0000_0001", 1.0)]);
        let mut keys = Keys::new(&mut fake);
        // Only the adapter, which needs no lookup, while the names are out.
        for second in 0..4 {
            let (readings, _) = keys.readings(&s, t + Duration::from_secs(second));
            assert_eq!(readings.len(), 1, "second {second}");
            assert_eq!(readings[0].kind, Kind::Network);
        }
        let (readings, _) = keys.readings(&s, t + Duration::from_secs(4));
        assert_eq!(readings.len(), 3);
        drop(keys);
        assert_eq!(fake.asked, [Ask::Disk(0), Ask::Gpus], "asked once each");
    }

    #[test]
    fn names_are_asked_once_and_unreadable_devices_are_left_out() {
        let mut fake = Fake {
            models: HashMap::from([(0, "SSD A".into())]),
            gpus: vec![("0000_0001".into(), "GPU X".into())],
            ..Fake::default()
        };
        let t = Instant::now();
        let mut keys = Keys::new(&mut fake);
        // Disk 1 has no readable model; GPU 0000_0005 is not a hardware GPU.
        let s = stats(
            vec![disk("0 C:", 1.0), disk("1 D:", 1.0)],
            vec![gpu("0000_0001", 1.0), gpu("0000_0005", 1.0)],
        );
        keys.readings(&s, t);
        for second in 1..30 {
            let (readings, _) = keys.readings(&s, t + Duration::from_secs(second));
            assert_eq!(readings.len(), 3, "disk 0, GPU X, the adapter");
        }
        // A disk missing from a few samples keeps its name.
        let only_c = stats(vec![disk("0 C:", 1.0)], vec![]);
        keys.readings(&only_c, t + Duration::from_secs(40));
        // Ten minutes on, the unreadable ones are asked for again, once.
        for second in (50..=700).step_by(30) {
            keys.readings(&s, t + Duration::from_secs(second));
        }
        drop(keys);
        assert_eq!(
            fake.asked,
            [
                Ask::Disk(0),
                Ask::Disk(1),
                Ask::Gpus,
                Ask::Disk(1),
                Ask::Gpus
            ]
        );
    }

    #[test]
    fn a_disk_number_gone_for_a_minute_is_asked_for_again() {
        let mut fake = Fake {
            models: HashMap::from([(1, "USB A".into())]),
            ..Fake::default()
        };
        let t = Instant::now();
        let mut keys = Keys::new(&mut fake);
        let e = stats(vec![disk("1 E:", 1.0)], vec![]);
        settled(&mut keys, &e, t);
        keys.readings(&stats(vec![], vec![]), t + Duration::from_secs(30));
        keys.readings(&stats(vec![], vec![]), t + Duration::from_secs(90));
        let (readings, _) = settled(&mut keys, &e, t + Duration::from_secs(91));
        assert_eq!(readings[0].key, "disk:USB A#0");
        drop(keys);
        assert_eq!(fake.asked, [Ask::Disk(1), Ask::Disk(1)]);
    }

    #[test]
    fn at_most_the_device_limit_is_recorded_and_adapters_come_last() {
        let mut fake = Fake {
            models: HashMap::from([(0, "SSD A".into())]),
            ..Fake::default()
        };
        let mut s = stats(vec![disk("0 C:", 1.0)], vec![]);
        s.net_adapters = (0..MAX_DEVICES as u64 + 5)
            .map(|luid| NetAdapterUsage {
                luid,
                ..NetAdapterUsage::default()
            })
            .collect();
        let (readings, ids) = settled(&mut Keys::new(&mut fake), &s, Instant::now());
        assert_eq!(readings.len(), MAX_DEVICES);
        assert_eq!(ids.len(), MAX_DEVICES);
        assert_eq!(readings[0].key, "disk:SSD A#0");
    }

    /// Names from fixed lists, slowly for one disk; no hardware.
    struct SlowNames;

    impl Names for SlowNames {
        fn disk_model(&mut self, number: u32) -> Option<String> {
            if number == 9 {
                std::thread::sleep(Duration::from_millis(300));
            }
            Some(format!("Disk {number}"))
        }
        fn gpus(&mut self) -> Vec<(String, String)> {
            vec![("0000_0001".into(), "GPU X".into())]
        }
    }

    #[test]
    fn the_worker_answers_on_its_own_thread_and_never_twice() {
        let mut worker = Worker::new(SlowNames);
        assert!(worker.answers().is_empty(), "nothing started yet");
        let started = Instant::now();
        worker.ask(Ask::Disk(9));
        worker.ask(Ask::Disk(9));
        worker.ask(Ask::Gpus);
        assert!(
            started.elapsed() < Duration::from_millis(200),
            "asking never waits"
        );
        let mut got = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while got.len() < 2 && Instant::now() < deadline {
            got.extend(worker.answers());
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            got,
            [
                Answer::Disk(9, Some("Disk 9".into())),
                Answer::Gpus(vec![("0000_0001".into(), "GPU X".into())]),
            ]
        );
        std::thread::sleep(Duration::from_millis(50));
        assert!(worker.answers().is_empty(), "the second ask was not sent");
        // Answered: may be asked again.
        worker.ask(Ask::Disk(9));
        assert!(worker.open.contains(&Ask::Disk(9)));
    }
}
