/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The Soniox model catalogue for the whole app (Android `SonioxCatalog.kt`): the settings' model menu and, per
//! session, the mode and the strict-hints check (the start frame needs `supports_language_hints_strict`). In memory
//! only; the list function fetches it with the stored key and region.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::task::AbortHandle;

use super::models::SonioxModel;
use crate::stt::{BoxFuture, SttError};

/// Fetches the catalogue (Android: `listSonioxModels` with the stored key and region).
pub type SonioxList = Arc<dyn Fn() -> BoxFuture<'static, Result<Vec<SonioxModel>, SttError>> + Send + Sync>;

/// How long [`SonioxCatalog::entry`] waits for a load. The session's audio waits in the controller's buffers
/// meanwhile.
const CATALOG_WAIT: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SonioxCatalogState {
    pub models: Option<Vec<SonioxModel>>,
    pub loading: bool,
    pub error: Option<String>,
}

/// A shared handle: clones use the same catalogue.
#[derive(Clone)]
pub struct SonioxCatalog {
    inner: Arc<Inner>,
}

struct Inner {
    runtime: Handle,
    list: SonioxList,
    state: watch::Sender<SonioxCatalogState>,
    load: Mutex<Load>,
    wait: Duration,
}

/// The running load: only the one with the current `generation` writes its result.
#[derive(Default)]
struct Load {
    generation: u64,
    task: Option<AbortHandle>,
}

impl SonioxCatalog {
    /// Loads run on `runtime`.
    pub fn new(runtime: Handle, list: SonioxList) -> Self {
        Self::with_wait(runtime, list, CATALOG_WAIT)
    }

    pub(crate) fn with_wait(runtime: Handle, list: SonioxList, wait: Duration) -> Self {
        let (state, _) = watch::channel(SonioxCatalogState::default());
        SonioxCatalog { inner: Arc::new(Inner { runtime, list, state, load: Mutex::default(), wait }) }
    }

    /// The current state, and its changes.
    pub fn state(&self) -> watch::Receiver<SonioxCatalogState> {
        self.inner.state.subscribe()
    }

    /// The key or region changed: cancels a running load (its result is never written), drops the list, loads.
    pub fn reload(&self) {
        let mut load = self.inner.lock();
        if let Some(task) = load.task.take() {
            task.abort();
        }
        load.generation += 1;
        self.inner.state.send_replace(SonioxCatalogState::default());
        self.start(&mut load);
    }

    /// Loads unless a load is running; a loaded list stays shown meanwhile.
    pub fn refresh(&self) {
        let mut load = self.inner.lock();
        self.start(&mut load);
    }

    fn start(&self, load: &mut Load) {
        if self.inner.state.borrow().loading {
            return;
        }
        self.inner.state.send_modify(|state| {
            state.loading = true;
            state.error = None;
        });
        load.generation += 1;
        let (inner, generation) = (self.inner.clone(), load.generation);
        let task = self.inner.runtime.spawn(async move {
            let result = (inner.list)().await;
            let mut load = inner.lock();
            // A load cancelled by a reload leaves the state to its replacement.
            if load.generation != generation {
                return;
            }
            load.task = None;
            inner.state.send_modify(|state| match result {
                Ok(models) => *state = SonioxCatalogState { models: Some(models), ..Default::default() },
                Err(e) => {
                    state.error = Some(e.0);
                    state.loading = false;
                }
            });
        });
        load.task = Some(task.abort_handle());
    }

    /// `model`'s entry, or None if the catalogue doesn't list it (no refetch once loaded). Not loaded: loads it and
    /// waits up to 5 s; past that (or on failure) None, while the load keeps running.
    pub async fn entry(&self, model: &str) -> Option<SonioxModel> {
        let loaded = self.inner.state.borrow().models.clone();
        let models = match loaded {
            Some(models) => models,
            None => {
                self.refresh();
                let mut state = self.state();
                let done = tokio::time::timeout(self.inner.wait, state.wait_for(|state| !state.loading)).await;
                done.ok()?.ok()?.models.clone()?
            }
        };
        models.into_iter().find(|entry| entry.id == model)
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Load> {
        self.load.lock().unwrap_or_else(|e| e.into_inner())
    }
}

#[cfg(test)]
mod tests;
