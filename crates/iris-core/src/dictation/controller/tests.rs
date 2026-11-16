/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `DictationControllerTest.kt` on tokio's paused clock: a recording sends a chunk every period, read at
//! the virtual time it is sent.

use super::*;
use crate::settings::{STT_PROVIDER_OPENAI, STT_PROVIDER_SONIOX, STT_PROVIDER_VERCEL_GATEWAY, Values, to_settings};
use crate::stt::OnPartial;
use std::collections::HashSet;
use std::f64::consts::PI;
use std::ops::RangeInclusive;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use tokio::time::sleep;

const DONE_MS: u64 = DONE_DISPLAY.as_millis() as u64;
const ERROR_MS: u64 = ERROR_DISPLAY.as_millis() as u64;
const DETECTION_MS: u64 = LANGUAGE_DETECTION_TIMEOUT.as_millis() as u64;

/// Runs every task that is ready without moving the clock (Kotlin's `runCurrent`).
async fn run_current() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// Moves the clock `ms` forward, running every task due up to and including then (Kotlin's `advanceTimeBy`, which
/// stops before the tasks due at the end, then `runCurrent`).
async fn advance(ms: u64) {
    sleep(Duration::from_millis(ms)).await;
    run_current().await;
}

fn listening(text: &str) -> DictationState {
    DictationState::Listening { text: text.into() }
}

fn finalizing(text: &str) -> DictationState {
    DictationState::Finalizing { text: text.into() }
}

fn done(text: &str) -> DictationState {
    DictationState::Done { text: text.into(), notice: None }
}

fn error(message: &str) -> DictationState {
    DictationState::Error { message: message.into() }
}

/// Cloud defaults: the tests below were written against OpenAI, the default before on device.
fn open_ai() -> Settings {
    Settings { stt_provider_id: STT_PROVIDER_OPENAI.into(), ..to_settings(&Values::new(), &HashSet::new()).unwrap() }
}

fn speed(percent: i32) -> Settings {
    Settings { stt_speed_percent: percent, ..open_ai() }
}

fn with_words() -> Settings {
    Settings {
        stt_language: "en".into(),
        custom_words_enabled: true,
        custom_words: vec!["Deepgram".into()],
        correct_similar_words: true,
        ..open_ai()
    }
}

type Shared<T> = Arc<Mutex<T>>;

/// The Nth chunk (from 0) of a recording, or the error that ends it.
type ChunkAt = Arc<dyn Fn(usize) -> Result<Vec<u8>, SttError> + Send + Sync>;

/// A recording: the Nth chunk every `period` until the recording is dropped, then `release` until the microphone is
/// released.
#[derive(Clone)]
struct Audio {
    period: Duration,
    chunk: ChunkAt,
    release: Duration,
}

impl Audio {
    fn every(ms: u64, chunk: impl Fn(usize) -> Result<Vec<u8>, SttError> + Send + Sync + 'static) -> Self {
        Self { period: Duration::from_millis(ms), chunk: Arc::new(chunk), release: Duration::ZERO }
    }

    /// Starts recording on the runtime: chunks carry `level` and their send time.
    fn record(&self, level: f32, released: Arc<AtomicBool>) -> Recording {
        let (tx, rx) = mpsc::unbounded_channel();
        let Audio { period, chunk, release } = self.clone();
        tokio::spawn(async move {
            for n in 0.. {
                let bytes = match chunk(n) {
                    Ok(bytes) => bytes,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                };
                if tx.send(Ok(AudioChunk { bytes, levels: vec![level], read_at: Instant::now() })).is_err() {
                    break;
                }
                tokio::select! {
                    biased;
                    _ = tx.closed() => break,
                    _ = sleep(period) => {}
                }
            }
            if !release.is_zero() {
                sleep(release).await;
            }
            released.store(true, Ordering::SeqCst);
        });
        rx
    }
}

/// A 100 ms chunk every 100 ms.
fn chunks() -> Audio {
    Audio::every(100, |_| Ok(vec![0; 2]))
}

/// One 32 ms frame at 16 kHz every 32 ms, the Nth (from 0) tagged with N in its first byte.
fn frames() -> Audio {
    Audio::every(32, |n| Ok(frame(n)))
}

fn frame(n: usize) -> Vec<u8> {
    let mut bytes = vec![0; 1024];
    bytes[0] = n as u8;
    bytes
}

/// [`frames`], never all zeros (a keep-alive chunk is).
fn marked_frames() -> Audio {
    Audio::every(32, |n| {
        let mut bytes = frame(n);
        bytes[1] = 1;
        Ok(bytes)
    })
}

/// [`frames`], each also collected in `recorded`.
fn recorded_frames(recorded: Shared<Vec<Vec<u8>>>) -> Audio {
    Audio::every(32, move |n| {
        recorded.lock().unwrap().push(frame(n));
        Ok(frame(n))
    })
}

/// A 50 ms chunk of a 200 Hz sine at 16 kHz every 50 ms; `sent` collects them.
fn sine(sent: Shared<Vec<Vec<u8>>>) -> Audio {
    Audio::every(50, move |n| {
        let chunk: Vec<u8> = (0..800)
            .flat_map(|i| {
                let t = (n * 800 + i) as f64 / 16_000.0;
                (((2.0 * PI * 200.0 * t).sin() * 10_000.0).round() as i16).to_le_bytes()
            })
            .collect();
        sent.lock().unwrap().push(chunk.clone());
        Ok(chunk)
    })
}

fn is_zeros(chunk: &[u8]) -> bool {
    chunk.iter().all(|b| *b == 0)
}

fn tags(chunks: &[Vec<u8>]) -> Vec<usize> {
    chunks.iter().map(|chunk| chunk[0] as usize).collect()
}

fn range(r: RangeInclusive<usize>) -> Vec<usize> {
    r.collect()
}

type VadModel = Box<dyn Fn() -> VadScorer + Send + Sync>;

/// The scorer is called once per frame, in order: frames in `voiced` (first, last; inclusive) are speech.
fn speech_at(voiced: &[(usize, usize)]) -> VadModel {
    let voiced = voiced.to_vec();
    Box::new(move || {
        let voiced = voiced.clone();
        let mut calls = 0;
        Box::new(move |_: &[f32]| {
            let frame = calls;
            calls += 1;
            Ok(if voiced.iter().any(|(first, last)| (*first..=*last).contains(&frame)) { 1.0 } else { 0.0 })
        })
    })
}

fn silence() -> VadModel {
    Box::new(|| Box::new(|_: &[f32]| Ok(0.0)))
}

type SttResult = Box<dyn Fn() -> BoxFuture<'static, Result<String, SttError>> + Send + Sync>;

#[derive(Default)]
struct Seen {
    calls: usize,
    model: String,
    language: String,
    timeout: Duration,
    custom_words: Option<Vec<String>>,
    received: Vec<Vec<u8>>,
}

/// Reports "chunk N" for each chunk (if it streams), then returns `result` once the audio ends.
struct FakeStt {
    rate: u32,
    streams: bool,
    live: bool,
    allows_speed_up: bool,
    wants_segment_ends: bool,
    result: SttResult,
    seen: Mutex<Seen>,
}

impl FakeStt {
    fn new(result: impl Fn() -> BoxFuture<'static, Result<String, SttError>> + Send + Sync + 'static) -> Self {
        Self {
            rate: 24_000,
            streams: true,
            live: true,
            allows_speed_up: true,
            wants_segment_ends: false,
            result: Box::new(result),
            seen: Mutex::default(),
        }
    }

    fn replying(text: &'static str) -> Self {
        Self::new(move || Box::pin(async move { Ok(text.to_string()) }))
    }

    /// Replies `text` `ms` after the audio ended.
    fn after(ms: u64, text: &'static str) -> Self {
        Self::new(move || {
            Box::pin(async move {
                sleep(Duration::from_millis(ms)).await;
                Ok(text.to_string())
            })
        })
    }

    fn failing(message: &'static str) -> Self {
        Self::new(move || Box::pin(async move { Err(SttError(message.into())) }))
    }

    fn rate(self, rate: u32) -> Self {
        Self { rate, ..self }
    }

    /// Also sets live text, as Kotlin's `live = streams` default.
    fn streams(self, streams: bool) -> Self {
        Self { streams, live: streams, ..self }
    }

    fn live(self, live: bool) -> Self {
        Self { live, ..self }
    }

    fn no_speed_up(self) -> Self {
        Self { allows_speed_up: false, ..self }
    }

    fn segment_ends(self) -> Self {
        Self { wants_segment_ends: true, ..self }
    }

    fn seen(&self) -> MutexGuard<'_, Seen> {
        self.seen.lock().unwrap()
    }

    fn received(&self) -> Vec<Vec<u8>> {
        self.seen().received.clone()
    }
}

impl SpeechToTextProvider for FakeStt {
    fn sample_rate(&self, _model: &str) -> u32 {
        self.rate
    }

    fn streams(&self) -> bool {
        self.streams
    }

    fn live_text(&self, _model: &str) -> bool {
        self.live
    }

    fn allows_speed_up(&self) -> bool {
        self.allows_speed_up
    }

    fn wants_segment_ends(&self) -> bool {
        self.wants_segment_ends
    }

    fn transcribe<'a>(
        &'a self,
        mut audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async move {
            {
                let mut seen = self.seen();
                seen.calls += 1;
                seen.model = request.model;
                seen.language = request.language;
                seen.timeout = request.timeout;
                seen.custom_words = Some(request.custom_words);
            }
            let mut chunks = 0;
            while let Some(chunk) = audio.next().await? {
                self.seen().received.push(chunk);
                chunks += 1;
                if self.streams {
                    on_partial(format!("chunk {chunks}"));
                }
            }
            (self.result)().await
        })
    }
}

type Hook = Box<dyn Fn(String) -> BoxFuture<'static, DictationState> + Send + Sync>;
type Detector = Box<dyn Fn(String) -> BoxFuture<'static, Option<String>> + Send + Sync>;

/// A controller's dependencies (Kotlin's `controller(...)` arguments); `..Setup::new(stt)` for the defaults.
struct Setup {
    stt: Arc<dyn SpeechToTextProvider>,
    audio: Audio,
    level: f32,
    settings: Settings,
    /// VAD only for tests that pass a model.
    vad_model: Option<VadModel>,
    /// None: collects the text in `finals` and shows it as Done.
    on_final_transcript: Option<Hook>,
    detect_language: Detector,
}

impl Setup {
    fn new(stt: Arc<dyn SpeechToTextProvider>) -> Self {
        Self {
            stt,
            audio: chunks(),
            level: 0.0,
            settings: open_ai(),
            vad_model: None,
            on_final_transcript: None,
            detect_language: Box::new(|_| Box::pin(async { None })),
        }
    }

    fn build(self) -> Harness {
        let finals = Shared::<Vec<String>>::default();
        let released = Arc::new(AtomicBool::new(false));
        let recorded_rate = Arc::new(AtomicU32::new(0));
        let settings = Settings { vad_enabled: self.vad_model.is_some(), ..self.settings };
        let (audio, level, stt) = (self.audio, self.level, self.stt);
        let (audio_released, rate) = (released.clone(), recorded_rate.clone());
        let collected = finals.clone();
        let deps = Deps {
            audio: Box::new(move |r| {
                rate.store(r, Ordering::SeqCst);
                audio.record(level, audio_released.clone())
            }),
            stt: Box::new(move |_| stt.clone()),
            settings: Box::new(move || settings.clone()),
            on_final_transcript: self.on_final_transcript.unwrap_or_else(|| {
                Box::new(move |text| {
                    collected.lock().unwrap().push(text.clone());
                    Box::pin(async move { done(&text) })
                })
            }),
            vad_model: self.vad_model.unwrap_or_else(|| Box::new(|| panic!("VAD off"))),
            detect_language: self.detect_language,
        };
        Harness { controller: DictationController::new(Handle::current(), deps), finals, released, recorded_rate }
    }
}

struct Harness {
    controller: DictationController,
    finals: Shared<Vec<String>>,
    released: Arc<AtomicBool>,
    recorded_rate: Arc<AtomicU32>,
}

impl Harness {
    fn state(&self) -> DictationState {
        self.controller.state().borrow().clone()
    }

    fn levels(&self) -> Vec<f32> {
        self.controller.levels().borrow().clone()
    }

    fn finals(&self) -> Vec<String> {
        self.finals.lock().unwrap().clone()
    }

    fn released(&self) -> bool {
        self.released.load(Ordering::SeqCst)
    }

    fn recorded_rate(&self) -> u32 {
        self.recorded_rate.load(Ordering::SeqCst)
    }

    /// Starts, stops after the first chunk and lets the chunk recorded during the stop end the audio.
    async fn dictate(&self) {
        self.controller.start();
        run_current().await;
        self.controller.stop();
        advance(101).await;
    }

    /// Starts, stops after `stop_ms` and lets the chunks recorded until `after_stop_ms` later in.
    async fn dictate_for(&self, stop_ms: u64, after_stop_ms: u64) {
        self.controller.start();
        run_current().await;
        advance(stop_ms).await;
        self.controller.stop();
        advance(after_stop_ms).await;
    }
}

fn stt(fake: FakeStt) -> Arc<FakeStt> {
    Arc::new(fake)
}

#[tokio::test(start_paused = true)]
async fn start_then_stop_finalizes_to_done_then_idle() {
    let stt = stt(FakeStt::replying("Hello world"));
    let h = Setup::new(stt.clone()).build();

    h.controller.start();
    run_current().await;
    assert_eq!(listening("chunk 1"), h.state());
    advance(150).await;
    assert_eq!(listening("chunk 2"), h.state());

    h.controller.stop();
    assert_eq!(finalizing("chunk 2"), h.state());
    // The chunk recorded during the stop is still sent, then the audio ends.
    advance(100).await;
    assert_eq!(done("Hello world"), h.state());
    assert_eq!(vec!["Hello world"], h.finals());
    assert!(h.released());
    assert_eq!("gpt-live-transcribe", stt.seen().model);

    advance(DONE_MS + 1).await;
    assert_eq!(DictationState::Idle, h.state());
}

#[tokio::test(start_paused = true)]
async fn session_records_at_the_providers_rate_for_the_active_model() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000));
    let settings = Settings {
        stt_provider_id: STT_PROVIDER_VERCEL_GATEWAY.into(),
        gateway_stt_model: "google/gemini-3.5-transcribe-live".into(),
        ..open_ai()
    };
    let h = Setup { settings, ..Setup::new(stt.clone()) }.build();

    h.dictate().await; // The chunk recorded during the stop ends the audio.

    assert_eq!(done("Hi"), h.state());
    assert_eq!(16_000, h.recorded_rate());
    assert_eq!("google/gemini-3.5-transcribe-live", stt.seen().model);
}

#[tokio::test(start_paused = true)]
async fn session_passes_the_active_providers_language() {
    for (provider, expected) in [(STT_PROVIDER_OPENAI, "fr"), (STT_PROVIDER_SONIOX, "en, bn")] {
        let stt = stt(FakeStt::replying("Hi"));
        let settings = Settings {
            stt_provider_id: provider.into(),
            stt_language: "fr".into(),
            soniox_languages: "en, bn".into(),
            ..open_ai()
        };
        let h = Setup { settings, ..Setup::new(stt.clone()) }.build();

        h.dictate().await;

        assert_eq!(done("Hi"), h.state());
        assert_eq!(expected, stt.seen().language);
    }
}

/// Desktop: the recording ends by dropping its receiver, so the session doesn't wait for the microphone's release
/// (Android's flow waits for its `finally`): Done right away, while the release still runs.
#[tokio::test(start_paused = true)]
async fn audio_ends_on_stop_without_waiting_for_the_mic_release() {
    let start = Instant::now();
    let audio_ended_at = Shared::<Option<Duration>>::default();
    let ended = audio_ended_at.clone();
    let stt = stt(FakeStt::new(move || {
        *ended.lock().unwrap() = Some(start.elapsed());
        Box::pin(async { Ok("Hi".to_string()) })
    }));
    let audio = Audio { release: Duration::from_millis(1_000), ..chunks() }; // A slow mic release.
    let h = Setup { audio, ..Setup::new(stt) }.build();

    h.controller.start();
    run_current().await;
    advance(150).await;
    h.controller.stop();
    advance(100).await;
    // Ended right after the chunk recorded during the tap (t = 200), not after the release.
    assert_eq!(Some(Duration::from_millis(200)), *audio_ended_at.lock().unwrap());
    assert_eq!(done("Hi"), h.state());
    assert!(!h.released());

    advance(1_000).await;
    assert!(h.released());
}

#[tokio::test(start_paused = true)]
async fn session_passes_the_stt_timeout_setting() {
    let stt = stt(FakeStt::replying("Hi"));
    let h = Setup { settings: Settings { stt_timeout_seconds: 20, ..open_ai() }, ..Setup::new(stt.clone()) }.build();

    h.dictate().await;

    assert_eq!(done("Hi"), h.state());
    assert_eq!(Duration::from_secs(20), stt.seen().timeout);
}

/// Stalls 1 s after the first chunk (chunks queue up meanwhile), then takes the rest, counting them.
#[derive(Default)]
struct Stalling(AtomicUsize);

impl SpeechToTextProvider for Stalling {
    fn transcribe<'a>(
        &'a self,
        mut audio: Box<dyn AudioSource + 'a>,
        _request: SttRequest,
        _on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async move {
            while audio.next().await?.is_some() {
                if self.0.fetch_add(1, Ordering::SeqCst) == 0 {
                    sleep(Duration::from_millis(1_000)).await;
                }
            }
            Ok("Hi".to_string())
        })
    }
}

#[tokio::test(start_paused = true)]
async fn chunks_queued_before_the_stop_tap_are_all_sent() {
    let stt = Arc::new(Stalling::default());
    let h = Setup::new(stt.clone()).build();

    h.dictate_for(950, 100).await;

    // The 10 chunks read before the tap (t = 0..900) and the one in progress (t = 1000).
    assert_eq!(11, stt.0.load(Ordering::SeqCst));
    assert_eq!(done("Hi"), h.state());
}

#[tokio::test(start_paused = true)]
async fn cancel_while_finalizing_discards_session() {
    let h = Setup::new(stt(FakeStt::after(1_000, "discarded"))).build();

    h.controller.start();
    run_current().await;
    h.controller.stop();
    advance(150).await;
    h.controller.cancel();
    assert_eq!(DictationState::Idle, h.state());

    advance(10_000).await;
    assert_eq!(DictationState::Idle, h.state());
    assert!(h.finals().is_empty());
}

#[tokio::test(start_paused = true)]
async fn start_and_stop_are_ignored_while_finalizing() {
    let stt = stt(FakeStt::after(10_000, "Hi"));
    let h = Setup::new(stt.clone()).build();

    h.controller.start();
    run_current().await;
    h.controller.stop();
    advance(150).await;
    assert!(!h.controller.start());
    h.controller.stop();
    run_current().await;
    assert!(matches!(h.state(), DictationState::Finalizing { .. }));
    assert_eq!(1, stt.seen().calls);

    advance(10_000).await;
    assert_eq!(done("Hi"), h.state());
    assert_eq!(vec!["Hi"], h.finals());
}

#[tokio::test(start_paused = true)]
async fn stop_is_ignored_unless_listening() {
    let h = Setup::new(stt(FakeStt::replying("Hi"))).build();

    h.controller.stop();
    run_current().await;
    assert_eq!(DictationState::Idle, h.state());

    h.dictate().await;
    assert_eq!(done("Hi"), h.state());
    h.controller.stop();
    assert_eq!(done("Hi"), h.state());
}

#[tokio::test(start_paused = true)]
async fn stop_returns_false_when_idle() {
    let h = Setup::new(stt(FakeStt::replying("Hi"))).build();

    assert!(!h.controller.stop());
    assert_eq!(DictationState::Idle, h.state());
}

#[tokio::test(start_paused = true)]
async fn cancel_if_listening_leaves_a_finalizing_session_to_end() {
    let h = Setup::new(stt(FakeStt::after(1_000, "Hi"))).build();

    h.controller.start();
    run_current().await;
    assert!(h.controller.stop());
    advance(150).await;
    assert!(!h.controller.cancel_if_listening());
    assert!(matches!(h.state(), DictationState::Finalizing { .. }));

    advance(1_000).await;
    assert_eq!(done("Hi"), h.state());
    assert_eq!(vec!["Hi"], h.finals());
}

#[tokio::test(start_paused = true)]
async fn level_follows_chunks_while_listening_and_resets_on_stop() {
    let h = Setup { level: 0.5, ..Setup::new(stt(FakeStt::replying("Hi"))) }.build();

    h.controller.start();
    run_current().await;
    assert_eq!(vec![0.5], h.levels());
    h.controller.stop();
    assert!(h.levels().is_empty());
    advance(101).await; // The chunk in progress at stop is sent but doesn't set the level.
    assert_eq!(done("Hi"), h.state());
    assert!(h.levels().is_empty());

    h.controller.start();
    run_current().await;
    assert_eq!(vec![0.5], h.levels());
    h.controller.cancel();
    assert!(h.levels().is_empty());
}

#[tokio::test(start_paused = true)]
async fn start_is_ignored_while_listening() {
    let stt = stt(FakeStt::replying("Hi"));
    let h = Setup::new(stt.clone()).build();

    h.controller.start();
    run_current().await;
    advance(150).await;
    assert!(!h.controller.start());
    run_current().await;
    assert_eq!(listening("chunk 2"), h.state());
    assert_eq!(1, stt.seen().calls);
}

/// Fails with "OpenAI error: Boom" on the second chunk.
struct FailsOnSecondChunk;

impl SpeechToTextProvider for FailsOnSecondChunk {
    fn transcribe<'a>(
        &'a self,
        mut audio: Box<dyn AudioSource + 'a>,
        _request: SttRequest,
        _on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async move {
            let mut chunks = 0;
            while audio.next().await?.is_some() {
                chunks += 1;
                if chunks == 2 {
                    return Err(SttError("OpenAI error: Boom".into()));
                }
            }
            Ok("unused".to_string())
        })
    }
}

#[tokio::test(start_paused = true)]
async fn level_resets_on_stt_error_while_listening() {
    let h = Setup { level: 0.5, ..Setup::new(Arc::new(FailsOnSecondChunk)) }.build();

    h.controller.start();
    run_current().await;
    assert_eq!(vec![0.5], h.levels());
    advance(101).await;
    assert_eq!(error("OpenAI error: Boom"), h.state());
    assert!(h.levels().is_empty());
}

#[tokio::test(start_paused = true)]
async fn start_while_done_starts_new_session_that_the_display_timer_leaves_alone() {
    let stt = stt(FakeStt::replying("Hi"));
    let h = Setup::new(stt.clone()).build();

    h.dictate().await;
    assert_eq!(done("Hi"), h.state());

    assert!(h.controller.start());
    run_current().await;
    assert_eq!(listening("chunk 1"), h.state());
    advance(DONE_MS + 1).await;
    assert!(matches!(h.state(), DictationState::Listening { .. }));
    assert_eq!(2, stt.seen().calls);
}

#[tokio::test(start_paused = true)]
async fn start_while_error_starts_new_session_that_the_display_timer_leaves_alone() {
    let stt = stt(FakeStt::replying(""));
    let h = Setup::new(stt.clone()).build();

    h.dictate().await;
    assert_eq!(error("No speech detected"), h.state());

    assert!(h.controller.start());
    run_current().await;
    assert_eq!(listening("chunk 1"), h.state());
    advance(ERROR_MS + 1).await;
    assert!(matches!(h.state(), DictationState::Listening { .. }));
    assert_eq!(2, stt.seen().calls);
}

#[tokio::test(start_paused = true)]
async fn recording_error_shows_message_then_idle() {
    let audio = Audio::every(100, |_| Err(SttError("Microphone permission is missing.".into())));
    let h = Setup { audio, ..Setup::new(stt(FakeStt::replying("unused"))) }.build();

    h.controller.start();
    run_current().await;
    assert_eq!(error("Microphone permission is missing."), h.state());

    advance(ERROR_MS + 1).await;
    assert_eq!(DictationState::Idle, h.state());
    assert!(h.finals().is_empty());
}

#[tokio::test(start_paused = true)]
async fn provider_error_shows_message() {
    let h = Setup::new(stt(FakeStt::failing("OpenAI error: Boom"))).build();

    h.dictate().await;
    assert_eq!(error("OpenAI error: Boom"), h.state());
    assert!(h.finals().is_empty());
}

#[tokio::test(start_paused = true)]
async fn cancel_while_listening_discards_session() {
    let h = Setup::new(stt(FakeStt::replying("discarded"))).build();

    h.controller.start();
    run_current().await;
    h.controller.cancel();
    run_current().await;
    assert_eq!(DictationState::Idle, h.state());
    assert!(h.released());

    advance(10_000).await;
    assert_eq!(DictationState::Idle, h.state());
    assert!(h.finals().is_empty());
}

/// Desktop: the hook can't throw; a panic in it is the equivalent.
#[tokio::test(start_paused = true)]
async fn final_transcript_hook_exception_shows_generic_error() {
    let hook: Hook = Box::new(|_| Box::pin(async { panic!("boom") }));
    let h = Setup { on_final_transcript: Some(hook), ..Setup::new(stt(FakeStt::replying("Hi"))) }.build();

    h.dictate().await;
    assert_eq!(error("Something went wrong"), h.state());
}

#[tokio::test(start_paused = true)]
async fn done_with_notice_shows_it_for_longer() {
    let notice = DictationState::Done { text: "Hi".into(), notice: Some("Copied to clipboard".into()) };
    let shown = notice.clone();
    let hook: Hook = Box::new(move |_| {
        let shown = shown.clone();
        Box::pin(async move { shown })
    });
    let h = Setup { on_final_transcript: Some(hook), ..Setup::new(stt(FakeStt::replying("Hi"))) }.build();

    h.dictate().await;
    assert_eq!(notice, h.state());

    advance(DONE_MS + 1).await;
    assert_eq!(notice, h.state());
    advance(ERROR_MS - DONE_MS).await;
    assert_eq!(DictationState::Idle, h.state());
}

#[tokio::test(start_paused = true)]
async fn stop_during_silence_ends_at_once_with_no_speech_and_sends_nothing() {
    // Never reached: no STT wait.
    let stt = stt(FakeStt::after(10_000, "unused").rate(16_000));
    let h = Setup { audio: frames(), vad_model: Some(silence()), ..Setup::new(stt.clone()) }.build();

    h.dictate_for(1_000, 33).await; // The frame read during the tap ends the audio.

    assert_eq!(error("No speech detected"), h.state());
    assert!(stt.received().is_empty());
    assert!(h.released());
}

#[tokio::test(start_paused = true)]
async fn level_follows_every_chunk_while_the_gate_sends_nothing() {
    let stt = stt(FakeStt::replying("unused").rate(16_000));
    let h = Setup { audio: frames(), level: 0.5, vad_model: Some(silence()), ..Setup::new(stt.clone()) }.build();

    h.controller.start();
    run_current().await;
    advance(500).await;
    assert_eq!(vec![0.5], h.levels());
    assert!(stt.received().is_empty());
}

/// Frames 0..100 before the stop at 3,210 ms, frame 101 ends the audio.
async fn dictate_speech_at_20_to_29(stt: FakeStt) -> Arc<FakeStt> {
    let stt = Arc::new(stt);
    let h = Setup { audio: frames(), vad_model: Some(speech_at(&[(20, 29)])), ..Setup::new(stt.clone()) }.build();
    h.dictate_for(100 * 32 + 10, 32).await;
    assert_eq!(done("Hi"), h.state());
    stt
}

#[tokio::test(start_paused = true)]
async fn streaming_provider_gets_pre_roll_speech_and_the_streaming_hangover() {
    let stt = dictate_speech_at_20_to_29(FakeStt::replying("Hi").rate(16_000)).await;

    // 450 ms pre-roll (frames 6..20, the first voiced one included), speech up to 29, then 52 hangover frames.
    assert_eq!(range(6..=81), tags(&stt.received()));
}

#[tokio::test(start_paused = true)]
async fn uploading_provider_gets_the_offline_hangover() {
    let stt = dictate_speech_at_20_to_29(FakeStt::replying("Hi").rate(16_000).streams(false)).await;

    assert_eq!(range(6..=44), tags(&stt.received()));
}

#[tokio::test(start_paused = true)]
async fn streaming_provider_without_live_text_gets_the_offline_hangover() {
    let stt = dictate_speech_at_20_to_29(FakeStt::replying("Hi").rate(16_000).live(false)).await;

    assert_eq!(range(6..=44), tags(&stt.received()));
}

#[tokio::test(start_paused = true)]
async fn vad_at_24_khz_sends_pre_roll_speech_and_the_streaming_hangover() {
    let stt = stt(FakeStt::replying("Hi"));
    // 1200-sample (50 ms) chunks at 24 kHz, the Nth tagged with N: 1.5625 VAD frames each.
    let audio = Audio::every(50, |n| {
        let mut bytes = vec![0; 2_400];
        bytes[0] = n as u8;
        Ok(bytes)
    });
    let h = Setup { audio, vad_model: Some(speech_at(&[(20, 29)])), ..Setup::new(stt.clone()) }.build();

    h.dictate_for(60 * 50 + 10, 50).await;

    assert_eq!(done("Hi"), h.state());
    assert_eq!(24_000, h.recorded_rate());
    // Onset at frame 21 (chunk 14) with 9 chunks (450 ms) of pre-roll; the 52nd hangover frame (81) completes in
    // chunk 52.
    assert_eq!(range(5..=52), tags(&stt.received()));
}

#[tokio::test(start_paused = true)]
async fn streaming_provider_gets_a_chunk_of_zeros_every_5s_of_withheld_audio() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000));
    let vad_model = Some(speech_at(&[(20, 29), (460, 469)]));
    let h = Setup { audio: marked_frames(), vad_model, ..Setup::new(stt.clone()) }.build();

    h.dictate_for(530 * 32 + 10, 32).await;

    assert_eq!(done("Hi"), h.state());
    // Speech 6..81 (76 chunks), last sent at 2.59 s; zeros at frames 238 (7.62 s) and 395 (12.64 s); speech again
    // from 446 (pre-roll) to 521 (76 chunks).
    let received = stt.received();
    assert_eq!(154, received.len());
    assert_eq!(vec![76, 77], (0..received.len()).filter(|i| is_zeros(&received[*i])).collect::<Vec<_>>());
    assert_eq!(1_024, received[76].len());
}

#[tokio::test(start_paused = true)]
async fn uploading_provider_gets_no_keep_alive() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000).streams(false));
    let vad_model = Some(speech_at(&[(20, 29), (460, 469)]));
    let h = Setup { audio: marked_frames(), vad_model, ..Setup::new(stt.clone()) }.build();

    h.dictate_for(530 * 32 + 10, 32).await;

    assert_eq!(done("Hi"), h.state());
    let received = stt.received();
    assert!(!received.is_empty());
    assert!(!received.iter().any(|chunk| is_zeros(chunk)));
}

#[tokio::test(start_paused = true)]
async fn keep_alive_zeros_dont_count_as_speech() {
    // Never reached: no STT wait.
    let stt = stt(FakeStt::after(10_000, "unused").rate(16_000));
    let h = Setup { audio: marked_frames(), vad_model: Some(silence()), ..Setup::new(stt.clone()) }.build();

    h.dictate_for(6_000, 33).await;

    assert_eq!(error("No speech detected"), h.state());
    // Only the keep-alive at 5 s reached the provider.
    let received = stt.received();
    assert_eq!(1, received.len());
    assert!(is_zeros(&received[0]));
}

#[tokio::test(start_paused = true)]
async fn vad_off_sends_every_chunk() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000));
    let h = Setup { audio: frames(), ..Setup::new(stt.clone()) }.build(); // The VAD model would panic if called.

    h.dictate_for(10 * 32 + 10, 32).await;

    assert_eq!(done("Hi"), h.state());
    assert_eq!(range(0..=11), tags(&stt.received()));
}

#[tokio::test(start_paused = true)]
async fn failing_vad_model_sends_every_chunk() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000));
    let failing: VadModel = Box::new(|| Box::new(|_: &[f32]| Err("No model".to_string())));
    let h = Setup { audio: frames(), vad_model: Some(failing), ..Setup::new(stt.clone()) }.build();

    h.dictate_for(10 * 32 + 10, 32).await;

    assert_eq!(done("Hi"), h.state());
    assert_eq!(range(0..=11), tags(&stt.received()));
}

#[tokio::test(start_paused = true)]
async fn uploading_provider_shows_no_live_text_then_the_final_text() {
    // 500 ms: the upload.
    let stt = stt(FakeStt::after(500, "Hello world").rate(16_000).streams(false));
    let h = Setup { audio: frames(), ..Setup::new(stt) }.build();

    h.controller.start();
    run_current().await;
    advance(1_000).await;
    assert_eq!(listening(""), h.state());
    h.controller.stop();
    advance(100).await;
    assert_eq!(finalizing(""), h.state());

    advance(500).await;
    assert_eq!(done("Hello world"), h.state());
    assert_eq!(vec!["Hello world"], h.finals());
    assert_eq!(16_000, h.recorded_rate());
}

/// Speech at frames 20..29 and 200..209, stopped at frame 240 (before the second segment's hangover ends).
async fn two_segments(stt: &Arc<FakeStt>, settings: Settings) {
    let vad_model = Some(speech_at(&[(20, 29), (200, 209)]));
    let h = Setup { audio: frames(), settings, vad_model, ..Setup::new(stt.clone()) }.build();
    h.dictate_for(240 * 32 + 10, 32).await;
    assert_eq!(done("Hi"), h.state());
}

fn segment_ends(received: &[Vec<u8>]) -> Vec<usize> {
    (0..received.len()).filter(|i| received[*i].is_empty()).collect()
}

#[tokio::test(start_paused = true)]
async fn opted_in_provider_gets_one_empty_array_between_segments() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000).streams(false).live(true).segment_ends());
    two_segments(&stt, speed(100)).await;

    let received = stt.received();
    // Segment 1 is 6..81 (pre-roll, speech, 52 hangover frames); frame 82 ends it and joins the pre-roll.
    assert_eq!(vec![76], segment_ends(&received));
    let sent: Vec<Vec<u8>> = received.into_iter().filter(|chunk| !chunk.is_empty()).collect();
    assert_eq!([range(6..=81), range(186..=241)].concat(), tags(&sent));
}

#[tokio::test(start_paused = true)]
async fn segment_end_survives_the_speed_up() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000).streams(false).live(true).segment_ends());
    two_segments(&stt, speed(150)).await;

    let received = stt.received();
    let ends = segment_ends(&received);
    assert_eq!(1, ends.len());
    let before: usize = received[..ends[0]].iter().map(Vec::len).sum();
    let expected = 76.0 * 1024.0 / 1.5;
    assert!((expected - before as f64).abs() <= 76.0 * 1024.0 * 0.02, "{before} bytes before the end");
}

#[tokio::test(start_paused = true)]
async fn provider_that_did_not_opt_in_never_gets_an_empty_array() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000));
    two_segments(&stt, speed(100)).await;

    let received = stt.received();
    assert!(!received.is_empty());
    assert!(!received.iter().any(Vec::is_empty));
}

/// Desktop: chunks are moved, not shared, so "the same array" is checked as equal bytes.
#[tokio::test(start_paused = true)]
async fn speed_100_sends_the_gated_arrays_unchanged() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000));
    let recorded = Shared::<Vec<Vec<u8>>>::default();
    let h = Setup {
        audio: recorded_frames(recorded.clone()),
        settings: speed(100),
        vad_model: Some(speech_at(&[(20, 29)])),
        ..Setup::new(stt.clone())
    }
    .build();

    h.dictate_for(100 * 32 + 10, 32).await;

    assert_eq!(done("Hi"), h.state());
    let received = stt.received();
    assert_eq!(range(6..=81), tags(&received));
    let recorded = recorded.lock().unwrap();
    received.iter().for_each(|chunk| assert_eq!(&recorded[chunk[0] as usize], chunk));
}

/// Desktop: chunks are moved, not shared, so "the same array" is checked as equal bytes.
#[tokio::test(start_paused = true)]
async fn speed_200_is_ignored_when_the_provider_disallows_it() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000).no_speed_up());
    let recorded = Shared::<Vec<Vec<u8>>>::default();
    let h = Setup {
        audio: recorded_frames(recorded.clone()),
        settings: speed(200),
        vad_model: Some(speech_at(&[(20, 29)])),
        ..Setup::new(stt.clone())
    }
    .build();

    h.dictate_for(100 * 32 + 10, 32).await;

    assert_eq!(done("Hi"), h.state());
    let received = stt.received();
    assert_eq!(range(6..=81), tags(&received));
    let recorded = recorded.lock().unwrap();
    received.iter().for_each(|chunk| assert_eq!(&recorded[chunk[0] as usize], chunk));
}

#[tokio::test(start_paused = true)]
async fn speed_200_sends_half_the_audio_including_the_flushed_tail() {
    let stt = stt(FakeStt::replying("Hi").rate(16_000));
    let sent = Shared::<Vec<Vec<u8>>>::default();
    let h = Setup { audio: sine(sent.clone()), settings: speed(200), ..Setup::new(stt.clone()) }.build();

    h.dictate_for(2_000, 51).await; // The chunk recorded during the stop ends the audio.

    assert_eq!(done("Hi"), h.state());
    let expected = sent.lock().unwrap().iter().map(Vec::len).sum::<usize>() as f64 / 2.0;
    // The flush tail (~6 ms = ~200 bytes with C Sonic) is part of what the provider got before its audio ended.
    // Desktop: C Sonic ends ~5 ms (160 bytes) short (see `audio::time_stretch`), so 100 + 160 bytes of slack; still
    // less than what a missing tail would cost.
    let received = stt.received();
    let got = received.iter().map(Vec::len).sum::<usize>() as f64;
    assert!((expected - got).abs() <= 260.0, "expected {expected} bytes, got {got}");
    assert!(!received.iter().any(Vec::is_empty));
}

#[tokio::test(start_paused = true)]
async fn speed_200_without_speech_still_ends_with_no_speech_and_sends_nothing() {
    // Never reached: no STT wait.
    let stt = stt(FakeStt::after(10_000, "unused").rate(16_000));
    let h =
        Setup { audio: frames(), settings: speed(200), vad_model: Some(silence()), ..Setup::new(stt.clone()) }.build();

    h.dictate_for(1_000, 33).await;

    assert_eq!(error("No speech detected"), h.state());
    assert!(stt.received().is_empty());
}

/// A detector that collects the texts it is asked about and answers `language`.
fn detector(detections: &Shared<Vec<String>>, language: Option<&'static str>) -> Detector {
    let detections = detections.clone();
    Box::new(move |text| {
        detections.lock().unwrap().push(text);
        Box::pin(async move { language.map(str::to_string) })
    })
}

#[tokio::test(start_paused = true)]
async fn fillers_are_removed_before_the_final_transcript_hook() {
    let h = Setup::new(stt(FakeStt::replying("Uh, so I think, hmm, it works"))).build();
    h.dictate().await;

    assert_eq!(vec!["So I think, it works"], h.finals());
    assert_eq!(done("So I think, it works"), h.state());
}

#[tokio::test(start_paused = true)]
async fn only_fillers_end_with_no_speech_and_record_nothing() {
    let h = Setup::new(stt(FakeStt::replying("Uh. Hmm."))).build();
    h.dictate().await;

    assert_eq!(error("No speech detected"), h.state());
    assert!(h.finals().is_empty());
}

#[tokio::test(start_paused = true)]
async fn filler_removal_off_passes_the_transcript_unchanged() {
    let detections = Shared::<Vec<String>>::default();
    let h = Setup {
        settings: Settings { remove_filler_words: false, ..open_ai() },
        detect_language: detector(&detections, Some("en")),
        ..Setup::new(stt(FakeStt::replying("Uh, um, hello")))
    }
    .build();
    h.dictate().await;

    assert_eq!(vec!["Uh, um, hello"], h.finals());
    assert!(detections.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn known_language_skips_the_detector() {
    let detections = Shared::<Vec<String>>::default();
    let h = Setup {
        settings: Settings { stt_language: "en".into(), ..open_ai() },
        detect_language: detector(&detections, None),
        ..Setup::new(stt(FakeStt::replying("um hello there")))
    }
    .build();
    h.dictate().await;

    assert_eq!(vec!["hello there"], h.finals());
    assert!(detections.lock().unwrap().is_empty());
}

#[tokio::test(start_paused = true)]
async fn unknown_language_asks_the_detector_with_the_raw_text() {
    let detections = Shared::<Vec<String>>::default();
    let english = Setup {
        detect_language: detector(&detections, Some("en")),
        ..Setup::new(stt(FakeStt::replying("um hello there")))
    }
    .build();
    english.dictate().await;
    let portuguese = Setup {
        detect_language: detector(&detections, Some("pt")),
        ..Setup::new(stt(FakeStt::replying("eu vi um carro")))
    }
    .build();
    portuguese.dictate().await;

    assert_eq!(vec!["hello there"], english.finals());
    assert_eq!(vec!["eu vi um carro"], portuguese.finals());
    assert_eq!(vec!["um hello there", "eu vi um carro"], *detections.lock().unwrap());
}

#[tokio::test(start_paused = true)]
async fn detector_is_skipped_without_a_gated_filler() {
    let detections = Shared::<Vec<String>>::default();
    let h =
        Setup { detect_language: detector(&detections, Some("en")), ..Setup::new(stt(FakeStt::replying("uh hello"))) }
            .build();
    h.dictate().await;

    assert_eq!(vec!["hello"], h.finals());
    assert!(detections.lock().unwrap().is_empty());
}

/// Desktop: the detection's timeout and a test sleep due at the same instant fire together, so the check before it
/// is 1 ms earlier than Android's (`advanceTimeBy` stops before the tasks due at its end).
#[tokio::test(start_paused = true)]
async fn slow_detector_times_out_to_unknown() {
    let detect_language: Detector = Box::new(|_| {
        Box::pin(async {
            sleep(Duration::from_millis(5_000)).await;
            Some("en".to_string())
        })
    });
    let h = Setup { detect_language, ..Setup::new(stt(FakeStt::replying("um hello"))) }.build();

    h.controller.start();
    run_current().await;
    h.controller.stop();
    advance(100 + DETECTION_MS - 1).await;
    assert!(matches!(h.state(), DictationState::Finalizing { .. }));

    advance(2).await;
    assert_eq!(done("um hello"), h.state());
    assert_eq!(vec!["um hello"], h.finals());
}

#[tokio::test(start_paused = true)]
async fn custom_words_are_corrected_after_filler_removal() {
    let stt = stt(FakeStt::replying("deep um gram works"));
    let h = Setup { settings: with_words(), ..Setup::new(stt.clone()) }.build();
    h.dictate().await;

    assert_eq!(vec!["Deepgram works"], h.finals());
    assert_eq!(Some(vec!["Deepgram".to_string()]), stt.seen().custom_words);
}

#[tokio::test(start_paused = true)]
async fn custom_words_off_leaves_the_text_and_sends_no_words() {
    let stt = stt(FakeStt::replying("deep gram works"));
    let settings = Settings { custom_words_enabled: false, ..with_words() };
    let h = Setup { settings, ..Setup::new(stt.clone()) }.build();
    h.dictate().await;

    assert_eq!(vec!["deep gram works"], h.finals());
    assert_eq!(Some(Vec::<String>::new()), stt.seen().custom_words);
}

#[tokio::test(start_paused = true)]
async fn correction_off_leaves_the_text_but_still_sends_the_words() {
    let stt = stt(FakeStt::replying("deep gram works"));
    let settings = Settings { correct_similar_words: false, ..with_words() };
    let h = Setup { settings, ..Setup::new(stt.clone()) }.build();
    h.dictate().await;

    assert_eq!(vec!["deep gram works"], h.finals());
    assert_eq!(Some(vec!["Deepgram".to_string()]), stt.seen().custom_words);
}
