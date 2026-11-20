//! Linux has no privileges to switch on: root already may debug and signal
//! every process. Reports whether that is the case.

pub fn enable_debug_privilege() -> crate::Result<bool> {
    Ok(crate::linux::util::is_root())
}
