//! The screen kept on while a `[[while_running]]` entry asks for it.
//!
//! Windows refuses a display request from a service (session 0), so the
//! service keeps only the computer awake and the window, also while hidden
//! in the tray, keeps the screen on. Closing the window gives it up.

use std::sync::{Mutex, PoisonError};

use wtm_policy::KeepAwake;
use wtm_sys::power::{Awake, PowerRequest};

use crate::backend::{Data, Mode};

/// The reason held, and the request (`None` when Windows refused it, so it
/// is not asked again every frame).
static HELD: Mutex<Option<(String, Option<PowerRequest>)>> = Mutex::new(None);

/// The reason to keep the screen on, if the service asks for it.
pub fn wanted(data: &Data) -> Option<String> {
    if !matches!(data.mode, Mode::Service { .. }) {
        return None;
    }
    let status = data.status.as_ref()?;
    if status.dry_run {
        return None;
    }
    match &status.power.awake {
        Some((KeepAwake::Display, who)) => {
            let names: Vec<String> = who.iter().map(|w| format!("'{w}'")).collect();
            Some(format!("RustWTM: {}", names.join(", ")))
        }
        _ => None,
    }
}

/// Holds the screen on for `reason`, or lets it turn off with `None`.
pub fn update(reason: Option<String>) {
    // Tests never keep the developer's screen on.
    if cfg!(test) {
        return;
    }
    let mut held = HELD.lock().unwrap_or_else(PoisonError::into_inner);
    if held.as_ref().map(|(r, _)| r) == reason.as_ref() {
        return;
    }
    *held = reason.map(|r| {
        let request = PowerRequest::new(&r)
            .ok()
            .filter(|q| q.set(Awake::Display).is_ok());
        (r, request)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_stays_on_only_when_the_service_enforces_a_display_entry() {
        let mut data = crate::ui_tests::data();
        data.mode = Mode::Service {
            admin: true,
            verified: true,
        };
        let status = data.status.as_mut().unwrap();
        status.dry_run = false;
        status.power.awake = Some((KeepAwake::Display, vec!["Movie".into(), "Talk".into()]));
        assert_eq!(wanted(&data).as_deref(), Some("RustWTM: 'Movie', 'Talk'"));
        let status = data.status.as_mut().unwrap();
        status.power.awake = Some((KeepAwake::System, vec!["Render".into()]));
        assert_eq!(wanted(&data), None, "the service keeps the computer awake");
        let status = data.status.as_mut().unwrap();
        status.power.awake = Some((KeepAwake::Display, vec!["Movie".into()]));
        status.dry_run = true;
        assert_eq!(wanted(&data), None, "monitoring only");
        data.status.as_mut().unwrap().dry_run = false;
        data.mode = Mode::Local;
        assert_eq!(wanted(&data), None, "no service");
    }
}
