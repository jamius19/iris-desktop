/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `VercelGatewayFileTranscriptionTest.kt` against wiremock.

use std::sync::atomic::{AtomicBool, Ordering};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;

const TEST_TIMEOUT: Duration = Duration::from_secs(15);
const TIMEOUT_MS: u64 = 10_000;
const MODEL: &str = "openai/gpt-4o-transcribe";

/// 50 ms each at 16 kHz.
fn chunk_a() -> Vec<u8> {
    vec![1; 1_600]
}

fn chunk_b() -> Vec<u8> {
    vec![2; 1_600]
}

/// Audio of `chunks`; sets `pulled` once read.
struct Audio {
    chunks: Vec<Vec<u8>>,
    pulled: Arc<AtomicBool>,
}

impl AudioSource for Audio {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        self.pulled.store(true, Ordering::SeqCst);
        let chunk = (!self.chunks.is_empty()).then(|| self.chunks.remove(0));
        Box::pin(async move { Ok(chunk) })
    }
}

fn audio(chunks: Vec<Vec<u8>>) -> Audio {
    Audio { chunks, pulled: Arc::default() }
}

fn stt_with(server: &MockServer, api_key: Option<&str>) -> VercelGatewayFileTranscription {
    let key = api_key.map(str::to_string);
    let api_key: KeySource = Arc::new(move || Ok(key.clone()));
    VercelGatewayFileTranscription::with_urls(
        net::http_client(),
        api_key,
        &format!("{}/v4/ai/transcription-model", server.uri()),
        &format!("{}/v1/models", server.uri()),
        Duration::ZERO,
    )
}

fn stt(server: &MockServer) -> VercelGatewayFileTranscription {
    stt_with(server, Some("vck-test"))
}

async fn run_with(
    stt: &VercelGatewayFileTranscription,
    audio: Audio,
    model: &str,
    language: &str,
    timeout_ms: u64,
) -> Result<String, SttError> {
    let request = SttRequest {
        model: model.into(),
        language: language.into(),
        timeout: Duration::from_millis(timeout_ms),
        custom_words: Vec::new(),
    };
    let on_partial: OnPartial = Box::new(|_| panic!("No live text"));
    tokio::time::timeout(TEST_TIMEOUT, stt.transcribe(Box::new(audio), request, on_partial))
        .await
        .expect("test timed out")
}

async fn run(stt: &VercelGatewayFileTranscription) -> Result<String, SttError> {
    run_with(stt, audio(vec![chunk_a(), chunk_b()]), MODEL, "", TIMEOUT_MS).await
}

async fn respond(server: &MockServer, status: u16, body: &str) {
    server.reset().await;
    Mock::given(method("POST")).respond_with(ResponseTemplate::new(status).set_body_string(body)).mount(server).await;
}

async fn request_json(server: &MockServer) -> Value {
    let requests = server.received_requests().await.unwrap();
    serde_json::from_slice(&requests[0].body).unwrap()
}

#[tokio::test]
async fn uploads_the_recording_as_base64_wav_and_returns_the_text() {
    let server = MockServer::start().await;
    respond(
        &server,
        200,
        r#"{"text":" Bonjour. ","segments":[],"language":"fr","durationInSeconds":1.25,"warnings":[]}"#,
    )
    .await;

    let text =
        run_with(&stt(&server), audio(vec![chunk_a(), chunk_b()]), "openai/gpt-transcribe", "fr", TIMEOUT_MS).await;

    assert_eq!(Ok("Bonjour.".to_string()), text);
    let request = &server.received_requests().await.unwrap()[0];
    let header = |name: &str| request.headers.get(name).map(|v| v.to_str().unwrap().to_string());
    assert_eq!("POST", request.method.as_str());
    assert_eq!("/v4/ai/transcription-model", request.url.path());
    assert_eq!(None, request.url.query());
    assert_eq!(Some("Bearer vck-test".into()), header("Authorization"));
    assert_eq!(Some("0.0.1".into()), header("ai-gateway-protocol-version"));
    assert_eq!(Some("api-key".into()), header("ai-gateway-auth-method"));
    assert_eq!(Some("4".into()), header("ai-transcription-model-specification-version"));
    assert_eq!(Some("openai/gpt-transcribe".into()), header("ai-model-id"));
    assert_eq!(Some("application/json"), header("Content-Type").as_deref().map(|v| v.split(';').next().unwrap()));
    let json: Value = serde_json::from_slice(&request.body).unwrap();
    // 100 ms of audio: padded to 1.25 s.
    let wav = STANDARD.decode(json["audio"].as_str().unwrap()).unwrap();
    assert_eq!(wav_bytes(&[chunk_a(), chunk_b()], 16_000), wav);
    assert_eq!(44 + 40_000, wav.len());
    assert_eq!("audio/wav", json["mediaType"]);
    assert_eq!(serde_json::json!({"openai": {"languages": ["fr"]}}), json["providerOptions"]);
}

#[tokio::test]
async fn no_options_without_a_language() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"text":"Hi"}"#).await;

    run_with(&stt(&server), audio(vec![chunk_a(), chunk_b()]), "spacexai/grok-stt", "", TIMEOUT_MS).await.unwrap();

    assert!(request_json(&server).await.get("providerOptions").is_none());
}

#[tokio::test]
async fn uploads_after_stop_at_16k_hz() {
    let server = MockServer::start().await;
    let stt = stt(&server);
    assert!(!stt.streams());
    assert_eq!(16_000, stt.sample_rate("openai/gpt-4o-transcribe"));
    assert!(!stt.takes_custom_words("openai/gpt-4o-transcribe"));
}

#[tokio::test]
async fn http_errors_map_like_cleanup() {
    let server = MockServer::start().await;
    let stt = stt(&server);

    respond(&server, 401, "").await;
    assert_eq!(Err(SttError("AI Gateway rejected the API key. Check it in Iris settings.".into())), run(&stt).await);
    respond(&server, 429, "").await;
    assert_eq!(Err(SttError("AI Gateway rate limit or quota reached".into())), run(&stt).await);
    respond(&server, 502, "").await;
    assert_eq!(Err(SttError("AI Gateway server error (HTTP 502)".into())), run(&stt).await);
    respond(&server, 400, r#"{"error":{"message":"Model acme/stt not found","type":"model_not_found"}}"#).await;
    assert_eq!(Err(SttError("AI Gateway error: Model acme/stt not found".into())), run(&stt).await);
}

#[tokio::test]
async fn response_without_text_is_unexpected() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"segments":[]}"#).await;

    assert_eq!(Err(SttError("Unexpected response from AI Gateway".into())), run(&stt(&server)).await);
}

#[tokio::test]
async fn slow_transcript_times_out() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"text":"late"}"#).set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;

    let error = run_with(&stt(&server), audio(vec![chunk_a(), chunk_b()]), MODEL, "", 200).await;

    assert_eq!(Err(SttError("Timed out waiting for the transcript".into())), error);
}

#[tokio::test]
async fn cancelling_cancels_the_upload() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"text":"late"}"#).set_delay(Duration::from_secs(10)),
        )
        .mount(&server)
        .await;
    let stt = Arc::new(stt(&server));

    let upload = stt.clone();
    let job = tokio::spawn(async move { run(&upload).await });
    // The upload is in flight.
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.received_requests().await.unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    job.abort();

    let result = tokio::time::timeout(Duration::from_secs(5), job).await.unwrap();
    assert!(result.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn missing_api_key_fails_before_recording() {
    let server = MockServer::start().await;
    let audio = audio(Vec::new());
    let pulled = audio.pulled.clone();

    let error = run_with(&stt_with(&server, None), audio, MODEL, "", TIMEOUT_MS).await;

    assert_eq!(Err(SttError("Add your Vercel AI Gateway API key in Iris settings".into())), error);
    assert!(!pulled.load(Ordering::SeqCst));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn model_id_that_cant_be_a_header_fails_before_recording() {
    let server = MockServer::start().await;
    let audio = audio(Vec::new());
    let pulled = audio.pulled.clone();

    let error = run_with(&stt(&server), audio, "openai/gpt-ü", "", TIMEOUT_MS).await;

    assert_eq!(Err(SttError("Invalid model id: openai/gpt-ü".into())), error);
    assert!(!pulled.load(Ordering::SeqCst));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn warm_up_is_a_keyless_head_throttled() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    let stt = stt(&server);

    stt.warm_up();
    stt.warm_up(); // Within the window: skipped.
    tokio::time::sleep(Duration::from_millis(500)).await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(1, requests.len());
    assert_eq!("HEAD", requests[0].method.as_str());
    assert_eq!("/v1/models", requests[0].url.path());
    assert!(requests[0].headers.get("Authorization").is_none());
}
