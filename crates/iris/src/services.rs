/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Home's Services checks: the microphone, and on Wayland the text input tools. Re-checked when Home shows again.
//! (Global Shortcut comes with the hotkeys in M2.)

use slint::ComponentHandle;

use crate::ui::{refresh, with_state};
use crate::{MainWindow, Services};

/// Where the README explains the Linux setup (ydotoold, wl-clipboard, compositor binding).
const LINUX_SETUP_URL: &str = "https://github.com/jamius19/iris-desktop#linux";

#[derive(Default)]
pub struct ServiceStatus {
    /// Microphones (id, name), monitors excluded.
    pub microphones: Vec<(String, String)>,
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
            let missing = text_input_missing();
            let _ = slint::invoke_from_event_loop(move || {
                with_state(|state| {
                    let services = state.window.global::<Services>();
                    services.set_microphone_found(!microphones.is_empty());
                    services.set_text_input_ready(missing.is_empty());
                    services.set_text_input_helper(missing.join(" ").into());
                    state.services.microphones = microphones;
                    refresh(state);
                })
            });
        });
    });
}

/// What paste on Wayland still needs, as helper sentences.
fn text_input_missing() -> Vec<&'static str> {
    if !wayland() {
        return Vec::new();
    }
    let mut missing = Vec::new();
    if !on_path("wl-copy") || !on_path("wl-paste") {
        missing.push("Install wl-clipboard.");
    }
    if !on_path("ydotool") || !ydotoold_running() {
        missing.push("Install ydotool and start ydotoold.");
    }
    missing
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| dir.join(program).is_file()))
}

/// ydotoold's socket exists in one of the places ydotool looks (Handy's list; the owner check comes with paste in M2).
fn ydotoold_running() -> bool {
    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(|dir| std::path::Path::new(&dir).join(".ydotool_socket"));
    runtime_dir
        .into_iter()
        .chain(["/tmp/.ydotool_socket".into(), "/run/ydotoold/socket".into()])
        .any(|socket| socket.exists())
}
