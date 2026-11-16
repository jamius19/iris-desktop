/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `SonioxSpeechToTextTest.kt` against a local WebSocket server.

use super::*;
use crate::net::test_support::WsServer;
use crate::stt::soniox::SonioxModel;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::net::TcpStream;
use tokio::sync::Notify;
use tokio::time::sleep;
use tokio_tungstenite::WebSocketStream;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const TIMEOUT: Duration = Duration::from_secs(10);
const FINISHED: &str = r#"{"tokens":[],"final_audio_proc_ms":1560,"total_audio_proc_ms":1680,"finished":true}"#;

/// What the server sends.
#[derive(Clone)]
enum Out {
    Text(String),
    Close(u16, &'static str),
}

fn text(text: &str) -> Out {
    Out::Text(text.into())
}

/// The server's replies: when the socket opens, after each audio frame, and after the end frame.
#[derive(Clone, Default)]
struct Upgrade {
    on_open: Vec<Out>,
    on_audio: Vec<Out>,
    on_end: Vec<Out>,
}

fn on_end(replies: Vec<Out>) -> Upgrade {
    Upgrade { on_end: replies, ..Default::default() }
}

struct Fixture {
    server: WsServer,
    /// Every frame the server got, in order: text frames as is, binary ones as "bin:<bytes>".
    frames: Arc<Mutex<Vec<String>>>,
    /// Notified when the server sees the client close or drop the socket.
    closed: Arc<Notify>,
    /// Models the provider looked up.
    lookups: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    async fn start(upgrade: Upgrade) -> Self {
        let frames: Arc<Mutex<Vec<String>>> = Arc::default();
        let closed: Arc<Notify> = Arc::default();
        let (handler_frames, handler_closed) = (frames.clone(), closed.clone());
        let server = WsServer::start(move |_request, socket| {
            serve(upgrade.clone(), handler_frames.clone(), handler_closed.clone(), socket)
        })
        .await;
        Fixture { server, frames, closed, lookups: Arc::default() }
    }

    fn frames(&self) -> Vec<String> {
        self.frames.lock().unwrap().clone()
    }

    /// The provider with `key` and a catalogue answering `entry` (after `delay`).
    fn stt(&self, key: Option<&str>, entry: Option<SonioxModel>, delay: Duration) -> SonioxSpeechToText {
        let key = key.map(str::to_string);
        let lookups = self.lookups.clone();
        SonioxSpeechToText::with_url(
            Arc::new(move || Ok(key.clone())),
            Arc::new(move |model| {
                lookups.lock().unwrap().push(model);
                let entry = entry.clone();
                Box::pin(async move {
                    sleep(delay).await;
                    entry
                })
            }),
            &self.server.url("/transcribe-websocket"),
        )
    }

    fn default_stt(&self) -> SonioxSpeechToText {
        self.stt(Some("sx-test"), None, Duration::ZERO)
    }

    /// Waits (up to 5 s) until the server got `n` frames.
    async fn await_frames(&self, n: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.frames.lock().unwrap().len() < n {
            assert!(Instant::now() < deadline, "server got {:?}", self.frames());
            sleep(Duration::from_millis(10)).await;
        }
    }

    async fn await_closed(&self) {
        tokio::time::timeout(Duration::from_secs(5), self.closed.notified()).await.expect("server saw the close");
    }
}

async fn serve(
    upgrade: Upgrade,
    frames: Arc<Mutex<Vec<String>>>,
    closed: Arc<Notify>,
    mut socket: WebSocketStream<TcpStream>,
) {
    play(&mut socket, &upgrade.on_open).await;
    while let Some(Ok(message)) = socket.next().await {
        match message {
            Message::Text(text) => {
                frames.lock().unwrap().push(text.to_string());
                if text.as_str() == END_OF_AUDIO {
                    play(&mut socket, &upgrade.on_end).await;
                }
            }
            Message::Binary(bytes) => {
                let bytes: Vec<String> = bytes.iter().map(u8::to_string).collect();
                frames.lock().unwrap().push(format!("bin:{}", bytes.join(",")));
                play(&mut socket, &upgrade.on_audio).await;
            }
            Message::Close(_) => closed.notify_one(),
            _ => {}
        }
    }
    closed.notify_one();
}

async fn play(socket: &mut WebSocketStream<TcpStream>, replies: &[Out]) {
    for reply in replies {
        let message = match reply {
            Out::Text(text) => Message::text(text.clone()),
            Out::Close(code, reason) => {
                Message::Close(Some(CloseFrame { code: (*code).into(), reason: (*reason).into() }))
            }
        };
        let _ = socket.send(message).await;
    }
}

/// One step of a scripted audio flow.
enum Step {
    Chunk(Vec<u8>),
    Wait(BoxFuture<'static, ()>),
    Fail(&'static str),
}

/// Audio that runs its steps as the provider pulls, then ends.
struct Script(VecDeque<Step>);

impl AudioSource for Script {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        Box::pin(async move {
            while let Some(step) = self.0.pop_front() {
                match step {
                    Step::Chunk(chunk) => return Ok(Some(chunk)),
                    Step::Wait(wait) => wait.await,
                    Step::Fail(message) => return Err(SttError(message.into())),
                }
            }
            Ok(None)
        })
    }
}

fn audio(chunks: &[&[u8]]) -> Box<Script> {
    Box::new(Script(chunks.iter().map(|chunk| Step::Chunk(chunk.to_vec())).collect()))
}

fn endless_audio() -> Box<Script> {
    Box::new(Script(VecDeque::from([Step::Wait(Box::pin(std::future::pending()))])))
}

fn request(language: &str, timeout: Duration, custom_words: &[&str]) -> SttRequest {
    SttRequest {
        model: "stt-rt-v6".into(),
        language: language.into(),
        timeout,
        custom_words: custom_words.iter().map(|word| word.to_string()).collect(),
    }
}

async fn run(
    stt: &SonioxSpeechToText,
    audio: Box<dyn AudioSource + '_>,
    request: SttRequest,
    on_partial: OnPartial,
) -> Result<String, SttError> {
    tokio::time::timeout(TEST_TIMEOUT, stt.transcribe(audio, request, on_partial)).await.expect("test timeout")
}

fn ignore() -> OnPartial {
    Box::new(|_| {})
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

fn start(hints: &[&str], strict: bool) -> Value {
    let hints: Vec<String> = hints.iter().map(|hint| hint.to_string()).collect();
    json(&soniox_start_message("stt-rt-v6", &hints, strict, &[]))
}

fn final_token(text: &str) -> String {
    format!(r#"{{"text":"{text}","start_ms":0,"end_ms":10,"confidence":0.9,"is_final":true}}"#)
}

fn partial(text: &str) -> String {
    format!(r#"{{"text":"{text}","confidence":0.5,"is_final":false}}"#)
}

fn tokens(tokens: &[String]) -> Out {
    Out::Text(format!(r#"{{"tokens":[{}],"final_audio_proc_ms":0,"total_audio_proc_ms":0}}"#, tokens.join(",")))
}

fn rt_model(strict: bool) -> SonioxModel {
    SonioxModel {
        id: "stt-rt-v6".into(),
        transcription_mode: Some("real_time".into()),
        supports_language_hints_strict: strict,
    }
}

#[tokio::test]
async fn sends_start_then_audio_then_end_and_returns_the_finals_when_finished() {
    let fixture = Fixture::start(on_end(vec![
        tokens(&[final_token("Hello"), final_token(" there."), partial(" How")]),
        tokens(&[final_token(" How are you?")]),
        text(FINISHED),
        Out::Close(1000, ""),
    ]))
    .await;

    let text = run(&fixture.default_stt(), audio(&[&[1, 2], &[3, 4]]), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Ok("Hello there. How are you?".into()), text);
    let upgrade = &fixture.server.requests()[0];
    assert_eq!(Some("Bearer sx-test".into()), upgrade.header("authorization"));
    assert_eq!("/transcribe-websocket", upgrade.uri);
    assert_eq!(None, upgrade.header("sec-websocket-protocol"));
    assert_eq!(0, upgrade.index);
    assert!(upgrade.query("api_key").is_empty()); // The key goes in the header only.
    let frames = fixture.frames();
    assert_eq!(start(&[], false), json(&frames[0]));
    assert_eq!(vec!["bin:1,2", "bin:3,4", ""], frames[1..]);
    assert!(fixture.lookups.lock().unwrap().is_empty()); // No hints: no catalogue lookup.
}

#[tokio::test]
async fn hints_are_sent_strict_when_the_model_supports_it() {
    let fixture = Fixture::start(on_end(vec![text(FINISHED)])).await;

    let stt = fixture.stt(Some("sx-test"), Some(rt_model(true)), Duration::ZERO);
    run(&stt, audio(&[&[1]]), request("en, BN", TIMEOUT, &[]), ignore()).await.unwrap();

    assert_eq!(start(&["en", "bn"], true), json(&fixture.frames()[0]));
    assert_eq!(vec!["stt-rt-v6"], *fixture.lookups.lock().unwrap());
}

#[tokio::test]
async fn custom_words_go_in_the_start_message() {
    let fixture = Fixture::start(on_end(vec![text(FINISHED)])).await;

    run(&fixture.default_stt(), audio(&[&[1]]), request("", TIMEOUT, &["Soniox"]), ignore()).await.unwrap();

    assert_eq!(json(r#"{"terms":["Soniox"]}"#), json(&fixture.frames()[0])["context"]);
}

#[tokio::test]
async fn strict_is_omitted_when_unsupported_or_unknown() {
    for entry in [None, Some(rt_model(false))] {
        let fixture = Fixture::start(on_end(vec![text(FINISHED)])).await;

        let stt = fixture.stt(Some("sx-test"), entry, Duration::ZERO);
        run(&stt, audio(&[&[1]]), request("en", TIMEOUT, &[]), ignore()).await.unwrap();

        assert_eq!(start(&["en"], false), json(&fixture.frames()[0]));
    }
}

#[tokio::test]
async fn slow_model_lookup_keeps_start_first_and_chunks_in_order() {
    let fixture = Fixture::start(on_end(vec![text(FINISHED)])).await;
    let stt = fixture.stt(Some("sx-test"), Some(rt_model(true)), Duration::from_millis(200));

    run(&stt, audio(&[&[1], &[2], &[3]]), request("en", TIMEOUT, &[]), ignore()).await.unwrap();

    let frames = fixture.frames();
    assert_eq!(start(&["en"], true), json(&frames[0]));
    assert_eq!(vec!["bin:1", "bin:2", "bin:3", ""], frames[1..]);
}

#[tokio::test]
async fn live_text_is_finals_plus_the_replaced_tail() {
    let fixture = Fixture::start(Upgrade {
        on_audio: vec![
            tokens(&[partial("Hel")]),
            tokens(&[final_token("Hello"), partial(" wor")]),
            tokens(&[partial(" world")]),
        ],
        on_end: vec![tokens(&[final_token(" world.")]), text(FINISHED)],
        ..Default::default()
    })
    .await;
    let partials: Arc<Mutex<Vec<String>>> = Arc::default();
    let (tail_seen, tail_seen_rx) = oneshot::channel::<()>();
    // Holds the end back until the live responses have been reported.
    let audio = Box::new(Script(VecDeque::from([
        Step::Chunk(vec![1, 2]),
        Step::Wait(Box::pin(async move {
            let _ = tail_seen_rx.await;
        })),
    ])));
    let (seen, tail_seen) = (partials.clone(), Mutex::new(Some(tail_seen)));
    let on_partial: OnPartial = Box::new(move |live| {
        if live == "Hello world"
            && let Some(tail_seen) = tail_seen.lock().unwrap().take()
        {
            let _ = tail_seen.send(());
        }
        seen.lock().unwrap().push(live);
    });

    let text = run(&fixture.default_stt(), audio, request("", TIMEOUT, &[]), on_partial).await;

    assert_eq!(Ok("Hello world.".into()), text);
    assert_eq!(vec!["Hel", "Hello wor", "Hello world", "Hello world.", "Hello world."], *partials.lock().unwrap());
}

#[tokio::test]
async fn error_frame_fails_with_its_message() {
    let fixture = Fixture::start(Upgrade {
        on_open: vec![
            text(r#"{"tokens":[],"error_code":401,"error_type":"unauthenticated","error_message":"Bad key."}"#),
            Out::Close(1000, ""),
        ],
        ..Default::default()
    })
    .await;

    let error = run(&fixture.default_stt(), endless_audio(), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("Soniox rejected the API key. Check it in Iris settings.".into())), error);
}

#[tokio::test]
async fn no_audio_received_is_blank() {
    let fixture = Fixture::start(on_end(vec![
        text(r#"{"tokens":[],"error_code":400,"error_type":"invalid_request","error_message":"No audio received."}"#),
        Out::Close(1000, ""),
    ]))
    .await;

    // Blank: the controller shows "No speech detected".
    assert_eq!(Ok(String::new()), run(&fixture.default_stt(), audio(&[]), request("", TIMEOUT, &[]), ignore()).await);
}

#[tokio::test]
async fn close_before_finished_fails() {
    let fixture = Fixture::start(on_end(vec![tokens(&[final_token("Half")]), Out::Close(1001, "Going away")])).await;

    let error = run(&fixture.default_stt(), audio(&[&[1]]), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("Soniox closed the connection: Going away".into())), error);
}

#[tokio::test]
async fn rejected_handshake_maps() {
    let fixture = Fixture::start(Upgrade::default()).await;
    fixture.server.reject_next(503, r#"{"status_code":503,"message":"Overloaded"}"#);

    let error = run(&fixture.default_stt(), endless_audio(), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("Soniox server error (503)".into())), error);
}

#[tokio::test]
async fn times_out_waiting_for_finished() {
    let fixture = Fixture::start(Upgrade::default()).await;

    let error =
        run(&fixture.default_stt(), audio(&[&[1, 2]]), request("", Duration::from_millis(200), &[]), ignore()).await;

    assert_eq!(Err(SttError("Timed out waiting for the transcript".into())), error);
}

#[tokio::test]
async fn cancelling_closes_the_socket() {
    let fixture = Arc::new(Fixture::start(Upgrade::default()).await);
    let session = tokio::spawn({
        let fixture = fixture.clone();
        async move {
            let audio = Box::new(Script(VecDeque::from([
                Step::Chunk(vec![1, 2]),
                Step::Wait(Box::pin(std::future::pending())),
            ])));
            fixture.default_stt().transcribe(audio, request("", TIMEOUT, &[]), ignore()).await
        }
    });
    fixture.await_frames(2).await; // Connected: the server got the start and the audio.

    session.abort();
    let _ = session.await;

    fixture.await_closed().await;
    assert!(!fixture.frames().contains(&END_OF_AUDIO.to_string()));
}

#[tokio::test]
async fn audio_failure_is_rethrown_without_the_end() {
    let fixture = Arc::new(Fixture::start(Upgrade::default()).await);
    // The controller's audio ends like this when VAD let nothing through.
    let waiter = fixture.clone();
    let audio = Box::new(Script(VecDeque::from([
        Step::Chunk(vec![1, 2]),
        Step::Wait(Box::pin(async move { waiter.await_frames(2).await })),
        Step::Fail("No speech detected"),
    ])));

    let error = run(&fixture.default_stt(), audio, request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("No speech detected".into())), error);
    fixture.await_closed().await;
    assert!(!fixture.frames().contains(&END_OF_AUDIO.to_string()));
}

/// Audio that records whether it was pulled.
struct Untouched(Arc<AtomicBool>);

impl AudioSource for Untouched {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        self.0.store(true, Ordering::SeqCst);
        Box::pin(async { Ok(None) })
    }
}

#[tokio::test]
async fn missing_api_key_fails_without_connecting() {
    let fixture = Fixture::start(Upgrade::default()).await;
    let collected = Arc::new(AtomicBool::new(false));

    let stt = fixture.stt(None, None, Duration::ZERO);
    let error = run(&stt, Box::new(Untouched(collected.clone())), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("Add your Soniox API key in Iris settings".into())), error);
    assert!(!collected.load(Ordering::SeqCst));
    assert!(fixture.server.requests().is_empty());
}

#[tokio::test]
async fn empty_chunks_are_not_sent() {
    let fixture = Fixture::start(on_end(vec![text(FINISHED)])).await;

    run(&fixture.default_stt(), audio(&[&[], &[1, 2], &[]]), request("", TIMEOUT, &[]), ignore()).await.unwrap();

    assert_eq!(vec!["bin:1,2", ""], fixture.frames()[1..]);
}

#[tokio::test]
async fn empty_audio_still_sends_start_then_end() {
    let fixture = Fixture::start(on_end(vec![text(FINISHED)])).await;

    let text = run(&fixture.default_stt(), audio(&[]), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Ok(String::new()), text);
    let frames = fixture.frames();
    assert_eq!(start(&[], false), json(&frames[0]));
    assert_eq!(vec![""], frames[1..]);
}

#[test]
fn streams_at_16k_hz_as_recorded() {
    let stt = SonioxSpeechToText::new(
        net::http_client(),
        Arc::new(|| Ok(None)),
        Arc::new(|_| Box::pin(async { None })),
        SonioxRegion::Us,
    );
    assert!(stt.streams());
    assert!(stt.live_text("stt-rt-v6"));
    assert!(!stt.allows_speed_up());
    assert_eq!(16_000, stt.sample_rate("stt-rt-v6"));
    assert!(stt.takes_custom_words("stt-rt-v6"));
}

// Dropped: webSocketReusesTheWarmedUpConnection and warmUpIsSkippedUntilASessionTakesTheWarmedConnection (no
// warm-up for WebSocket providers on desktop: tungstenite can't reuse a pooled connection).

/// Endless audio in 1 MiB chunks, a millisecond apart.
struct Flood;

impl AudioSource for Flood {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        Box::pin(async {
            sleep(Duration::from_millis(1)).await;
            Ok(Some(vec![0; 1 << 20]))
        })
    }
}

#[tokio::test]
async fn unwritable_socket_fails_the_session_while_recording() {
    // Holds the socket without reading, so the writes back up.
    let server = WsServer::start(|_request, socket| async move {
        sleep(Duration::from_secs(60)).await;
        drop(socket);
    })
    .await;
    let stt = SonioxSpeechToText {
        write_timeout: Duration::from_millis(200),
        ..SonioxSpeechToText::with_url(
            Arc::new(|| Ok(Some("sx-test".into()))),
            Arc::new(|_| Box::pin(async { None })),
            &server.url("/transcribe-websocket"),
        )
    };

    let error = tokio::time::timeout(
        Duration::from_secs(10),
        stt.transcribe(Box::new(Flood), request("", TIMEOUT, &[]), ignore()),
    )
    .await
    .expect("the write timeout ended the session");

    assert_eq!(Err(SttError("Network error: timeout".into())), error);
}
