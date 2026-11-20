//! GPU load per process and per engine from DRM fdinfo
//! (`/proc/PID/fdinfo/N`: `drm-engine-<name>: <ns>`), which i915, xe,
//! amdgpu, nouveau, msm and panfrost publish. Each DRM client is counted
//! once by `drm-client-id`, so duplicated descriptors do not double it.
//!
//! An adapter's "LUID" is its PCI address packed as
//! `{domain:08x}_{bus<<8|dev<<3|fn:08x}`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::time::Instant;

use crate::Result;
use crate::linux::util::{self, read_trim};

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuUsage {
    pub percent: f32,
    pub dedicated_bytes: u64,
    pub shared_bytes: u64,
    pub engine: Option<(String, String)>,
}

#[derive(Debug, Default, Clone)]
pub struct GpuSample {
    pub per_process: HashMap<u32, GpuUsage>,
    pub total_percent: f32,
    pub engines: Vec<(String, f32)>,
    pub adapters: Vec<GpuAdapterUsage>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GpuAdapterUsage {
    pub luid: String,
    pub percent: f32,
    pub engines: Vec<(String, f32)>,
    pub dedicated_used: Option<u64>,
    pub shared_used: Option<u64>,
}

const MAX_ADAPTERS: usize = 16;

/// The Windows engine type name for a DRM engine.
pub(crate) fn engine_type(drm: &str) -> &'static str {
    match drm {
        "render" | "gfx" | "3d" => "3D",
        "copy" | "blt" | "sdma" | "dma" => "Copy",
        "video" | "dec" | "vcn" | "vcn_unified" | "vcs" => "VideoDecode",
        "enc" | "enc_1" | "vce" => "VideoEncode",
        "video-enhance" | "vecs" | "jpeg" => "VideoProcessing",
        "compute" | "ccs" => "Compute",
        _ => "Other",
    }
}

/// `0000:00:02.0` as a LUID string.
pub(crate) fn luid_of_pci(pdev: &str) -> Option<String> {
    let (domain, rest) = pdev.split_once(':')?;
    let (bus, rest) = rest.split_once(':')?;
    let (dev, func) = rest.split_once('.')?;
    let domain = u32::from_str_radix(domain, 16).ok()?;
    let low = (u32::from_str_radix(bus, 16).ok()? << 8)
        | (u32::from_str_radix(dev, 16).ok()? << 3)
        | u32::from_str_radix(func, 16).ok()?;
    Some(format!("{domain:08x}_{low:08x}"))
}

#[derive(Debug, Clone, Default)]
struct Client {
    pid: u32,
    luid: String,
    /// Busy nanoseconds per engine type, and the engine's capacity.
    engines: Vec<(&'static str, u64, u32)>,
    resident: u64,
    local: u64,
}

fn parse_fdinfo(pid: u32, text: &str) -> Option<(u64, Client)> {
    let mut id = None;
    let mut client = Client {
        pid,
        ..Default::default()
    };
    let mut capacity: HashMap<String, u32> = HashMap::new();
    let mut busy: Vec<(String, u64)> = Vec::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let number = || {
            value
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<u64>().ok())
        };
        let bytes = || {
            let mut p = value.split_whitespace();
            let n: u64 = p.next()?.parse().ok()?;
            Some(match p.next() {
                Some("KiB") => n << 10,
                Some("MiB") => n << 20,
                Some("GiB") => n << 30,
                _ => n,
            })
        };
        if key == "drm-client-id" {
            id = number();
        } else if key == "drm-pdev" {
            client.luid = luid_of_pci(value).unwrap_or_default();
        } else if let Some(e) = key.strip_prefix("drm-engine-capacity-") {
            capacity.insert(e.to_owned(), number().unwrap_or(1) as u32);
        } else if let Some(e) = key.strip_prefix("drm-engine-") {
            busy.push((e.to_owned(), number().unwrap_or(0)));
        } else if key.starts_with("drm-resident-") {
            let local = key.ends_with("local") || key.ends_with("vram") || key.ends_with("vram0");
            let b = bytes().unwrap_or(0);
            if local {
                client.local += b;
            } else {
                client.resident += b;
            }
        } else if key.starts_with("drm-memory-") && !key.contains("resident") {
            // Older amdgpu: drm-memory-vram / gtt / cpu.
            let b = bytes().unwrap_or(0);
            if key.ends_with("vram") {
                client.local += b;
            } else {
                client.resident += b;
            }
        }
    }
    let id = id?;
    for (name, ns) in busy {
        let cap = capacity.get(&name).copied().unwrap_or(1).max(1);
        client.engines.push((engine_type(&name), ns, cap));
    }
    Some((id, client))
}

#[derive(Debug, Default)]
pub struct GpuSampler {
    /// DRM descriptors per process, re-scanned for new processes and every
    /// 10 seconds, so steady sampling reads only fdinfo files.
    drm_fds: HashMap<u32, Vec<u32>>,
    scanned: Option<Instant>,
    prev: Option<(Instant, Clients)>,
}

/// DRM clients by (device, client id).
type Clients = HashMap<(String, u64), Client>;

fn drm_fds_of(pid: u32) -> Vec<u32> {
    let dir = format!("/proc/{pid}/fd");
    let Ok(rd) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    rd.flatten()
        .filter(|e| {
            fs::read_link(e.path()).is_ok_and(|t| t.to_string_lossy().starts_with("/dev/dri/"))
        })
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect()
}

impl GpuSampler {
    pub fn new() -> Result<Self> {
        if !fs::metadata("/dev/dri").is_ok_and(|m| m.is_dir()) {
            return Err(crate::Error::Unsupported("GPU counters (no /dev/dri)"));
        }
        Ok(Self::default())
    }

    fn rescan(&mut self, now: Instant) {
        let pids: HashSet<u32> = util::numeric_entries("/proc").into_iter().collect();
        let full = self
            .scanned
            .is_none_or(|t| now.duration_since(t).as_secs() >= 10);
        self.drm_fds.retain(|pid, _| pids.contains(pid));
        for pid in pids {
            if full || !self.drm_fds.contains_key(&pid) {
                let fds = drm_fds_of(pid);
                self.drm_fds.insert(pid, fds);
            }
        }
        if full {
            self.scanned = Some(now);
        }
    }

    pub fn sample(&mut self) -> Result<GpuSample> {
        let now = Instant::now();
        self.rescan(now);
        let mut clients: HashMap<(String, u64), Client> = HashMap::new();
        for (&pid, fds) in &self.drm_fds {
            for fd in fds {
                let Ok(text) = fs::read_to_string(format!("/proc/{pid}/fdinfo/{fd}")) else {
                    continue;
                };
                if let Some((id, c)) = parse_fdinfo(pid, &text) {
                    clients.entry((c.luid.clone(), id)).or_insert(c);
                }
            }
        }
        let out = match &self.prev {
            Some((at, prev)) => {
                aggregate(&clients, prev, now.duration_since(*at).as_nanos() as f64)
            }
            None => aggregate(&clients, &HashMap::new(), 0.0),
        };
        self.prev = Some((now, clients));
        Ok(out)
    }
}

fn aggregate(
    now: &HashMap<(String, u64), Client>,
    prev: &HashMap<(String, u64), Client>,
    elapsed_ns: f64,
) -> GpuSample {
    let mut out = GpuSample::default();
    // (luid, engine) -> summed percent.
    let mut by_engine: HashMap<(String, &'static str), f64> = HashMap::new();
    let mut busiest: HashMap<u32, (f64, String, &'static str)> = HashMap::new();
    for (key, c) in now {
        let usage = out.per_process.entry(c.pid).or_default();
        usage.dedicated_bytes += c.local;
        usage.shared_bytes += c.resident;
        let Some(p) = prev.get(key) else { continue };
        if elapsed_ns <= 0.0 {
            continue;
        }
        for &(engine, ns, cap) in &c.engines {
            let before = p.engines.iter().find(|e| e.0 == engine).map_or(ns, |e| e.1);
            let pct = ns.saturating_sub(before) as f64 / elapsed_ns / f64::from(cap) * 100.0;
            *by_engine.entry((c.luid.clone(), engine)).or_default() += pct;
            let b = busiest
                .entry(c.pid)
                .or_insert((0.0, c.luid.clone(), engine));
            if pct > b.0 {
                *b = (pct, c.luid.clone(), engine);
            }
        }
    }
    for (pid, (pct, luid, engine)) in busiest {
        if let Some(u) = out.per_process.get_mut(&pid) {
            u.percent = pct.clamp(0.0, 100.0) as f32;
            if pct > 0.0 {
                u.engine = Some((luid, engine.to_owned()));
            }
        }
    }
    let mut totals: HashMap<&'static str, f64> = HashMap::new();
    for ((luid, engine), pct) in by_engine {
        let t = totals.entry(engine).or_default();
        *t = t.max(pct);
        let i = match out.adapters.binary_search_by(|a| a.luid.cmp(&luid)) {
            Ok(i) => i,
            Err(_) if out.adapters.len() >= MAX_ADAPTERS => continue,
            Err(i) => {
                out.adapters.insert(
                    i,
                    GpuAdapterUsage {
                        luid: luid.clone(),
                        ..Default::default()
                    },
                );
                i
            }
        };
        out.adapters[i]
            .engines
            .push((engine.to_owned(), pct.min(100.0) as f32));
    }
    for (key, c) in now {
        if let Ok(i) = out.adapters.binary_search_by(|a| a.luid.cmp(&key.0)) {
            let a = &mut out.adapters[i];
            a.dedicated_used = Some(a.dedicated_used.unwrap_or(0) + c.local);
            a.shared_used = Some(a.shared_used.unwrap_or(0) + c.resident);
        }
    }
    for a in &mut out.adapters {
        a.engines.sort_by(|x, y| x.0.cmp(&y.0));
        a.percent = a.engines.iter().fold(0.0, |m, e| m.max(e.1));
    }
    out.engines = totals
        .iter()
        .map(|(n, v)| ((*n).to_owned(), v.min(100.0) as f32))
        .collect();
    out.engines.sort_by(|a, b| a.0.cmp(&b.0));
    out.total_percent = totals.values().fold(0.0f64, |a, b| a.max(*b)).min(100.0) as f32;
    out
}

/// A GPU's identity.
#[derive(Debug, Clone)]
pub struct Adapter {
    pub luid: String,
    pub name: String,
    pub dedicated_bytes: u64,
    pub shared_bytes: u64,
    pub vendor_id: u32,
    pub device_id: u32,
    pub luid_value: u64,
}

fn hex_file(path: String) -> u32 {
    read_trim(path)
        .and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        .unwrap_or(0)
}

/// The PCI name of a device from the udev hardware database.
pub(crate) fn pci_name(slot: &str) -> Option<String> {
    let db = fs::read_to_string(format!("/run/udev/data/+pci:{slot}")).ok()?;
    let vendor = db
        .lines()
        .find_map(|l| l.strip_prefix("E:ID_VENDOR_FROM_DATABASE="));
    let model = db
        .lines()
        .find_map(|l| l.strip_prefix("E:ID_MODEL_FROM_DATABASE="))?;
    Some(match vendor {
        Some(v) => format!("{} {model}", v.split_whitespace().next().unwrap_or(v)),
        None => model.to_owned(),
    })
}

/// The display adapters (DRM cards backed by a PCI device).
pub fn adapters() -> Result<Vec<Adapter>> {
    let mut out: Vec<Adapter> = Vec::new();
    let Ok(rd) = fs::read_dir("/sys/class/drm") else {
        return Ok(out);
    };
    let mut cards: Vec<String> = rd
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("card") && !n.contains('-'))
        .collect();
    cards.sort();
    let ram = crate::system::memory_status()
        .map(|m| m.total_phys)
        .unwrap_or(0);
    for card in cards {
        let dev = format!("/sys/class/drm/{card}/device");
        let Ok(real) = fs::canonicalize(&dev) else {
            continue;
        };
        let slot = real
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Some(luid) = luid_of_pci(&slot) else {
            continue;
        };
        if out.iter().any(|a| a.luid == luid) {
            continue;
        }
        let vendor_id = hex_file(format!("{dev}/vendor"));
        let device_id = hex_file(format!("{dev}/device"));
        let dedicated =
            crate::linux::util::read_u64(format!("{dev}/mem_info_vram_total")).unwrap_or(0);
        let luid_value = u64::from_str_radix(&luid.replace('_', ""), 16).unwrap_or(0);
        out.push(Adapter {
            name: pci_name(&slot).unwrap_or_else(|| format!("GPU {vendor_id:04x}:{device_id:04x}")),
            luid,
            dedicated_bytes: dedicated,
            // As on Windows: up to half of RAM can be lent to the GPU.
            shared_bytes: ram / 2,
            vendor_id,
            device_id,
            luid_value,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const I915: &str = "pos:\t0\nflags:\t02100002\nmnt_id:\t26\nino:\t1039\ndrm-driver:\ti915\ndrm-client-id:\t42\ndrm-pdev:\t0000:00:02.0\ndrm-total-system0:\t12 MiB\ndrm-resident-system0:\t10 MiB\ndrm-engine-render:\t1000000000 ns\ndrm-engine-copy:\t0 ns\ndrm-engine-video:\t500000000 ns\ndrm-engine-capacity-video:\t2\n";

    #[test]
    fn parses_fdinfo_and_rates() {
        let (id, c) = parse_fdinfo(7, I915).unwrap();
        assert_eq!(id, 42);
        assert_eq!(c.luid, "00000000_00000010");
        assert_eq!(c.resident, 10 << 20);
        let mut later = c.clone();
        later.engines = vec![
            ("3D", 1_500_000_000, 1),
            ("Copy", 0, 1),
            ("VideoDecode", 1_500_000_000, 2),
        ];
        let key = (c.luid.clone(), 42);
        let prev = HashMap::from([(key.clone(), c)]);
        let now = HashMap::from([(key, later)]);
        let s = aggregate(&now, &prev, 1e9);
        let p = &s.per_process[&7];
        assert!((p.percent - 50.0).abs() < 0.01, "{}", p.percent);
        assert_eq!(p.engine.as_ref().unwrap().1, "3D");
        assert!((s.total_percent - 50.0).abs() < 0.01);
        assert_eq!(s.adapters.len(), 1);
        assert!(
            s.engines
                .iter()
                .any(|(n, v)| n == "VideoDecode" && (*v - 50.0).abs() < 0.01)
        );
    }

    #[test]
    fn pci_luids() {
        assert_eq!(luid_of_pci("0000:03:00.0").unwrap(), "00000000_00000300");
    }
}
