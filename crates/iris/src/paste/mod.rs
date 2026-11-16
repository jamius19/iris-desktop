/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Text insertion, from Handy `clipboard.rs` + `input.rs`: the transcript goes on the clipboard, a paste chord is sent
//! and the clipboard is restored, or the transcript is typed directly. Windows and X11 use arboard and enigo (X11
//! prefers xdotool, then ydotool); Wayland uses wl-clipboard and ydotool ([linux]). Not ported: Handy's `paste_tx`,
//! wtype/dotool/kwtype, external script and auto-submit.

#[cfg(target_os = "linux")]
mod linux;

use std::sync::{Mutex, PoisonError};
use std::thread::sleep;
use std::time::Duration;

use arboard::{Clipboard, ImageData};
use enigo::{Direction, Enigo, Key, Keyboard};
use log::info;

/// The notice when paste fails; the transcript is then left on the clipboard.
pub const PASTE_FAILED: &str = "Couldn't paste. The text is on the clipboard.";

/// The notice when neither the paste nor the clipboard worked: the transcript is only in it.
pub fn not_copied(text: &str) -> String {
    format!("Couldn't paste or copy. Your text: {text}")
}

/// arboard's clipboard, kept for the whole run (on X11 the copied text lives only as long as it). Its lock also
/// serialises pastes: overlapping ones would restore each other's transcript as the user's clipboard.
static CLIPBOARD: Mutex<Option<Clipboard>> = Mutex::new(None);

pub struct PasteOptions {
    /// `ctrl_v`, `direct`, `shift_insert` or `ctrl_shift_v`.
    pub method: String,
    pub append_trailing_space: bool,
    pub restore_clipboard: bool,
    /// After writing the clipboard, before the paste chord.
    pub delay_ms: u64,
    /// After the paste chord, before restoring the clipboard.
    pub delay_after_ms: u64,
}

impl PasteOptions {
    pub fn from_settings(s: &iris_core::settings::Settings) -> Self {
        Self {
            method: s.paste_method.clone(),
            append_trailing_space: s.append_trailing_space,
            restore_clipboard: s.restore_clipboard,
            delay_ms: u64::try_from(s.paste_delay_ms).unwrap_or(0),
            delay_after_ms: u64::try_from(s.paste_delay_after_ms).unwrap_or(0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Chord {
    CtrlV,
    ShiftInsert,
    CtrlShiftV,
}

/// The clipboard before paste.
enum Saved {
    Text(String),
    Image(ImageData<'static>),
    /// Wayland: the first offered type of a clipboard without text.
    #[cfg(target_os = "linux")]
    Bytes {
        mime: String,
        data: Vec<u8>,
    },
    /// Wayland: the clipboard couldn't be read, so the restore leaves the transcript rather than wiping it.
    #[cfg(target_os = "linux")]
    Unreadable,
    Nothing,
}

/// Inserts `text` into the focused app. Blocking (run on a dedicated paste thread). `Err(notice)`: [`PASTE_FAILED`]
/// when the text is on the clipboard, else [`not_copied`].
pub fn paste(text: &str, options: &PasteOptions) -> Result<(), String> {
    let mut clipboard = CLIPBOARD.lock().unwrap_or_else(PoisonError::into_inner);
    let text = if options.append_trailing_space { format!("{text} ") } else { text.to_owned() };
    info!(
        "Using paste method: {}, delay before: {}ms, delay after: {}ms",
        options.method, options.delay_ms, options.delay_after_ms
    );
    let chord = match options.method.as_str() {
        "direct" => None,
        "shift_insert" => Some(Chord::ShiftInsert),
        "ctrl_shift_v" => Some(Chord::CtrlShiftV),
        _ => Some(Chord::CtrlV),
    };
    match chord {
        Some(chord) => paste_via_clipboard(&mut clipboard, &text, chord, options),
        None => paste_direct(&text).map_err(|e| {
            log::warn!("Paste failed: {e}");
            match write_clipboard(&mut clipboard, &text) {
                Ok(()) => PASTE_FAILED.to_owned(),
                Err(e) => {
                    log::warn!("Couldn't put the text on the clipboard: {e}");
                    not_copied(&text)
                }
            }
        }),
    }
}

/// Puts `text` on the clipboard (after a paste that failed unexpectedly).
pub fn copy(text: &str) -> Result<(), String> {
    let mut clipboard = CLIPBOARD.lock().unwrap_or_else(PoisonError::into_inner);
    write_clipboard(&mut clipboard, text)
}

/// What Wayland paste still needs, as helper sentences ("Install wl-clipboard.", "Install ydotool and start
/// ydotoold."); empty when not on Wayland.
pub fn text_input_missing() -> Vec<&'static str> {
    #[cfg(target_os = "linux")]
    return linux::text_input_missing();
    #[cfg(not(target_os = "linux"))]
    Vec::new()
}

fn paste_via_clipboard(
    clipboard: &mut Option<Clipboard>,
    text: &str,
    chord: Chord,
    options: &PasteOptions,
) -> Result<(), String> {
    let saved = save_clipboard(clipboard);
    if let Err(e) = write_clipboard(clipboard, text) {
        log::warn!("Couldn't put the text on the clipboard: {e}");
        return Err(not_copied(text));
    }
    sleep(Duration::from_millis(options.delay_ms));
    finish_clipboard_paste(send_chord(chord), options.delay_after_ms, || {
        if options.restore_clipboard {
            restore_clipboard(clipboard, saved);
        }
    })
    .map_err(|e| {
        log::warn!("Paste failed: {e}");
        PASTE_FAILED.to_owned()
    })
}

/// After a sent chord, waits for the target to read the clipboard and restores it. After a failed one the transcript
/// stays on the clipboard, so the dictation isn't lost.
fn finish_clipboard_paste(
    paste_result: Result<(), String>,
    paste_delay_after_ms: u64,
    restore_clipboard: impl FnOnce(),
) -> Result<(), String> {
    if paste_result.is_ok() {
        sleep(Duration::from_millis(paste_delay_after_ms));
        restore_clipboard();
    }
    paste_result
}

/// Text, else an image (arboard) or the first offered type (wl-clipboard), so a screenshot isn't wiped. The image is
/// only probed without text: reading one decodes the full bitmap.
fn save_clipboard(clipboard: &mut Option<Clipboard>) -> Saved {
    #[cfg(target_os = "linux")]
    if linux::wl_clipboard() {
        return linux::wl_save();
    }
    let Ok(clipboard) = arboard(clipboard) else {
        return Saved::Nothing;
    };
    if let Some(text) = clipboard.get_text().ok().filter(|text| !text.is_empty()) {
        return Saved::Text(text);
    }
    clipboard.get_image().map_or(Saved::Nothing, Saved::Image)
}

fn write_clipboard(clipboard: &mut Option<Clipboard>, text: &str) -> Result<(), String> {
    // On Wayland, arboard is tried once more if wl-copy fails.
    #[cfg(target_os = "linux")]
    if linux::wl_clipboard() {
        match linux::wl_copy(text) {
            Ok(()) => return Ok(()),
            Err(e) => log::warn!("wl-copy failed, trying arboard: {e}"),
        }
    }
    arboard(clipboard)?.set_text(text).map_err(|e| format!("Failed to write to clipboard: {e}"))
}

fn restore_clipboard(clipboard: &mut Option<Clipboard>, saved: Saved) {
    let result = match saved {
        Saved::Text(text) => write_clipboard(clipboard, &text),
        Saved::Image(image) => {
            info!("Restoring image to clipboard");
            arboard(clipboard).and_then(|c| c.set_image(image).map_err(|e| e.to_string()))
        }
        #[cfg(target_os = "linux")]
        Saved::Bytes { mime, data } => {
            info!("Restoring {mime} to clipboard");
            linux::wl_copy_bytes(&mime, &data)
        }
        #[cfg(target_os = "linux")]
        Saved::Unreadable => {
            info!("The clipboard couldn't be saved; leaving the transcript on it");
            Ok(())
        }
        // Nothing was there to begin with: don't leave the transcript behind.
        Saved::Nothing => clear_clipboard(clipboard),
    };
    if let Err(e) = result {
        log::warn!("Restoring the clipboard failed: {e}");
    }
}

fn clear_clipboard(clipboard: &mut Option<Clipboard>) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    if linux::wl_clipboard() {
        return linux::wl_clear();
    }
    arboard(clipboard)?.clear().map_err(|e| e.to_string())
}

fn arboard(slot: &mut Option<Clipboard>) -> Result<&mut Clipboard, String> {
    if slot.is_none() {
        *slot = Some(Clipboard::new().map_err(|e| format!("Failed to open the clipboard: {e}"))?);
    }
    Ok(slot.as_mut().expect("set above"))
}

fn enigo() -> Result<Enigo, String> {
    Enigo::new(&enigo::Settings::default()).map_err(|e| format!("Failed to initialize Enigo: {e}"))
}

fn send_chord(chord: Chord) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if linux::try_send_chord(chord) {
            return Ok(());
        }
        // enigo's XTest only reaches XWayland apps, so the paste would silently go nowhere.
        if linux::wayland() {
            return Err("ydotool couldn't send the paste keys".into());
        }
    }
    #[cfg(windows)]
    let (v, insert) = (Key::Other(0x56), Key::Other(0x2D)); // VK_V, VK_INSERT: layout-independent
    #[cfg(not(windows))]
    let (v, insert) = (Key::Unicode('v'), Key::Insert);
    let (modifiers, key) = match chord {
        Chord::CtrlV => (&[Key::Control][..], v),
        Chord::CtrlShiftV => (&[Key::Control, Key::Shift][..], v),
        Chord::ShiftInsert => (&[Key::Shift][..], insert),
    };
    // Handy's 100 ms hold: a mistimed chord can't be detected here.
    send_paste_chord(&mut enigo()?, modifiers, key, 100)
}

/// Presses the modifiers, clicks `key` and keeps the modifiers held for `hold_ms`: most apps read them from the key
/// event, but some poll the keyboard state while handling it.
fn send_paste_chord(enigo: &mut Enigo, modifiers: &[Key], key: Key, hold_ms: u64) -> Result<(), String> {
    for &modifier in modifiers {
        enigo.key(modifier, Direction::Press).map_err(|e| format!("Failed to press modifier key: {e}"))?;
    }
    enigo.key(key, Direction::Click).map_err(|e| format!("Failed to click paste key: {e}"))?;
    sleep(Duration::from_millis(hold_ms));
    for &modifier in modifiers.iter().rev() {
        enigo.key(modifier, Direction::Release).map_err(|e| format!("Failed to release modifier key: {e}"))?;
    }
    Ok(())
}

fn paste_direct(text: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if let Some(result) = linux::try_type_text(text) {
            return result;
        }
        if linux::wayland() {
            return Err("ydotool isn't installed".into());
        }
        info!("Falling back to enigo for direct text input");
    }
    enigo()?.text(text).map_err(|e| format!("Failed to send text directly: {e}"))
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;

    #[test]
    fn clipboard_is_kept_when_key_injection_error_is_returned() {
        let restored = Cell::new(false);
        let result = finish_clipboard_paste(Err("input failed".into()), 0, || restored.set(true));

        assert_eq!(result.unwrap_err(), "input failed");
        assert!(!restored.get());
    }

    #[test]
    fn clipboard_is_restored_after_paste() {
        let restored = Cell::new(false);
        let result = finish_clipboard_paste(Ok(()), 0, || restored.set(true));

        assert!(result.is_ok());
        assert!(restored.get());
    }
}
