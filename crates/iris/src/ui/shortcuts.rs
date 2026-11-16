/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The global shortcuts: the Dictation page's Shortcut section (key caps, recording, reset) and Home's Global
//! Shortcut row (registration status, Setup).

use std::sync::atomic::{AtomicU64, Ordering};

use iris_core::settings::{DEFAULT_SHORTCUT_CANCEL, DEFAULT_SHORTCUT_TRANSCRIBE, Settings};
use slint::{ComponentHandle, SharedString};

use super::{AppState, model, refresh, saved, with_state};
use crate::hotkey::ShortcutStatus;
use crate::{MainWindow, Prefs, Services};

const IN_USE: &str = "Another app uses this shortcut. Pick another one.";
const INVALID: &str = "This shortcut doesn't work here. Pick another one.";
const NO_ACCESS: &str = "Iris can't read the keyboard. Set up keyboard access, or bind `iris --toggle-transcription` \
                         in your desktop's keyboard settings.";

/// The latest shortcut recording.
static RECORDING: AtomicU64 = AtomicU64::new(0);

pub fn wire(window: &MainWindow) {
    let prefs = window.global::<Prefs>();
    prefs.on_record_shortcut(|key| {
        with_state(|state| {
            // A new recording ends the previous one, whose late results are then ignored.
            let recording = RECORDING.fetch_add(1, Ordering::SeqCst) + 1;
            log::debug!("Shortcut recording requested for {key} (UI {recording})");
            let latest = move || {
                let latest = RECORDING.load(Ordering::SeqCst);
                if latest != recording {
                    log::debug!("Dropped a result of shortcut recording UI {recording}: UI {latest} is newer");
                }
                latest == recording
            };
            let prefs = state.window.global::<Prefs>();
            prefs.set_recording_shortcut(key.clone());
            prefs.set_recording_caps(model(Vec::new()));
            let key = key.to_string();
            state.dictation.hotkeys.record(
                move |keys| {
                    let _ = slint::invoke_from_event_loop(move || {
                        if latest() {
                            with_state(|state| state.window.global::<Prefs>().set_recording_caps(model(caps(&keys))));
                        }
                    });
                },
                move |binding| {
                    let _ = slint::invoke_from_event_loop(move || {
                        if latest() {
                            log::debug!("Shortcut recording UI {recording} done: {binding:?}");
                            with_state(|state| recorded(state, &key, binding));
                        }
                    });
                },
            );
        })
    });
    prefs.on_stop_recording_shortcut(|| {
        log::debug!("Shortcut recording: the field asked to stop (Esc or focus lost)");
        with_state(|state| state.dictation.hotkeys.stop_recording())
    });
    prefs.on_reset_shortcut(|key| {
        with_state(|state| {
            let default = if key == "cancel" { DEFAULT_SHORTCUT_CANCEL } else { DEFAULT_SHORTCUT_TRANSCRIBE };
            save(state, &key, default);
        })
    });
    window.global::<Services>().on_shortcut_setup(|| with_state(setup));
}

/// Shows the shortcut settings and the Global Shortcut row.
pub fn fill(state: &AppState, settings: &Settings) {
    let prefs = state.window.global::<Prefs>();
    prefs.set_shortcut_transcribe_caps(model(caps(&settings.shortcut_transcribe)));
    prefs.set_shortcut_cancel_caps(model(caps(&settings.shortcut_cancel)));
    prefs.set_shortcut_activation(settings.shortcut_activation.clone().into());
    let status = state.services.shortcut;
    let field_error = match status {
        Some(ShortcutStatus::InUse) => IN_USE,
        Some(ShortcutStatus::Invalid) => INVALID,
        _ => "",
    };
    prefs.set_shortcut_transcribe_error(field_error.into());
    let services = state.window.global::<Services>();
    services.set_shortcut_registered(status == Some(ShortcutStatus::Registered));
    services.set_shortcut_helper(
        match status {
            Some(ShortcutStatus::NoAccess) => NO_ACCESS,
            Some(ShortcutStatus::Registered) | None => "",
            _ => field_error,
        }
        .into(),
    );
}

/// (Re)registers the transcribe shortcut on the hotkey thread; Linux retries keyboard access each time (after the udev
/// Setup). The status comes back later and refreshes the UI; only the latest request's status is shown.
pub fn register(state: &mut AppState) {
    static LATEST: AtomicU64 = AtomicU64::new(0);
    let request = LATEST.fetch_add(1, Ordering::SeqCst) + 1;
    let binding = state.store.settings(&state.stored_secrets).shortcut_transcribe;
    state.dictation.hotkeys.register_transcribe(&binding.clone(), move |status| {
        if status != ShortcutStatus::Registered {
            log::warn!("The transcribe shortcut {binding} isn't registered: {status:?}");
        }
        let _ = slint::invoke_from_event_loop(move || {
            if LATEST.load(Ordering::SeqCst) == request {
                with_state(|state| {
                    state.services.shortcut = Some(status);
                    refresh(state);
                });
            }
        });
    });
}

/// A recording finished: saves the binding (Ok(None) = cancelled; Err = the keyboard can't be read).
fn recorded(state: &mut AppState, key: &str, binding: Result<Option<String>, ShortcutStatus>) {
    let prefs = state.window.global::<Prefs>();
    prefs.set_recording_shortcut(SharedString::new());
    prefs.set_recording_caps(model(Vec::new()));
    match binding {
        Ok(Some(binding)) => save(state, key, &binding),
        // The transcribe shortcut comes back on after a recording.
        Ok(None) => refresh(state),
        // Shown on Home's Global Shortcut row, and under the Transcribe field until the next refresh.
        Err(status) => {
            state.services.shortcut = Some(status);
            refresh(state);
            state.window.global::<Prefs>().set_shortcut_transcribe_error(NO_ACCESS.into());
        }
    }
}

fn save(state: &mut AppState, key: &str, binding: &str) {
    let result = if key == "cancel" {
        state.store.set_shortcut_cancel(binding)
    } else {
        state.store.set_shortcut_transcribe(binding)
    };
    let error = saved(state, result);
    if key != "cancel" {
        // A failed save leaves the stored binding, which the hotkey thread still has (re-registered after recording).
        if error.is_empty() {
            register(state);
        } else {
            state.window.global::<Prefs>().set_shortcut_transcribe_error(error);
        }
    }
}

/// Global Shortcut → Setup: Linux without keyboard access installs the udev rule (pkexec) and retries; otherwise
/// the Dictation page.
fn setup(state: &mut AppState) {
    #[cfg(target_os = "linux")]
    if state.services.shortcut == Some(ShortcutStatus::NoAccess) {
        // pkexec waits for the password dialog: off the UI thread.
        state.runtime.spawn_blocking(|| {
            let result = crate::hotkey::install_udev_rule();
            let _ = slint::invoke_from_event_loop(move || {
                with_state(|state| {
                    if let Err(e) = result {
                        log::warn!("Installing the keyboard access rule failed: {e}");
                    }
                    register(state);
                    refresh(state);
                })
            });
        });
        return;
    }
    state.window.invoke_open_page(5);
}

/// The binding's keys as shown: `ctrl_left+space` → "Left Ctrl", "Space".
pub fn caps(binding: &str) -> Vec<SharedString> {
    binding.split('+').filter(|k| !k.is_empty()).map(|key| cap(key).into()).collect()
}

fn cap(key: &str) -> String {
    let (key, side) = match (key.strip_suffix("_left"), key.strip_suffix("_right")) {
        (Some(key), _) => (key, "Left "),
        (_, Some(key)) => (key, "Right "),
        _ => (key, ""),
    };
    let name = match key {
        "ctrl" | "control" => "Ctrl".to_string(),
        "alt" | "option" => "Alt".to_string(),
        "shift" => "Shift".to_string(),
        "super" | "meta" | "cmd" | "command" | "win" => (if cfg!(windows) { "Win" } else { "Super" }).to_string(),
        "space" => "Space".to_string(),
        "escape" | "esc" => "Esc".to_string(),
        other => {
            let mut chars = other.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    };
    format!("{side}{name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_name_the_keys() {
        let caps: Vec<String> = caps("ctrl_left+alt+space").iter().map(ToString::to_string).collect();
        assert_eq!(vec!["Left Ctrl", "Alt", "Space"], caps);
        assert_eq!(vec![SharedString::from("Esc")], super::caps("escape"));
        assert_eq!(vec![SharedString::from("F12")], super::caps("f12"));
    }
}
