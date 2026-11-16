/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `OnDeviceSpeechToTextTest.kt` on tokio's paused clock; Moonshine (not on desktop) is Parakeet TDT v3,
//! another per-phrase model.

use super::*;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tokio::time::sleep;

const PER_PHRASE: &str = "parakeet-tdt-v3";
const NEMOTRON: &str = "nemotron-streaming-en";

type Shared<T> = Arc<Mutex<T>>;

/// Runs every task that is ready without moving the clock (Kotlin's `runCurrent`).
async fn run_current() {
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
}

/// One step of a scripted audio flow.
enum Step {
    Chunk(Vec<u8>),
    Sleep(u64),
    /// Lets the other tasks run (Kotlin's `yield`).
    Yield,
    Fail(&'static str),
    Check(Box<dyn FnOnce() + Send>),
}

/// Audio that runs its steps as the provider pulls (as a Kotlin flow runs while collected), then ends.
struct Script(VecDeque<Step>);

impl AudioSource for Script {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        Box::pin(async move {
            while let Some(step) = self.0.pop_front() {
                match step {
                    Step::Chunk(chunk) => return Ok(Some(chunk)),
                    Step::Sleep(ms) => sleep(Duration::from_millis(ms)).await,
                    Step::Yield => run_current().await,
                    Step::Fail(message) => return Err(SttError(message.into())),
                    Step::Check(check) => check(),
                }
            }
            Ok(None)
        })
    }
}

/// Audio of just `chunks`.
fn chunks(chunks: Vec<Vec<u8>>) -> Vec<Step> {
    chunks.into_iter().map(Step::Chunk).collect()
}

/// `n` samples of `value`.
fn chunk_of(n: usize, value: i16) -> Vec<u8> {
    (0..n).flat_map(|_| value.to_le_bytes()).collect()
}

/// `n` samples of 1,000.
fn chunk(n: usize) -> Vec<u8> {
    chunk_of(n, 1_000)
}

/// A segment end.
fn end() -> Vec<u8> {
    Vec::new()
}

/// The decode's reply to a segment's samples.
type Reply = Arc<dyn Fn(Vec<f32>) -> BoxFuture<'static, Result<String, SttError>> + Send + Sync>;

/// Replies with `texts` in order.
fn replies(texts: &[&'static str]) -> Reply {
    let texts = texts.to_vec();
    let i = AtomicUsize::new(0);
    Arc::new(move |_| {
        let text = texts[i.fetch_add(1, Ordering::SeqCst)].to_string();
        Box::pin(async move { Ok(text) })
    })
}

/// Never replies (Kotlin's `awaitCancellation`).
fn hung() -> Reply {
    Arc::new(|_| Box::pin(std::future::pending()))
}

fn failing(message: &'static str) -> Reply {
    Arc::new(move |_| Box::pin(async move { Err(SttError(message.into())) }))
}

type Results = UnboundedSender<Result<String, SttError>>;

/// A streaming model's dictation: takes the chunks and sends the texts. Dropped once the provider drops the results.
type StreamBody = Arc<dyn Fn(UnboundedReceiver<Vec<u8>>, Results) -> BoxFuture<'static, ()> + Send + Sync>;

fn stream_of<F>(body: impl Fn(UnboundedReceiver<Vec<u8>>, Results) -> F + Send + Sync + 'static) -> StreamBody
where
    F: Future<Output = ()> + Send + 'static,
{
    Arc::new(move |chunks, results| Box::pin(body(chunks, results)))
}

/// Sends `per_chunk` in order, one per chunk taken (its size collected in `taken`), then `last`.
fn stream_texts(per_chunk: &[&'static str], last: &'static str, taken: Shared<Vec<usize>>) -> StreamBody {
    let per_chunk = per_chunk.to_vec();
    stream_of(move |mut chunks, results| {
        let (mut texts, taken) = (per_chunk.clone().into_iter(), taken.clone());
        async move {
            while let Some(chunk) = chunks.recv().await {
                taken.lock().unwrap().push(chunk.len());
                let _ = results.send(Ok(texts.next().unwrap().to_string()));
            }
            let _ = results.send(Ok(last.to_string()));
        }
    })
}

/// Sets its flag when dropped (Kotlin's `finally`).
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Takes one chunk, then hangs until abandoned, which sets `cancelled`.
fn hung_stream(cancelled: &Arc<AtomicBool>) -> StreamBody {
    let cancelled = cancelled.clone();
    stream_of(move |mut chunks, _results| {
        let guard = SetOnDrop(cancelled.clone());
        async move {
            let _guard = guard;
            chunks.recv().await;
            std::future::pending::<()>().await;
        }
    })
}

/// A provider's options (Kotlin's `provider(...)` arguments); `..Options::default()` for the rest.
struct Options {
    downloaded: bool,
    max_segment_bytes: usize,
    stream: StreamBody,
    decode: Reply,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            downloaded: true,
            max_segment_bytes: MAX_SEGMENT_BYTES,
            stream: Arc::new(|_, _| panic!("Not a streaming model")),
            decode: replies(&[]),
        }
    }
}

/// What the providers saw and reported.
#[derive(Default)]
struct Seen {
    decoded: Shared<Vec<Vec<f32>>>,
    languages: Shared<Vec<String>>,
    partials: Shared<Vec<String>>,
    streams: Arc<AtomicUsize>,
    cancels: Arc<AtomicUsize>,
}

impl Seen {
    fn provider(&self, options: Options) -> OnDeviceSpeechToText {
        let (decoded, languages) = (self.decoded.clone(), self.languages.clone());
        let preloads = self.languages.clone();
        let streams = self.streams.clone();
        let Options { downloaded, max_segment_bytes, stream, decode: reply } = options;
        OnDeviceSpeechToText {
            is_downloaded: Box::new(move |_| downloaded),
            preload: Box::new(move |_, language| preloads.lock().unwrap().push(format!("preload:{language}"))),
            decode: Arc::new(move |_, language, samples| {
                decoded.lock().unwrap().push(samples.clone());
                languages.lock().unwrap().push(language);
                reply(samples)
            }),
            stream: Box::new(move |_, chunks| {
                streams.fetch_add(1, Ordering::SeqCst);
                let (results, taken) = mpsc::unbounded_channel();
                let abandoned = results.clone();
                let body = stream(chunks, results);
                tokio::spawn(async move {
                    tokio::select! {
                        _ = body => {}
                        _ = abandoned.closed() => {}
                    }
                });
                taken
            }),
            selection: Box::new(|| (PER_PHRASE.to_string(), String::new())),
            cancel: {
                let cancels = self.cancels.clone();
                Box::new(move || {
                    cancels.fetch_add(1, Ordering::SeqCst);
                })
            },
            max_segment_bytes,
        }
    }

    fn on_partial(&self) -> OnPartial {
        let partials = self.partials.clone();
        Box::new(move |text| partials.lock().unwrap().push(text))
    }

    async fn run(&self, stt: &OnDeviceSpeechToText, audio: Vec<Step>) -> Result<String, SttError> {
        self.run_with(stt, audio, PER_PHRASE, "").await
    }

    async fn run_with(
        &self,
        stt: &OnDeviceSpeechToText,
        audio: Vec<Step>,
        model: &str,
        language: &str,
    ) -> Result<String, SttError> {
        stt.transcribe(Box::new(Script(audio.into())), request(model, language), self.on_partial()).await
    }

    /// Runs `audio` through `stt` in a task of its own (Kotlin's `launch`), to cancel it.
    fn launch(&self, stt: OnDeviceSpeechToText, audio: Vec<Step>, model: &str) -> JoinHandle<()> {
        let (request, on_partial) = (request(model, ""), self.on_partial());
        tokio::spawn(async move {
            let _ = stt.transcribe(Box::new(Script(audio.into())), request, on_partial).await;
        })
    }

    fn decoded_sizes(&self) -> Vec<usize> {
        self.decoded.lock().unwrap().iter().map(Vec::len).collect()
    }

    fn languages(&self) -> Vec<String> {
        self.languages.lock().unwrap().clone()
    }

    fn partials(&self) -> Vec<String> {
        self.partials.lock().unwrap().clone()
    }
}

fn request(model: &str, language: &str) -> SttRequest {
    SttRequest {
        model: model.into(),
        language: language.into(),
        timeout: Duration::from_millis(1),
        custom_words: Vec::new(),
    }
}

fn err(message: &str) -> Result<String, SttError> {
    Err(SttError(message.into()))
}

#[tokio::test(start_paused = true)]
async fn model_not_downloaded_fails_before_the_audio_is_collected() {
    let seen = Seen::default();
    let collected = Arc::new(AtomicBool::new(false));
    let flag = collected.clone();
    let stt = seen.provider(Options { downloaded: false, decode: replies(&[]), ..Options::default() });

    let result = seen.run(&stt, vec![Step::Check(Box::new(move || flag.store(true, Ordering::SeqCst)))]).await;

    assert_eq!(err("Download the Parakeet TDT v3 model in Iris settings"), result);
    assert!(!collected.load(Ordering::SeqCst));
    assert!(seen.languages().is_empty());
}

#[tokio::test(start_paused = true)]
async fn segment_ends_are_decoded_in_order_while_recording() {
    let seen = Seen::default();
    let stt = seen.provider(Options { decode: replies(&["Hello there.", "How are you?"]), ..Options::default() });
    let partials = seen.partials.clone();
    let audio = vec![
        Step::Chunk(chunk(100)),
        Step::Chunk(chunk(50)),
        Step::Chunk(end()),
        Step::Sleep(100),
        // The first segment was decoded while recording.
        Step::Check(Box::new(move || assert_eq!(vec!["Hello there."], *partials.lock().unwrap()))),
        Step::Chunk(chunk(30)),
        Step::Chunk(end()),
        Step::Chunk(end()), // An empty segment: nothing to decode.
    ];

    assert_eq!(Ok("Hello there. How are you?".to_string()), seen.run(&stt, audio).await);
    assert_eq!(vec![150, 30], seen.decoded_sizes());
    assert_eq!(vec!["Hello there.", "Hello there. How are you?"], seen.partials());
}

#[tokio::test(start_paused = true)]
async fn cjk_segments_join_unspaced_and_blank_ones_are_skipped() {
    let seen = Seen::default();
    let stt = seen.provider(Options { decode: replies(&["你好", "  ", "世界"]), ..Options::default() });
    let audio = chunks(vec![chunk(10), end(), chunk(10), end(), chunk(10)]);

    assert_eq!(Ok("你好世界".to_string()), seen.run(&stt, audio).await);
    assert_eq!(3, seen.decoded_sizes().len());
    assert_eq!(vec!["你好", "你好世界"], seen.partials());
}

#[tokio::test(start_paused = true)]
async fn without_segment_ends_the_audio_is_decoded_once_after_it_ends() {
    let seen = Seen::default();
    let stt = seen.provider(Options { decode: replies(&["All of it."]), ..Options::default() });
    let decoded = seen.decoded.clone();
    let mut audio: Vec<Step> = (0..5).flat_map(|_| [Step::Chunk(chunk(100)), Step::Sleep(50)]).collect();
    audio.push(Step::Check(Box::new(move || assert!(decoded.lock().unwrap().is_empty()))));

    assert_eq!(Ok("All of it.".to_string()), seen.run(&stt, audio).await);
    assert_eq!(vec![500], seen.decoded_sizes());
}

#[tokio::test(start_paused = true)]
async fn a_segment_over_the_cap_is_cut_after_the_quietest_chunk() {
    let seen = Seen::default();
    let stt = seen.provider(Options { max_segment_bytes: 400, decode: replies(&["a", "b"]), ..Options::default() });
    // 50 samples (100 bytes) each: loud, loud, quiet, loud, loud.
    let audio = chunks(vec![chunk(50), chunk(50), chunk_of(50, 0), chunk(50), chunk(50)]);

    assert_eq!(Ok("a b".to_string()), seen.run(&stt, audio).await);
    assert_eq!(vec![150, 100], seen.decoded_sizes());
    assert!(seen.decoded.lock().unwrap()[0][100..].iter().all(|s| *s == 0.0));
}

#[tokio::test(start_paused = true)]
async fn decode_failure_propagates() {
    let seen = Seen::default();
    let stt = seen.provider(Options { decode: failing("On-device transcription failed"), ..Options::default() });

    assert_eq!(err("On-device transcription failed"), seen.run(&stt, chunks(vec![chunk(10)])).await);
}

#[tokio::test(start_paused = true)]
async fn a_hung_decode_times_out_after_the_deadline() {
    let seen = Seen::default();
    let stt = seen.provider(Options { decode: hung(), ..Options::default() });
    let start = Instant::now();

    let result = seen.run(&stt, chunks(vec![chunk(16_000)])).await; // 1 s of audio.

    assert_eq!(err("On-device transcription timed out"), result);
    // 60 s plus twice the pending audio.
    assert_eq!(Duration::from_secs(62), start.elapsed());
}

#[tokio::test(start_paused = true)]
async fn cancelling_returns_promptly_during_a_decode() {
    let seen = Seen::default();
    let start = Instant::now();
    let session = seen.launch(
        seen.provider(Options { decode: hung(), ..Options::default() }),
        chunks(vec![chunk(10)]),
        PER_PHRASE,
    );
    run_current().await;
    assert_eq!(1, seen.decoded_sizes().len());

    session.abort();
    run_current().await;
    assert!(session.is_finished());
    assert_eq!(Duration::ZERO, start.elapsed());
    // Desktop: the engine's decode in flight is stopped too.
    assert_eq!(1, seen.cancels.load(Ordering::SeqCst));
}

/// Desktop: only a per-phrase session that ends without its text cancels the engine's work.
#[tokio::test(start_paused = true)]
async fn the_engine_is_cancelled_only_when_a_session_ends_without_its_text() {
    let seen = Seen::default();
    let stt = seen.provider(Options { decode: replies(&["Hello"]), ..Options::default() });
    assert_eq!(Ok("Hello".to_string()), seen.run(&stt, chunks(vec![chunk(10)])).await);
    assert_eq!(0, seen.cancels.load(Ordering::SeqCst));

    let hung = seen.provider(Options { decode: hung(), ..Options::default() });
    assert_eq!(err("On-device transcription timed out"), seen.run(&hung, chunks(vec![chunk(10)])).await);
    assert_eq!(1, seen.cancels.load(Ordering::SeqCst));
}

#[tokio::test(start_paused = true)]
async fn only_whisper_gets_the_language_and_only_one_it_knows() {
    let seen = Seen::default();
    let stt =
        seen.provider(Options { decode: Arc::new(|_| Box::pin(async { Ok("x".to_string()) })), ..Options::default() });

    seen.run_with(&stt, chunks(vec![chunk(10)]), "whisper-small", " DE ").await.unwrap();
    seen.run_with(&stt, chunks(vec![chunk(10)]), "whisper-small", "yue").await.unwrap();
    seen.run_with(&stt, chunks(vec![chunk(10)]), "parakeet-tdt-v3", "de").await.unwrap();

    assert_eq!(vec!["preload:de", "de", "preload:", "", "preload:", ""], seen.languages());
}

#[tokio::test(start_paused = true)]
async fn warm_up_preloads_the_stored_model() {
    let seen = Seen::default();
    let stt = seen.provider(Options { decode: replies(&[]), ..Options::default() });

    stt.warm_up();

    assert_eq!(vec!["preload:"], seen.languages());
}

#[test]
fn pcm16_becomes_floats_across_chunks() {
    let floats = pcm16_to_floats(&[vec![0x00, 0x80, 0xFF, 0x7F], vec![0x00, 0x40]]);

    assert_eq!(vec![-1.0, 32767.0 / 32768.0, 0.5], floats);
}

#[tokio::test(start_paused = true)]
async fn a_streaming_model_reports_text_per_chunk_while_recording() {
    let seen = Seen::default();
    let stream = stream_texts(&["Hello", "Hello there"], "Hello there.", Shared::default());
    let stt = seen.provider(Options { stream, ..Options::default() });
    let partials = seen.partials.clone();
    let audio = vec![
        Step::Chunk(chunk(100)),
        Step::Chunk(chunk(100)),
        Step::Sleep(100),
        Step::Check(Box::new(move || assert_eq!(vec!["Hello", "Hello there"], *partials.lock().unwrap()))),
    ];

    assert_eq!(Ok("Hello there.".to_string()), seen.run_with(&stt, audio, NEMOTRON, "de").await);
    assert_eq!(vec!["Hello", "Hello there", "Hello there."], seen.partials());
    // No language for it.
    assert_eq!(vec!["preload:"], seen.languages());
}

#[tokio::test(start_paused = true)]
async fn a_streaming_model_skips_segment_ends_in_one_stream() {
    let seen = Seen::default();
    let taken = Shared::<Vec<usize>>::default();
    let stt = seen.provider(Options {
        stream: stream_texts(&["a", "a b", "a b c"], "a b c", taken.clone()),
        ..Options::default()
    });
    let audio = chunks(vec![chunk(100), end(), chunk(50), end(), end(), chunk(30)]);

    assert_eq!(Ok("a b c".to_string()), seen.run_with(&stt, audio, NEMOTRON, "").await);
    assert_eq!(vec![200, 100, 60], *taken.lock().unwrap());
    assert_eq!(1, seen.streams.load(Ordering::SeqCst));
    assert!(seen.decoded_sizes().is_empty());
}

#[tokio::test(start_paused = true)]
async fn a_streaming_model_reports_only_changed_text() {
    let seen = Seen::default();
    let stt = seen.provider(Options {
        stream: stream_texts(&["a", "a", " a ", "a b"], "a b", Shared::default()),
        ..Options::default()
    });
    let audio = chunks(vec![chunk(10), chunk(10), chunk(10), chunk(10)]);

    assert_eq!(Ok("a b".to_string()), seen.run_with(&stt, audio, NEMOTRON, "").await);
    assert_eq!(vec!["a", "a b"], seen.partials());
}

#[tokio::test(start_paused = true)]
async fn a_streaming_model_with_only_blank_text_returns_blank() {
    let seen = Seen::default();
    let stt =
        seen.provider(Options { stream: stream_texts(&[" ", ""], "  ", Shared::default()), ..Options::default() });

    assert_eq!(Ok(String::new()), seen.run_with(&stt, chunks(vec![chunk(10), chunk(10)]), NEMOTRON, "").await);
    assert!(seen.partials().is_empty());
}

#[tokio::test(start_paused = true)]
async fn streaming_load_failure_propagates() {
    let seen = Seen::default();
    let stream = stream_of(|_, results| async move {
        let _ = results.send(err("Couldn't load the on-device model"));
    });
    let stt = seen.provider(Options { stream, ..Options::default() });

    let result = seen.run_with(&stt, chunks(vec![chunk(10)]), NEMOTRON, "").await;

    assert_eq!(err("Couldn't load the on-device model"), result);
}

#[tokio::test(start_paused = true)]
async fn streaming_decode_failure_propagates() {
    let seen = Seen::default();
    let stream = stream_of(|mut chunks, results| async move {
        chunks.recv().await;
        let _ = results.send(err("On-device transcription failed"));
    });
    let stt = seen.provider(Options { stream, ..Options::default() });

    let result = seen.run_with(&stt, chunks(vec![chunk(10)]), NEMOTRON, "").await;

    assert_eq!(err("On-device transcription failed"), result);
}

#[tokio::test(start_paused = true)]
async fn a_hung_stream_times_out_after_the_deadline() {
    let seen = Seen::default();
    let stt = seen.provider(Options { stream: hung_stream(&Arc::default()), ..Options::default() });
    let start = Instant::now();

    let result = seen.run_with(&stt, chunks(vec![chunk(16_000)]), NEMOTRON, "").await; // 1 s of audio.

    assert_eq!(err("On-device transcription timed out"), result);
    // 60 s plus twice the pending audio.
    assert_eq!(Duration::from_secs(62), start.elapsed());
}

#[tokio::test(start_paused = true)]
async fn the_streaming_deadline_counts_only_undecoded_audio() {
    let seen = Seen::default();
    let stream = stream_of(|mut chunks, results| async move {
        chunks.recv().await;
        let _ = results.send(Ok("a".to_string()));
        std::future::pending::<()>().await;
    });
    let stt = seen.provider(Options { stream, ..Options::default() });
    let start = Instant::now();
    // The first second is decoded before the stop.
    let audio = vec![Step::Chunk(chunk(16_000)), Step::Yield, Step::Chunk(chunk(16_000))];

    let result = seen.run_with(&stt, audio, NEMOTRON, "").await;

    assert_eq!(err("On-device transcription timed out"), result);
    assert_eq!(Duration::from_secs(62), start.elapsed());
}

#[tokio::test(start_paused = true)]
async fn cancelling_cancels_the_stream() {
    let seen = Seen::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let stt = seen.provider(Options { stream: hung_stream(&cancelled), ..Options::default() });
    let session = seen.launch(stt, chunks(vec![chunk(10)]), NEMOTRON);
    run_current().await;
    assert!(!cancelled.load(Ordering::SeqCst));

    session.abort();
    run_current().await;
    assert!(session.is_finished());
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(Duration::ZERO, start.elapsed());
}

/// Desktop: the stream is abandoned by an abort, which lands once the tasks run (`run_current`), not before
/// transcribe returns as with Kotlin's structured concurrency.
#[tokio::test(start_paused = true)]
async fn no_speech_from_the_audio_cancels_the_stream() {
    let seen = Seen::default();
    let cancelled = Arc::new(AtomicBool::new(false));
    let start = Instant::now();
    let stt = seen.provider(Options { stream: hung_stream(&cancelled), ..Options::default() });
    // The stream has taken the chunk.
    let audio = vec![Step::Chunk(chunk(10)), Step::Yield, Step::Fail("No speech detected")];

    let result = seen.run_with(&stt, audio, NEMOTRON, "").await;
    run_current().await;

    assert_eq!(err("No speech detected"), result);
    assert!(cancelled.load(Ordering::SeqCst));
    assert_eq!(Duration::ZERO, start.elapsed());
}
