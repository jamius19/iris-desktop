/*
* Author: Jamius Siam
* Since: 18/11/2026
*/

//! Desktop only (Android has no history).

use super::*;
use crate::settings::{Values, to_settings};
use std::collections::HashSet;
use std::time::Duration;

fn settings(enabled: bool, limit: i32) -> Settings {
    Settings { history_enabled: enabled, history_limit: limit, ..to_settings(&Values::new(), &HashSet::new()).unwrap() }
}

fn entry(id: i32, timestamp_ms: i64, raw: &str, final_text: Option<&str>) -> HistoryEntry {
    HistoryEntry { id, timestamp_ms, raw: raw.into(), final_text: final_text.map(Into::into) }
}

struct Store {
    _dir: tempfile::TempDir,
    path: PathBuf,
    history: History,
}

impl Store {
    fn entries(&self) -> Vec<HistoryEntry> {
        self.history.entries().borrow().clone()
    }

    /// Waits until the file holds `expected`.
    async fn assert_stored(&self, expected: &[HistoryEntry]) {
        for _ in 0..500 {
            if load(&self.path) == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(expected, load(&self.path));
    }

    /// Records `raw` (pasted as is) at `n` ms with the default limit.
    fn record(&self, n: i64, raw: &str) {
        self.history.record(&settings(true, 20), n, raw, raw);
    }
}

fn history(initial: Option<&str>) -> Store {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("history.json");
    if let Some(initial) = initial {
        fs::write(&path, initial).unwrap();
    }
    let history = History::new(path.clone(), Handle::current());
    Store { _dir: dir, path, history }
}

#[tokio::test]
async fn a_missing_or_corrupt_file_reads_as_empty_and_is_replaced() {
    assert_eq!(Vec::<HistoryEntry>::new(), history(None).entries());
    let store = history(Some("{not json"));
    assert_eq!(Vec::<HistoryEntry>::new(), store.entries());

    store.record(1_000, "hello");

    store.assert_stored(&[entry(1, 1_000, "hello", None)]).await;
}

#[tokio::test]
async fn record_adds_the_newest_first() {
    let store = history(None);
    store.record(1_000, "one");
    store.record(2_000, "two");

    let expected = vec![entry(2, 2_000, "two", None), entry(1, 1_000, "one", None)];
    assert_eq!(expected, store.entries());
    store.assert_stored(&expected).await;
}

#[tokio::test]
async fn record_keeps_only_the_limit() {
    let store = history(None);
    for n in 1..=6 {
        store.history.record(&settings(true, 5), n, &format!("text {n}"), &format!("text {n}"));
    }

    let expected: Vec<_> = (2..=6).rev().map(|n| entry(n as i32, n, &format!("text {n}"), None)).collect();
    assert_eq!(expected, store.entries());
    store.assert_stored(&expected).await;
}

#[tokio::test]
async fn lowering_the_limit_trims_the_oldest() {
    let store = history(None);
    store.record(1, "one");
    store.record(2, "two");
    store.record(3, "three");

    store.history.trim(2);

    let expected = vec![entry(3, 3, "three", None), entry(2, 2, "two", None)];
    assert_eq!(expected, store.entries());
    store.assert_stored(&expected).await;
    store.history.trim(10);
    assert_eq!(expected, store.entries());
}

#[tokio::test]
async fn disabled_history_records_nothing() {
    let store = history(None);
    store.history.record(&settings(false, 20), 1, "hello", "Hello.");

    assert_eq!(Vec::<HistoryEntry>::new(), store.entries());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!store.path.exists());
}

#[tokio::test]
async fn delete_removes_only_that_entry() {
    let store = history(None);
    store.record(1, "one");
    store.record(2, "two");
    store.record(3, "three");

    store.history.delete(2);

    let expected = vec![entry(3, 3, "three", None), entry(1, 1, "one", None)];
    assert_eq!(expected, store.entries());
    store.history.delete(99);
    assert_eq!(expected, store.entries());
    store.assert_stored(&expected).await;
}

#[tokio::test]
async fn clear_removes_every_entry() {
    let store = history(None);
    store.record(1, "one");
    store.record(2, "two");
    store.assert_stored(&store.entries()).await;

    store.history.clear();

    assert_eq!(Vec::<HistoryEntry>::new(), store.entries());
    store.assert_stored(&[]).await;
    assert_eq!(r#"{"entries":[]}"#, fs::read_to_string(&store.path).unwrap());
}

#[tokio::test]
async fn final_text_is_kept_only_when_it_differs() {
    let store = history(None);
    store.history.record(&settings(true, 20), 1, "uh hello", "Hello.");
    store.history.record(&settings(true, 20), 2, "hi", "hi");

    let entries = store.entries();
    assert_eq!(Some("Hello.".to_string()), entries[1].final_text);
    assert_eq!(None, entries[0].final_text);
    assert!(!encode(&entries[..1]).contains("finalText"));
    assert!(encode(&entries[1..]).contains(r#""finalText":"Hello.""#));
}

#[tokio::test]
async fn history_survives_a_restart() {
    let store = history(None);
    store.history.record(&settings(true, 20), 1, "uh hello", "Hello.");
    store.record(2, "two");
    let before = store.entries();
    store.assert_stored(&before).await;

    let reopened = History::new(store.path.clone(), Handle::current());

    assert_eq!(before, *reopened.entries().borrow());
}
