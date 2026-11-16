/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Downloads on-device models from Hugging Face into `root` (`<data>/models/<id>/`), one at a time (Android's
//! `OnDeviceModelStore.kt` on Handy's transport). The file goes to `<file>.tmp`, is checked against its catalogue size
//! and SHA-256, then renamed. A model is downloaded when its final file exists with its catalogue size. Failures keep
//! the `.tmp`, so the next [`ModelStore::download`] resumes with a Range request (also after a restart);
//! [`ModelStore::cancel`] deletes it.

use super::download::{self, DownloadError, HttpDownloadOutcome};
use super::models::{ON_DEVICE_MODELS, OnDeviceModel};
use log::{info, warn};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::task::JoinHandle;

const TMP: &str = ".tmp";
const NETWORK_ERROR: &str = "Download failed (network error). Click Download to resume.";
const STORAGE_ERROR: &str = "Couldn't save the model (storage error).";
const CORRUPTED: &str = "Download was corrupted. Try again.";
const FREE_MARGIN_BYTES: u64 = 50_000_000;

/// `downloading`'s progress is `done_bytes` of `total_bytes`; `error` is the last failed download's; `deleting` is the
/// model whose files are being deleted (it can't be downloaded meanwhile).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoreState {
    pub downloaded: BTreeSet<String>,
    pub downloading: Option<String>,
    pub deleting: Option<String>,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub error: Option<StoreError>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StoreError {
    pub model_id: String,
    pub message: String,
}

#[derive(Clone)]
pub struct ModelStore {
    inner: Arc<Inner>,
}

struct Inner {
    client: reqwest::Client,
    root: PathBuf,
    runtime: Handle,
    before_delete: Box<dyn Fn(&str) + Send + Sync>,
    free_bytes: Box<dyn Fn(&Path) -> u64 + Send + Sync>,
    models: &'static [OnDeviceModel],
    url: Box<dyn Fn(&OnDeviceModel) -> String + Send + Sync>,
    state: watch::Sender<StoreState>,
    /// The running (or last) download's cancel flag.
    cancel: Mutex<Arc<AtomicBool>>,
}

impl ModelStore {
    /// Downloads run on `runtime`. `client` is used as given: build it HTTP/1.1-only with a connect timeout, as Handy
    /// does (`http1_only()`, 15 s), since HTTP/2's stream window caps a large download on high-latency links.
    /// `before_delete` blocks until the engine has released the model (called on the blocking pool).
    pub fn new(
        client: reqwest::Client,
        root: PathBuf,
        runtime: Handle,
        before_delete: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        let free_bytes = |path: &Path| {
            fs4::available_space(path).unwrap_or_else(|e| {
                warn!("Free space of {} unknown: {e}", path.display());
                u64::MAX
            })
        };
        Self::with(client, root, runtime, before_delete, free_bytes, &ON_DEVICE_MODELS, OnDeviceModel::url).0
    }

    /// [`Self::new`] with Kotlin's test seams; also returns the startup scan's task.
    fn with(
        client: reqwest::Client,
        root: PathBuf,
        runtime: Handle,
        before_delete: impl Fn(&str) + Send + Sync + 'static,
        free_bytes: impl Fn(&Path) -> u64 + Send + Sync + 'static,
        models: &'static [OnDeviceModel],
        url: impl Fn(&OnDeviceModel) -> String + Send + Sync + 'static,
    ) -> (Self, JoinHandle<()>) {
        let store = Self {
            inner: Arc::new(Inner {
                client,
                root,
                runtime: runtime.clone(),
                before_delete: Box::new(before_delete),
                free_bytes: Box::new(free_bytes),
                models,
                url: Box::new(url),
                state: watch::Sender::new(StoreState::default()),
                cancel: Mutex::default(),
            }),
        };
        let this = store.clone();
        // Off the caller's thread; until it ends no model counts as downloaded.
        let scan = runtime.spawn_blocking(move || {
            let found: Vec<_> = this.inner.models.iter().filter(|m| this.is_complete(m)).map(|m| m.id).collect();
            this.inner.state.send_modify(|s| s.downloaded.extend(found.into_iter().map(String::from)));
        });
        (store, scan)
    }

    pub fn state(&self) -> watch::Receiver<StoreState> {
        self.inner.state.subscribe()
    }

    pub fn is_downloaded(&self, id: &str) -> bool {
        self.inner.state.borrow().downloaded.contains(id)
    }

    /// `<root>/<id>/<file>.gguf`; `id` must be in the model table.
    pub fn path(&self, id: &str) -> PathBuf {
        self.dir(id).join(self.model(id).file)
    }

    /// Starts downloading (or resuming) `id` and deletes other models' partial files; ignored while any download
    /// runs or while `id` is being deleted.
    pub fn download(&self, id: &str) {
        let model = self.model(id);
        let mut cancel = self.inner.cancel.lock().unwrap();
        let started = self.inner.state.send_if_modified(|s| {
            if s.downloading.is_some() || s.deleting.as_deref() == Some(id) {
                return false;
            }
            s.downloading = Some(id.to_string());
            s.done_bytes = 0;
            s.total_bytes = model.size_bytes;
            s.error = None;
            true
        });
        if !started {
            return;
        }
        *cancel = Arc::new(AtomicBool::new(false));
        let (this, cancel) = (self.clone(), Arc::clone(&cancel));
        self.inner.runtime.spawn(async move { this.run(model, &cancel).await });
    }

    /// Stops the running download and deletes its partial file.
    pub fn cancel(&self) {
        self.inner.cancel.lock().unwrap().store(true, Ordering::Relaxed);
    }

    /// Deletes `id`'s files (cancelling its download), once the engine has released it.
    pub fn delete(&self, id: &str) {
        self.inner.state.send_modify(|s| s.deleting = Some(id.to_string()));
        let (this, id) = (self.clone(), id.to_string());
        self.inner.runtime.spawn(async move {
            let mut state = this.state();
            if state.borrow().downloading.as_deref() == Some(&id) {
                this.cancel();
                let _ = state.wait_for(|s| s.downloading.as_deref() != Some(&id)).await;
            }
            this.inner.state.send_modify(|s| {
                s.downloaded.remove(&id);
            });
            let (that, released) = (this.clone(), id.clone());
            let _ = this
                .inner
                .runtime
                .spawn_blocking(move || {
                    (that.inner.before_delete)(&released);
                    let _ = fs::remove_dir_all(that.dir(&released));
                })
                .await;
            this.inner.state.send_if_modified(|s| {
                let done = s.deleting.as_deref() == Some(&id);
                if done {
                    s.deleting = None;
                }
                done
            });
        });
    }

    fn model(&self, id: &str) -> &'static OnDeviceModel {
        self.inner.models.iter().find(|m| m.id == id).unwrap_or_else(|| panic!("unknown model {id}"))
    }

    fn dir(&self, id: &str) -> PathBuf {
        self.inner.root.join(id)
    }

    fn is_complete(&self, model: &OnDeviceModel) -> bool {
        fs::metadata(self.path(model.id)).is_ok_and(|m| m.is_file() && m.len() == model.size_bytes)
    }

    fn delete_tmp_files(&self, id: &str) {
        let Ok(entries) = fs::read_dir(self.dir(id)) else { return };
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy().ends_with(TMP) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }

    async fn run(&self, model: &'static OnDeviceModel, cancel: &AtomicBool) {
        let started = Instant::now();
        let result = self.fetch(model, cancel).await;
        match result {
            Ok(HttpDownloadOutcome::Completed) => {
                info!("Downloaded {} in {} s", model.id, started.elapsed().as_secs())
            }
            Ok(HttpDownloadOutcome::Cancelled) => self.delete_tmp_files(model.id),
            Err(_) => {}
        }
        self.inner.state.send_modify(|s| {
            if result == Ok(HttpDownloadOutcome::Completed) {
                s.downloaded.insert(model.id.to_string());
            }
            s.downloading = None;
            s.done_bytes = 0;
            s.total_bytes = 0;
            if let Err(message) = result {
                s.error = Some(StoreError { model_id: model.id.to_string(), message });
            }
        });
    }

    /// Downloads `model` unless its final file is there; the error is the user-readable message.
    async fn fetch(&self, model: &OnDeviceModel, cancel: &AtomicBool) -> Result<HttpDownloadOutcome, String> {
        // An abandoned model's partial download would otherwise use storage until it's deleted by hand.
        for other in self.inner.models.iter().filter(|m| m.id != model.id) {
            self.delete_tmp_files(other.id);
        }
        let path = self.path(model.id);
        let mut tmp = OsString::from(&path);
        tmp.push(TMP);
        let tmp = PathBuf::from(tmp);
        fs::create_dir_all(self.dir(model.id)).map_err(storage_error)?;
        let needed =
            if file_len(&path) == model.size_bytes { 0 } else { model.size_bytes.saturating_sub(file_len(&tmp)) };
        let required = needed + FREE_MARGIN_BYTES;
        if (self.inner.free_bytes)(&self.inner.root) < required {
            return Err(format!("Not enough storage: needs {} MB free.", required.div_ceil(1_000_000)));
        }
        if path.exists() {
            if file_len(&path) == model.size_bytes {
                return Ok(HttpDownloadOutcome::Completed);
            }
            let _ = fs::remove_file(&path);
        }

        let url = (self.inner.url)(model);
        let progress = |done: u64| self.inner.state.send_modify(|s| s.done_bytes = done);
        let mut retried = false;
        let outcome = loop {
            match download::download_http_resumable(
                &self.inner.client,
                &url,
                &tmp,
                model.size_bytes,
                model.sha256,
                cancel,
                &progress,
            )
            .await
            {
                // The partial is gone (a range at another start, or a shorter object): once more from 0.
                Err(DownloadError::Http(206 | 416, _)) if !retried => retried = true,
                Err(e) => {
                    warn!("Download of {} failed: {e}", model.id);
                    return Err(match e {
                        DownloadError::Network(_) => NETWORK_ERROR.into(),
                        DownloadError::Http(status, _) => format!("Download failed (HTTP {status})."),
                        DownloadError::Corrupt(_) => CORRUPTED.into(),
                        DownloadError::Storage(_) => STORAGE_ERROR.into(),
                    });
                }
                Ok(outcome) => break outcome,
            }
        };
        if outcome == HttpDownloadOutcome::Cancelled || cancel.load(Ordering::Relaxed) {
            return Ok(HttpDownloadOutcome::Cancelled);
        }
        fs::rename(&tmp, &path).map_err(storage_error)?;
        Ok(HttpDownloadOutcome::Completed)
    }
}

fn storage_error(e: std::io::Error) -> String {
    warn!("Model storage error: {e}");
    STORAGE_ERROR.into()
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |m| m.len())
}

#[cfg(test)]
mod tests;
