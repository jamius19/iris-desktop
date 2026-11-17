/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Microphone capture (Android `AudioRecorder.kt` on handy-recorder): each recording opens the microphone at the
//! session's rate, sends ~50 ms PCM16 mono chunks with Handy's waveform levels, and closes the microphone once the
//! receiver is dropped.

use std::thread;
use std::time::Duration;

use handy_recorder::{AudioChunk as RecorderChunk, Channels, ErrorKind, Recorder, RecorderConfig, Sink};
use iris_core::audio::visualizer::AudioVisualiser;
use iris_core::dictation::controller::{AudioChunk, Recording};
use iris_core::stt::SttError;
use tokio::sync::mpsc::{self, UnboundedSender};
use tokio::time::Instant;

const OPEN_FAILED: &str = "Couldn't open the microphone";
const START_FAILED: &str = "Couldn't start recording. Is another app using the microphone?";
const WINDOWS_BLOCKED: &str = "Windows blocked the microphone. Allow it in Settings > Privacy > Microphone.";

/// Starts recording from `device` ("" = the system default) at `sample_rate` Hz.
pub fn record(device: &str, sample_rate: u32) -> Recording {
    let (tx, rx) = mpsc::unbounded_channel();
    let config = RecorderConfig {
        device: Some(device.to_string()).filter(|d| !d.is_empty()),
        sample_rate: Some(sample_rate),
        channels: Channels::MixToMono,
        // 50 ms: the stop takes effect after the chunk in progress, so smaller is snappier.
        frames_per_chunk: Some(sample_rate as usize / 20),
        take_headset: false,
    };
    thread::Builder::new()
        .name("iris-capture".into())
        .spawn(move || run(config, sample_rate, tx))
        .expect("failed to start the capture thread");
    rx
}

fn run(config: RecorderConfig, sample_rate: u32, tx: UnboundedSender<Result<AudioChunk, SttError>>) {
    let failures = tx.clone();
    let recorder = match Recorder::open_with_failure_handler(config, move |e| {
        log::warn!("The microphone failed: {e}");
        let _ = failures.send(Err(SttError(message(e.kind(), OPEN_FAILED))));
    }) {
        Ok(recorder) => recorder,
        Err(e) => {
            log::warn!("Opening the microphone failed: {e}");
            let _ = tx.send(Err(SttError(message(e.kind(), OPEN_FAILED))));
            return;
        }
    };
    log::info!("Recording from {} at {} Hz", recorder.info().device.name, sample_rate);
    let sink = ChunkSink {
        tx: tx.clone(),
        // Handy's sink: 512-sample window, 16 buckets, 400–4000 Hz (at 16 kHz; 24 kHz keeps the same window time).
        visualiser: AudioVisualiser::new(sample_rate, sample_rate as usize * 32 / 1000, 16, 400.0, 4000.0),
        levels: Vec::new(),
    };
    if let Err(e) = recorder.start(sink) {
        log::warn!("Starting the recording failed: {}", e.error);
        let _ = tx.send(Err(SttError(message(e.error.kind(), START_FAILED))));
        return;
    }
    // Until the session drops its receiver.
    while !tx.is_closed() {
        thread::sleep(Duration::from_millis(20));
    }
    if let Err(e) = recorder.stop() {
        log::debug!("Stopping the recording: {e}");
    }
    if let Err(e) = recorder.close() {
        log::warn!("Closing the microphone failed: {e}");
    }
}

/// The user-facing text for a recorder error of `kind`; `fallback` for the rest.
fn message(kind: ErrorKind, fallback: &str) -> String {
    match kind {
        ErrorKind::PermissionDenied if cfg!(windows) => WINDOWS_BLOCKED,
        ErrorKind::DeviceBusy => START_FAILED,
        _ => fallback,
    }
    .to_string()
}

struct ChunkSink {
    tx: UnboundedSender<Result<AudioChunk, SttError>>,
    visualiser: AudioVisualiser,
    levels: Vec<f32>,
}

impl Sink for ChunkSink {
    fn process_chunk(&mut self, chunk: RecorderChunk<'_>) {
        let read_at = Instant::now();
        let samples = &chunk.samples[..chunk.valid_frames];
        if let Some(levels) = self.visualiser.feed(samples) {
            self.levels = levels;
        }
        let bytes = samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect();
        let _ = self.tx.send(Ok(AudioChunk { bytes, levels: std::mem::take(&mut self.levels), read_at }));
    }
}
