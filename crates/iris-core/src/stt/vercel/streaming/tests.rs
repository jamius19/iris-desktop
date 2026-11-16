/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `VercelGatewaySpeechToTextTest.kt` against a local WebSocket server.

use std::collections::VecDeque;
use std::sync::Mutex;

use futures_util::SinkExt;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use tokio::net::{TcpListener, TcpStream};

use super::*;
use crate::net::test_support::WsServer;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const TIMEOUT_MS: u64 = 10_000;
const MODEL: &str = "openai/gpt-live-transcribe";
const START_TYPE: &str = "transcription-stream.start";
const AUDIO_DONE_TYPE: &str = "transcription-stream.audio-done";
const ERROR_PART: &str = r#"{"type":"error","error":{"name":"Error","message":"Boom"}}"#;

/// 50 ms each at 24 kHz, so two of them need no padding.
fn chunk_a() -> Vec<u8> {
    vec![1; 2_400]
}

fn chunk_b() -> Vec<u8> {
    vec![2; 2_400]
}

fn type_of(message: &str) -> String {
    let json: Value = serde_json::from_str(message).unwrap();
    json["type"].as_str().unwrap().to_string()
}

fn delta(text: &str) -> String {
    format!(r#"{{"type":"transcript-delta","delta":"{text}"}}"#)
}

fn partial(text: &str) -> String {
    format!(r#"{{"type":"transcript-partial","text":"{text}"}}"#)
}

fn final_(text: &str) -> String {
    format!(r#"{{"type":"transcript-final","text":"{text}"}}"#)
}

fn finish(text: &str) -> String {
    format!(r#"{{"type":"finish","text":"{text}","segments":[]}}"#)
}

/// A frame the server got: text frames by their JSON `type`, binary frames as is.
#[derive(Clone, Debug, PartialEq)]
enum Seen {
    Type(String),
    Binary(Vec<u8>),
}

fn ty(t: &str) -> Seen {
    Seen::Type(t.to_string())
}

fn bin(bytes: Vec<u8>) -> Seen {
    Seen::Binary(bytes)
}

/// What the server does on one connection.
#[derive(Clone)]
enum Script {
    /// Only records.
    Silent,
    /// Answers `audio-done` with these parts.
    Answering(Vec<String>),
    /// Answers every binary frame with this text.
    OnBinarySend(&'static str),
    /// Drops the connection on the first binary frame (no close frame).
    DropOnBinary,
    /// Closes with this code and reason on `audio-done`.
    CloseOnAudioDone(u16, &'static str),
}

#[derive(Default)]
struct Connection {
    /// Text frames raw, binary as is.
    texts: Vec<String>,
    seen: Vec<Seen>,
    /// The client closed or dropped the socket.
    closed: bool,
}

/// The server side: one script per connection (index 0, 1, …; later ones are silent).
struct Gateway {
    server: WsServer,
    connections: Arc<Mutex<Vec<Connection>>>,
}

impl Gateway {
    async fn start(scripts: Vec<Script>) -> Self {
        let connections: Arc<Mutex<Vec<Connection>>> = Arc::default();
        let (scripts, shared) = (Arc::new(scripts), connections.clone());
        let server = WsServer::start(move |recorded, mut socket| {
            let (scripts, connections) = (scripts.clone(), shared.clone());
            async move {
                let script = scripts.get(recorded.index).cloned().unwrap_or(Script::Silent);
                let with = |f: &mut dyn FnMut(&mut Connection)| {
                    let mut connections = connections.lock().unwrap();
                    if connections.len() <= recorded.index {
                        connections.resize_with(recorded.index + 1, Connection::default);
                    }
                    f(&mut connections[recorded.index]);
                };
                with(&mut |_| {});
                while let Some(Ok(message)) = socket.next().await {
                    match message {
                        Message::Text(text) => {
                            let text = text.to_string();
                            let kind = type_of(&text);
                            with(&mut |c| {
                                c.texts.push(text.clone());
                                c.seen.push(ty(&kind));
                            });
                            if kind != AUDIO_DONE_TYPE {
                                continue;
                            }
                            match &script {
                                Script::Answering(parts) => {
                                    for part in parts {
                                        let _ = socket.send(Message::text(part.clone())).await;
                                    }
                                }
                                Script::CloseOnAudioDone(code, reason) => {
                                    let frame = CloseFrame { code: (*code).into(), reason: (*reason).into() };
                                    let _ = socket.close(Some(frame)).await;
                                }
                                _ => {}
                            }
                        }
                        Message::Binary(bytes) => {
                            with(&mut |c| c.seen.push(bin(bytes.to_vec())));
                            match &script {
                                Script::OnBinarySend(text) => {
                                    let _ = socket.send(Message::text(*text)).await;
                                }
                                Script::DropOnBinary => break,
                                _ => {}
                            }
                        }
                        Message::Close(_) => break,
                        _ => {}
                    }
                }
                with(&mut |c| c.closed = true);
            }
        })
        .await;
        Gateway { server, connections }
    }

    fn url(&self) -> String {
        self.server.url("/v4/ai/transcription-model")
    }

    /// `host:port`.
    fn address(&self) -> String {
        self.server.url("").trim_start_matches("http://").to_string()
    }

    fn seen(&self, index: usize) -> Vec<Seen> {
        self.connections.lock().unwrap().get(index).map(|c| c.seen.clone()).unwrap_or_default()
    }

    fn texts(&self, index: usize) -> Vec<String> {
        self.connections.lock().unwrap().get(index).map(|c| c.texts.clone()).unwrap_or_default()
    }

    fn closed(&self, index: usize) -> bool {
        self.connections.lock().unwrap().get(index).is_some_and(|c| c.closed)
    }

    fn request_count(&self) -> usize {
        self.server.requests().len()
    }
}

/// A TCP front whose connections `drops` picks (by index) are closed once the handshake request arrives, before any
/// response (Android's `SocketEffect.CloseSocket`); the others are forwarded to `backend` (`host:port`).
struct Front {
    port: u16,
    count: Arc<AtomicUsize>,
    task: JoinHandle<()>,
}

impl Front {
    async fn start(backend: Option<String>, drops: fn(usize) -> bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let count = Arc::new(AtomicUsize::new(0));
        let task_count = count.clone();
        let task = tokio::spawn(async move {
            while let Ok((mut client, _)) = listener.accept().await {
                let index = task_count.fetch_add(1, Ordering::SeqCst);
                let backend = backend.clone().filter(|_| !drops(index));
                tokio::spawn(async move {
                    match backend {
                        Some(backend) => {
                            let mut upstream = TcpStream::connect(backend).await.unwrap();
                            let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
                        }
                        None => {
                            let mut request = [0u8; 4096];
                            let _ = client.read(&mut request).await;
                        }
                    }
                });
            }
        });
        Front { port, count, task }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}/v4/ai/transcription-model", self.port)
    }

    fn count(&self) -> usize {
        self.count.load(Ordering::SeqCst)
    }
}

impl Drop for Front {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One step of a scripted audio source.
enum Step {
    Chunk(Vec<u8>),
    /// Waits until the condition holds.
    Until(Box<dyn Fn() -> bool + Send>),
    Fail(&'static str),
    /// Never ends (Kotlin's `awaitCancellation`).
    Endless,
}

/// Audio that runs its steps as the provider pulls (as a Kotlin flow runs while collected), then ends.
struct Audio(VecDeque<Step>);

impl AudioSource for Audio {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        Box::pin(async move {
            while let Some(step) = self.0.pop_front() {
                match step {
                    Step::Chunk(chunk) => return Ok(Some(chunk)),
                    Step::Until(condition) => {
                        while !condition() {
                            sleep(Duration::from_millis(10)).await;
                        }
                    }
                    Step::Fail(message) => return Err(SttError(message.into())),
                    Step::Endless => std::future::pending::<()>().await,
                }
            }
            Ok(None)
        })
    }
}

fn audio(steps: Vec<Step>) -> Audio {
    Audio(steps.into())
}

fn chunks(chunks: Vec<Vec<u8>>) -> Audio {
    audio(chunks.into_iter().map(Step::Chunk).collect())
}

fn endless() -> Audio {
    audio(vec![Step::Endless])
}

fn stt_with(url: &str, api_key: Option<&str>, reconnect_timeout: Duration) -> VercelGatewaySpeechToText {
    let key = api_key.map(str::to_string);
    let api_key: KeySource = Arc::new(move || Ok(key.clone()));
    VercelGatewaySpeechToText::with_urls(api_key, url, reconnect_timeout, Duration::from_millis(50))
}

fn stt(url: &str) -> VercelGatewaySpeechToText {
    stt_with(url, Some("vck-test"), Duration::from_millis(5_000))
}

type Partials = Arc<Mutex<Vec<String>>>;

async fn transcribe(
    stt: &VercelGatewaySpeechToText,
    audio: Audio,
    model: &str,
    language: &str,
    timeout_ms: u64,
    partials: &Partials,
) -> Result<String, SttError> {
    let request = SttRequest {
        model: model.into(),
        language: language.into(),
        timeout: Duration::from_millis(timeout_ms),
        custom_words: Vec::new(),
    };
    let partials = partials.clone();
    let on_partial: OnPartial = Box::new(move |text| partials.lock().unwrap().push(text));
    timeout(TEST_TIMEOUT, stt.transcribe(Box::new(audio), request, on_partial)).await.expect("test timed out")
}

async fn run(stt: &VercelGatewaySpeechToText, audio: Audio) -> Result<String, SttError> {
    transcribe(stt, audio, MODEL, "", TIMEOUT_MS, &Partials::default()).await
}

/// Waits (bounded) until `condition` holds.
async fn eventually(condition: impl Fn() -> bool) -> bool {
    timeout(Duration::from_secs(5), async {
        while !condition() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .is_ok()
}

#[tokio::test]
async fn streams_audio_and_returns_the_finish_text() {
    let gateway =
        Gateway::start(vec![Script::Answering(vec![delta("Hello"), delta(" world"), finish("Hello, world.")])]).await;
    let partials = Partials::default();

    let text =
        transcribe(&stt(&gateway.url()), chunks(vec![chunk_a(), chunk_b()]), MODEL, "en", TIMEOUT_MS, &partials).await;

    assert_eq!(Ok("Hello, world.".to_string()), text);
    assert_eq!(vec!["Hello", "Hello world"], *partials.lock().unwrap());
    let request = &gateway.server.requests()[0];
    assert_eq!("/v4/ai/transcription-model?ai-model-id=openai%2Fgpt-live-transcribe", request.uri);
    assert_eq!(vec![MODEL.to_string()], request.query("ai-model-id"));
    assert_eq!(Some("Bearer vck-test".into()), request.header("Authorization"));
    assert_eq!(Some("ai-gateway-transcription.v1".into()), request.header("Sec-WebSocket-Protocol"));
    assert_eq!(Some("0.0.1".into()), request.header("ai-gateway-protocol-version"));
    assert_eq!(Some("api-key".into()), request.header("ai-gateway-auth-method"));
    assert_eq!(Some("4".into()), request.header("ai-transcription-model-specification-version"));
    assert_eq!(Some(MODEL.into()), request.header("ai-model-id"));
    let start: Value = serde_json::from_str(&gateway.texts(0)[0]).unwrap();
    let expected: Value = serde_json::from_str(
        r#"{"type":"transcription-stream.start","inputAudioFormat":{"type":"audio/pcm","rate":24000},
            "providerOptions":{"openai":{"language":"en"}}}"#,
    )
    .unwrap();
    assert_eq!(expected, start);
    assert!(eventually(|| gateway.closed(0)).await);
    assert_eq!(vec![ty(START_TYPE), bin(chunk_a()), bin(chunk_b()), ty(AUDIO_DONE_TYPE)], gateway.seen(0));
}

#[tokio::test]
async fn short_audio_is_padded_with_silence_to_one_hundred_ms_at_the_models_rate() {
    let gateway = Gateway::start(vec![Script::Answering(vec![finish("")])]).await;

    let partials = Partials::default();
    let model = "google/gemini-3.5-transcribe-live";
    transcribe(&stt(&gateway.url()), chunks(vec![vec![1, 2]]), model, "", TIMEOUT_MS, &partials).await.unwrap();

    let audio: Vec<Vec<u8>> = gateway
        .seen(0)
        .into_iter()
        .filter_map(|seen| match seen {
            Seen::Binary(bytes) => Some(bytes),
            Seen::Type(_) => None,
        })
        .collect();
    assert_eq!(vec![2, 16_000 * 2 / 10 - 2], audio.iter().map(Vec::len).collect::<Vec<_>>());
    assert!(audio[1].iter().all(|b| *b == 0));
    let start: Value = serde_json::from_str(&gateway.texts(0)[0]).unwrap();
    assert_eq!(16_000, start["inputAudioFormat"]["rate"]);
}

#[tokio::test]
async fn assembles_live_text_and_falls_back_to_it_for_a_blank_finish() {
    let gateway = Gateway::start(vec![Script::Answering(vec![
        r#"{"type":"stream-start","warnings":[{"type":"unsupported","feature":"language"}]}"#.into(),
        partial("one two three"),
        partial("one two"), // Only cuts words off: not shown.
        partial("one two three four"),
        r#"{"type":"response-metadata","modelId":"x"}"#.into(),
        final_("One two three four."),
        finish(" "),
    ])])
    .await;
    let partials = Partials::default();

    let text =
        transcribe(&stt(&gateway.url()), chunks(vec![chunk_a(), chunk_b()]), MODEL, "", TIMEOUT_MS, &partials).await;

    assert_eq!(Ok("One two three four.".to_string()), text);
    assert_eq!(vec!["one two three", "one two three four", "One two three four."], *partials.lock().unwrap());
}

#[tokio::test]
async fn error_part_while_listening_reconnects_and_resends_the_audio() {
    let gateway =
        Arc::new(Gateway::start(vec![Script::OnBinarySend(ERROR_PART), Script::Answering(vec![finish("Hi.")])]).await);
    let reconnected = gateway.clone();
    let audio = audio(vec![
        Step::Chunk(chunk_a()),
        Step::Until(Box::new(move || reconnected.seen(1).first() == Some(&ty(START_TYPE)))),
        Step::Chunk(chunk_b()),
    ]);

    let text = run(&stt(&gateway.url()), audio).await;

    assert_eq!(Ok("Hi.".to_string()), text);
    assert_eq!(2, gateway.request_count());
    assert!(eventually(|| gateway.closed(1)).await);
    assert_eq!(vec![ty(START_TYPE), bin(chunk_a()), bin(chunk_b()), ty(AUDIO_DONE_TYPE)], gateway.seen(1));
}

#[tokio::test]
async fn close_without_finish_after_stop_reconnects_and_resends_everything() {
    let gateway =
        Gateway::start(vec![Script::CloseOnAudioDone(1011, "Upstream gone"), Script::Answering(vec![finish("Hi.")])])
            .await;

    let text = run(&stt(&gateway.url()), chunks(vec![chunk_a(), chunk_b()])).await;

    assert_eq!(Ok("Hi.".to_string()), text);
    assert!(eventually(|| gateway.closed(1)).await);
    assert_eq!(vec![ty(START_TYPE), bin(chunk_a()), bin(chunk_b()), ty(AUDIO_DONE_TYPE)], gateway.seen(1));
}

#[tokio::test]
async fn second_failure_fails_the_session() {
    let gateway =
        Gateway::start(vec![Script::Answering(vec![ERROR_PART.into()]), Script::Answering(vec![ERROR_PART.into()])])
            .await;

    let error = run(&stt(&gateway.url()), chunks(vec![chunk_a()])).await;

    assert_eq!(Err(SttError("AI Gateway error: Boom".into())), error);
    assert_eq!(2, gateway.request_count());
}

#[tokio::test]
async fn reconnect_handshake_without_a_response_is_tried_again() {
    // Connection 0 drops after opening (no HTTP response), 1 gets no handshake response, 2 (the server's 1) works.
    let gateway = Gateway::start(vec![Script::DropOnBinary, Script::Answering(vec![finish("Hi.")])]).await;
    let front = Front::start(Some(gateway.address()), |index| index == 1).await;

    let text = run(&stt(&front.url()), chunks(vec![chunk_a(), chunk_b()])).await;

    assert_eq!(Ok("Hi.".to_string()), text);
    assert_eq!(3, front.count());
    assert!(eventually(|| gateway.closed(1)).await);
    assert_eq!(vec![ty(START_TYPE), bin(chunk_a()), bin(chunk_b()), ty(AUDIO_DONE_TYPE)], gateway.seen(1));
}

#[tokio::test]
async fn handshakes_that_keep_failing_without_a_response_end_at_the_reconnect_deadline() {
    let front = Front::start(None, |_| true).await;
    let started_at = Instant::now();

    let error = run(&stt_with(&front.url(), Some("vck-test"), Duration::from_millis(500)), endless()).await;

    assert_eq!(Err(SttError("Reconnecting to AI Gateway timed out".into())), error);
    assert!(front.count() >= 3); // The first try, the reconnect and at least one more handshake.
    assert!(started_at.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn stall_after_the_audio_reconnects_and_resends_everything() {
    let gateway = Gateway::start(vec![Script::Silent, Script::Answering(vec![finish("Hi.")])]).await;

    let partials = Partials::default();
    let text = transcribe(&stt(&gateway.url()), chunks(vec![chunk_a(), chunk_b()]), MODEL, "", 200, &partials).await;

    assert_eq!(Ok("Hi.".to_string()), text);
    assert!(eventually(|| gateway.closed(1)).await);
    assert_eq!(vec![ty(START_TYPE), bin(chunk_a()), bin(chunk_b()), ty(AUDIO_DONE_TYPE)], gateway.seen(1));
}

#[tokio::test]
async fn rejected_key_is_not_retried() {
    let gateway = Gateway::start(vec![]).await;
    gateway.server.reject_next(401, r#"{"error":{"message":"Invalid key"}}"#);

    let error = run(&stt(&gateway.url()), endless()).await;

    assert_eq!(Err(SttError("AI Gateway rejected the API key. Check it in Iris settings.".into())), error);
    assert_eq!(1, gateway.request_count());
}

#[tokio::test]
async fn unknown_model_shows_the_gateway_message_and_is_not_retried() {
    let gateway = Gateway::start(vec![]).await;
    gateway.server.reject_next(404, r#"{"error":{"message":"Model 'x/y' not found"}}"#);

    let error = transcribe(&stt(&gateway.url()), endless(), "x/y", "", TIMEOUT_MS, &Partials::default()).await;

    assert_eq!(Err(SttError("AI Gateway error: Model 'x/y' not found".into())), error);
    assert_eq!(1, gateway.request_count());
}

#[tokio::test]
async fn times_out_when_the_server_stalls_again_after_the_reconnect() {
    let gateway = Gateway::start(vec![Script::Silent, Script::Silent]).await;

    let error = transcribe(&stt(&gateway.url()), chunks(vec![chunk_a()]), MODEL, "", 200, &Partials::default()).await;

    assert_eq!(Err(SttError("Timed out waiting for the transcript".into())), error);
    assert_eq!(2, gateway.request_count());
}

#[tokio::test]
async fn cancelling_closes_the_socket() {
    let gateway = Arc::new(Gateway::start(vec![Script::Silent]).await);
    let url = gateway.url();
    let session = tokio::spawn(async move { run(&stt(&url), endless()).await });
    assert!(eventually(|| !gateway.seen(0).is_empty()).await); // Connected: the server got the start frame.

    session.abort();

    assert!(session.await.unwrap_err().is_cancelled());
    assert!(eventually(|| gateway.closed(0)).await);
}

#[tokio::test]
async fn audio_failure_is_rethrown_without_padding_or_audio_done() {
    let gateway = Arc::new(Gateway::start(vec![Script::Silent]).await);
    let received = gateway.clone();
    // The controller's audio ends like this when VAD let nothing through.
    let audio = audio(vec![
        Step::Chunk(chunk_a()), // Under 100 ms: a normal end would pad it.
        Step::Until(Box::new(move || received.seen(0).len() >= 2)), // The server got the start frame and the chunk.
        Step::Fail("No speech detected"),
    ]);

    let error = run(&stt(&gateway.url()), audio).await;

    assert_eq!(Err(SttError("No speech detected".into())), error);
    assert!(eventually(|| gateway.closed(0)).await);
    assert_eq!(vec![ty(START_TYPE), bin(chunk_a())], gateway.seen(0));
    assert_eq!(1, gateway.request_count());
}

#[tokio::test]
async fn missing_api_key_fails_without_connecting() {
    let gateway = Gateway::start(vec![]).await;

    let error = run(&stt_with(&gateway.url(), Some(" "), Duration::from_millis(5_000)), endless()).await;

    assert_eq!(Err(SttError("Add your Vercel AI Gateway API key in Iris settings".into())), error);
    assert_eq!(0, gateway.request_count());
}

#[tokio::test]
async fn model_id_that_cant_be_a_header_fails_without_connecting() {
    let gateway = Gateway::start(vec![]).await;

    let error = transcribe(&stt(&gateway.url()), endless(), "openai/gpt-ü", "", TIMEOUT_MS, &Partials::default()).await;

    assert_eq!(Err(SttError("Invalid model id: openai/gpt-ü".into())), error);
    assert_eq!(0, gateway.request_count());
}
