/*
* Author: Jamius Siam
* Since: 18/11/2026
*/

//! The History page: fills `HistoryList` from the history, again after every change; copy, delete and clear.

use std::time::Duration;

use iris_core::dictation::history::HistoryEntry;
use slint::{ComponentHandle, SharedString};
use tokio::runtime::Runtime;

use super::recovery::COPY_FAILED;
use super::{AppState, model, with_state};
use crate::dictation::Dictation;
use crate::{HistoryItem, HistoryList, MainWindow};

pub fn wire(window: &MainWindow, runtime: &Runtime, dictation: &Dictation) {
    let list = window.global::<HistoryList>();
    list.set_copy_error(COPY_FAILED.into());
    list.on_copy(|key, text| with_state(|s| copy(s, key, text)));
    list.on_delete(|id| {
        with_state(|s| {
            reset_copy_state(&s.window);
            s.dictation.history.delete(id)
        })
    });
    list.on_clear(|| {
        with_state(|s| {
            reset_copy_state(&s.window);
            s.dictation.history.clear()
        })
    });
    let mut entries = dictation.history.entries();
    fill(window, &entries.borrow_and_update());
    runtime.spawn(async move {
        while entries.changed().await.is_ok() {
            let current = entries.borrow_and_update().clone();
            let _ = slint::invoke_from_event_loop(move || with_state(|s| fill(&s.window, &current)));
        }
    });
}

fn fill(window: &MainWindow, entries: &[HistoryEntry]) {
    window.global::<HistoryList>().set_items(model(entries.iter().map(item).collect()));
}

/// Ids can be reused after a delete or clear: no check or error carries over to a new entry.
fn reset_copy_state(window: &MainWindow) {
    let list = window.global::<HistoryList>();
    list.set_failed(SharedString::new());
    list.set_copied(SharedString::new());
}

fn item(entry: &HistoryEntry) -> HistoryItem {
    HistoryItem {
        id: entry.id,
        time: time_text(entry.timestamp_ms).into(),
        raw: entry.raw.clone().into(),
        final_text: entry.final_text.clone().unwrap_or_default().into(),
    }
}

/// Local time, "18 Nov 2026, 2:05 PM".
fn time_text(timestamp_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(timestamp_ms)
        .map(|t| t.with_timezone(&chrono::Local).format("%-d %b %Y, %-I:%M %p").to_string())
        .unwrap_or_default()
}

/// Copies off the UI thread (wl-copy can take a while); the button shows a check for 1.5 s, or the item the error.
fn copy(state: &mut AppState, key: SharedString, text: SharedString) {
    state.runtime.spawn_blocking(move || {
        let copied = crate::paste::copy(&text);
        let _ = slint::invoke_from_event_loop(move || {
            with_state(|state| {
                let list = state.window.global::<HistoryList>();
                match copied {
                    Ok(()) => {
                        list.set_failed(SharedString::new());
                        list.set_copied(key.clone());
                        slint::Timer::single_shot(Duration::from_millis(1_500), move || {
                            with_state(|state| {
                                let list = state.window.global::<HistoryList>();
                                if list.get_copied() == key {
                                    list.set_copied(SharedString::new());
                                }
                            })
                        });
                    }
                    Err(e) => {
                        log::warn!("Copying a history entry failed: {e}");
                        list.set_copied(SharedString::new());
                        list.set_failed(key);
                    }
                }
            })
        });
    });
}
