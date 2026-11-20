//! GPU temperature, fan and memory clock from the card's hwmon (amdgpu,
//! nouveau, xe) and DRM sysfs, by the LUID [`crate::gpu`] writes.

use std::fs;

use crate::linux::util::read_trim;

#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct GpuPerf {
    pub temperature: Option<f32>,
    pub temperature_max: Option<f32>,
    pub fan_rpm: Option<u32>,
    pub memory_mhz: Option<u32>,
    pub memory_max_mhz: Option<u32>,
}

pub fn parse_luid(text: &str) -> Option<(u32, u32)> {
    let (high, low) = text.split_once('_')?;
    Some((
        u32::from_str_radix(high, 16).ok()?,
        u32::from_str_radix(low, 16).ok()?,
    ))
}

fn pci_slot(luid: &str) -> Option<String> {
    let (domain, low) = parse_luid(luid)?;
    Some(format!(
        "{domain:04x}:{:02x}:{:02x}.{}",
        low >> 8,
        (low >> 3) & 0x1f,
        low & 7
    ))
}

/// The amdgpu `pp_dpm_mclk` line marked `*`, and the highest one.
fn mclk(text: &str) -> (Option<u32>, Option<u32>) {
    let parse = |l: &str| {
        l.split_whitespace()
            .nth(1)?
            .trim_end_matches("Mhz")
            .trim_end_matches("MHz")
            .parse::<u32>()
            .ok()
    };
    let current = text
        .lines()
        .find(|l| l.trim_end().ends_with('*'))
        .and_then(parse);
    let max = text.lines().filter_map(parse).max();
    (current, max)
}

pub fn read(luid: &str) -> Option<GpuPerf> {
    let slot = pci_slot(luid)?;
    let dev = format!("/sys/bus/pci/devices/{slot}");
    let mut perf = GpuPerf::default();
    if let Ok(rd) = fs::read_dir(format!("{dev}/hwmon")) {
        for dir in rd.flatten().map(|e| e.path()) {
            let milli = |leaf: &str| {
                read_trim(dir.join(leaf))
                    .and_then(|v| v.parse::<i64>().ok())
                    .filter(|v| *v > 0 && *v < 200_000)
                    .map(|v| v as f32 / 1000.0)
            };
            perf.temperature = perf.temperature.or_else(|| milli("temp1_input"));
            perf.temperature_max = perf
                .temperature_max
                .or_else(|| milli("temp1_crit"))
                .or_else(|| milli("temp1_max"));
            if perf.fan_rpm.is_none() {
                perf.fan_rpm = read_trim(dir.join("fan1_input")).and_then(|v| v.parse().ok());
            }
        }
    }
    if let Some(text) = read_trim(format!("{dev}/pp_dpm_mclk")) {
        (perf.memory_mhz, perf.memory_max_mhz) = mclk(&text);
    }
    (perf != GpuPerf::default()).then_some(perf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_round_trip() {
        assert_eq!(pci_slot("00000000_00000300").unwrap(), "0000:03:00.0");
        assert_eq!(pci_slot("00000000_00000010").unwrap(), "0000:00:02.0");
    }

    #[test]
    fn reads_mclk() {
        let (c, m) = mclk("0: 96Mhz\n1: 456Mhz *\n2: 1000Mhz\n");
        assert_eq!((c, m), (Some(456), Some(1000)));
    }
}
