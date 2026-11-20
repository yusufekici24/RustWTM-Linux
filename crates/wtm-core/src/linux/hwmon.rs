//! Sensors from /sys/class/hwmon: temperatures, fans, voltages, power and
//! the PWM duty of fans (read-only: the firmware keeps fan control).

use super::{FanChannel, HardwareReport, Sensor, SensorKind};
use std::fs;
use std::path::Path;

fn read(p: &Path) -> Option<String> {
    fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// A friendlier device name for common hwmon drivers.
fn device_name(chip: &str, dir: &Path) -> String {
    let pretty = match chip {
        "coretemp" | "k10temp" | "zenpower" => "CPU",
        "nvme" => "NVMe drive",
        "acpitz" => "ACPI thermal zone",
        "pch_cannonlake" | "pch_skylake" | "pch_cometlake" | "pch_meteorlake" => "Chipset",
        "iwlwifi_1" | "iwlwifi" => "Wi-Fi",
        "amdgpu" | "nouveau" | "i915" | "xe" => "GPU",
        "BAT0" | "BAT1" => "Battery",
        "hp" | "hp_wmi" => "HP embedded controller",
        "thinkpad" | "dell_smm" | "asus" | "nct6775" | "it87" => "Embedded controller",
        _ => "",
    };
    let model = read(&dir.join("device/model")).unwrap_or_default();
    match (pretty.is_empty(), model.is_empty()) {
        (false, true) => format!("{pretty} ({chip})"),
        (false, false) => format!("{pretty} {model}"),
        (true, _) => chip.to_owned(),
    }
}

pub(super) fn snapshot(now: u64) -> HardwareReport {
    let mut report = HardwareReport {
        provider: "Linux hwmon".into(),
        sampled_at_ms: now,
        notice:
            "Sensors are read from the kernel's hwmon drivers. Fan curves stay with the firmware."
                .into(),
        ..Default::default()
    };
    let Ok(rd) = fs::read_dir("/sys/class/hwmon") else {
        return report;
    };
    let mut dirs: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    dirs.sort();
    for dir in dirs.into_iter().take(64) {
        let chip = read(&dir.join("name")).unwrap_or_else(|| "hwmon".into());
        let device = device_name(&chip, &dir);
        let hw = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<String> = files
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        for f in names {
            let Some(base) = f.strip_suffix("_input") else {
                if let Some(n) = f
                    .strip_prefix("pwm")
                    .filter(|n| n.chars().all(|c| c.is_ascii_digit()))
                {
                    let duty = read(&dir.join(&f))
                        .and_then(|v| v.parse::<f32>().ok())
                        .map(|v| v / 255.0 * 100.0);
                    report.sensors.push(Sensor {
                        id: format!("{hw}/{f}"),
                        device: device.clone(),
                        name: format!("Fan {n} duty"),
                        kind: SensorKind::Control,
                        value: duty,
                    });
                    report.channels.push(FanChannel {
                        id: format!("{hw}/{f}"),
                        device: device.clone(),
                        name: format!("Fan {n}"),
                        min_percent: 30.0,
                        max_percent: 100.0,
                        writable: false,
                        reason: "Fan control is left to the firmware on Linux".into(),
                    });
                }
                continue;
            };
            let raw = read(&dir.join(&f)).and_then(|v| v.parse::<f64>().ok());
            let (kind, scale) = if base.starts_with("temp") {
                (SensorKind::Temperature, 1000.0)
            } else if base.starts_with("fan") {
                (SensorKind::Fan, 1.0)
            } else if base.starts_with("in") {
                (SensorKind::Voltage, 1000.0)
            } else if base.starts_with("power") {
                (SensorKind::Power, 1_000_000.0)
            } else if base.starts_with("freq") {
                (SensorKind::Clock, 1_000_000.0)
            } else {
                continue;
            };
            let label = read(&dir.join(format!("{base}_label"))).unwrap_or_else(|| match kind {
                SensorKind::Temperature => {
                    format!("Temperature {}", base.trim_start_matches("temp"))
                }
                SensorKind::Fan => format!("Fan {}", base.trim_start_matches("fan")),
                SensorKind::Voltage => format!("Voltage {}", base.trim_start_matches("in")),
                SensorKind::Power => "Power".into(),
                _ => base.to_owned(),
            });
            let value = raw.map(|v| (v / scale) as f32).filter(|v| {
                v.is_finite() && (kind != SensorKind::Temperature || (-40.0..200.0).contains(v))
            });
            report.sensors.push(Sensor {
                id: format!("{hw}/{base}"),
                device: device.clone(),
                name: label,
                kind,
                value,
            });
        }
    }
    report.validate();
    report
}

#[cfg(test)]
mod tests {
    #[test]
    fn reads_without_panicking() {
        let r = super::snapshot(1);
        assert_eq!(r.provider, "Linux hwmon");
    }
}
