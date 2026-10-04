// Everything that differs between operating systems, behind one set of names.
//
// The rest of the app calls `platform::…` and never touches Win32 or a Linux
// API directly. Each OS file exposes the same functions; the compiler picks one.

use std::path::PathBuf;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use self::windows::*;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use self::linux::*;

/// Wall-clock time in the user's time zone, for log lines and backup names.
pub struct LocalTime {
    pub year: u32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
}

/// News from the desktop shell's helper (the GNOME Shell extension on Linux).
pub enum ShellEvent {
    /// It took the island on (true), or went away (false).
    Active(bool),
    /// Pointer relative to the island window, logical pixels, and whether a
    /// mouse button is held.
    Pointer { x: f64, y: f64, pressed: bool },
}

/// Where the GNOME Shell extension stands, for Settings.
#[derive(serde::Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ShellExtensionStatus {
    /// A GNOME session: the extension means something here.
    pub applicable: bool,
    pub installed: bool,
    /// The installed files are the ones this build carries.
    pub up_to_date: bool,
    /// In GNOME's list of extensions to start.
    pub enabled: bool,
    /// GNOME's own switch that turns every user extension off.
    pub user_extensions_disabled: bool,
    /// Running and looking after our island right now.
    pub active: bool,
}

/// The user's home directory, where `.claude/settings.json` lives.
pub fn home_dir() -> PathBuf {
    std::env::var_os(HOME_VAR)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
