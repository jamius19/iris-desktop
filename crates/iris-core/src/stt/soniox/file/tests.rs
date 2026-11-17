/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `SonioxFileTranscriptionTest.kt` against wiremock.

use super::*;
use crate::stt::soniox::SonioxModel;
use serde_json::Value;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const TIMEOUT: Duration = Duration::from_secs(10);

/// 50 ms at 16 kHz.
fn chunk() -> Vec<u8> {
    vec![1; 1_600]
}

/// Responses per method and path, in order. A DELETE without one gets 204, anything else 404.
async fn server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("DELETE")).respond_with(ResponseTemplate::new(204)).with_priority(10).mount(&server).await;
    server
}

async fn on(server: &MockServer, verb: &str, route: &str, responses: &[(u16, &str)]) {
    for (code, body) in responses {
        Mock::given(method(verb))
            .and(path(route))
            .respond_with(ResponseTemplate::new(*code).set_body_string(*body))
            .up_to_n_times(1)
            .mount(server)
            .await;
    }
}

fn stt(server: &MockServer, key: Option<&str>, entry: Option<SonioxModel>) -> SonioxFileTranscription {
    let key = key.map(str::to_string);
    SonioxFileTranscription::with_urls(
        net::http_client(),
        Arc::new(move || Ok(key.clone())),
        Arc::new(move |_| {
            let entry = entry.clone();
            Box::pin(async move { entry })
        }),
        &format!("{}/", server.uri()),
        &format!("{}/v1/models", server.uri()),
    )
}

/// Audio of `chunks`.
struct Chunks(VecDeque<Vec<u8>>);

impl AudioSource for Chunks {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        let chunk = self.0.pop_front();
        Box::pin(async move { Ok(chunk) })
    }
}

fn request(language: &str, timeout: Duration, words: &[&str]) -> SttRequest {
    SttRequest {
        model: "stt-async-v6".into(),
        language: language.into(),
        timeout,
        custom_words: words.iter().map(|word| word.to_string()).collect(),
    }
}

async fn run_with(
    stt: &SonioxFileTranscription,
    audio: Box<dyn AudioSource + '_>,
    request: SttRequest,
) -> Result<String, SttError> {
    let no_live_text: OnPartial = Box::new(|_| panic!("No live text"));
    tokio::time::timeout(TEST_TIMEOUT, stt.transcribe(audio, request, no_live_text)).await.expect("test timeout")
}

async fn run(
    stt: &SonioxFileTranscription,
    language: &str,
    timeout: Duration,
    words: &[&str],
) -> Result<String, SttError> {
    run_with(stt, Box::new(Chunks(VecDeque::from([chunk()]))), request(language, timeout, words)).await
}

fn failure(result: Result<String, SttError>) -> String {
    result.expect_err("an error").0
}

/// Waits (up to 5 s) until the server got a `verb` request for `route`.
async fn await_request(server: &MockServer, verb: &str, route: &str) -> Request {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let requests = server.received_requests().await.unwrap();
        if let Some(request) = requests.into_iter().find(|r| r.method.as_str() == verb && r.url.path() == route) {
            return request;
        }
        assert!(Instant::now() < deadline, "No {verb} {route}");
        sleep(Duration::from_millis(10)).await;
    }
}

async fn paths(server: &MockServer, verb: &str) -> Vec<String> {
    let requests = server.received_requests().await.unwrap();
    requests.iter().filter(|r| r.method.as_str() == verb).map(|r| r.url.path().to_string()).collect()
}

fn job(status: &str) -> String {
    format!(r#"{{"id":"t1","status":"{status}","model":"stt-async-v6"}}"#)
}

/// Upload and create succeed.
async fn upload_and_create(server: &MockServer) {
    on(server, "POST", "/v1/files", &[(201, r#"{"id":"f1","filename":"audio.wav","size":40044}"#)]).await;
    on(server, "POST", "/v1/transcriptions", &[(201, r#"{"id":"t1","status":"queued","file_id":"f1"}"#)]).await;
}

fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[tokio::test]
async fn uploads_creates_polls_fetches_and_cleans_up() {
    let server = server().await;
    upload_and_create(&server).await;
    let (queued, processing, completed) = (job("queued"), job("processing"), job("completed"));
    on(&server, "GET", "/v1/transcriptions/t1", &[(200, &queued), (200, &processing), (200, &completed)]).await;
    on(
        &server,
        "GET",
        "/v1/transcriptions/t1/transcript",
        &[(200, r#"{"id":"t1","text":" Hello there. ","tokens":[]}"#)],
    )
    .await;
    let entry = SonioxModel {
        id: "stt-async-v6".into(),
        transcription_mode: Some("async".into()),
        supports_language_hints_strict: true,
    };

    let text = run(&stt(&server, Some("sx-test"), Some(entry)), "en", TIMEOUT, &[]).await;

    assert_eq!(Ok("Hello there.".into()), text);
    let recorded = server.received_requests().await.unwrap();
    let upload = &recorded[0];
    assert_eq!("POST /v1/files", format!("{} {}", upload.method, upload.url.path()));
    assert_eq!("Bearer sx-test", upload.headers.get("authorization").unwrap());
    assert!(upload.headers.get("content-type").unwrap().to_str().unwrap().starts_with("multipart/form-data"));
    let form: String = upload.body.iter().map(|&b| b as char).collect(); // ISO-8859-1
    assert!(form.contains(r#"name="file"; filename="audio.wav""#));
    assert!(form.to_lowercase().contains("content-type: audio/wav"));
    assert!(form.contains("RIFF"));
    let create = &recorded[1];
    assert_eq!("POST /v1/transcriptions", format!("{} {}", create.method, create.url.path()));
    assert_eq!("Bearer sx-test", create.headers.get("authorization").unwrap());
    assert_eq!(
        json(r#"{"model":"stt-async-v6","file_id":"f1","language_hints":["en"],"language_hints_strict":true}"#),
        serde_json::from_slice::<Value>(&create.body).unwrap()
    );
    assert_eq!(
        vec![
            "/v1/transcriptions/t1",
            "/v1/transcriptions/t1",
            "/v1/transcriptions/t1",
            "/v1/transcriptions/t1/transcript"
        ],
        paths(&server, "GET").await
    );
    await_request(&server, "DELETE", "/v1/transcriptions/t1").await;
    let delete = await_request(&server, "DELETE", "/v1/files/f1").await;
    assert_eq!("Bearer sx-test", delete.headers.get("authorization").unwrap());
}

#[tokio::test]
async fn strict_is_only_sent_when_supported() {
    let unsupported = SonioxModel {
        id: "stt-async-v6".into(),
        transcription_mode: Some("async".into()),
        supports_language_hints_strict: false,
    };
    for entry in [None, Some(unsupported)] {
        let server = server().await;
        upload_and_create(&server).await;
        on(&server, "GET", "/v1/transcriptions/t1", &[(200, &job("completed"))]).await;
        on(&server, "GET", "/v1/transcriptions/t1/transcript", &[(200, r#"{"id":"t1","text":"Hi"}"#)]).await;

        run(&stt(&server, Some("sx-test"), entry), "en", TIMEOUT, &[]).await.unwrap();

        let create = await_request(&server, "POST", "/v1/transcriptions").await;
        assert_eq!(
            json(r#"{"model":"stt-async-v6","file_id":"f1","language_hints":["en"]}"#),
            serde_json::from_slice::<Value>(&create.body).unwrap()
        );
    }
}

#[tokio::test]
async fn custom_words_go_in_the_context() {
    let server = server().await;
    upload_and_create(&server).await;
    on(&server, "GET", "/v1/transcriptions/t1", &[(200, &job("completed"))]).await;
    on(&server, "GET", "/v1/transcriptions/t1/transcript", &[(200, r#"{"id":"t1","text":"Hi"}"#)]).await;

    run(&stt(&server, Some("sx-test"), None), "", TIMEOUT, &["Soniox", "Jamius Siam"]).await.unwrap();

    let create = await_request(&server, "POST", "/v1/transcriptions").await;
    assert_eq!(
        json(r#"{"model":"stt-async-v6","file_id":"f1","context":{"terms":["Soniox","Jamius Siam"]}}"#),
        serde_json::from_slice::<Value>(&create.body).unwrap()
    );
}

#[tokio::test]
async fn failed_job_fails_and_cleans_up() {
    for status in ["error", "failed"] {
        let server = server().await;
        upload_and_create(&server).await;
        let body = format!(
            r#"{{"id":"t1","status":"{status}","error_type":"invalid_audio_file",
                "error_message":"Bad audio"}}"#
        );
        on(&server, "GET", "/v1/transcriptions/t1", &[(200, &body)]).await;

        let error = run(&stt(&server, Some("sx-test"), None), "", TIMEOUT, &[]).await;

        assert_eq!("Soniox error: Bad audio", failure(error));
        await_request(&server, "DELETE", "/v1/transcriptions/t1").await;
        await_request(&server, "DELETE", "/v1/files/f1").await;
    }
}

#[tokio::test]
async fn upload_error_maps_and_deletes_nothing() {
    let server = server().await;
    on(
        &server,
        "POST",
        "/v1/files",
        &[(401, r#"{"status_code":401,"error_type":"unauthenticated","message":"Bad key"}"#)],
    )
    .await;

    let error = run(&stt(&server, Some("sx-test"), None), "", TIMEOUT, &[]).await;

    assert_eq!("Soniox rejected the API key. Check it in Iris settings.", failure(error));
    sleep(Duration::from_millis(300)).await;
    assert!(paths(&server, "DELETE").await.is_empty());
}

#[tokio::test]
async fn create_error_deletes_the_file() {
    let server = server().await;
    on(&server, "POST", "/v1/files", &[(201, r#"{"id":"f1"}"#)]).await;
    on(
        &server,
        "POST",
        "/v1/transcriptions",
        &[(
            400,
            r#"{"status_code":400,"error_type":"invalid_request","message":"",
                "validation_errors":[{"error_type":"invalid","location":"model","message":"Model not found"}]}"#,
        )],
    )
    .await;

    let error = run(&stt(&server, Some("sx-test"), None), "", TIMEOUT, &[]).await;

    assert_eq!("Soniox error: Model not found", failure(error));
    await_request(&server, "DELETE", "/v1/files/f1").await;
    sleep(Duration::from_millis(200)).await;
    assert_eq!(vec!["/v1/files/f1"], paths(&server, "DELETE").await);
}

#[tokio::test]
async fn poll_deadline_times_out_and_cleans_up() {
    let server = server().await;
    upload_and_create(&server).await;
    let processing = job("processing");
    on(&server, "GET", "/v1/transcriptions/t1", &[(200, processing.as_str()); 20]).await;

    // 300 ms setting + 50 ms of audio / 4: the poll deadline.
    let error = run(&stt(&server, Some("sx-test"), None), "", Duration::from_millis(300), &[]).await;

    assert_eq!("Timed out waiting for the transcript", failure(error));
    await_request(&server, "DELETE", "/v1/transcriptions/t1").await;
    await_request(&server, "DELETE", "/v1/files/f1").await;
}

#[tokio::test]
async fn cancel_mid_poll_deletes_the_file() {
    let server = server().await;
    upload_and_create(&server).await;
    let processing = job("processing");
    on(&server, "GET", "/v1/transcriptions/t1", &[(200, processing.as_str()); 20]).await;

    let stt = Arc::new(stt(&server, Some("sx-test"), None));
    let session = tokio::spawn({
        let stt = stt.clone();
        async move { run(&stt, "", TIMEOUT, &[]).await }
    });
    await_request(&server, "GET", "/v1/transcriptions/t1").await;
    session.abort();
    let _ = session.await;

    await_request(&server, "DELETE", "/v1/files/f1").await;
    await_request(&server, "DELETE", "/v1/transcriptions/t1").await;
}

#[tokio::test]
async fn malformed_transcript_is_unexpected() {
    let server = server().await;
    upload_and_create(&server).await;
    on(&server, "GET", "/v1/transcriptions/t1", &[(200, &job("completed"))]).await;
    on(&server, "GET", "/v1/transcriptions/t1/transcript", &[(200, r#"{"id":"t1","tokens":[]}"#)]).await;

    let error = run(&stt(&server, Some("sx-test"), None), "", TIMEOUT, &[]).await;

    assert_eq!("Unexpected response from Soniox", failure(error));
    await_request(&server, "DELETE", "/v1/files/f1").await;
}

#[tokio::test]
async fn blank_transcript_is_returned_blank() {
    let server = server().await;
    upload_and_create(&server).await;
    on(&server, "GET", "/v1/transcriptions/t1", &[(200, &job("completed"))]).await;
    on(&server, "GET", "/v1/transcriptions/t1/transcript", &[(200, r#"{"id":"t1","text":"  ","tokens":[]}"#)]).await;

    // The controller turns a blank transcript into "No speech detected".
    assert_eq!(Ok(String::new()), run(&stt(&server, Some("sx-test"), None), "", TIMEOUT, &[]).await);
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
async fn missing_api_key_fails_before_recording() {
    let server = server().await;
    let collected = Arc::new(AtomicBool::new(false));

    let stt = stt(&server, None, None);
    let error = run_with(&stt, Box::new(Untouched(collected.clone())), request("", TIMEOUT, &[])).await;

    assert_eq!("Add your Soniox API key in Iris settings", failure(error));
    assert!(!collected.load(Ordering::SeqCst));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn invalid_api_key_fails_before_recording() {
    let server = server().await;
    let collected = Arc::new(AtomicBool::new(false));

    let stt = stt(&server, Some("bad\u{a0}key"), None);
    let error = run_with(&stt, Box::new(Untouched(collected.clone())), request("", TIMEOUT, &[])).await;

    assert_eq!("Invalid Soniox API key", failure(error));
    assert!(!collected.load(Ordering::SeqCst));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn uploads_after_stop_at_16k_hz() {
    let server = server().await;
    let stt = stt(&server, Some("sx-test"), None);
    assert!(!stt.streams());
    assert!(!stt.live_text("stt-async-v6"));
    assert!(stt.allows_speed_up());
    assert_eq!(16_000, stt.sample_rate("stt-async-v6"));
    assert!(stt.takes_custom_words("stt-async-v6"));
}

#[tokio::test]
async fn warm_up_is_a_keyless_head_throttled() {
    let server = server().await;
    Mock::given(method("HEAD")).respond_with(ResponseTemplate::new(405)).mount(&server).await;
    let stt = stt(&server, Some("sx-test"), None);

    stt.warm_up();
    stt.warm_up(); // Within the window: skipped.
    let warm_up = await_request(&server, "HEAD", "/v1/models").await;

    assert!(warm_up.headers.get("authorization").is_none());
    sleep(Duration::from_millis(500)).await;
    assert_eq!(1, server.received_requests().await.unwrap().len());
}
