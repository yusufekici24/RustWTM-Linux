//! A global function-key shortcut. Desktops on Linux own global shortcuts
//! (and Wayland gives them to nobody else), so RustWTM registers its
//! shortcut with GNOME's custom keybindings instead (see
//! `wtm_core::task_manager`); this in-process hook is not available.

pub struct Hotkey {
    _private: (),
}

impl Hotkey {
    pub fn new(function_key: u8, _pressed: impl FnMut() + Send + 'static) -> Result<Self, String> {
        if !(2..=12).contains(&function_key) {
            return Err("Choose a function key from F2 to F12".into());
        }
        Err("Global shortcuts are set in the desktop's keyboard settings on Linux".into())
    }
}

impl std::fmt::Debug for Hotkey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Hotkey")
    }
}
