/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! On-device transcription with a downloaded model (Android `OnDeviceSpeechToText.kt`); the engine (Handy's
//! transcribe-cpp worker in the app) is injected. Per-phrase models: each speech segment (ended by the VAD gate's empty
//! chunks, or cut at a pause past [`MAX_SEGMENT_BYTES`]) is decoded while recording continues and the joined text so
//! far goes to `on_partial`; the rest is decoded after the stop. A streaming model: one engine stream for the whole
//! dictation, segment ends skipped, its text to `on_partial` whenever it changes. No network: the request's timeout is
//! ignored; a deadline only catches a hung engine. Statistics come in M4.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use log::debug;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::models::{self, OnDeviceModel, whisper_language};
use crate::stt::wav::split_at_pauses;
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest, joined};

pub const SAMPLE_RATE: u32 = 16_000;

/// Model input per decode at most (Parakeet: 15–20 s chunks; Whisper truncates past ~29.5 s).
pub const MAX_SEGMENT_BYTES: usize = 20 * 16_000 * 2;

/// How far back from the cap a long segment's cut looks for a pause.
const SPLIT_SEARCH_BYTES: usize = 8 * 16_000 * 2;

/// Deadline after the stop: this plus twice the audio not decoded yet (a model load plus decoding at RTF ≤ 2).
const BASE_DEADLINE: Duration = Duration::from_secs(60);

const TIMED_OUT: &str = "On-device transcription timed out";

/// A decode of one segment's samples (16 kHz mono, -1..1) with `(model, language)`.
pub type Decode =
    Arc<dyn Fn(&'static OnDeviceModel, String, Vec<f32>) -> BoxFuture<'static, Result<String, SttError>> + Send + Sync>;

/// A streaming model's dictation: takes PCM16 chunks from the receiver, sends the whole text after each chunk taken,
/// then the final text once the chunks end (the sender closes). Dropping the returned receiver abandons the stream.
pub type Stream = Box<
    dyn Fn(
            &'static OnDeviceModel,
            mpsc::UnboundedReceiver<Vec<u8>>,
        ) -> mpsc::UnboundedReceiver<Result<String, SttError>>
        + Send
        + Sync,
>;

/// Loads a model with a language ahead of a decode. Fire-and-forget.
pub type Preload = Box<dyn Fn(&'static OnDeviceModel, &str) + Send + Sync>;

pub struct OnDeviceSpeechToText {
    pub is_downloaded: Box<dyn Fn(&str) -> bool + Send + Sync>,
    pub preload: Preload,
    pub decode: Decode,
    pub stream: Stream,
    /// The stored (model id, language), for [`warm_up`](SpeechToTextProvider::warm_up).
    pub selection: Box<dyn Fn() -> (String, String) + Send + Sync>,
    pub max_segment_bytes: usize,
}

impl SpeechToTextProvider for OnDeviceSpeechToText {
    fn sample_rate(&self, _model: &str) -> u32 {
        SAMPLE_RATE
    }

    fn streams(&self) -> bool {
        false
    }

    // Per-phrase models decode segments in isolation: the long hangover keeps whole phrases together. The streaming
    // model just keeps it: the pause, and a whole window, reach its stream.
    fn live_text(&self, _model: &str) -> bool {
        true
    }

    fn wants_segment_ends(&self) -> bool {
        true
    }

    fn transcribe<'a>(
        &'a self,
        audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async move {
            let model = models::by_id(&request.model);
            // Before pulling audio: the microphone never opens without the model.
            if !(self.is_downloaded)(model.id) {
                return Err(SttError(format!("Download the {} model in Iris settings", model.label)));
            }
            let language = language_for(model, &request.language);
            (self.preload)(model, &language);
            if model.streams {
                self.streamed(audio, model, Arc::from(on_partial)).await
            } else {
                self.per_phrase(audio, model, language, Arc::from(on_partial)).await
            }
        })
    }

    fn warm_up(&self) {
        let (id, language) = (self.selection)();
        let model = models::by_id(&id);
        if (self.is_downloaded)(model.id) {
            (self.preload)(model, &language_for(model, &language));
        }
    }
}

impl OnDeviceSpeechToText {
    async fn per_phrase(
        &self,
        mut audio: Box<dyn AudioSource + '_>,
        model: &'static OnDeviceModel,
        language: String,
        on_partial: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<String, SttError> {
        let (segments, mut queued) = mpsc::unbounded_channel::<Vec<Vec<u8>>>();
        // Queued and buffered audio not decoded yet, for the deadline after the stop.
        let pending = Arc::new(AtomicU64::new(0));
        let decode_pending = pending.clone();
        let decode = self.decode.clone();
        // Never makes the collector wait (the microphone would overflow): segments queue up.
        let mut decoder = AbortOnDrop(tokio::spawn(async move {
            let mut text = String::new();
            let mut count = 0;
            while let Some(segment) = queued.recv().await {
                let bytes: usize = segment.iter().map(Vec::len).sum();
                let part = decode(model, language.clone(), pcm16_to_floats(&segment)).await?;
                decode_pending.fetch_sub(bytes as u64, Ordering::Relaxed);
                count += 1;
                let part = part.trim();
                if !part.is_empty() {
                    text = joined(&text, part);
                    on_partial(text.clone());
                }
            }
            debug!("Decoded {count} segments");
            Ok(text)
        }));
        let queue = |segment: Vec<Vec<u8>>| {
            if !segment.is_empty() {
                let _ = segments.send(segment);
            }
        };
        let mut buffer: Vec<Vec<u8>> = Vec::new();
        let mut buffer_bytes = 0;
        loop {
            let chunk = tokio::select! {
                chunk = audio.next() => chunk?,
                // The decoder only ends early on a failure.
                result = &mut decoder.0 => return finished(result),
            };
            let Some(chunk) = chunk else { break };
            if chunk.is_empty() {
                queue(std::mem::take(&mut buffer));
                buffer_bytes = 0;
                continue;
            }
            buffer_bytes += chunk.len();
            pending.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            buffer.push(chunk);
            if buffer_bytes > self.max_segment_bytes {
                let mut parts = split_at_pauses(&buffer, self.max_segment_bytes, SPLIT_SEARCH_BYTES);
                buffer = parts.pop().unwrap_or_default();
                parts.into_iter().for_each(queue);
                buffer_bytes = buffer.iter().map(Vec::len).sum();
            }
        }
        let stopped_at = Instant::now();
        queue(buffer);
        drop(segments);
        let text = within_deadline(&mut decoder, pending.load(Ordering::Relaxed)).await?;
        debug!("Timing: stop->final {} ms", stopped_at.elapsed().as_millis());
        Ok(text)
    }

    /// One engine stream for the whole dictation (context kept across phrases): segment ends are skipped.
    async fn streamed(
        &self,
        mut audio: Box<dyn AudioSource + '_>,
        model: &'static OnDeviceModel,
        on_partial: Arc<dyn Fn(String) + Send + Sync>,
    ) -> Result<String, SttError> {
        let (chunks, taken) = mpsc::unbounded_channel();
        let mut results = (self.stream)(model, taken);
        // Bytes per chunk not decoded yet, oldest first, for the deadline after the stop.
        let queued = Arc::new(Mutex::new(VecDeque::<usize>::new()));
        let pending = Arc::new(AtomicU64::new(0));
        let (decoder_queued, decoder_pending) = (queued.clone(), pending.clone());
        let mut decoder = AbortOnDrop(tokio::spawn(async move {
            let mut text = String::new();
            while let Some(result) = results.recv().await {
                // The final emission has no chunk.
                if let Some(bytes) = decoder_queued.lock().unwrap_or_else(|e| e.into_inner()).pop_front() {
                    decoder_pending.fetch_sub(bytes as u64, Ordering::Relaxed);
                }
                let t = result?.trim().to_string();
                if t != text {
                    text = t;
                    on_partial(text.clone());
                }
            }
            Ok(text)
        }));
        loop {
            let chunk = tokio::select! {
                chunk = audio.next() => chunk?,
                result = &mut decoder.0 => return finished(result),
            };
            let Some(chunk) = chunk else { break };
            if chunk.is_empty() {
                continue;
            }
            queued.lock().unwrap_or_else(|e| e.into_inner()).push_back(chunk.len());
            pending.fetch_add(chunk.len() as u64, Ordering::Relaxed);
            let _ = chunks.send(chunk);
        }
        let stopped_at = Instant::now();
        drop(chunks);
        let text = within_deadline(&mut decoder, pending.load(Ordering::Relaxed)).await?;
        debug!("Timing: stop->final {} ms, streamed", stopped_at.elapsed().as_millis());
        Ok(text)
    }
}

/// Aborts the task when dropped, so a dropped transcribe call (a cancelled dictation) stops its decoder.
struct AbortOnDrop(JoinHandle<Result<String, SttError>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn finished(result: Result<Result<String, SttError>, tokio::task::JoinError>) -> Result<String, SttError> {
    match result {
        Ok(result) => result,
        Err(e) => Err(SttError(if e.is_panic() { "On-device transcription failed".into() } else { e.to_string() })),
    }
}

/// The decoder's text once it has decoded everything, or a timeout past the deadline for `pending_bytes`.
async fn within_deadline(decoder: &mut AbortOnDrop, pending_bytes: u64) -> Result<String, SttError> {
    let pending = Duration::from_millis(pending_bytes * 1000 / (SAMPLE_RATE as u64 * 2));
    match tokio::time::timeout(BASE_DEADLINE + 2 * pending, &mut decoder.0).await {
        Ok(result) => finished(result),
        Err(_) => Err(SttError(TIMED_OUT.into())),
    }
}

/// Only Whisper takes a language, and only one it knows.
fn language_for(model: &OnDeviceModel, language: &str) -> String {
    if model.id == "whisper-small" { whisper_language(language) } else { String::new() }
}

/// Little-endian PCM16 `chunks` as samples in -1..1.
pub fn pcm16_to_floats(chunks: &[Vec<u8>]) -> Vec<f32> {
    chunks
        .iter()
        .flat_map(|chunk| chunk.as_chunks::<2>().0.iter().map(|pair| i16::from_le_bytes(*pair) as f32 / 32768.0))
        .collect()
}

#[cfg(test)]
mod tests;
