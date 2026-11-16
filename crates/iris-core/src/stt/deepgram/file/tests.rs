/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `DeepgramFileTranscriptionTest.kt` against wiremock (a raw TCP server where the test watches the
//! connection itself).

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::stt::deepgram::test_audio::{Script, chunks, script};

const TIMEOUT: Duration = Duration::from_secs(10);

/// 50 ms each at 16 kHz.
const CHUNK_A: [u8; 1_600] = [1; 1_600];
const CHUNK_B: [u8; 1_600] = [2; 1_600];

fn stt_with(api_key: Option<&str>, url: String) -> DeepgramFileTranscription {
    let key = api_key.map(String::from);
    DeepgramFileTranscription::with_urls(net::http_client(), Arc::new(move || Ok(key.clone())), url.clone(), url)
}

fn stt(server: &MockServer) -> DeepgramFileTranscription {
    stt_with(Some("dg-test"), format!("{}/v1/listen", server.uri()))
}

async fn respond(server: &MockServer, code: u16, body: &str) {
    server.reset().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(code).set_body_string(body)).mount(server).await;
}

/// Fails the test instead of hanging.
async fn within<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(15), future).await.expect("test timed out")
}

fn audio() -> Script {
    chunks(&[&CHUNK_A, &CHUNK_B])
}

/// Transcribes `audio` with nova-3; live text fails the test.
async fn run(
    stt: &DeepgramFileTranscription,
    audio: Script,
    language: &str,
    timeout: Duration,
    words: &[&str],
) -> Result<String, SttError> {
    let request = SttRequest {
        model: "nova-3".into(),
        language: language.into(),
        timeout,
        custom_words: words.iter().map(|w| w.to_string()).collect(),
    };
    within(stt.transcribe(Box::new(audio), request, Box::new(|_| panic!("No live text")))).await
}

async fn requests(server: &MockServer) -> Vec<wiremock::Request> {
    server.received_requests().await.unwrap()
}

fn response(transcript: &str) -> String {
    format!(
        r#"{{"metadata":{{"request_id":"r","duration":1.25,"channels":1}},
           "results":{{"channels":[{{"alternatives":[{{"transcript":"{transcript}","confidence":0.9,
           "words":[]}}]}}]}}}}"#
    )
}

#[tokio::test]
async fn uploads_the_recording_as_wav_and_returns_the_transcript() {
    let server = MockServer::start().await;
    respond(&server, 200, &response(" Bonjour. ")).await;

    let text = run(&stt(&server), audio(), "fr", TIMEOUT, &[]).await;

    assert_eq!(Ok("Bonjour.".into()), text);
    let request = &requests(&server).await[0];
    assert_eq!("POST", request.method.as_str());
    assert_eq!("/v1/listen", request.url.path());
    assert_eq!("Token dg-test", request.headers.get("authorization").unwrap());
    assert_eq!("audio/wav", request.headers.get("content-type").unwrap());
    let query: BTreeMap<String, String> = request.url.query_pairs().into_owned().collect();
    let expected: BTreeMap<String, String> =
        [("model", "nova-3"), ("language", "fr"), ("smart_format", "true"), ("punctuate", "true")]
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
    assert_eq!(expected, query);
    // 100 ms of audio: padded to 1.25 s.
    assert_eq!(wav_bytes(&[CHUNK_A.to_vec(), CHUNK_B.to_vec()], 16_000), request.body);
    assert_eq!(44 + 40_000, request.body.len());
}

#[tokio::test]
async fn blank_language_is_multi() {
    let server = MockServer::start().await;
    respond(&server, 200, &response("Hola, hello.")).await;

    run(&stt(&server), audio(), "", TIMEOUT, &[]).await.unwrap();

    let language: Vec<_> = requests(&server).await[0]
        .url
        .query_pairs()
        .filter(|(k, _)| k == "language")
        .map(|(_, v)| v.into_owned())
        .collect();
    assert_eq!(vec!["multi"], language);
}

#[tokio::test]
async fn custom_words_are_sent_as_keyterms() {
    let server = MockServer::start().await;
    respond(&server, 200, &response("Hi")).await;

    run(&stt(&server), audio(), "", TIMEOUT, &["Deepgram", "Jamius Siam"]).await.unwrap();

    let keyterms: Vec<_> = requests(&server).await[0]
        .url
        .query_pairs()
        .filter(|(k, _)| k == "keyterm")
        .map(|(_, v)| v.into_owned())
        .collect();
    assert_eq!(vec!["Deepgram", "Jamius Siam"], keyterms);
}

#[tokio::test]
async fn blank_transcript_is_returned_blank() {
    let server = MockServer::start().await;
    respond(&server, 200, &response("")).await;

    // The controller turns a blank transcript into "No speech detected".
    assert_eq!(Ok(String::new()), run(&stt(&server), audio(), "", TIMEOUT, &[]).await);
}

#[tokio::test]
async fn uploads_after_stop_at_16_khz() {
    let stt = stt_with(Some("dg-test"), LISTEN_URL.into());
    assert!(!stt.streams());
    assert!(!stt.live_text("nova-3"));
    assert_eq!(16_000, stt.sample_rate("nova-3"));
    assert!(stt.takes_custom_words("nova-3"));
    assert!(stt.takes_custom_words("nova-3-medical"));
    assert!(!stt.takes_custom_words("nova-2"));
}

#[tokio::test]
async fn http_errors_map_like_the_streaming_handshake() {
    let server = MockServer::start().await;
    let stt = stt(&server);
    let cases = [
        (
            401,
            r#"{"err_code":"INVALID_AUTH","err_msg":"Invalid credentials."}"#,
            "Deepgram rejected the API key. Check it in Iris settings.",
        ),
        (402, "", "Deepgram account is out of credits"),
        (429, "", "Deepgram rate limit reached"),
        (504, "", "Deepgram server error (HTTP 504)"),
        (
            400,
            r#"{"err_code":"Bad Request","err_msg":"No such model/language/tier combination found."}"#,
            "Deepgram error: No such model/language/tier combination found.",
        ),
    ];
    for (code, body, message) in cases {
        respond(&server, code, body).await;
        assert_eq!(Err(SttError(message.into())), run(&stt, audio(), "", TIMEOUT, &[]).await);
    }
}

#[tokio::test]
async fn response_without_transcript_is_unexpected() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"metadata":{},"results":{"channels":[]}}"#).await;

    assert_eq!(
        Err(SttError("Unexpected response from Deepgram".into())),
        run(&stt(&server), audio(), "", TIMEOUT, &[]).await
    );
}

#[tokio::test]
async fn slow_transcript_times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(response("late")).set_delay(Duration::from_secs(3)))
        .mount(&server)
        .await;

    let error = run(&stt(&server), audio(), "", Duration::from_millis(200), &[]).await;

    assert_eq!(Err(SttError("Timed out waiting for the transcript".into())), error);
}

#[tokio::test]
async fn cancelling_cancels_the_upload() {
    // A server that never answers and reports when the client drops the connection.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/listen", listener.local_addr().unwrap());
    let (arrived, mut in_flight) = oneshot::channel();
    let (closed, connection_closed) = oneshot::channel();
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut arrived = Some(arrived);
        let mut buffer = [0; 8_192];
        while let Ok(1..) = stream.read(&mut buffer).await {
            if let Some(arrived) = arrived.take() {
                let _ = arrived.send(());
            }
        }
        let _ = closed.send(());
    });
    let stt = stt_with(Some("dg-test"), url);

    // The upload is in flight, then the session is dropped.
    within(async {
        tokio::select! {
            _ = run(&stt, audio(), "", TIMEOUT, &[]) => panic!("upload ended"),
            _ = &mut in_flight => {}
        }
    })
    .await;

    within(connection_closed).await.unwrap();
}

#[tokio::test]
async fn missing_api_key_fails_before_recording() {
    let server = MockServer::start().await;
    let audio = script(vec![]);
    let pulled = audio.pulled.clone();

    let error = run(&stt_with(None, server.uri()), audio, "", TIMEOUT, &[]).await;

    assert_eq!(Err(SttError("Add your Deepgram API key in Iris settings".into())), error);
    assert!(!pulled.load(std::sync::atomic::Ordering::SeqCst));
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn invalid_api_key_fails_before_recording() {
    let server = MockServer::start().await;
    let audio = chunks(&[&[1, 2]]);
    let pulled = audio.pulled.clone();

    let error = run(&stt_with(Some("bad\u{a0}key"), server.uri()), audio, "", TIMEOUT, &[]).await;

    assert_eq!(Err(SttError("Invalid Deepgram API key".into())), error);
    assert!(!pulled.load(std::sync::atomic::Ordering::SeqCst));
    assert!(requests(&server).await.is_empty());
}

#[tokio::test]
async fn warm_up_is_a_keyless_head_throttled() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD")).respond_with(ResponseTemplate::new(405)).mount(&server).await;
    let stt = stt(&server);

    stt.warm_up();
    stt.warm_up(); // Within the window: skipped.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let requests = requests(&server).await;
    assert_eq!(1, requests.len());
    assert_eq!("HEAD", requests[0].method.as_str());
    assert_eq!("/v1/listen", requests[0].url.path());
    assert!(requests[0].headers.get("authorization").is_none());
}
