//! Battery, power profiles and keep-awake requests on Linux.
//!
//! power-profiles-daemon's three profiles stand in for the Windows power
//! plans and keep the Windows plan GUIDs as their identifiers, so the
//! crates above (and saved configurations) need no translation:
//! power-saver is "Power saver", balanced "Balanced", performance "High
//! performance". Without the daemon, the ACPI platform profile is used.
//!
//! The processor frequency cap is cpufreq's `scaling_max_freq` for the CPUs
//! of an efficiency class. Linux has no per-plan, per-source values: a cap
//! is applied when written for the power source the machine is on, and
//! remembered for the other one in /run/rustwtm.

use std::fs;
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::linux::util::{read_trim, read_u64};
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryStatus {
    pub percent: Option<u8>,
    pub on_ac: Option<bool>,
    pub charging: bool,
    pub seconds_left: Option<u32>,
    pub saver: bool,
}

pub fn decode_battery(
    ac_line: u8,
    flag: u8,
    percent: u8,
    saver: u8,
    seconds: u32,
) -> Option<BatteryStatus> {
    if flag & 128 != 0 || flag == 255 {
        return None;
    }
    Some(BatteryStatus {
        percent: (percent <= 100).then_some(percent),
        on_ac: match ac_line {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        },
        charging: flag & 8 != 0,
        seconds_left: (seconds != u32::MAX).then_some(seconds),
        saver: saver & 1 != 0,
    })
}

fn supplies() -> Vec<std::path::PathBuf> {
    fs::read_dir("/sys/class/power_supply")
        .map(|rd| rd.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

/// Mains power is connected; `None` on machines without a mains supply
/// entry (most desktops).
pub(crate) fn on_ac() -> Option<bool> {
    let mut seen = false;
    for p in supplies() {
        if read_trim(p.join("type")).as_deref() == Some("Mains") {
            seen = true;
            if read_u64(p.join("online")) == Some(1) {
                return Some(true);
            }
        }
    }
    seen.then_some(false)
}

/// The system battery (summed over BAT0, BAT1, ...), if the PC has one.
pub fn battery() -> Option<BatteryStatus> {
    let bats: Vec<_> = supplies()
        .into_iter()
        .filter(|p| {
            read_trim(p.join("type")).as_deref() == Some("Battery")
                && read_trim(p.join("scope")).as_deref() != Some("Device")
        })
        .collect();
    if bats.is_empty() {
        return None;
    }
    let (mut now, mut full, mut power) = (0u64, 0u64, 0u64);
    let mut charging = false;
    let mut capacity = None;
    for b in &bats {
        let e_now = read_u64(b.join("energy_now")).or_else(|| read_u64(b.join("charge_now")));
        let e_full = read_u64(b.join("energy_full")).or_else(|| read_u64(b.join("charge_full")));
        if let (Some(n), Some(f)) = (e_now, e_full) {
            now += n;
            full += f;
        }
        power += read_u64(b.join("power_now"))
            .or_else(|| read_u64(b.join("current_now")))
            .unwrap_or(0);
        charging |= read_trim(b.join("status")).as_deref() == Some("Charging");
        capacity = capacity.or_else(|| read_u64(b.join("capacity")));
    }
    let percent = (now * 100 + full / 2)
        .checked_div(full)
        .or(capacity)
        .map(|p| p.min(100) as u8);
    let ac = on_ac();
    let discharging = ac == Some(false) || (!charging && ac.is_none());
    let seconds_left =
        (discharging && power > 0 && now > 0).then(|| (now as f64 / power as f64 * 3600.0) as u32);
    Some(BatteryStatus {
        percent,
        on_ac: ac,
        charging,
        seconds_left,
        saver: active_profile().as_deref() == Some("power-saver"),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PowerSource {
    Ac,
    Dc,
}

impl PowerSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ac => "Plugged in",
            Self::Dc => "Battery",
        }
    }
}

pub const HIGH_PERFORMANCE: &str = "8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C";
const BALANCED: &str = "381B4222-F694-41F0-9685-FF5BB260DF2E";
const POWER_SAVER: &str = "A1841308-3541-4FAB-BC81-F71556F20B4A";

/// (plan GUID, power-profiles-daemon name, platform_profile name, label).
const PROFILES: [(&str, &str, &str, &str); 3] = [
    (POWER_SAVER, "power-saver", "low-power", "Power saver"),
    (BALANCED, "balanced", "balanced", "Balanced"),
    (
        HIGH_PERFORMANCE,
        "performance",
        "performance",
        "High performance",
    ),
];

fn ppd_available() -> bool {
    crate::linux::util::run("powerprofilesctl", &["get"]).is_some()
}

fn active_profile() -> Option<String> {
    if let Some(p) = crate::linux::util::run("powerprofilesctl", &["get"]) {
        return Some(p.trim().to_owned());
    }
    let platform = read_trim("/sys/firmware/acpi/platform_profile")?;
    PROFILES
        .iter()
        .find(|p| p.2 == platform)
        .map(|p| p.1.to_owned())
}

pub fn active_scheme() -> Result<String> {
    let profile = active_profile().ok_or(Error::Unsupported("power profiles"))?;
    PROFILES
        .iter()
        .find(|p| p.1 == profile)
        .map(|p| p.0.to_owned())
        .ok_or_else(|| Error::Message(format!("unknown power profile {profile}")))
}

pub fn plans() -> Result<Vec<(String, String)>> {
    if ppd_available() {
        let list = crate::linux::util::run("powerprofilesctl", &["list"]).unwrap_or_default();
        return Ok(PROFILES
            .iter()
            .filter(|p| list.contains(&format!("{}:", p.1)))
            .map(|p| (p.0.to_owned(), p.3.to_owned()))
            .collect());
    }
    let choices = read_trim("/sys/firmware/acpi/platform_profile_choices")
        .ok_or(Error::Unsupported("power profiles"))?;
    Ok(PROFILES
        .iter()
        .filter(|p| choices.split_whitespace().any(|c| c == p.2))
        .map(|p| (p.0.to_owned(), p.3.to_owned()))
        .collect())
}

/// Makes `plan` (a plan GUID) the active profile.
pub fn set_active_scheme(plan: &str) -> Result<()> {
    let p = PROFILES
        .iter()
        .find(|p| {
            p.0.eq_ignore_ascii_case(plan.trim_matches(|c| c == '{' || c == '}'))
        })
        .ok_or_else(|| Error::Message(format!("unknown power plan {plan}")))?;
    if ppd_available() {
        crate::linux::util::run_checked("powerprofilesctl", &["set", p.1])?;
        return Ok(());
    }
    fs::write("/sys/firmware/acpi/platform_profile", p.2)
        .map_err(|e| Error::io("platform_profile", e))
}

/// Re-applies `plan`'s settings: frequency caps take effect when written.
pub fn refresh_scheme(_plan: &str) -> Result<()> {
    Ok(())
}

/// The CPUs a frequency class covers: class 0 the slowest tier (every CPU
/// on uniform machines), class 1 the faster tiers.
fn class_cpus(class: u8) -> Result<Vec<u32>> {
    let t = crate::cpu::topology()?;
    let min = t.classes().first().copied().unwrap_or(0);
    let cpus: Vec<u32> = t
        .cpus
        .iter()
        .filter(|c| match class {
            0 => !t.hybrid() || c.efficiency_class == min,
            1 => t.hybrid() && c.efficiency_class > min,
            _ => false,
        })
        .map(|c| c.id)
        .collect();
    if cpus.is_empty() {
        return Err(Error::Os {
            call: "frequency class",
            errno: libc::EINVAL,
        });
    }
    Ok(cpus)
}

const STATE: &str = "/run/rustwtm/frequency";

static REMEMBERED: Mutex<Vec<(u8, PowerSource, u32)>> = Mutex::new(Vec::new());

fn remembered(class: u8, source: PowerSource) -> Option<u32> {
    if let Ok(list) = REMEMBERED.lock()
        && let Some(v) = list.iter().find(|e| e.0 == class && e.1 == source)
    {
        return Some(v.2);
    }
    let text = fs::read_to_string(STATE).ok()?;
    let key = format!("{class}:{source:?}=");
    text.lines()
        .find_map(|l| l.strip_prefix(&key)?.parse().ok())
}

fn remember(class: u8, source: PowerSource, value: u32) {
    if let Ok(mut list) = REMEMBERED.lock() {
        list.retain(|e| !(e.0 == class && e.1 == source));
        list.push((class, source, value));
        let text: String = list
            .iter()
            .map(|(c, s, v)| format!("{c}:{s:?}={v}\n"))
            .collect();
        let _ = fs::create_dir_all("/run/rustwtm");
        let _ = fs::write(STATE, text);
    }
}

fn current_source() -> PowerSource {
    if on_ac() == Some(false) {
        PowerSource::Dc
    } else {
        PowerSource::Ac
    }
}

/// The frequency cap in MHz for `class` on `source`; 0 is no cap.
pub fn read_frequency(_plan: &str, class: u8, source: PowerSource) -> Result<u32> {
    if source != current_source() {
        return Ok(remembered(class, source).unwrap_or(0));
    }
    let cpus = class_cpus(class)?;
    let c = cpus[0];
    let base = format!("/sys/devices/system/cpu/cpu{c}/cpufreq");
    let max = read_u64(format!("{base}/scaling_max_freq")).ok_or(Error::Unsupported("cpufreq"))?;
    let hw = read_u64(format!("{base}/cpuinfo_max_freq")).unwrap_or(max);
    Ok(if max >= hw { 0 } else { (max / 1000) as u32 })
}

pub fn write_frequency(_plan: &str, class: u8, source: PowerSource, value: u32) -> Result<()> {
    let cpus = class_cpus(class)?;
    remember(class, source, value);
    if source != current_source() {
        return Ok(());
    }
    for c in cpus {
        let base = format!("/sys/devices/system/cpu/cpu{c}/cpufreq");
        let hw = read_u64(format!("{base}/cpuinfo_max_freq")).unwrap_or(0);
        let min = read_u64(format!("{base}/cpuinfo_min_freq")).unwrap_or(0);
        let khz = if value == 0 {
            hw
        } else {
            (u64::from(value) * 1000).clamp(min, hw.max(min))
        };
        fs::write(format!("{base}/scaling_max_freq"), khz.to_string())
            .map_err(|e| Error::io("scaling_max_freq", e))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awake {
    System,
    Display,
}

/// A keep-awake request: an inhibitor helper process (systemd-inhibit, or
/// gnome-session-inhibit for the screen in a GNOME session) that holds the
/// lock while it lives.
#[derive(Debug)]
pub struct PowerRequest {
    reason: String,
    held: Mutex<Vec<(Awake, Child)>>,
}

impl PowerRequest {
    pub fn new(reason: &str) -> Result<Self> {
        Ok(Self {
            reason: reason.to_owned(),
            held: Mutex::new(Vec::new()),
        })
    }

    fn spawn(&self, awake: Awake) -> Result<Child> {
        let gnome = std::env::var_os("DBUS_SESSION_BUS_ADDRESS").is_some()
            && crate::linux::util::run("which", &["gnome-session-inhibit"]).is_some();
        let mut cmd = if awake == Awake::Display && gnome {
            let mut c = Command::new("gnome-session-inhibit");
            c.args([
                "--inhibit",
                "idle",
                "--reason",
                &self.reason,
                "sleep",
                "infinity",
            ]);
            c
        } else {
            let what = match awake {
                Awake::System => "sleep",
                Awake::Display => "idle:sleep",
            };
            let mut c = Command::new("systemd-inhibit");
            c.args([
                &format!("--what={what}"),
                "--who=RustWTM",
                &format!("--why={}", self.reason),
                "--mode=block",
                "sleep",
                "infinity",
            ]);
            c
        };
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.spawn().map_err(|e| Error::io("systemd-inhibit", e))
    }

    pub fn set(&self, awake: Awake) -> Result<()> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if held
            .iter_mut()
            .any(|(a, c)| *a == awake && c.try_wait().ok().flatten().is_none())
        {
            return Ok(());
        }
        held.retain(|(a, _)| *a != awake);
        let child = self.spawn(awake)?;
        held.push((awake, child));
        Ok(())
    }

    pub fn clear(&self, awake: Awake) -> Result<()> {
        let mut held = self
            .held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for (_, mut c) in held.extract_if(.., |(a, _)| *a == awake) {
            let _ = c.kill();
            let _ = c.wait();
        }
        Ok(())
    }
}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        let held = self.held.get_mut().unwrap_or_else(|e| e.into_inner());
        for (_, c) in held.iter_mut() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_windows_status() {
        assert!(decode_battery(1, 128, 0, 0, 0).is_none());
        let b = decode_battery(0, 8, 50, 1, 600).unwrap();
        assert_eq!(b.percent, Some(50));
        assert!(b.charging && b.saver);
    }

    #[test]
    fn plan_guids_map_to_profiles() {
        assert_eq!(
            PROFILES.iter().find(|p| p.1 == "performance").unwrap().0,
            HIGH_PERFORMANCE
        );
        let _ = battery();
    }
}
