//! The Linux counterpart of devices.ps1: disks, network adapters,
//! batteries, GPUs, the processor and memory, read from sysfs and procfs
//! into the same JSON shape [`crate::devices::parse`] checks.

use serde_json::{Value, json};
use std::fs;

fn stamp() -> u64 {
    crate::hardware::unix_ms()
}

fn read(path: impl AsRef<std::path::Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

fn num(path: impl AsRef<std::path::Path>) -> Option<f64> {
    read(path)?.parse().ok()
}

fn field(name: &str, value: Option<String>) -> Option<Value> {
    value
        .filter(|v| !v.is_empty())
        .map(|v| json!({"name": name, "value": v}))
}

fn metric(
    id: &str,
    name: &str,
    unit: &str,
    value: Option<f64>,
    ceiling: Option<f64>,
    counter: bool,
) -> Value {
    json!({"id": id, "name": name, "unit": unit, "value": value, "ceiling": ceiling, "counter": counter})
}

fn diskstats() -> std::collections::HashMap<String, (u64, u64, u64)> {
    let mut out = std::collections::HashMap::new();
    for l in fs::read_to_string("/proc/diskstats")
        .unwrap_or_default()
        .lines()
    {
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 14 {
            let n = |i: usize| f[i].parse::<u64>().unwrap_or(0);
            out.insert(f[2].to_owned(), (n(5), n(9), n(12)));
        }
    }
    out
}

fn disks(devices: &mut Vec<Value>, warnings: &mut Vec<String>) {
    let before = diskstats();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let after = diskstats();
    for number in 0..32u32 {
        let Ok(pd) = wtm_sys::physical_disk::read(number) else {
            break;
        };
        let kernel = disk_kernel_name(number).unwrap_or_default();
        let dev = format!("/sys/block/{kernel}/device");
        let serial = read(format!("{dev}/serial")).or_else(|| read(format!("{dev}/wwid")));
        let id = serial
            .clone()
            .unwrap_or_else(|| format!("session-disk-{number}"));
        let rate = |i: usize| -> Option<f64> {
            let (b, a) = (before.get(&kernel)?, after.get(&kernel)?);
            let (x, y) = match i {
                0 => (b.0, a.0),
                1 => (b.1, a.1),
                _ => (b.2, a.2),
            };
            Some(y.saturating_sub(x) as f64 * if i == 2 { 100.0 / 500.0 } else { 512.0 * 2.0 })
        };
        let temp = wtm_sys::physical_disk::temperature(number)
            .ok()
            .map(|t| f64::from(t.celsius));
        let fields: Vec<Value> = [
            field("Disk number", Some(number.to_string())),
            field("Model", Some(pd.model.clone())),
            field("Capacity (bytes)", pd.size.map(|s| s.to_string())),
            field("Bus", pd.bus_name().map(str::to_owned)),
            field("Device", Some(format!("/dev/{kernel}"))),
            field("Firmware", read(format!("{dev}/firmware_rev"))),
            field("Serial number", serial),
            field(
                "Storage health",
                Some(if pd.ssd == Some(true) {
                    "Solid state".into()
                } else {
                    "Rotational".into()
                }),
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        devices.push(json!({
            "id": format!("disk:{id}"),
            "kind": "Disk",
            "name": format!("Disk {number} - {}", pd.model),
            "at": stamp(),
            "fields": fields,
            "metrics": [
                metric("active", "Active time", "%", rate(2).map(|v| v.clamp(0.0, 100.0)), Some(100.0), false),
                metric("read", "Read", "B/s", rate(0), None, false),
                metric("write", "Write", "B/s", rate(1), None, false),
                metric("temperature", "Temperature", "C", temp, None, false),
            ],
            "warning": "",
        }));
    }
    if devices.is_empty() {
        warnings.push("No physical disks found".into());
    }
}

fn disk_kernel_name(number: u32) -> Option<String> {
    let mut names: Vec<String> = fs::read_dir("/sys/block")
        .ok()?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| {
            !["loop", "ram", "zram", "dm-", "md", "sr", "fd", "nbd"]
                .iter()
                .any(|p| n.starts_with(p))
        })
        .filter(|n| num(format!("/sys/block/{n}/size")).unwrap_or(0.0) > 0.0)
        .collect();
    names.sort_by(|a, b| wtm_sys::collate::natural(a, b));
    names.into_iter().nth(number as usize)
}

fn networks(devices: &mut Vec<Value>) {
    let Ok(adapters) = wtm_sys::system::net_adapters() else {
        return;
    };
    for a in adapters.into_iter().take(16) {
        let base = format!("/sys/class/net/{}", a.alias);
        let fields: Vec<Value> = [
            field("Adapter", Some(a.description.clone())),
            field("Status", read(format!("{base}/operstate"))),
            field(
                "Link speed",
                (a.link_speed > 0).then(|| format!("{} Mbps", a.link_speed / 1_000_000)),
            ),
            field("MAC address", read(format!("{base}/address"))),
            field(
                "Driver",
                fs::read_link(format!("{base}/device/driver"))
                    .ok()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())),
            ),
        ]
        .into_iter()
        .flatten()
        .collect();
        devices.push(json!({
            "id": format!("network:{}", a.alias),
            "kind": "Network",
            "name": a.alias,
            "at": stamp(),
            "warning": "",
            "fields": fields,
            "metrics": [
                metric("receive", "Receive", "B/s", Some(a.counters.received as f64), None, true),
                metric("send", "Send", "B/s", Some(a.counters.sent as f64), None, true),
            ],
        }));
    }
}

fn batteries(devices: &mut Vec<Value>, warnings: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir("/sys/class/power_supply") else {
        return;
    };
    let mut any = false;
    for p in rd.flatten().map(|e| e.path()).take(8) {
        if read(p.join("type")).as_deref() != Some("Battery")
            || read(p.join("scope")).as_deref() == Some("Device")
        {
            continue;
        }
        any = true;
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        // energy_* in µWh, charge_* in µAh (then scaled by voltage).
        let volts = num(p.join("voltage_now")).map(|v| v / 1e6);
        let wh = |key: &str| -> Option<f64> {
            num(p.join(format!("energy_{key}")))
                .map(|v| v / 1000.0)
                .or_else(|| {
                    let mah = num(p.join(format!("charge_{key}")))? / 1000.0;
                    let v = num(p.join("voltage_min_design"))
                        .map(|v| v / 1e6)
                        .or(volts)?;
                    Some(mah * v)
                })
        };
        let (design, full, now) = (wh("full_design"), wh("full"), wh("now"));
        let health = match (full, design) {
            (Some(f), Some(d)) if d > 0.0 => Some((100.0 * f / d).min(100.0)),
            _ => None,
        };
        let charge = match (now, full) {
            (Some(n), Some(f)) if f > 0.0 => Some((100.0 * n / f).min(100.0)),
            _ => num(p.join("capacity")),
        };
        let status = read(p.join("status")).unwrap_or_default();
        let watts = num(p.join("power_now"))
            .map(|w| w / 1e6)
            .or_else(|| Some(num(p.join("current_now"))? / 1e6 * volts?));
        let signed = watts.map(|w| if status == "Discharging" { -w } else { w });
        let fields: Vec<Value> = [
            field("Manufacturer", read(p.join("manufacturer"))),
            field("Design capacity (mWh)", design.map(|v| format!("{v:.0}"))),
            field("Full capacity (mWh)", full.map(|v| format!("{v:.0}"))),
            field(
                "AC connected",
                wtm_sys::power::battery()
                    .and_then(|b| b.on_ac)
                    .map(|v| v.to_string()),
            ),
            field("Charging", Some((status == "Charging").to_string())),
            field("Technology", read(p.join("technology"))),
        ]
        .into_iter()
        .flatten()
        .collect();
        devices.push(json!({
            "id": format!("battery:{name}"),
            "kind": "Battery",
            "name": read(p.join("model_name")).unwrap_or_else(|| "Battery".into()),
            "at": stamp(),
            "warning": "",
            "fields": fields,
            "metrics": [
                metric("charge", "Charge", "%", charge, Some(100.0), false),
                metric("health", "Capacity health", "%", health, Some(100.0), false),
                metric("power", "Charge / discharge", "W", signed, None, false),
                metric("voltage", "Voltage", "V", volts, None, false),
                metric("cycles", "Cycles", "", num(p.join("cycle_count")).filter(|c| *c > 0.0), None, false),
            ],
        }));
    }
    if !any {
        warnings
            .push("Battery telemetry unavailable (including desktops without a battery)".into());
    }
}

fn gpus(devices: &mut Vec<Value>, warnings: &mut Vec<String>) {
    let Ok(mut sampler) = wtm_sys::gpu::GpuSampler::new() else {
        warnings.push("GPU counters unavailable".into());
        return;
    };
    let _ = sampler.sample();
    std::thread::sleep(std::time::Duration::from_millis(500));
    let sample = sampler.sample().unwrap_or_default();
    for a in wtm_sys::gpu::adapters().unwrap_or_default() {
        let usage = sample.adapters.iter().find(|u| u.luid == a.luid);
        let mut metrics = vec![
            metric(
                "load",
                "Busiest engine",
                "%",
                usage.map(|u| f64::from(u.percent)),
                Some(100.0),
                false,
            ),
            metric(
                "dedicated",
                "Dedicated memory",
                "bytes",
                usage.and_then(|u| u.dedicated_used).map(|v| v as f64),
                None,
                false,
            ),
            metric(
                "shared",
                "Shared memory",
                "bytes",
                usage.and_then(|u| u.shared_used).map(|v| v as f64),
                None,
                false,
            ),
        ];
        for (name, v) in usage
            .map(|u| u.engines.clone())
            .unwrap_or_default()
            .into_iter()
            .take(16)
        {
            metrics.push(metric(
                &format!("engine:0:{name}"),
                &name,
                "%",
                Some(f64::from(v)),
                Some(100.0),
                false,
            ));
        }
        devices.push(json!({
            "id": format!("gpu:{}:0", a.luid),
            "kind": "GPU",
            "name": a.name,
            "at": stamp(),
            "fields": [{"name": "History scope", "value": "Since RustWTM started"}],
            "metrics": metrics,
            "warning": "",
        }));
    }
}

fn system(devices: &mut Vec<Value>) {
    let info = wtm_sys::cpu_info::info();
    let load = (|| {
        let a = wtm_sys::system::cpu_times().ok()?;
        std::thread::sleep(std::time::Duration::from_millis(300));
        let b = wtm_sys::system::cpu_times().ok()?;
        let total = (b.kernel + b.user).saturating_sub(a.kernel + a.user) as f64;
        let idle = b.idle.saturating_sub(a.idle) as f64;
        (total > 0.0).then(|| (100.0 * (1.0 - idle / total)).clamp(0.0, 100.0))
    })();
    let fields: Vec<Value> = [
        field("Models", info.name.clone()),
        field("Logical processors", Some(info.layout.logical.to_string())),
        field("Scope", Some("Whole system".into())),
    ]
    .into_iter()
    .flatten()
    .collect();
    devices.push(json!({
        "id": "cpu:system", "kind": "CPU", "name": "All processors", "at": stamp(), "warning": "",
        "fields": fields,
        "metrics": [metric("load", "CPU usage", "%", load, Some(100.0), false)],
    }));
    if let (Ok(m), Ok(c)) = (
        wtm_sys::system::memory_status(),
        wtm_sys::memory::composition(),
    ) {
        let total = m.total_phys as f64;
        devices.push(json!({
            "id": "memory:system", "kind": "Memory", "name": "System memory", "at": stamp(), "warning": "",
            "fields": [{"name": "Capacity (bytes)", "value": m.total_phys.to_string()}],
            "metrics": [
                metric("used", "In use", "bytes", Some(c.in_use() as f64), Some(total), false),
                metric("available", "Available", "bytes", Some(m.avail_phys as f64), Some(total), false),
                metric("commit", "Committed", "bytes", Some(m.commit_limit.saturating_sub(m.commit_avail) as f64), Some(m.commit_limit.max(1) as f64), false),
                metric("cache", "Cached", "bytes", c.cached().map(|v| v as f64), None, false),
            ],
        }));
    }
}

pub(crate) fn probe(_smart: bool) -> Result<Value, String> {
    let mut devices = Vec::new();
    let mut warnings = Vec::new();
    disks(&mut devices, &mut warnings);
    networks(&mut devices);
    batteries(&mut devices, &mut warnings);
    gpus(&mut devices, &mut warnings);
    system(&mut devices);
    devices.truncate(128);
    warnings.truncate(32);
    Ok(json!({"devices": devices, "warnings": warnings}))
}

/// The kernel driver of a PCI GPU and its version (the module's, or the
/// kernel's for in-tree drivers).
pub(crate) fn display_driver(vendor: u32, device: u32) -> Option<(String, Option<String>)> {
    for e in fs::read_dir("/sys/bus/pci/devices").ok()?.flatten() {
        let p = e.path();
        let hex = |f: &str| {
            read(p.join(f)).and_then(|v| u32::from_str_radix(v.trim_start_matches("0x"), 16).ok())
        };
        if hex("vendor") != Some(vendor) || hex("device") != Some(device) {
            continue;
        }
        let driver = fs::read_link(p.join("driver"))
            .ok()?
            .file_name()?
            .to_string_lossy()
            .into_owned();
        let version = read(format!("/sys/module/{driver}/version"))
            .or_else(|| read("/proc/sys/kernel/osrelease").map(|k| format!("kernel {k}")))
            .unwrap_or_default();
        return Some((format!("{driver} {version}").trim().to_owned(), None));
    }
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn probe_passes_the_shared_checks() {
        let v = super::probe(false).unwrap();
        crate::devices::parse(v).unwrap();
    }
}
