//! Operating-system specific code. Each OS has its own folder and exposes the
//! same items, so the rest of the app never needs `cfg` checks:
//!
//! - `DEFAULT_HOTKEY`: the shortcut used until the user picks one.
//! - `MODIFIER_HOTKEYS`, `start_modifier_hotkey`, `stop_modifier_hotkey`,
//!   `modifier_hotkey_blocked`: hotkeys made of one modifier key, where the OS
//!   allows listening for them. `set_modifier_hotkey_sends`: Return while it's
//!   held sends, rather than making it part of a shortcut.
//! - `send_paste_shortcut`: presses the paste keys for the focused app.
//! - `send_copy_shortcut`, `can_copy_selection`: presses the copy keys, for
//!   command mode, where it's safe to.
//! - `frontmost_app`: which app has focus, so dictation can match its tone.
//! - `setup_notes`: OS-specific hints shown in Settings.
//! - `permissions`, `request_permission`, `permission_settings_url`: the OS
//!   permissions dictation needs, for onboarding and Settings.
//! - `style_main_window`: native window chrome tweaks.
//! - `NEURAL_ENGINE`, `extract_zip`, `ORT_ACCELERATOR`: speech model acceleration.
//! - `FocusedField`: reads the focused text field in another app, so a
//!   correction made after dictating can be learned. Never available on Linux.
//! - `pause_media`, `resume_media`, `PausedMedia`: pause what's playing while
//!   the user dictates, and resume only that afterwards.
//! - `SYSTEM_AUDIO_NOTE`, `system_audio_sources`, `open_system_audio`: recording
//!   what the computer plays, for meeting notes.

use enigo::{Direction, Enigo, Key, Keyboard};
use serde::{Deserialize, Serialize};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum PermissionKind {
    Microphone,
    Accessibility,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Permission {
    pub kind: PermissionKind,
    pub granted: bool,
}

/// The app with keyboard focus. Each field is empty when the OS doesn't say.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FrontApp {
    /// Bundle id on macOS, executable name on Windows, window class on Linux.
    pub id: String,
    /// The name people know it by, e.g. "Slack".
    pub name: String,
    /// The focused window's title, which tells web apps in a browser apart.
    pub title: String,
}

/// A change in a single-modifier hotkey.
// Only the macOS listener sends these so far.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyEvent {
    Down,
    Up,
    /// Another key was pressed while the hotkey was held.
    Interrupted,
    /// Return or keypad Enter, while Finish and send claims them.
    Send,
}

/// Receives single-modifier hotkey changes. Called on the listener's own
/// thread, so it should return quickly.
pub type HotkeyHandler = Box<dyn Fn(HotkeyEvent) + Send + Sync>;

#[cfg(not(target_os = "macos"))]
pub fn send_live_text(enigo: &mut Option<Enigo>, text: &str) -> Result<(), String> {
    if enigo.is_none() {
        *enigo = Some(Enigo::new(&enigo::Settings::default()).map_err(|e| e.to_string())?);
    }
    enigo
        .as_mut()
        .ok_or("Keyboard simulation unavailable")?
        .text(text)
        .map_err(|e| e.to_string())
}

/// Holds the modifiers, taps `key`, then releases the modifiers even if the tap failed.
fn press_combo(enigo: &mut Enigo, modifiers: &[Key], key: Key) -> Result<(), String> {
    let mut result = Ok(());
    let mut held = Vec::new();
    for modifier in modifiers {
        match enigo.key(*modifier, Direction::Press) {
            Ok(()) => held.push(*modifier),
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    if result.is_ok() {
        result = enigo.key(key, Direction::Click);
    }
    for modifier in held.iter().rev() {
        let _ = enigo.key(*modifier, Direction::Release);
    }
    result.map_err(|e| format!("Couldn't simulate the shortcut: {e}"))
}
