/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! API keys in the OS keychain. Every call runs off the UI thread; the field learns the result from a key event.

use iris_core::copy::{API_KEY_DELETE_ERROR, API_KEY_SAVE_ERROR};
use iris_core::settings::SecretStore;
use slint::ComponentHandle;

use super::{key_event, off_thread, refresh, with_state};
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
                        refresh(state);
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
                        refresh(state);
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

/// Finds which keys are stored (keychains can't be listed: each name is probed).
pub fn probe_stored() {
    with_state(|state| {
        let settings = state.store.settings(&state.stored_secrets);
        let names = SecretStore::names(settings.custom_cleanup_providers.iter().map(|p| p.id.as_str()));
        off_thread(
            state,
            move |secrets| secrets.stored(&names),
            |state, stored| {
                state.stored_secrets = stored;
                refresh(state);
            },
        );
    });
}
