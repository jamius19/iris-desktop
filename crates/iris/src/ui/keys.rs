/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! API keys in the OS keychain. Every call runs off the UI thread; the field learns the result from a key event.

use iris_core::copy::{API_KEY_DELETE_ERROR, API_KEY_SAVE_ERROR};
use iris_core::settings::SecretStore;
use iris_core::settings::secrets::{OPENAI_API_KEY, SONIOX_API_KEY};
use slint::ComponentHandle;

use super::{AppState, catalogs, key_event, off_thread, refresh, with_state};
use crate::{MainWindow, Prefs};

pub fn wire(window: &MainWindow) {
    let prefs = window.global::<Prefs>();
    prefs.on_save_key(|target, name, value| {
        let value = value.trim().to_string();
        if value.is_empty() {
            return;
        }
        with_state(|state| {
            off_thread(
                state,
                move |secrets| secrets.put(&name, &value).map(|_| name),
                move |state, result| match result {
                    Ok(name) => {
                        state.stored_secrets.insert(name.to_string());
                        state.unreadable_secrets.remove(name.as_str());
                        key_changed(state, &name);
                        key_event(state, &target, "");
                    }
                    Err(e) => {
                        log::warn!("Saving an API key failed: {e}");
                        key_event(state, &target, API_KEY_SAVE_ERROR);
                    }
                },
            )
        });
    });
    prefs.on_clear_key(|target, name| {
        with_state(|state| {
            off_thread(
                state,
                move |secrets| secrets.clear(&name).map(|_| name),
                move |state, result| match result {
                    Ok(name) => {
                        state.stored_secrets.remove(name.as_str());
                        state.unreadable_secrets.remove(name.as_str());
                        key_changed(state, &name);
                        key_event(state, &target, "");
                    }
                    Err(e) => {
                        log::warn!("Deleting an API key failed: {e}");
                        key_event(state, &target, API_KEY_DELETE_ERROR);
                    }
                },
            )
        });
    });
}

/// A key was saved, replaced or cleared: the lists that depend on it (OpenAI's, Soniox's) are dropped and reloaded,
/// then the pages refresh.
fn key_changed(state: &mut AppState, name: &str) {
    match name {
        OPENAI_API_KEY => catalogs::openai_key_changed(state),
        SONIOX_API_KEY => state.dictation.providers.soniox_catalog.reload(),
        _ => {}
    }
    refresh(state);
}

/// Finds which keys are stored (keychains can't be listed: each name is probed). A key saved or cleared while the
/// probe ran (the key-event sequence moved) may be missing from its result, so it probes again.
pub fn probe_stored() {
    with_state(probe);
}

fn probe(state: &mut AppState) {
    let settings = state.store.settings(&state.stored_secrets);
    let names = SecretStore::names(settings.custom_cleanup_providers.iter().map(|p| p.id.as_str()));
    let started_at = state.key_op_seq;
    off_thread(
        state,
        move |secrets| secrets.stored(&names),
        move |state, found| {
            if state.key_op_seq != started_at {
                probe(state);
                return;
            }
            state.stored_secrets = found.stored;
            state.unreadable_secrets = found.unreadable;
            refresh(state);
        },
    );
}
