//! Program icons. Linux executables carry none; the desktop's icon comes
//! from the application's .desktop entry and the icon theme, which the GUI
//! resolves itself. Nothing is read here.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub size: u32,
    pub rgba: Vec<u8>,
}

pub fn small_icon(_path: &str) -> Option<IconImage> {
    None
}
