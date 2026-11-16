/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The global shortcuts: the Dictation page's Shortcut section (key caps, recording, reset) and Home's Global
//! Shortcut row (registration status, Setup).

use iris_core::settings::{DEFAULT_SHORTCUT_CANCEL, DEFAULT_SHORTCUT_TRANSCRIBE, Settings};
use slint::{ComponentHandle, SharedString};

use super::{AppState, model, refresh, saved, with_state};
use crate::hotkey::ShortcutStatus;
use crate::{MainWindow, Prefs, Services};

const IN_USE: &str = "Another app uses this shortcut. Pick another one.";
const INVALID: &str = "This shortcut doesn't work here. Pick another one.";
const NO_ACCESS: &str = "Iris can't read the keyboard. Set up keyboard access, or bind `iris --toggle-transcription` \
                         in your desktop's keyboard settings.";

pub fn wire(window: &MainWindow) {
    let prefs = window.global::<Prefs>();
    prefs.on_record_shortcut(|key| {
        with_state(|state| {
            state.window.global::<Prefs>().set_recording_shortcut(key.clone());
            let key = key.to_string();
            state.dictation.hotkeys.record(move |binding| {
                let _ = slint::invoke_from_event_loop(move || with_state(|state| recorded(state, &key, binding)));
            });
        })
    });
    prefs.on_stop_recording_shortcut(|| with_state(|state| state.dictation.hotkeys.stop_recording()));
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

/// (Re)registers the transcribe shortcut; Linux retries keyboard access each time (after the udev Setup).
pub fn register(state: &mut AppState) {
    let binding = state.store.settings(&state.stored_secrets).shortcut_transcribe;
    let status = state.dictation.hotkeys.register_transcribe(&binding);
    if status != ShortcutStatus::Registered {
        log::warn!("The transcribe shortcut {binding} isn't registered: {status:?}");
    }
    state.services.shortcut = Some(status);
}

/// A recording finished: saves the binding (None = cancelled).
fn recorded(state: &mut AppState, key: &str, binding: Option<String>) {
    state.window.global::<Prefs>().set_recording_shortcut(SharedString::new());
    match binding {
        Some(binding) => save(state, key, &binding),
        // The transcribe shortcut comes back on after a recording.
        None => refresh(state),
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
        register(state);
        if error.is_empty() {
            refresh(state);
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
