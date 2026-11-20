//! Windows power-plan frequency policy. These settings affect shared cores,
//! not individual PIDs. Policy class numbers are not vendor P/E identifiers.
use crate::Result;
use serde::{Deserialize, Serialize};
use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS,
    HANDLE, HLOCAL, LocalFree, WIN32_ERROR,
};
use windows::Win32::System::Power::{
    ACCESS_SCHEME, POWER_REQUEST_TYPE, PowerClearRequest, PowerCreateRequest, PowerEnumerate,
    PowerGetActiveScheme, PowerReadACValueIndex, PowerReadDCValueIndex, PowerReadFriendlyName,
    PowerRequestDisplayRequired, PowerRequestSystemRequired, PowerSetActiveScheme, PowerSetRequest,
    PowerWriteACValueIndex, PowerWriteDCValueIndex,
};
use windows::Win32::System::Threading::{
    POWER_REQUEST_CONTEXT_SIMPLE_STRING, REASON_CONTEXT, REASON_CONTEXT_0,
};
use windows::core::{GUID, PWSTR};

const PROCESSOR: GUID = GUID::from_u128(0x54533251_82be_4824_96c1_47b60b740d00);
const FREQUENCY: [GUID; 2] = [
    GUID::from_u128(0x75b0ae3f_bce0_45a7_8c89_c9611c25e100),
    GUID::from_u128(0x75b0ae3f_bce0_45a7_8c89_c9611c25e101),
];

/// The battery as Windows reports it; each value `None` when unknown.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatteryStatus {
    /// Charge, 0..=100.
    pub percent: Option<u8>,
    /// Plugged in.
    pub on_ac: Option<bool>,
    pub charging: bool,
    /// Time left on battery, in seconds.
    pub seconds_left: Option<u32>,
    pub saver: bool,
}

/// What `SYSTEM_POWER_STATUS` says, or `None` without a system battery
/// (desktops) or when Windows does not know.
pub fn decode_battery(
    ac_line: u8,
    flag: u8,
    percent: u8,
    saver: u8,
    seconds: u32,
) -> Option<BatteryStatus> {
    // 128: no system battery; 255: status unknown.
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

/// The system battery, if the PC has one.
pub fn battery() -> Option<BatteryStatus> {
    let mut s = windows::Win32::System::Power::SYSTEM_POWER_STATUS::default();
    // SAFETY: `s` is a writable SYSTEM_POWER_STATUS.
    unsafe { windows::Win32::System::Power::GetSystemPowerStatus(&mut s) }.ok()?;
    decode_battery(
        s.ACLineStatus,
        s.BatteryFlag,
        s.BatteryLifePercent,
        s.SystemStatusFlag,
        s.BatteryLifeTime,
    )
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

fn setting(class: u8) -> Result<&'static GUID> {
    FREQUENCY.get(usize::from(class)).ok_or_else(|| {
        windows::core::Error::from_hresult(ERROR_INVALID_PARAMETER.to_hresult()).into()
    })
}

pub fn active_scheme() -> Result<String> {
    let mut pointer = std::ptr::null_mut();
    // SAFETY: writable out-pointer receives a LocalAlloc-owned GUID.
    unsafe { PowerGetActiveScheme(None, &mut pointer) }.ok()?;
    if pointer.is_null() {
        return Err(
            windows::core::Error::from_hresult(ERROR_INVALID_PARAMETER.to_hresult()).into(),
        );
    }
    // SAFETY: the successful call returned a readable GUID; copying precedes free.
    let guid = unsafe { *pointer };
    // SAFETY: returned allocation must be released with LocalFree.
    unsafe { LocalFree(Some(HLOCAL(pointer.cast()))) };
    Ok(format!("{guid:?}"))
}

pub fn read_frequency(plan: &str, class: u8, source: PowerSource) -> Result<u32> {
    let plan = GUID::try_from(plan)?;
    let setting = setting(class)?;
    let mut value = 0;
    // SAFETY: GUIDs and out-pointer are live for this synchronous call.
    unsafe {
        match source {
            PowerSource::Ac => PowerReadACValueIndex(
                None,
                Some(&plan),
                Some(&PROCESSOR),
                Some(setting),
                &mut value,
            )
            .ok()?,
            PowerSource::Dc => WIN32_ERROR(PowerReadDCValueIndex(
                None,
                Some(&plan),
                Some(&PROCESSOR),
                Some(setting),
                &mut value,
            ))
            .ok()?,
        }
    };
    Ok(value)
}

pub fn write_frequency(plan: &str, class: u8, source: PowerSource, value: u32) -> Result<()> {
    let plan = GUID::try_from(plan)?;
    let setting = setting(class)?;
    // SAFETY: valid GUID references; Windows enforces policy write permissions.
    unsafe {
        WIN32_ERROR(match source {
            PowerSource::Ac => {
                PowerWriteACValueIndex(None, &plan, Some(&PROCESSOR), Some(setting), value).0
            }
            PowerSource::Dc => {
                PowerWriteDCValueIndex(None, &plan, Some(&PROCESSOR), Some(setting), value)
            }
        })
        .ok()?
    };
    Ok(())
}

/// The built-in High performance power plan.
pub const HIGH_PERFORMANCE: &str = "8C5E7FDA-E8BF-4A96-9A85-A6E23A8C635C";

fn win32(error: WIN32_ERROR) -> crate::Error {
    windows::core::Error::from_hresult(error.to_hresult()).into()
}

/// The power plans of this machine as `(GUID, name)`, the GUID as
/// [`active_scheme`] writes it. Read-only.
pub fn plans() -> Result<Vec<(String, String)>> {
    let mut out = Vec::new();
    for index in 0.. {
        let mut guid = GUID::zeroed();
        let mut size = size_of::<GUID>() as u32;
        // SAFETY: `guid` is writable for `size` bytes.
        let error = unsafe {
            PowerEnumerate(
                None,
                None,
                None,
                ACCESS_SCHEME,
                index,
                Some((&raw mut guid).cast()),
                &mut size,
            )
        };
        if error == ERROR_NO_MORE_ITEMS {
            break;
        }
        if error != ERROR_SUCCESS {
            return Err(win32(error));
        }
        out.push((format!("{guid:?}"), plan_name(&guid).unwrap_or_default()));
    }
    Ok(out)
}

/// The name Windows shows for a power plan, in the system's language.
fn plan_name(guid: &GUID) -> Result<String> {
    let mut size = 0u32;
    // SAFETY: a size query; nothing is written without a buffer.
    let error = unsafe { PowerReadFriendlyName(None, Some(guid), None, None, None, &mut size) };
    if error != ERROR_SUCCESS && error != ERROR_MORE_DATA {
        return Err(win32(error));
    }
    let mut buffer = vec![0u16; (size as usize).div_ceil(2).max(1)];
    let mut size = (buffer.len() * 2) as u32;
    // SAFETY: `buffer` is writable for `size` bytes.
    let error = unsafe {
        PowerReadFriendlyName(
            None,
            Some(guid),
            None,
            None,
            Some(buffer.as_mut_ptr().cast()),
            &mut size,
        )
    };
    if error != ERROR_SUCCESS {
        return Err(win32(error));
    }
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    Ok(String::from_utf16_lossy(&buffer[..end]))
}

/// What a [`PowerRequest`] keeps awake.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Awake {
    /// The computer does not go to sleep by itself.
    System,
    /// Nor does the screen turn off. Windows refuses this from session 0
    /// (a service): it must come from a signed-in user's process.
    Display,
}

impl Awake {
    fn kind(self) -> POWER_REQUEST_TYPE {
        match self {
            Self::System => PowerRequestSystemRequired,
            Self::Display => PowerRequestDisplayRequired,
        }
    }
}

/// A power request with a reason that `powercfg /requests` shows. Nothing
/// is kept awake until [`set`](Self::set); dropping it, or the process
/// ending, gives everything up.
#[derive(Debug)]
pub struct PowerRequest {
    handle: HANDLE,
}

// SAFETY: the handle is a kernel object usable from any thread.
unsafe impl Send for PowerRequest {}

impl PowerRequest {
    pub fn new(reason: &str) -> Result<Self> {
        let mut text: Vec<u16> = reason.encode_utf16().chain([0]).collect();
        let context = REASON_CONTEXT {
            // POWER_REQUEST_CONTEXT_VERSION
            Version: 0,
            Flags: POWER_REQUEST_CONTEXT_SIMPLE_STRING,
            Reason: REASON_CONTEXT_0 {
                SimpleReasonString: PWSTR(text.as_mut_ptr()),
            },
        };
        // SAFETY: `context` and the string it points to outlive the call,
        // which copies the reason.
        let handle = unsafe { PowerCreateRequest(&context)? };
        Ok(Self { handle })
    }

    pub fn set(&self, awake: Awake) -> Result<()> {
        // SAFETY: a live power request handle.
        unsafe { PowerSetRequest(self.handle, awake.kind())? };
        Ok(())
    }

    pub fn clear(&self, awake: Awake) -> Result<()> {
        // SAFETY: a live power request handle.
        unsafe { PowerClearRequest(self.handle, awake.kind())? };
        Ok(())
    }
}

impl Drop for PowerRequest {
    fn drop(&mut self) {
        // SAFETY: owned handle, closed once; this ends its requests.
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// Makes `plan` (a GUID) the active power plan.
pub fn set_active_scheme(plan: &str) -> Result<()> {
    refresh_scheme(plan)
}

pub fn refresh_scheme(plan: &str) -> Result<()> {
    let plan = GUID::try_from(plan)?;
    // SAFETY: the scheme GUID reference is valid for the synchronous call.
    unsafe { PowerSetActiveScheme(None, Some(&plan)) }.ok()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_power_plans_with_names_and_the_active_one_among_them() {
        // Read-only: no plan is switched and nothing is kept awake.
        let plans = plans().unwrap();
        let active = active_scheme().unwrap();
        println!("EXPECTED: the active plan among {plans:?}; ACTUAL: {active}");
        assert!(!plans.is_empty());
        assert!(
            plans
                .iter()
                .all(|(guid, name)| guid.len() == 36 && !name.is_empty())
        );
        assert!(
            plans
                .iter()
                .any(|(guid, _)| guid.eq_ignore_ascii_case(&active))
        );
        // A request is only created: nothing is held.
        drop(PowerRequest::new("RustWTM test").unwrap());
    }

    #[test]
    fn battery_status_reads_windows_sentinels() {
        assert_eq!(decode_battery(1, 128, 255, 0, u32::MAX), None, "no battery");
        assert_eq!(decode_battery(255, 255, 255, 0, u32::MAX), None, "unknown");
        let b = decode_battery(0, 1, 84, 1, 9000).unwrap();
        assert_eq!(b.percent, Some(84));
        assert_eq!(b.on_ac, Some(false));
        assert!(!b.charging && b.saver);
        assert_eq!(b.seconds_left, Some(9000));
        let charging = decode_battery(1, 8, 255, 0, u32::MAX).unwrap();
        assert!(charging.charging);
        assert_eq!((charging.percent, charging.seconds_left), (None, None));
    }
    #[test]
    fn reads_current_plan_without_changing_it() {
        let plan = active_scheme().unwrap();
        assert_eq!(plan.len(), 36);
        let _ = read_frequency(&plan, 0, PowerSource::Ac);
        assert!(read_frequency(&plan, 2, PowerSource::Ac).is_err());
        assert_eq!(active_scheme().unwrap(), plan);
    }
}
