/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `SonioxCatalogTest.kt` on tokio's paused clock (no sockets here).

use super::*;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::oneshot;
use tokio::time::{advance, sleep};

fn rt() -> SonioxModel {
    SonioxModel {
        id: "stt-rt-v6".into(),
        transcription_mode: Some("real_time".into()),
        supports_language_hints_strict: true,
    }
}

fn asynchronous() -> SonioxModel {
    SonioxModel {
        id: "stt-async-v6".into(),
        transcription_mode: Some("async".into()),
        supports_language_hints_strict: false,
    }
}

/// Runs every task that is ready without moving the clock (Kotlin's `runCurrent`).
async fn run_current() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// Moves the clock by `ms` and runs what became ready (Kotlin's `advanceTimeBy`).
async fn advance_by(ms: u64) {
    run_current().await;
    advance(Duration::from_millis(ms)).await;
    run_current().await;
}

/// A list function that counts its calls and answers with `reply`.
fn counting<F, Fut>(fetches: &Arc<AtomicUsize>, reply: F) -> SonioxList
where
    F: Fn(usize) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<SonioxModel>, SttError>> + Send + 'static,
{
    let fetches = fetches.clone();
    Arc::new(move || {
        let n = fetches.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(reply(n))
    })
}

fn state(models: Option<Vec<SonioxModel>>, loading: bool, error: Option<&str>) -> SonioxCatalogState {
    SonioxCatalogState { models, loading, error: error.map(str::to_string) }
}

#[tokio::test(start_paused = true)]
async fn reload_loads() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let catalog =
        SonioxCatalog::new(Handle::current(), counting(&fetches, |_| async { Ok(vec![asynchronous(), rt()]) }));

    catalog.reload();
    assert!(catalog.state().borrow().loading);
    run_current().await;

    assert_eq!(state(Some(vec![asynchronous(), rt()]), false, None), *catalog.state().borrow());
    assert_eq!(1, fetches.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn loaded_entry_does_not_fetch_again() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let catalog = SonioxCatalog::new(Handle::current(), counting(&fetches, |_| async { Ok(vec![rt()]) }));
    catalog.reload();
    run_current().await;

    assert_eq!(Some(rt()), catalog.entry("stt-rt-v6").await);
    assert_eq!(None, catalog.entry("typed-model").await);
    assert_eq!(1, fetches.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn unloaded_entry_fetches_once() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let catalog = SonioxCatalog::new(
        Handle::current(),
        counting(&fetches, |_| async {
            sleep(Duration::from_millis(100)).await;
            Ok(vec![rt()])
        }),
    );

    assert_eq!(Some(rt()), catalog.entry("stt-rt-v6").await);
    assert_eq!(1, fetches.load(Ordering::SeqCst));
    assert_eq!(Some(rt()), catalog.entry("stt-rt-v6").await);
    assert_eq!(1, fetches.load(Ordering::SeqCst));
}

// The wait is injected (500 ms, Android 5 s) and the fetch scaled with it (800 ms, Android 8 s).
#[tokio::test(start_paused = true)]
async fn slow_fetch_gives_null_after_the_wait_and_still_fills_the_state() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let catalog = SonioxCatalog::with_wait(
        Handle::current(),
        counting(&fetches, |_| async {
            sleep(Duration::from_millis(800)).await;
            Ok(vec![rt()])
        }),
        Duration::from_millis(500),
    );

    let entry = tokio::spawn({
        let catalog = catalog.clone();
        async move { catalog.entry("stt-rt-v6").await }
    });
    run_current().await;
    advance_by(499).await;
    assert!(!entry.is_finished());
    advance_by(2).await;
    assert!(entry.is_finished());
    assert_eq!(None, entry.await.unwrap());
    assert!(catalog.state().borrow().loading);

    advance_by(300).await;
    assert_eq!(state(Some(vec![rt()]), false, None), *catalog.state().borrow());
}

// Reduced: Android's second case (a non-`SttException` from the list function, "Couldn't load models") has no Rust
// equivalent: the list function returns an `SttError` (the app maps a keychain failure to one). A second, different
// error replaces the first instead.
#[tokio::test(start_paused = true)]
async fn failure_sets_the_error() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let catalog = SonioxCatalog::new(
        Handle::current(),
        counting(&fetches, |n| async move {
            let message = if n == 1 { "Set your Soniox API key to load models" } else { "Couldn't load models" };
            Err(SttError(message.into()))
        }),
    );

    catalog.reload();
    run_current().await;
    assert_eq!(state(None, false, Some("Set your Soniox API key to load models")), *catalog.state().borrow());

    catalog.refresh();
    run_current().await;
    assert_eq!(state(None, false, Some("Couldn't load models")), *catalog.state().borrow());
    assert_eq!(None, catalog.entry("stt-rt-v6").await); // Fails again: None, no wait.
}

#[tokio::test(start_paused = true)]
async fn reload_mid_load_cancels_it_and_restarts() {
    let (first_tx, first_rx) = oneshot::channel::<Vec<SonioxModel>>();
    let first_rx = Arc::new(Mutex::new(Some(first_rx)));
    let fetches = Arc::new(AtomicUsize::new(0));
    let catalog = SonioxCatalog::new(
        Handle::current(),
        counting(&fetches, move |n| {
            let first = first_rx.lock().unwrap().take();
            async move {
                match (n, first) {
                    (1, Some(first)) => Ok(first.await.unwrap_or_default()),
                    _ => Ok(vec![asynchronous()]),
                }
            }
        }),
    );
    catalog.reload();
    run_current().await;

    catalog.reload(); // E.g. the key changed.
    let _ = first_tx.send(vec![rt()]);
    run_current().await;

    assert_eq!(2, fetches.load(Ordering::SeqCst));
    assert_eq!(state(Some(vec![asynchronous()]), false, None), *catalog.state().borrow());
}

#[tokio::test(start_paused = true)]
async fn refresh_while_loading_does_not_restart() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let catalog = SonioxCatalog::new(
        Handle::current(),
        counting(&fetches, |_| async {
            sleep(Duration::from_millis(100)).await;
            Ok(vec![rt()])
        }),
    );
    catalog.reload();
    run_current().await;

    catalog.refresh();
    advance_by(101).await;

    assert_eq!(1, fetches.load(Ordering::SeqCst));
    assert_eq!(state(Some(vec![rt()]), false, None), *catalog.state().borrow());
}

#[tokio::test(start_paused = true)]
async fn refresh_keeps_the_loaded_list_while_loading() {
    let fetches = Arc::new(AtomicUsize::new(0));
    let catalog = SonioxCatalog::new(
        Handle::current(),
        counting(&fetches, |_| async {
            sleep(Duration::from_millis(100)).await;
            Ok(vec![rt()])
        }),
    );
    catalog.reload();
    advance_by(101).await;

    catalog.refresh();

    assert_eq!(state(Some(vec![rt()]), true, None), *catalog.state().borrow());
}
