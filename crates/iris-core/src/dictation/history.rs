/*
* Author: Jamius Siam
* Since: 18/11/2026
*/

//! Dictation history for the History page (desktop only; Android has none): the last `history_limit` dictations,
//! newest first, each with the provider's raw transcript and, when different, the final (pasted) text. One JSON file
//! in the local data folder, `{"entries":[{"id","timestampMs","raw","finalText"?}]}`, not in backups. Never logged.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::settings::Settings;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    /// Unique among the stored entries: one more than the newest's.
    pub id: i32,
    /// When it was recorded (Unix epoch milliseconds).
    pub timestamp_ms: i64,
    /// The speech-to-text output as the provider returned it.
    pub raw: String,
    /// What was pasted (after filler words, custom words and cleanup); None when it equals `raw`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub final_text: Option<String>,
}

#[derive(Deserialize)]
struct HistoryFile {
    entries: Vec<HistoryEntry>,
}

pub fn encode(entries: &[HistoryEntry]) -> String {
    serde_json::json!({ "entries": entries }).to_string()
}

/// None if `encoded` isn't a history file.
pub fn decode(encoded: &str) -> Option<Vec<HistoryEntry>> {
    serde_json::from_str::<HistoryFile>(encoded).ok().map(|file| file.entries)
}

/// The entries in the file at `path`: none if it is missing, or (logged, without its contents) unreadable or
/// corrupt; the next write starts over.
pub fn load(path: &Path) -> Vec<HistoryEntry> {
    match fs::read_to_string(path) {
        Ok(text) => decode(&text).unwrap_or_else(|| {
            log::warn!("History file {} is corrupt, starting over", path.display());
            Vec::new()
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(e) => {
            log::warn!("Couldn't read the history file {}, starting over: {e}", path.display());
            Vec::new()
        }
    }
}

/// The history, kept in memory and written to its file after each change. Recording never waits for the file: writes
/// run on the blocking pool, one at a time, each writing the latest entries.
#[derive(Clone)]
pub struct History {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    runtime: Handle,
    entries: watch::Sender<Vec<HistoryEntry>>,
    writing: Mutex<()>,
}

impl History {
    /// Loads `path` ([`load`]); `runtime` runs the writes.
    pub fn new(path: PathBuf, runtime: Handle) -> Self {
        let entries = watch::channel(load(&path)).0;
        Self { inner: Arc::new(Inner { path, runtime, entries, writing: Mutex::new(()) }) }
    }

    /// The entries, newest first, updated after every change.
    pub fn entries(&self) -> watch::Receiver<Vec<HistoryEntry>> {
        self.inner.entries.subscribe()
    }

    /// A delivered dictation; nothing when `s.history_enabled` is off. Keeps the newest `s.history_limit`.
    pub fn record(&self, s: &Settings, timestamp_ms: i64, raw: &str, final_text: &str) {
        if !s.history_enabled {
            return;
        }
        self.update(|entries| {
            let id = entries.first().map_or(1, |e| e.id + 1);
            let final_text = (final_text != raw).then(|| final_text.to_string());
            entries.insert(0, HistoryEntry { id, timestamp_ms, raw: raw.to_string(), final_text });
            entries.truncate(s.history_limit as usize);
            true
        });
    }

    /// Drops the oldest entries beyond `limit` (the limit was lowered).
    pub fn trim(&self, limit: i32) {
        self.update(|entries| {
            let before = entries.len();
            entries.truncate(limit as usize);
            entries.len() != before
        });
    }

    pub fn delete(&self, id: i32) {
        self.update(|entries| {
            let before = entries.len();
            entries.retain(|e| e.id != id);
            entries.len() != before
        });
    }

    pub fn clear(&self) {
        self.update(|entries| {
            let changed = !entries.is_empty();
            entries.clear();
            changed
        });
    }

    /// Applies `change`; writes the file only when it returns true (something changed).
    fn update(&self, change: impl FnOnce(&mut Vec<HistoryEntry>) -> bool) {
        if !self.inner.entries.send_if_modified(change) {
            return;
        }
        let inner = self.inner.clone();
        self.inner.runtime.spawn_blocking(move || {
            let _writing = inner.writing.lock().unwrap_or_else(|e| e.into_inner());
            // The latest entries, so writes finishing out of order still leave the newest in the file.
            let encoded = encode(&inner.entries.borrow());
            if let Err(e) = super::statistics::write(&inner.path, &encoded) {
                log::warn!("Couldn't write the history file {}: {e}", inner.path.display());
            }
        });
    }
}

#[cfg(test)]
mod tests;
