/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `OpenAiRealtimeSpeechToTextTest.kt` against a local WebSocket server.

use super::*;
use crate::net::test_support::WsServer;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;
use tokio::time::sleep;

const TIMEOUT: Duration = Duration::from_secs(10);
const COMMITTED: &str = r#"{"type":"input_audio_buffer.committed","item_id":"item_1"}"#;

/// One step of a scripted audio flow.
enum Step {
    Chunk(Vec<u8>),
    /// Waits for this before the next step (cancel-safe: the step stays until it completes).
    Wait(Pin<Box<dyn Future<Output = ()> + Send>>),
    Fail(&'static str),
    /// Never ends (Kotlin's `awaitCancellation`).
    Endless,
}

/// Audio that runs its steps as the provider pulls (as a Kotlin flow runs while collected), then ends.
struct Script(VecDeque<Step>);

impl AudioSource for Script {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        Box::pin(async move {
            loop {
                match self.0.front_mut() {
                    None => return Ok(None),
                    Some(Step::Wait(wait)) => wait.as_mut().await,
                    Some(Step::Endless) => std::future::pending().await,
                    Some(_) => {}
                }
                match self.0.pop_front() {
                    Some(Step::Chunk(chunk)) => return Ok(Some(chunk)),
                    Some(Step::Fail(message)) => return Err(SttError(message.into())),
                    _ => {}
                }
            }
        })
    }
}

fn audio(steps: Vec<Step>) -> Box<dyn AudioSource> {
    Box::new(Script(steps.into()))
}

fn chunks(chunks: &[&[u8]]) -> Box<dyn AudioSource> {
    audio(chunks.iter().map(|c| Step::Chunk(c.to_vec())).collect())
}

fn endless() -> Box<dyn AudioSource> {
    audio(vec![Step::Endless])
}

fn kind(message: &str) -> String {
    serde_json::from_str::<Value>(message).unwrap()["type"].as_str().unwrap().to_string()
}

fn delta(text: &str) -> String {
    format!(r#"{{"type":"conversation.item.input_audio_transcription.delta","item_id":"item_1","delta":"{text}"}}"#)
}

fn completed(text: &str) -> String {
    format!(
        r#"{{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_1","transcript":"{text}"}}"#
    )
}

/// What the server does on a message from the client.
enum Reply {
    Text(String),
    Close(u16, &'static str),
}

type OnMessage = Box<dyn Fn(&str) -> Vec<Reply> + Send + Sync>;

/// The test server: what it received, and a notification once the client closed or dropped the socket.
struct Server {
    ws: WsServer,
    received: Arc<Mutex<Vec<String>>>,
    closed: Arc<Notify>,
}

impl Server {
    fn received(&self) -> Vec<String> {
        self.received.lock().unwrap().clone()
    }

    fn kinds(&self) -> Vec<String> {
        self.received().iter().map(|m| kind(m)).collect()
    }

    fn stt(&self) -> OpenAiRealtimeSpeechToText {
        stt(Some("sk-test"), self.ws.url("/v1/realtime?intent=transcription"))
    }

    async fn saw_close(&self) -> bool {
        tokio::time::timeout(Duration::from_secs(5), self.closed.notified()).await.is_ok()
    }
}

/// A server that sends `on_open` once connected, records the text messages it gets and answers each with
/// `on_message`. It replies to the client's close after `close_reply_delay`.
async fn server(on_open: Vec<String>, close_reply_delay: Duration, on_message: OnMessage) -> Server {
    let received: Arc<Mutex<Vec<String>>> = Arc::default();
    let closed = Arc::new(Notify::new());
    let on_message = Arc::new(on_message);
    let (task_received, task_closed) = (received.clone(), closed.clone());
    let ws = WsServer::start(move |_request, mut socket| {
        let (received, closed, on_message, on_open) =
            (task_received.clone(), task_closed.clone(), on_message.clone(), on_open.clone());
        async move {
            for message in on_open {
                socket.send(Message::text(message)).await.unwrap();
            }
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Text(text) => {
                        received.lock().unwrap().push(text.to_string());
                        for reply in on_message(text.as_str()) {
                            match reply {
                                Reply::Text(text) => socket.send(Message::text(text)).await.unwrap(),
                                Reply::Close(code, reason) => {
                                    let frame = CloseFrame { code: code.into(), reason: reason.into() };
                                    let _ = socket.close(Some(frame)).await;
                                }
                            }
                        }
                    }
                    Message::Close(_) => sleep(close_reply_delay).await,
                    _ => {}
                }
            }
            closed.notify_one();
        }
    })
    .await;
    Server { ws, received, closed }
}

/// Answers the commit with the committed item and `text` as its transcript.
fn completes_with(text: &'static str) -> OnMessage {
    Box::new(move |message| {
        if kind(message) == "input_audio_buffer.commit" {
            vec![Reply::Text(COMMITTED.into()), Reply::Text(completed(text))]
        } else {
            vec![]
        }
    })
}

fn silent() -> OnMessage {
    Box::new(|_| vec![])
}

fn stt(api_key: Option<&str>, url: String) -> OpenAiRealtimeSpeechToText {
    let api_key = api_key.map(String::from);
    OpenAiRealtimeSpeechToText::with_url(Arc::new(move || Ok(api_key.clone())), url, net::PING_INTERVAL)
}

fn request(language: &str, timeout: Duration, custom_words: &[&str]) -> SttRequest {
    SttRequest {
        model: "gpt-live-transcribe".into(),
        language: language.into(),
        timeout,
        custom_words: custom_words.iter().map(|w| w.to_string()).collect(),
    }
}

type Partials = Arc<Mutex<Vec<String>>>;

fn recorder() -> (Partials, OnPartial) {
    let partials: Partials = Arc::default();
    let sink = partials.clone();
    (partials, Box::new(move |text| sink.lock().unwrap().push(text)))
}

fn ignore() -> OnPartial {
    Box::new(|_| {})
}

/// Runs `transcribe`, failing the test instead of hanging.
async fn run(
    stt: &OpenAiRealtimeSpeechToText,
    audio: Box<dyn AudioSource>,
    request: SttRequest,
    on_partial: OnPartial,
) -> Result<String, SttError> {
    tokio::time::timeout(Duration::from_secs(15), stt.transcribe(audio, request, on_partial)).await.expect("hung")
}

fn appended_audio(message: &str) -> Vec<u8> {
    let message: Value = serde_json::from_str(message).unwrap();
    STANDARD.decode(message["audio"].as_str().unwrap()).unwrap()
}

#[tokio::test]
async fn streams_audio_commits_and_returns_final_transcript() {
    let server = server(
        vec![],
        Duration::ZERO,
        Box::new(|message| {
            if kind(message) == "input_audio_buffer.commit" {
                vec![
                    Reply::Text(COMMITTED.into()),
                    Reply::Text(delta("Hello")),
                    Reply::Text(delta(" world")),
                    Reply::Text(
                        r#"{"type":"conversation.item.input_audio_transcription.completed","item_id":"item_1",
                           "content_index":0,"transcript":"Hello, world."}"#
                            .into(),
                    ),
                ]
            } else {
                vec![]
            }
        }),
    )
    .await;
    let (partials, on_partial) = recorder();

    let text = run(&server.stt(), chunks(&[&[1, 2], &[3, 4]]), request("en", TIMEOUT, &[]), on_partial).await;

    assert_eq!(Ok("Hello, world.".into()), text);
    assert_eq!(vec!["Hello", "Hello world", "Hello, world."], *partials.lock().unwrap());
    let upgrades = server.ws.requests();
    assert_eq!((1, 0), (upgrades.len(), upgrades[0].index)); // One connection.
    assert_eq!(Some("Bearer sk-test".into()), upgrades[0].header("authorization"));
    assert_eq!("/v1/realtime?intent=transcription", upgrades[0].uri);
    assert_eq!(vec!["transcription"], upgrades[0].query("intent"));
    assert_eq!(
        vec![
            "session.update",
            "input_audio_buffer.append",
            "input_audio_buffer.append",
            "input_audio_buffer.append", // Silence padding up to the 100 ms minimum.
            "input_audio_buffer.commit",
        ],
        server.kinds()
    );
}

#[tokio::test]
async fn short_audio_is_padded_with_silence_to_the_minimum_commit() {
    let server = server(vec![], Duration::ZERO, completes_with("")).await;

    run(&server.stt(), chunks(&[&[1, 2]]), request("", TIMEOUT, &[]), ignore()).await.unwrap();

    let audio: Vec<Vec<u8>> = server
        .received()
        .iter()
        .filter(|m| kind(m) == "input_audio_buffer.append")
        .map(|m| appended_audio(m))
        .collect();
    assert_eq!(vec![2, MIN_COMMIT_BYTES - 2], audio.iter().map(Vec::len).collect::<Vec<_>>());
    assert!(audio[1].iter().all(|b| *b == 0));
}

#[tokio::test]
async fn custom_words_go_in_the_session_update() {
    let server = server(vec![], Duration::ZERO, completes_with("")).await;

    run(&server.stt(), chunks(&[&[1, 2]]), request("", TIMEOUT, &["Iris"]), ignore()).await.unwrap();

    assert_eq!(session_update("gpt-live-transcribe", "", &["Iris".into()]), server.received()[0]);
}

#[tokio::test]
async fn full_audio_is_not_padded() {
    let server = server(vec![], Duration::ZERO, completes_with("")).await;

    run(&server.stt(), chunks(&[&[0; MIN_COMMIT_BYTES]]), request("", TIMEOUT, &[]), ignore()).await.unwrap();

    assert_eq!(1, server.kinds().iter().filter(|k| *k == "input_audio_buffer.append").count());
}

#[tokio::test]
async fn returns_without_waiting_for_the_close_handshake() {
    let server = server(vec![], Duration::from_secs(3), completes_with("Hi.")).await;
    let start = Instant::now();

    let text = run(&server.stt(), chunks(&[&[1, 2]]), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Ok("Hi.".into()), text);
    assert!(start.elapsed() < Duration::from_secs(2)); // Didn't wait for the server's close reply.
}

#[tokio::test]
async fn server_error_event_fails_the_session() {
    let on_open = vec![r#"{"type":"error","error":{"type":"server_error","message":"Boom"}}"#.to_string()];
    let server = server(on_open, Duration::ZERO, silent()).await;

    let error = run(&server.stt(), endless(), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("OpenAI error: Boom".into())), error);
}

#[tokio::test]
async fn rejected_handshake_maps_to_api_key_error() {
    let server = server(vec![], Duration::ZERO, silent()).await;
    server.ws.reject_next(401, "");

    let error = run(&server.stt(), endless(), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("OpenAI rejected the API key. Check it in Iris settings.".into())), error);
}

#[tokio::test]
async fn missing_api_key_fails_without_connecting() {
    let server = server(vec![], Duration::ZERO, silent()).await;
    let stt = stt(None, server.ws.url("/v1/realtime?intent=transcription"));

    let error = run(&stt, endless(), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("Add your OpenAI API key in Iris settings".into())), error);
    assert!(server.ws.requests().is_empty());
}

#[tokio::test]
async fn deltas_before_commit_then_completed_after_commit() {
    let server = server(
        vec![],
        Duration::ZERO,
        Box::new(|message| match kind(message).as_str() {
            "input_audio_buffer.append" => vec![Reply::Text(delta("Hello")), Reply::Text(delta(" world"))],
            "input_audio_buffer.commit" => vec![Reply::Text(COMMITTED.into()), Reply::Text(completed("Hello world."))],
            _ => vec![],
        }),
    )
    .await;
    let (partials, _) = recorder();
    let deltas_seen = Arc::new(Notify::new());
    let (sink, seen) = (partials.clone(), deltas_seen.clone());
    let on_partial: OnPartial = Box::new(move |live| {
        if live == "Hello world" {
            seen.notify_one();
        }
        sink.lock().unwrap().push(live);
    });
    // Holds the commit back until both deltas have been reported.
    let waiter = deltas_seen.clone();
    let audio = audio(vec![
        Step::Chunk(vec![0; MIN_COMMIT_BYTES]), // A full commit, so no padding append triggers more deltas.
        Step::Wait(Box::pin(async move { waiter.notified().await })),
    ]);

    let text = run(&server.stt(), audio, request("", TIMEOUT, &[]), on_partial).await;

    assert_eq!(Ok("Hello world.".into()), text);
    assert_eq!(vec!["Hello", "Hello world", "Hello world."], *partials.lock().unwrap());
}

#[tokio::test]
async fn times_out_waiting_for_the_final_transcript() {
    let server = server(vec![], Duration::ZERO, silent()).await;

    let error = run(&server.stt(), chunks(&[&[1, 2]]), request("", Duration::from_millis(200), &[]), ignore()).await;

    assert_eq!(Err(SttError("Timed out waiting for the transcript".into())), error);
}

#[tokio::test]
async fn cancelling_closes_the_socket() {
    let server = server(vec![], Duration::ZERO, silent()).await;
    let stt = Arc::new(server.stt());
    let session = tokio::spawn(async move { stt.transcribe(endless(), request("", TIMEOUT, &[]), ignore()).await });
    // Connected: the server got session.update.
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.received().is_empty() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("connected");

    session.abort();

    assert!(server.saw_close().await);
}

#[tokio::test]
async fn server_close_mid_session_fails_the_session() {
    let server = server(
        vec![],
        Duration::ZERO,
        Box::new(|message| {
            if kind(message) == "input_audio_buffer.append" { vec![Reply::Close(1011, "Going away")] } else { vec![] }
        }),
    )
    .await;
    let audio = audio(vec![Step::Chunk(vec![1, 2]), Step::Endless]);

    let error = run(&server.stt(), audio, request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("OpenAI closed the connection: Going away".into())), error);
}

#[tokio::test]
async fn audio_failure_is_rethrown_without_padding_or_commit() {
    let server = Arc::new(server(vec![], Duration::ZERO, silent()).await);
    // The controller's audio ends like this when VAD let nothing through.
    let watched = server.clone();
    let audio = audio(vec![
        Step::Chunk(vec![1, 2]), // Under 100 ms: a normal end would pad it.
        Step::Wait(Box::pin(async move {
            // The server got session.update and the chunk.
            while watched.received().len() < 2 {
                sleep(Duration::from_millis(10)).await;
            }
        })),
        Step::Fail("No speech detected"),
    ]);

    let error = run(&server.stt(), audio, request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError("No speech detected".into())), error);
    assert!(server.saw_close().await);
    assert_eq!(vec!["session.update", "input_audio_buffer.append"], server.kinds());
}

#[tokio::test]
async fn malformed_events_are_ignored() {
    let on_open = vec![
        "not json".to_string(),
        "[1,2]".into(),
        r#"{"type":"input_audio_buffer.committed"}"#.into(),
        r#"{"type":"conversation.item.input_audio_transcription.delta","item_id":{"id":1}}"#.into(),
    ];
    let server = server(on_open, Duration::ZERO, completes_with("Still fine.")).await;

    let text = run(&server.stt(), chunks(&[&[1, 2]]), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Ok("Still fine.".into()), text);
}

#[test]
fn takes_custom_words_where_the_session_sends_them() {
    let stt = stt(Some("sk-test"), "http://127.0.0.1:1".into());
    for model in ["gpt-transcribe", "gpt-live-transcribe", "whisper-1", "gpt-4o-transcribe"] {
        assert!(stt.takes_custom_words(model), "{model}");
    }
    assert!(stt.takes_custom_words("gpt-4o-mini-transcribe-2025-12-15"));
    assert!(!stt.takes_custom_words("gpt-realtime-whisper"));
    assert!(!stt.takes_custom_words("gpt-4o-transcribe-diarize"));
}

// Desktop: pings are sent by the session (OkHttp's pingInterval).

#[tokio::test]
async fn missed_pong_fails_the_session() {
    // Never reads, so no pong comes back.
    let ws = WsServer::start(|_request, socket| async move {
        sleep(Duration::from_secs(5)).await;
        drop(socket);
    })
    .await;
    let api_key: KeySource = Arc::new(|| Ok(Some("sk-test".into())));
    let stt = OpenAiRealtimeSpeechToText::with_url(api_key, ws.url("/v1/realtime"), Duration::from_millis(100));

    let error = run(&stt, endless(), request("", TIMEOUT, &[]), ignore()).await;

    assert_eq!(Err(SttError(format!("Network error: {}", NetError::ping_timeout().message))), error);
}
