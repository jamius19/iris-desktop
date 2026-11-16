/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The on-device engine behind [`OnDeviceSpeechToText`]: Handy's [`EngineSupervisor`] (transcribe-cpp in a worker
//! process) holding one model at a time, plus Android `OnDeviceRecognizer`'s idle release driven by Unload Model.
//! All calls that touch the supervisor block: run them off the async runtime.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use iris_core::stt::SttError;
use iris_core::stt::local::idle::idle_release_wait_ms;
use iris_core::stt::local::models::OnDeviceModel;
use iris_core::stt::local::provider::{OnDeviceSpeechToText, pcm16_to_floats};
use iris_core::stt::local::store::ModelStore;
use tokio::sync::{mpsc, watch};
use transcribe_cpp::{Backend, RunOptions, StreamOptions};

use super::supervisor::{DeviceSelector, EngineError, EngineSupervisor, LoadSpec};

const LOAD_FAILED: &str = "Couldn't load the on-device model";
const DECODE_FAILED: &str = "On-device transcription failed";

/// Longest single idle wait between checks of the elapsed idle time.
const IDLE_CHECK: Duration = Duration::from_secs(10);

/// Reads the settings the engine needs: the Accelerator (`auto`/`cpu`) and Unload Model (minutes; -1 never).
pub type EngineSettings = Arc<dyn Fn() -> (String, i32) + Send + Sync>;

#[derive(Clone)]
pub struct LocalEngine {
    inner: Arc<Inner>,
}

struct Inner {
    supervisor: EngineSupervisor,
    store: ModelStore,
    settings: EngineSettings,
    /// The loaded model's `(id, backend)`; held for the length of a load so a decode waits for a preload's load.
    loaded: Mutex<Option<(&'static str, Backend)>>,
    loading: watch::Sender<bool>,
    epoch: Instant,
    /// Milliseconds since `epoch` of the last load, decode or stream.
    last_use: AtomicU64,
    idle: Mutex<Idle>,
    idle_changed: Condvar,
}

#[derive(Default)]
struct Idle {
    /// No dictation is listening or finalizing: the release countdown runs.
    idle: bool,
    since: u64,
}

impl LocalEngine {
    pub fn new(store: ModelStore, settings: EngineSettings) -> Self {
        let engine = Self {
            inner: Arc::new(Inner {
                supervisor: EngineSupervisor::new(true),
                store,
                settings,
                loaded: Mutex::new(None),
                loading: watch::channel(false).0,
                epoch: Instant::now(),
                last_use: AtomicU64::new(0),
                idle: Mutex::new(Idle { idle: true, since: 0 }),
                idle_changed: Condvar::new(),
            }),
        };
        let watcher = engine.clone();
        thread::Builder::new()
            .name("iris-idle-unload".into())
            .spawn(move || watcher.idle_release())
            .expect("failed to start the idle-unload thread");
        engine
    }

    /// A model load is running (the overlay's "Loading model" notice).
    pub fn loading(&self) -> watch::Receiver<bool> {
        self.inner.loading.subscribe()
    }

    /// The provider on this engine; `selection` is the stored (model id, language) for warm-up.
    pub fn provider(&self, selection: impl Fn() -> (String, String) + Send + Sync + 'static) -> OnDeviceSpeechToText {
        let (store, preload, decode, stream) = (self.inner.store.clone(), self.clone(), self.clone(), self.clone());
        OnDeviceSpeechToText {
            is_downloaded: Box::new(move |id| store.is_downloaded(id)),
            preload: Box::new(move |model, _language| {
                let engine = preload.clone();
                thread::spawn(move || {
                    if let Err(e) = engine.ensure_loaded(model) {
                        log::warn!("Preload failed: {e}");
                    }
                });
            }),
            decode: Arc::new(move |model, language, samples| {
                let engine = decode.clone();
                Box::pin(async move {
                    tokio::task::spawn_blocking(move || engine.decode(model, &language, samples))
                        .await
                        .unwrap_or_else(|_| Err(SttError(DECODE_FAILED.into())))
                })
            }),
            stream: Box::new(move |model, chunks| stream.stream(model, chunks)),
            selection: Box::new(selection),
            cancel: Box::new({
                let engine = self.clone();
                // Kills the worker only while a decode runs (the next use reloads the model, as in Handy).
                move || engine.inner.supervisor.cancel()
            }),
            max_segment_bytes: iris_core::stt::local::provider::MAX_SEGMENT_BYTES,
        }
    }

    /// Main thread. No dictation is listening or finalizing: the model is released once unused for the Unload Model
    /// delay. Not idle stops the countdown; a repeat doesn't restart it.
    pub fn set_idle(&self, idle: bool) {
        let mut state = self.inner.lock_idle();
        if state.idle == idle {
            return;
        }
        state.idle = idle;
        state.since = self.now_ms();
        self.inner.idle_changed.notify_all();
    }

    /// Wakes the idle countdown (the Unload Model setting changed; time already idle counts).
    pub fn settings_changed(&self) {
        self.inner.idle_changed.notify_all();
    }

    /// Unloads `id` if it is the loaded model and waits until its worker exited (before its file is deleted).
    pub fn unload_model(&self, id: &str) {
        let mut loaded = self.inner.lock_loaded();
        if loaded.is_some_and(|(loaded, _)| loaded == id) {
            *loaded = None;
            self.inner.supervisor.unload().wait();
            log::info!("Unloaded {id}");
        }
    }

    fn now_ms(&self) -> u64 {
        self.inner.epoch.elapsed().as_millis() as u64
    }

    fn touch(&self) {
        self.inner.last_use.store(self.now_ms(), Ordering::Relaxed);
    }

    /// Loads `model` with the current Accelerator unless it is already loaded.
    fn ensure_loaded(&self, model: &'static OnDeviceModel) -> Result<(), SttError> {
        let backend = if (self.inner.settings)().0 == "cpu" { Backend::Cpu } else { Backend::Auto };
        let mut loaded = self.inner.lock_loaded();
        if *loaded == Some((model.id, backend)) && self.inner.supervisor.loaded().is_some() {
            return Ok(());
        }
        if backend == Backend::Auto {
            // Choosing Auto again is the way to try a GPU that failed before (Handy's accelerator change).
            self.inner.supervisor.retry_gpu("the on-device model is loading with the Auto accelerator");
        }
        let started = Instant::now();
        self.inner.loading.send_replace(true);
        let spec = LoadSpec { path: self.inner.store.path(model.id), backend, device: DeviceSelector::Auto };
        let result = self.inner.supervisor.load(spec);
        self.inner.loading.send_replace(false);
        self.touch();
        match result {
            Ok(info) => {
                log::info!(
                    "Loaded {} in {} ms on {} ({})",
                    model.id,
                    started.elapsed().as_millis(),
                    info.device,
                    info.backend
                );
                *loaded = Some((model.id, backend));
                Ok(())
            }
            Err(e) => {
                log::error!("Loading {} failed: {e}", model.id);
                *loaded = None;
                Err(SttError(LOAD_FAILED.into()))
            }
        }
    }

    fn decode(&self, model: &'static OnDeviceModel, language: &str, samples: Vec<f32>) -> Result<String, SttError> {
        self.ensure_loaded(model)?;
        let started = Instant::now();
        let audio_ms = samples.len() as u64 / 16;
        self.touch();
        let result = self.inner.supervisor.transcribe(samples, run_options(language));
        self.touch();
        match result {
            Ok(transcript) => {
                log::debug!("Decoded {audio_ms} ms of audio in {} ms", started.elapsed().as_millis());
                Ok(transcript.text)
            }
            Err(e) => Err(decode_error(e)),
        }
    }

    /// One dictation through `model`'s engine stream: sends the whole text after each chunk taken, then the final
    /// text. A stream that can't finalize, or finalizes blank, is transcribed again in one run (Handy's fallback).
    fn stream(
        &self,
        model: &'static OnDeviceModel,
        mut chunks: mpsc::UnboundedReceiver<Vec<u8>>,
    ) -> mpsc::UnboundedReceiver<Result<String, SttError>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let engine = self.clone();
        thread::spawn(move || {
            // Audio queues in `chunks` meanwhile: recording started with the session.
            if let Err(e) = engine.ensure_loaded(model) {
                let _ = tx.send(Err(e));
                return;
            }
            let progress = tx.clone();
            let mut text = String::new();
            let on_progress = move |p: super::supervisor::StreamProgress| {
                if let Some(t) = p.text {
                    text = t.display();
                }
                // One per feed, changed or not: the provider counts them against the chunks it sent.
                let _ = progress.send(Ok(text.clone()));
            };
            let handle =
                match engine.inner.supervisor.start_stream(run_options(""), StreamOptions::default(), on_progress) {
                    Ok(handle) => handle,
                    Err(e) => {
                        let _ = tx.send(Err(decode_error(e)));
                        return;
                    }
                };
            let mut recording = Vec::new();
            while let Some(chunk) = chunks.blocking_recv() {
                let samples = pcm16_to_floats(&[chunk]);
                recording.extend_from_slice(&samples);
                engine.touch();
                handle.feed(samples);
            }
            if tx.is_closed() || recording.is_empty() {
                // Cancelled or failed (dropping the handle resets the stream), or nothing to transcribe.
                return;
            }
            let result = match handle.finalize(false) {
                Ok(Some(finalized)) if !finalized.text.full.trim().is_empty() => Ok(finalized.text.full),
                Ok(_) => {
                    log::info!("The live transcription gave no text; transcribing the recording in one run");
                    engine.decode(model, "", recording)
                }
                Err(e) => Err(decode_error(e)),
            };
            engine.touch();
            let _ = tx.send(result);
        });
        rx
    }

    /// Releases the model once idle for the Unload Model delay (Android `armIdleRelease`).
    fn idle_release(&self) {
        let mut state = self.inner.lock_idle();
        loop {
            let unload_minutes = (self.inner.settings)().1;
            let loaded = self.inner.supervisor.loaded().is_some();
            let wait = idle_release_wait_ms(
                unload_minutes,
                state.since as i64,
                self.inner.last_use.load(Ordering::Relaxed) as i64,
                self.now_ms() as i64,
            );
            match wait {
                Some(wait) if state.idle && loaded && wait <= 0 => {
                    drop(state);
                    let mut loaded = self.inner.lock_loaded();
                    let still_idle = self.inner.lock_idle().idle;
                    if still_idle && let Some((id, _)) = loaded.take() {
                        self.inner.supervisor.unload().wait();
                        log::info!("Unloaded {id} after {unload_minutes} min idle");
                    }
                    drop(loaded);
                    state = self.inner.lock_idle();
                }
                Some(wait) if state.idle && loaded => {
                    let timeout = Duration::from_millis(wait as u64).min(IDLE_CHECK);
                    state = self.inner.idle_changed.wait_timeout(state, timeout).unwrap_or_else(|e| e.into_inner()).0;
                }
                _ => {
                    state =
                        self.inner.idle_changed.wait_timeout(state, IDLE_CHECK).unwrap_or_else(|e| e.into_inner()).0;
                }
            }
        }
    }
}

impl Inner {
    fn lock_loaded(&self) -> MutexGuard<'_, Option<(&'static str, Backend)>> {
        self.loaded.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn lock_idle(&self) -> MutexGuard<'_, Idle> {
        self.idle.lock().unwrap_or_else(|e| e.into_inner())
    }
}

fn run_options(language: &str) -> RunOptions {
    RunOptions { language: Some(language.to_string()).filter(|l| !l.is_empty()), ..Default::default() }
}

fn decode_error(e: EngineError) -> SttError {
    log::error!("On-device decode failed: {e}");
    SttError(DECODE_FAILED.into())
}
