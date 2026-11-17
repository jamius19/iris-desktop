/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Home's Services checks: the microphone, the global shortcut, and on Wayland the text input tools. Re-checked when
//! Home shows again and when the window gets focus.

use slint::ComponentHandle;

use crate::hotkey::ShortcutStatus;
use crate::ui::{refresh, with_state};
use crate::{MainWindow, Services};

/// Where the README explains the Linux setup (ydotoold, wl-clipboard, compositor binding).
const LINUX_SETUP_URL: &str = "https://github.com/jamius19/iris-desktop#linux";

#[derive(Default)]
pub struct ServiceStatus {
    /// Microphones (id, name), monitors excluded.
    pub microphones: Vec<(String, String)>,
    /// The transcribe shortcut's registration (None before the first).
    pub shortcut: Option<ShortcutStatus>,
}

/// The app runs in a Wayland session (paste then needs wl-clipboard and ydotool).
pub fn wayland() -> bool {
    cfg!(target_os = "linux") && std::env::var_os("WAYLAND_DISPLAY").is_some()
}

pub fn wire(window: &MainWindow) {
    let services = window.global::<Services>();
    services.set_wayland(wayland());
    services.on_microphone_setup(|| {
        if cfg!(windows) {
            let _ = open::that_detached("ms-settings:privacy-microphone");
        }
        check();
    });
    services.on_text_input_setup(|| {
        let _ = open::that_detached(LINUX_SETUP_URL);
    });
    services.on_recheck(check);
}

/// Checks the services off the UI thread and updates Home.
pub fn check() {
    with_state(|state| {
        state.runtime.spawn_blocking(|| {
            let microphones = match handy_recorder::list_input_devices() {
                Ok(devices) => devices.into_iter().filter(|d| !d.is_monitor).map(|d| (d.id, d.name)).collect(),
                Err(e) => {
                    log::warn!("Listing microphones failed: {e}");
                    Vec::new()
                }
            };
            let missing = crate::paste::text_input_missing();
            let _ = slint::invoke_from_event_loop(move || {
                with_state(|state| {
                    let services = state.window.global::<Services>();
                    services.set_microphone_found(!microphones.is_empty());
                    services.set_text_input_ready(missing.is_empty());
                    services.set_text_input_helper(missing.join(" ").into());
                    state.services.microphones = microphones;
                    // Keyboard access may have been set up meanwhile (Linux udev rule).
                    if state.services.shortcut == Some(ShortcutStatus::NoAccess) {
                        crate::ui::register_shortcut(state);
                    }
                    refresh(state);
                })
            });
        });
    });
}
