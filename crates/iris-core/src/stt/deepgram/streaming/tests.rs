/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `DeepgramSpeechToTextTest.kt` against a local WebSocket server; its two warm-up tests are dropped (no
//! warm-up for WebSockets on desktop). Plus a missed pong (OkHttp's `pingInterval`, desktop's own pings).

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use futures_util::SinkExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;

use super::*;
use crate::net::test_support::WsServer;
use crate::stt::deepgram::test_audio::{Script, Step, chunks, script};

const TIMEOUT: Duration = Duration::from_secs(10);
const METADATA: &str = r#"{"type":"Metadata","request_id":"r","duration":0.1,"channels":1}"#;

/// What the server does on an event.
#[derive(Clone)]
enum Act {
    Send(String),
    Close(u16, &'static str),
}

/// The server's script (the Kotlin test's `upgrade`).
#[derive(Clone, Default)]
struct Upgrade {
    on_open: Vec<Act>,
    on_audio: Vec<Act>,
    on_close_stream: Vec<Act>,
}

/// What the server got.
#[derive(Clone, Default)]
struct Seen {
    /// Text frames.
    texts: Arc<Mutex<Vec<String>>>,
    /// Binary (audio) frames.
    audio: Arc<Mutex<Vec<Vec<u8>>>>,
    /// Set when the client closed or dropped the socket.
    closed: Arc<AtomicBool>,
}

impl Seen {
    fn texts(&self) -> Vec<String> {
        self.texts.lock().unwrap().clone()
    }

    fn audio(&self) -> Vec<Vec<u8>> {
        self.audio.lock().unwrap().clone()
    }
}

async fn start(upgrade: Upgrade) -> (WsServer, Seen) {
    let seen = Seen::default();
    let server_seen = seen.clone();
    let server = WsServer::start(move |_request, socket| serve(socket, upgrade.clone(), server_seen.clone())).await;
    (server, seen)
}

async fn serve(mut socket: WebSocketStream<TcpStream>, upgrade: Upgrade, seen: Seen) {
    act(&mut socket, &upgrade.on_open).await;
    // Reads on after a Close, so the reply goes out like a real server's.
    while let Some(Ok(message)) = socket.next().await {
        match message {
            Message::Text(text) => {
                seen.texts.lock().unwrap().push(text.to_string());
                if text.as_str() == CLOSE_STREAM {
                    act(&mut socket, &upgrade.on_close_stream).await;
                }
            }
            Message::Binary(bytes) => {
                seen.audio.lock().unwrap().push(bytes.to_vec());
                act(&mut socket, &upgrade.on_audio).await;
            }
            Message::Close(_) => seen.closed.store(true, Ordering::SeqCst),
            _ => {}
        }
    }
    seen.closed.store(true, Ordering::SeqCst);
}

async fn act(socket: &mut WebSocketStream<TcpStream>, acts: &[Act]) {
    for act in acts {
        let _ = match act {
            Act::Send(text) => socket.send(Message::text(text.clone())).await,
            Act::Close(code, reason) => {
                socket.close(Some(CloseFrame { code: CloseCode::from(*code), reason: (*reason).into() })).await
            }
        };
    }
}

fn send(texts: &[&str]) -> Vec<Act> {
    texts.iter().map(|text| Act::Send(text.to_string())).collect()
}

fn stt_with(api_key: Option<&str>, url: String) -> DeepgramSpeechToText {
    let key = api_key.map(String::from);
    DeepgramSpeechToText::with_url(Arc::new(move || Ok(key.clone())), url, net::PING_INTERVAL)
}

fn stt(server: &WsServer) -> DeepgramSpeechToText {
    stt_with(Some("dg-test"), server.url("/v1/listen"))
}

fn request(language: &str, timeout: Duration, custom_words: &[&str]) -> SttRequest {
    SttRequest {
        model: "nova-3".into(),
        language: language.into(),
        timeout,
        custom_words: custom_words.iter().map(|w| w.to_string()).collect(),
    }
}

fn no_partials() -> OnPartial {
    Box::new(|_| {})
}

/// Fails the test instead of hanging.
async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), future).await.expect("test timed out")
}

async fn wait_until(condition: impl Fn() -> bool) {
    within(async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
}

async fn transcribe(stt: &DeepgramSpeechToText, audio: Script, request: SttRequest) -> Result<String, SttError> {
    within(stt.transcribe(Box::new(audio), request, no_partials())).await
}

fn result(transcript: &str, is_final: bool) -> String {
    format!(
        r#"{{"type":"Results","channel_index":[0,1],"duration":1.0,"start":0.0,"is_final":{is_final},
           "speech_final":false,"channel":{{"alternatives":[{{"transcript":"{transcript}","confidence":0.9}}]}}}}"#
    )
}

#[tokio::test]
async fn streams_binary_audio_then_close_stream_and_returns_the_finals_on_metadata() {
    let (server, seen) = start(Upgrade {
        on_close_stream: send(&[
            &result("Hello there.", true),
            &result("How", false),
            &result("How are you?", true),
            METADATA,
        ]),
        ..Upgrade::default()
    })
    .await;

    let text = transcribe(&stt(&server), chunks(&[&[1, 2], &[3, 4]]), request("en", TIMEOUT, &[])).await;

    assert_eq!(Ok("Hello there. How are you?".into()), text);
    let request = &server.requests()[0];
    assert_eq!(Some("Token dg-test".into()), request.header("authorization"));
    assert_eq!(vec!["nova-3"], request.query("model"));
    assert_eq!(vec!["en"], request.query("language"));
    assert_eq!(vec!["16000"], request.query("sample_rate"));
    assert_eq!(vec![vec![1, 2], vec![3, 4]], seen.audio());
    assert_eq!(vec![CLOSE_STREAM], seen.texts());
}

#[tokio::test]
async fn custom_words_are_sent_as_keyterms() {
    let (server, _) = start(Upgrade { on_close_stream: send(&[METADATA]), ..Upgrade::default() }).await;

    transcribe(&stt(&server), chunks(&[&[1, 2]]), request("", TIMEOUT, &["Deepgram", "Jamius Siam"])).await.unwrap();

    assert_eq!(vec!["Deepgram", "Jamius Siam"], server.requests()[0].query("keyterm"));
}

#[tokio::test]
async fn server_close_after_close_stream_returns_the_finals() {
    let (server, _) = start(Upgrade {
        on_close_stream: vec![Act::Send(result("Bye.", true)), Act::Close(1000, "")],
        ..Upgrade::default()
    })
    .await;

    let text = transcribe(&stt(&server), chunks(&[&[1, 2]]), request("", TIMEOUT, &[])).await;

    assert_eq!(Ok("Bye.".into()), text);
}

#[tokio::test]
async fn metadata_before_close_stream_does_not_end_the_stream() {
    let (server, _) = start(Upgrade {
        on_open: send(&[METADATA]),
        on_close_stream: send(&[&result("Late.", true), METADATA]),
        ..Upgrade::default()
    })
    .await;
    // The early Metadata arrives while recording.
    let audio = script(vec![Step::Chunk(vec![1, 2]), Step::Sleep(200)]);

    let text = transcribe(&stt(&server), audio, request("", TIMEOUT, &[])).await;

    assert_eq!(Ok("Late.".into()), text);
}

#[tokio::test]
async fn live_text_reaches_on_partial() {
    let (server, _) = start(Upgrade {
        on_audio: send(&[&result("Hel", false), &result("Hello.", true), &result("How", false)]),
        on_close_stream: send(&[&result("How are you?", true), METADATA]),
        ..Upgrade::default()
    })
    .await;
    let partials = Arc::new(Mutex::new(Vec::new()));
    let interim_seen = Arc::new(AtomicBool::new(false));
    // Holds CloseStream back until the live results have been reported.
    let seen = interim_seen.clone();
    let audio = script(vec![Step::Chunk(vec![1, 2]), Step::Until(Box::new(move || seen.load(Ordering::SeqCst)))]);
    let (on_partial_partials, on_partial_seen) = (partials.clone(), interim_seen.clone());
    let on_partial: OnPartial = Box::new(move |live| {
        if live == "Hello. How" {
            on_partial_seen.store(true, Ordering::SeqCst);
        }
        on_partial_partials.lock().unwrap().push(live);
    });

    let text = within(stt(&server).transcribe(Box::new(audio), request("", TIMEOUT, &[]), on_partial)).await;

    assert_eq!(Ok("Hello. How are you?".into()), text);
    assert_eq!(vec!["Hel", "Hello.", "Hello. How", "Hello. How are you?"], *partials.lock().unwrap());
}

#[tokio::test]
async fn error_event_does_not_fail_the_session() {
    let (server, _) = start(Upgrade {
        on_open: send(&[r#"{"type":"Error","variant":"SchemaError","description":"Bad message"}"#]),
        on_close_stream: send(&[&result("Still fine.", true), METADATA]),
        ..Upgrade::default()
    })
    .await;
    // The Error arrives while recording.
    let audio = script(vec![Step::Chunk(vec![1, 2]), Step::Sleep(200)]);

    let text = transcribe(&stt(&server), audio, request("", TIMEOUT, &[])).await;

    assert_eq!(Ok("Still fine.".into()), text);
}

/// An upgrade answered with `status`, the `dg-error` header and no body (the shared server sends no headers).
async fn reject_with_dg_error(status: u16, dg_error: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/listen", listener.local_addr().unwrap());
    let response = format!("HTTP/1.1 {status} Bad Request\r\ndg-error: {dg_error}\r\ncontent-length: 0\r\n\r\n");
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        while !request.ends_with(b"\r\n\r\n") {
            let n = stream.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..n]);
        }
        stream.write_all(response.as_bytes()).await.unwrap();
    });
    url
}

#[tokio::test]
async fn rejected_handshakes_map_to_readable_errors() {
    let (server, _) = start(Upgrade::default()).await;
    let endless = || script(vec![Step::Pending]);

    server.reject_next(401, r#"{"err_code":"INVALID_AUTH","err_msg":"Invalid credentials."}"#);
    assert_eq!(
        Err(SttError("Deepgram rejected the API key. Check it in Iris settings.".into())),
        transcribe(&stt(&server), endless(), request("", TIMEOUT, &[])).await
    );
    server.reject_next(402, "");
    assert_eq!(
        Err(SttError("Deepgram account is out of credits".into())),
        transcribe(&stt(&server), endless(), request("", TIMEOUT, &[])).await
    );
    server.reject_next(400, r#"{"err_code":"Bad Request","err_msg":"No such model/language/tier combination found."}"#);
    assert_eq!(
        Err(SttError("Deepgram error: No such model/language/tier combination found.".into())),
        transcribe(&stt(&server), endless(), request("yo", TIMEOUT, &[])).await
    );
    let url = reject_with_dg_error(400, "Bad language").await;
    assert_eq!(
        Err(SttError("Deepgram error: Bad language".into())),
        transcribe(&stt_with(Some("dg-test"), url), endless(), request("yo", TIMEOUT, &[])).await
    );
}

#[tokio::test]
async fn server_close_mid_session_fails_the_session() {
    let (server, _) = start(Upgrade { on_audio: vec![Act::Close(1011, "NET-0001")], ..Upgrade::default() }).await;

    let error =
        transcribe(&stt(&server), script(vec![Step::Chunk(vec![1, 2]), Step::Pending]), request("", TIMEOUT, &[]))
            .await;

    assert_eq!(Err(SttError("Deepgram closed the connection: NET-0001".into())), error);
}

#[tokio::test]
async fn times_out_waiting_for_metadata() {
    let (server, _) = start(Upgrade::default()).await;

    let error = transcribe(&stt(&server), chunks(&[&[1, 2]]), request("", Duration::from_millis(200), &[])).await;

    assert_eq!(Err(SttError("Timed out waiting for the transcript".into())), error);
}

#[tokio::test]
async fn cancelling_closes_the_socket() {
    let (server, seen) = start(Upgrade::default()).await;
    let stt = stt(&server);
    let audio = script(vec![Step::Chunk(vec![1, 2]), Step::Pending]);

    // Dropping the session once connected (the server got the audio).
    within(async {
        tokio::select! {
            _ = stt.transcribe(Box::new(audio), request("", TIMEOUT, &[]), no_partials()) => panic!("session ended"),
            _ = wait_until(|| !seen.audio().is_empty()) => {}
        }
    })
    .await;

    wait_until(|| seen.closed.load(Ordering::SeqCst)).await;
    assert!(seen.texts().is_empty()); // No CloseStream.
}

#[tokio::test]
async fn audio_failure_is_rethrown_without_close_stream() {
    let (server, seen) = start(Upgrade::default()).await;
    // The controller's audio ends like this when VAD let nothing through.
    let frames = seen.clone();
    let audio = script(vec![
        Step::Chunk(vec![1, 2]),
        Step::Until(Box::new(move || !frames.audio().is_empty())),
        Step::Fail("No speech detected"),
    ]);

    let error = transcribe(&stt(&server), audio, request("", TIMEOUT, &[])).await;

    assert_eq!(Err(SttError("No speech detected".into())), error);
    wait_until(|| seen.closed.load(Ordering::SeqCst)).await;
    assert!(seen.texts().is_empty());
}

#[tokio::test]
async fn missing_api_key_fails_without_connecting() {
    let (server, _) = start(Upgrade::default()).await;
    let audio = script(vec![]);
    let pulled = audio.pulled.clone();

    let error = transcribe(&stt_with(None, server.url("/v1/listen")), audio, request("", TIMEOUT, &[])).await;

    assert_eq!(Err(SttError("Add your Deepgram API key in Iris settings".into())), error);
    assert!(!pulled.load(Ordering::SeqCst));
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn empty_chunks_are_not_sent() {
    let (server, seen) = start(Upgrade { on_close_stream: send(&[METADATA]), ..Upgrade::default() }).await;

    let text = transcribe(&stt(&server), chunks(&[&[], &[1, 2]]), request("", TIMEOUT, &[])).await;

    assert_eq!(Ok(String::new()), text); // Blank: the controller shows "No speech detected".
    assert_eq!(vec![2], seen.audio().iter().map(Vec::len).collect::<Vec<_>>());
}

#[tokio::test]
async fn streams_at_16_khz() {
    let stt = stt_with(Some("dg-test"), LISTEN_URL.into());
    assert!(stt.streams());
    assert!(stt.live_text("nova-3"));
    assert_eq!(16_000, stt.sample_rate("nova-3"));
    assert!(stt.takes_custom_words("nova-3"));
    assert!(stt.takes_custom_words("nova-3-medical"));
    assert!(!stt.takes_custom_words("nova-2"));
}

#[tokio::test]
async fn missed_pong_fails_the_session() {
    // Never reads, so never answers a ping.
    let server = WsServer::start(|_request, socket| async move {
        tokio::time::sleep(Duration::from_secs(10)).await;
        drop(socket);
    })
    .await;
    let key: KeySource = Arc::new(|| Ok(Some("dg-test".into())));
    let stt = DeepgramSpeechToText::with_url(key, server.url("/v1/listen"), Duration::from_millis(100));

    let error = transcribe(&stt, script(vec![Step::Chunk(vec![1, 2]), Step::Pending]), request("", TIMEOUT, &[])).await;

    assert_eq!(Err(SttError(deepgram_network_error_message(&NetError::ping_timeout()))), error);
}

/// Endless audio in 1 MiB chunks, a millisecond apart.
struct Flood;

impl AudioSource for Flood {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        Box::pin(async {
            tokio::time::sleep(Duration::from_millis(1)).await;
            Ok(Some(vec![0; 1 << 20]))
        })
    }
}

#[tokio::test]
async fn unwritable_socket_fails_the_session_while_recording() {
    // Holds the socket without reading, so the writes back up.
    let server = WsServer::start(|_request, socket| async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop(socket);
    })
    .await;
    let stt = DeepgramSpeechToText { write_timeout: Duration::from_millis(200), ..stt(&server) };

    let error = tokio::time::timeout(
        Duration::from_secs(10),
        stt.transcribe(Box::new(Flood), request("", TIMEOUT, &[]), no_partials()),
    )
    .await
    .expect("the write timeout ended the session");

    assert_eq!(Err(SttError("Network error: timeout".into())), error);
}
