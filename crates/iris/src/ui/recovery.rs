/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Home's "Last dictation wasn't inserted" card: a transcript that could be neither pasted nor copied stays in memory
//! (a Slint property, never on disk or in the log) until it is copied, dismissed or replaced by the next one.

use slint::{ComponentHandle, SharedString};

use super::{AppState, with_state};
use crate::{MainWindow, Services};

pub(super) const COPY_FAILED: &str = "Couldn't copy. Select the text and press Ctrl+C.";

pub fn wire(window: &MainWindow) {
    let services = window.global::<Services>();
    services.on_copy_lost_transcript(|| with_state(copy));
    services.on_dismiss_lost_transcript(|| with_state(|state| show(state, SharedString::new())));
}

/// Shows `text` on Home, replacing an earlier one. Any thread.
pub fn keep(text: String) {
    let _ = slint::invoke_from_event_loop(move || with_state(|state| show(state, text.into())));
}

fn show(state: &AppState, text: SharedString) {
    let services = state.window.global::<Services>();
    services.set_lost_transcript(text);
    services.set_lost_transcript_error(SharedString::new());
}

/// Copies off the UI thread (wl-copy can take a while); clears the card once copied, unless a newer text replaced it.
fn copy(state: &mut AppState) {
    let text = state.window.global::<Services>().get_lost_transcript();
    state.runtime.spawn_blocking(move || {
        let copied = crate::paste::copy(&text);
        let _ = slint::invoke_from_event_loop(move || {
            with_state(|state| {
                let services = state.window.global::<Services>();
                if services.get_lost_transcript() != text {
                    return;
                }
                match copied {
                    Ok(()) => show(state, SharedString::new()),
                    Err(e) => {
                        log::warn!("Copying the last dictation failed: {e}");
                        services.set_lost_transcript_error(COPY_FAILED.into());
                    }
                }
            })
        });
    });
}
