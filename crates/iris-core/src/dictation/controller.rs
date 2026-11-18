/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Runs one dictation session at a time (Android `DictationController.kt`): Idle → Listening → Finalizing →
//! Done/Error → Idle. [`DictationController::start`] begins a session (also while a result shows),
//! [`stop`](DictationController::stop) ends the recording of a listening one, [`cancel`](DictationController::cancel)
//! discards any. A finalizing session can't be stopped or restarted; it ends through the provider's own timeouts.
//! The dictation is never lost: errors end in [`DictationState::Error`], a transcript always reaches
//! `on_final_transcript`. A session warms its speech-to-text provider at start (an upload provider's connection for
//! the upload after stop) and, with cleanup on, the cleanup provider at start and stop. A stopped session whose text
//! reached `on_final_transcript` is measured for statistics (`record`).

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use log::{debug, error, warn};
use tokio::runtime::Handle;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::custom_words::correct_custom_words;
use super::filler_words::{has_gated_filler, output_language, remove_filler_words};
use super::state::DictationState;
use super::statistics::{DictationRecord, word_count};
use crate::audio::speech_gate::{OFFLINE_HANGOVER_MS, STREAMING_HANGOVER_MS, SpeechGate, VadScorer};
use crate::audio::time_stretch::TimeStretch;
use crate::settings::{DEFAULT_STT_SPEED_PERCENT, Settings};
use crate::stt::{AudioSource, BoxFuture, SpeechToTextProvider, SttError, SttRequest};

/// How long Done shows; Error, and Done with a notice, show twice as long.
pub const DONE_DISPLAY: Duration = Duration::from_millis(1_500);
pub const ERROR_DISPLAY: Duration = Duration::from_millis(3_000);

/// Text language detection (filler words) can't hold up Finalizing longer than this (then: unknown).
pub const LANGUAGE_DETECTION_TIMEOUT: Duration = Duration::from_secs(1);

pub const GENERIC_ERROR: &str = "Something went wrong";
pub const NO_SPEECH: &str = "No speech detected";

/// A chunk of zeros goes to a streaming provider after this long without sent audio.
const KEEP_ALIVE: Duration = Duration::from_secs(5);

/// One chunk of recorded PCM16 LE mono audio.
#[derive(Clone, Debug, PartialEq)]
pub struct AudioChunk {
    pub bytes: Vec<u8>,
    /// Waveform levels (0..1 each) for the overlay; empty when this chunk brought none.
    pub levels: Vec<f32>,
    /// When the chunk was read from the microphone.
    pub read_at: Instant,
}

/// A recording: chunks until the receiver is dropped (which stops the microphone). An error ends it.
pub type Recording = mpsc::UnboundedReceiver<Result<AudioChunk, SttError>>;

/// Picks the speech-to-text provider for a session's settings.
pub type SttFor = Box<dyn Fn(&Settings) -> Arc<dyn SpeechToTextProvider> + Send + Sync>;

/// Gets a stopped session's measurements with its settings (statistics).
pub type Record = Box<dyn Fn(&Settings, DictationRecord) + Send + Sync>;

/// What the controller runs a session with.
pub struct Deps {
    /// Starts recording at the given sample rate (Hz).
    pub audio: Box<dyn Fn(u32) -> Recording + Send + Sync>,
    /// The speech-to-text provider for a session's settings; picked once per session.
    pub stt: SttFor,
    /// The current settings, read once per session.
    pub settings: Box<dyn Fn() -> Settings + Send + Sync>,
    /// Warms the connection of the cleanup provider for a session's settings; called as recording starts and again
    /// on stop if cleanup is enabled, so the cleanup call finds a warm connection. Must return at once.
    pub warm_up_cleanup: Box<dyn Fn(&Settings) + Send + Sync>,
    /// Called with the raw transcript (as the provider returned it) and the final text of each successful session
    /// (fillers removed and custom words corrected, as the settings say); returns the [`DictationState::Done`] to
    /// show. It runs inside the session (still Finalizing): cancelling the session cancels it.
    pub on_final_transcript: Box<dyn Fn(String, String) -> BoxFuture<'static, DictationState> + Send + Sync>,
    /// A new VAD scorer (16 kHz frame → speech probability) for each session with Skip Silence on; it may load a
    /// model, so the app builds it off the async workers.
    pub vad_model: Box<dyn Fn() -> BoxFuture<'static, VadScorer> + Send + Sync>,
    /// The transcript's language (ISO 639-1) detected from its text, or None if unsure; only called when filler
    /// removal is on, the settings don't give the language and a language-gated filler is in the text.
    pub detect_language: Box<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>,
    /// Called once per session whose `on_final_transcript` returned Done (stopped, non-blank text), with the
    /// session's settings and its measurements; must return at once. Not for a transcript that arrived before the stop.
    pub record: Record,
}

#[derive(Clone)]
pub struct DictationController {
    inner: Arc<Inner>,
}

struct Inner {
    deps: Deps,
    runtime: Handle,
    state: watch::Sender<DictationState>,
    levels: watch::Sender<Vec<f32>>,
    session: Mutex<Session>,
}

/// The current session's bookkeeping; every state change happens under its lock.
#[derive(Default)]
struct Session {
    /// Bumped by every start and cancel: an older session's late updates are dropped.
    generation: u64,
    task: Option<JoinHandle<()>>,
    /// When stop was called, or None while listening.
    stopped_at: Option<Instant>,
    /// The session's settings if cleanup is on: the cleanup connection is warmed again on stop.
    cleanup_settings: Option<Settings>,
}

impl DictationController {
    /// `runtime` runs the sessions.
    pub fn new(runtime: Handle, deps: Deps) -> Self {
        Self {
            inner: Arc::new(Inner {
                deps,
                runtime,
                state: watch::channel(DictationState::Idle).0,
                levels: watch::channel(Vec::new()).0,
                session: Mutex::new(Session::default()),
            }),
        }
    }

    pub fn state(&self) -> watch::Receiver<DictationState> {
        self.inner.state.subscribe()
    }

    /// Waveform levels of the latest chunk while listening; empty otherwise.
    pub fn levels(&self) -> watch::Receiver<Vec<f32>> {
        self.inner.levels.subscribe()
    }

    /// Starts a new session, also while a result is shown; false (ignored) while one is listening or finalizing.
    pub fn start(&self) -> bool {
        let mut session = self.inner.lock();
        if matches!(*self.inner.state.borrow(), DictationState::Listening { .. } | DictationState::Finalizing { .. }) {
            return false;
        }
        if let Some(task) = session.task.take() {
            task.abort();
        }
        session.generation += 1;
        session.stopped_at = None;
        session.cleanup_settings = None;
        self.inner.levels.send_replace(Vec::new());
        self.inner.state.send_replace(DictationState::Listening { text: String::new() });
        let generation = session.generation;
        session.task = Some(self.inner.runtime.spawn(run(self.inner.clone(), generation)));
        true
    }

    /// Stops recording a listening session, which then finalizes; true if it did. Ignored (false) in any other
    /// state.
    pub fn stop(&self) -> bool {
        let mut session = self.inner.lock();
        let text = match &*self.inner.state.borrow() {
            DictationState::Listening { text } => text.clone(),
            _ => return false,
        };
        session.stopped_at = Some(Instant::now());
        // A long dictation may outlive the idle connection (pool/server/NAT); runs alongside STT finalization.
        if let Some(config) = &session.cleanup_settings {
            (self.inner.deps.warm_up_cleanup)(config);
        }
        self.inner.levels.send_replace(Vec::new());
        self.inner.state.send_replace(DictationState::Finalizing { text });
        true
    }

    /// Cancels any session, discarding its result, and returns to Idle.
    pub fn cancel(&self) {
        let mut session = self.inner.lock();
        self.inner.cancel_session(&mut session);
    }

    /// Cancels the session only while it is listening; true if it did. A finalizing session is left to end.
    pub fn cancel_if_listening(&self) -> bool {
        let mut session = self.inner.lock();
        if !matches!(*self.inner.state.borrow(), DictationState::Listening { .. }) {
            return false;
        }
        self.inner.cancel_session(&mut session);
        true
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, Session> {
        self.session.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Aborts the session's task and returns to Idle; called under the session lock.
    fn cancel_session(&self, session: &mut Session) {
        if let Some(task) = session.task.take() {
            task.abort();
        }
        session.generation += 1;
        self.levels.send_replace(Vec::new());
        self.state.send_replace(DictationState::Idle);
    }

    /// Sets the state if `generation` is still the current session.
    fn publish(&self, generation: u64, state: impl FnOnce(&DictationState) -> Option<DictationState>) {
        let session = self.lock();
        if session.generation != generation {
            return;
        }
        let new = state(&self.state.borrow());
        if let Some(new) = new {
            self.state.send_replace(new);
        }
    }

    fn stopped_at(&self, generation: u64) -> Option<Instant> {
        let session = self.lock();
        if session.generation == generation { session.stopped_at } else { None }
    }
}

/// Aborts the task when dropped (the session task's own abort reaches the work it runs).
struct AbortOnDrop(JoinHandle<Result<DictationState, SttError>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn run(inner: Arc<Inner>, generation: u64) {
    // In its own task, so a panic ends in Error rather than a session stuck in Listening.
    let mut work = AbortOnDrop(tokio::spawn(session(inner.clone(), generation)));
    let result = match (&mut work.0).await {
        Ok(Ok(state)) => state,
        Ok(Err(e)) => DictationState::Error { message: e.0 },
        Err(e) => {
            error!("Dictation session failed: {e}");
            DictationState::Error { message: GENERIC_ERROR.into() }
        }
    };
    let brief = matches!(result, DictationState::Done { notice: None, .. });
    inner.publish(generation, |_| {
        // E.g. an STT error while listening.
        inner.levels.send_replace(Vec::new());
        Some(result)
    });
    tokio::time::sleep(if brief { DONE_DISPLAY } else { ERROR_DISPLAY }).await;
    inner.publish(generation, |_| Some(DictationState::Idle));
}

async fn session(inner: Arc<Inner>, generation: u64) -> Result<DictationState, SttError> {
    let config = (inner.deps.settings)();
    if config.cleanup_enabled {
        let mut session = inner.lock();
        if session.generation == generation {
            session.cleanup_settings = Some(config.clone());
            (inner.deps.warm_up_cleanup)(&config);
        }
    }
    let stt = (inner.deps.stt)(&config);
    stt.warm_up();
    let model = config.active_stt_model().to_string();
    let rate = stt.sample_rate(&model);
    let hangover = if stt.live_text(&model) { STREAMING_HANGOVER_MS } else { OFFLINE_HANGOVER_MS };
    let vad = if config.vad_enabled { Some((inner.deps.vad_model)().await) } else { None };
    let gate = vad.map(|vad| SpeechGate::new(rate, hangover, vad, stt.wants_segment_ends()));
    // Applied after the gate and the stop: only sent chunks are stretched, and the tail is flushed at the stop (after
    // the no-speech check, so a session without speech sends nothing).
    let as_recorded = config.stt_speed_percent == DEFAULT_STT_SPEED_PERCENT || !stt.allows_speed_up();
    let stretch = (!as_recorded).then(|| TimeStretch::new(rate, config.stt_speed_percent as f32 / 100.0));
    let mut audio = SessionAudio {
        inner: inner.clone(),
        generation,
        rate,
        recording: None,
        // Keep-alive for a socket while the gate withholds audio (servers may end a stream that gets none).
        keep_alive: gate.is_some() && stt.streams(),
        gate,
        stretch,
        out: VecDeque::new(),
        ended: false,
        recorded_bytes: 0,
        sent_bytes: 0,
        last_sent_at: None,
    };
    let request = SttRequest {
        model,
        language: config.active_stt_language().to_string(),
        timeout: Duration::from_secs(config.stt_timeout_seconds.max(0) as u64),
        custom_words: config.active_custom_words().to_vec(),
    };
    let partials = inner.clone();
    let on_partial = Box::new(move |text: String| {
        partials.publish(generation, |state| match state {
            DictationState::Listening { .. } => Some(DictationState::Listening { text }),
            DictationState::Finalizing { .. } => Some(DictationState::Finalizing { text }),
            _ => None,
        })
    });
    let transcript = stt.transcribe(Box::new(&mut audio), request, on_partial).await;
    let transcribed_at = Instant::now();
    // Read now: a transcript that arrives before the stop (the Gateway's early `finish`) has none, and a stop during
    // the hook must not count.
    let stop = inner.stopped_at(generation);
    let (recorded_ms, sent_ms) = (ms(audio.recorded_bytes, rate) as i64, ms(audio.sent_bytes, rate) as i64);
    // Releases the microphone as soon as the provider is done with it (a transcript before the stop).
    drop(audio);
    let raw = transcript?;
    let text = without_fillers(&inner, raw.clone(), &config).await;
    let text =
        if config.correct_similar_words { correct_custom_words(&text, config.active_custom_words()) } else { text };
    if text.trim().is_empty() {
        return Ok(DictationState::Error { message: NO_SPEECH.into() });
    }
    let done = (inner.deps.on_final_transcript)(raw, text).await;
    if let (Some(stop), DictationState::Done { text, .. }) = (stop, &done) {
        // Soniox real-time bills the whole stream; Speed shortens what the others get.
        let billed_ms = if !stt.allows_speed_up() {
            recorded_ms
        } else if as_recorded {
            sent_ms
        } else {
            sent_ms * 100 / config.stt_speed_percent as i64
        };
        let record = DictationRecord {
            recorded_ms,
            billed_ms,
            wait_ms: transcribed_at.duration_since(stop).as_millis() as i64,
            done_ms: stop.elapsed().as_millis() as i64,
            words: word_count(text),
        };
        (inner.deps.record)(&config, record);
    }
    Ok(done)
}

/// `text` without filler words if the setting is on; the language from the settings, else detected from `text`.
async fn without_fillers(inner: &Inner, text: String, config: &Settings) -> String {
    if !config.remove_filler_words {
        return text;
    }
    let language = match output_language(config) {
        Some(language) => Some(language),
        None if has_gated_filler(&text) => {
            let started = Instant::now();
            let language = tokio::time::timeout(LANGUAGE_DETECTION_TIMEOUT, (inner.deps.detect_language)(text.clone()))
                .await
                .unwrap_or(None);
            debug!(
                "Filler words: language {} (detected in {} ms)",
                language.as_deref().unwrap_or("unknown"),
                started.elapsed().as_millis()
            );
            language
        }
        None => None,
    };
    remove_filler_words(&text, language.as_deref())
}

/// The audio a provider pulls: recording starts on the first pull (after the provider's own checks, so a missing
/// model or key never opens the microphone), runs through the VAD gate, keep-alive and Audio Speed, and ends with the
/// chunk during which stop was called, so speech up to the stop is kept.
struct SessionAudio {
    inner: Arc<Inner>,
    generation: u64,
    rate: u32,
    recording: Option<Recording>,
    gate: Option<SpeechGate>,
    keep_alive: bool,
    stretch: Option<TimeStretch>,
    /// Chunks ready for the provider.
    out: VecDeque<Vec<u8>>,
    ended: bool,
    recorded_bytes: u64,
    sent_bytes: u64,
    last_sent_at: Option<Instant>,
}

impl SessionAudio {
    /// Cancel-safe: the only await is `recv`; everything after a chunk is taken runs without awaiting. Keep it so.
    async fn pull(&mut self) -> Result<Option<Vec<u8>>, SttError> {
        loop {
            if let Some(chunk) = self.out.pop_front() {
                return Ok(Some(chunk));
            }
            if self.ended {
                return Ok(None);
            }
            let recording = self.recording.get_or_insert_with(|| (self.inner.deps.audio)(self.rate));
            let Some(chunk) = recording.recv().await else {
                // The recording ended on its own (e.g. the device went away): as at a stop.
                self.end()?;
                continue;
            };
            let chunk = chunk?;
            let stop = self.inner.stopped_at(self.generation);
            if stop.is_none() && !chunk.levels.is_empty() {
                self.inner.publish(self.generation, |state| {
                    if matches!(state, DictationState::Listening { .. }) {
                        self.inner.levels.send_replace(chunk.levels.clone());
                    }
                    None
                });
            }
            self.recorded_bytes += chunk.bytes.len() as u64;
            let send = match &mut self.gate {
                Some(gate) => gate.process(&chunk.bytes),
                None => vec![chunk.bytes.clone()],
            };
            if self.keep_alive {
                match self.last_sent_at {
                    Some(last) if send.is_empty() => {
                        if chunk.read_at.duration_since(last) >= KEEP_ALIVE {
                            // Zeros, not counted as sent: a session without speech still ends with NO_SPEECH.
                            self.emit(vec![0; chunk.bytes.len()]);
                            self.last_sent_at = Some(chunk.read_at);
                        }
                    }
                    _ => self.last_sent_at = Some(chunk.read_at),
                }
            }
            for bytes in send {
                self.sent_bytes += bytes.len() as u64;
                self.emit(bytes);
            }
            if stop.is_some_and(|stop| chunk.read_at > stop) {
                self.end()?;
            }
        }
    }

    /// Queues `bytes` for the provider, stretched if Audio Speed is set.
    fn emit(&mut self, bytes: Vec<u8>) {
        match &mut self.stretch {
            Some(stretch) => self.out.extend(stretch.process(&bytes)),
            None => self.out.push_back(bytes),
        }
    }

    /// The recording ended at the stop: releases the microphone, then fails if nothing was sent.
    fn end(&mut self) -> Result<(), SttError> {
        self.ended = true;
        self.recording = None;
        if let Some(gate) = &self.gate {
            let summary =
                format!("VAD: sent {} ms of {} ms", ms(self.sent_bytes, self.rate), ms(self.recorded_bytes, self.rate));
            match gate.failure() {
                Some(failure) => warn!("{summary} (VAD failed: sent everything from then on: {failure})"),
                None => debug!("{summary}"),
            }
            // Nothing to transcribe: fail before the provider pads or commits any audio.
            if self.sent_bytes == 0 {
                return Err(SttError(NO_SPEECH.into()));
            }
        }
        if let Some(tail) = self.stretch.take().and_then(TimeStretch::finish) {
            self.out.push_back(tail);
        }
        Ok(())
    }
}

// By reference: the session reads the byte counts after the provider is done.
impl AudioSource for &mut SessionAudio {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        Box::pin(self.pull())
    }
}

/// Duration of `bytes` of PCM16 mono at `rate`.
fn ms(bytes: u64, rate: u32) -> u64 {
    bytes * 1000 / (rate as u64 * 2)
}

#[cfg(test)]
mod tests;
