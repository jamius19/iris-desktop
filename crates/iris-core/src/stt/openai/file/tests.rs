/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `OpenAiFileTranscriptionTest.kt` against wiremock.

use super::*;
use futures_util::StreamExt;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use wiremock::matchers::{any, method};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TIMEOUT: Duration = Duration::from_secs(10);

/// 50 ms each at 16 kHz.
fn chunk_a() -> Vec<u8> {
    vec![1; 1_600]
}

fn chunk_b() -> Vec<u8> {
    vec![2; 1_600]
}

struct Chunks(VecDeque<Vec<u8>>);

impl AudioSource for Chunks {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        Box::pin(async move { Ok(self.0.pop_front()) })
    }
}

fn stt(server: &MockServer, api_key: &str, max_part_bytes: usize) -> OpenAiFileTranscription {
    let api_key = api_key.to_string();
    OpenAiFileTranscription::with_urls(
        Arc::new(move || Ok(Some(api_key.clone()))),
        format!("{}/v1/audio/transcriptions", server.uri()),
        format!("{}/v1/models", server.uri()),
        max_part_bytes,
    )
}

fn default_stt(server: &MockServer) -> OpenAiFileTranscription {
    stt(server, "sk-test", 20_000_000)
}

fn request(model: &str, language: &str, timeout: Duration, words: &[&str]) -> SttRequest {
    SttRequest {
        model: model.into(),
        language: language.into(),
        timeout,
        custom_words: words.iter().map(|w| w.to_string()).collect(),
    }
}

/// Transcribes `audio`, failing the test instead of hanging; live text fails the test.
async fn run_with(
    stt: &OpenAiFileTranscription,
    audio: Box<dyn AudioSource + '_>,
    request: SttRequest,
) -> Result<String, SttError> {
    let on_partial: OnPartial = Box::new(|_| panic!("No live text"));
    tokio::time::timeout(Duration::from_secs(15), stt.transcribe(audio, request, on_partial)).await.expect("hung")
}

async fn run(stt: &OpenAiFileTranscription, request: SttRequest) -> Result<String, SttError> {
    run_with(stt, Box::new(Chunks(VecDeque::from([chunk_a(), chunk_b()]))), request).await
}

fn custom(language: &str) -> SttRequest {
    request("custom-transcribe", language, TIMEOUT, &[])
}

/// Answers the next request (only) with `code` and `body`.
async fn respond(server: &MockServer, code: u16, body: &str) {
    Mock::given(any())
        .respond_with(ResponseTemplate::new(code).set_body_string(body))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

fn failure(result: Result<String, SttError>) -> String {
    result.expect_err("should fail").0
}

/// A multipart form field: name, file name, content type, bytes.
#[derive(Debug)]
struct Field {
    name: String,
    file_name: Option<String>,
    content_type: Option<String>,
    body: Vec<u8>,
}

fn find(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// The fields of a multipart `body` with `boundary`, in order.
fn parse_form(body: &[u8], boundary: &str) -> Vec<Field> {
    let delimiter = format!("--{boundary}");
    let mut fields = Vec::new();
    let mut at = find(body, delimiter.as_bytes(), 0).expect("first boundary") + delimiter.len();
    while body[at..].starts_with(b"\r\n") {
        let headers_end = find(body, b"\r\n\r\n", at).expect("part headers");
        let headers = String::from_utf8(body[at + 2..headers_end].to_vec()).unwrap();
        let next = find(body, format!("\r\n{delimiter}").as_bytes(), headers_end).expect("next boundary");
        let header = |name: &str| {
            headers
                .lines()
                .find_map(|line| line.split_once(": ").filter(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, v)| v))
                .map(String::from)
        };
        let disposition = header("content-disposition").expect("content disposition");
        let quoted = |key: &str| {
            disposition.split_once(&format!("{key}=\"")).map(|(_, rest)| rest.split('"').next().unwrap().to_string())
        };
        fields.push(Field {
            name: quoted(" name").expect("name"),
            file_name: quoted("filename"),
            content_type: header("content-type"),
            body: body[headers_end + 4..next].to_vec(),
        });
        at = next + 2 + delimiter.len();
    }
    fields
}

/// The form fields of the `index`th request the server got.
async fn fields(server: &MockServer, index: usize) -> Vec<Field> {
    let request = &server.received_requests().await.unwrap()[index];
    let content_type = request.headers["content-type"].to_str().unwrap();
    let boundary = content_type.split_once("boundary=").expect("boundary").1;
    parse_form(&request.body, boundary)
}

fn field<'a>(fields: &'a [Field], name: &str) -> &'a Field {
    fields.iter().find(|f| f.name == name).unwrap_or_else(|| panic!("no {name}"))
}

fn value(fields: &[Field], name: &str) -> String {
    String::from_utf8(field(fields, name).body.clone()).unwrap()
}

fn names(fields: &[Field]) -> Vec<&str> {
    fields.iter().map(|f| f.name.as_str()).collect()
}

#[tokio::test]
async fn uploads_the_recording_as_wav_and_returns_the_text() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"text":" Bonjour tout le monde. ","usage":{"type":"duration","seconds":2}}"#).await;

    let text = run(&default_stt(&server), custom("fr")).await;

    assert_eq!(Ok("Bonjour tout le monde.".into()), text);
    let request = &server.received_requests().await.unwrap()[0];
    assert_eq!("POST", request.method.as_str());
    assert_eq!("/v1/audio/transcriptions", request.url.path());
    assert_eq!("Bearer sk-test", request.headers["authorization"]);
    let fields = fields(&server, 0).await;
    assert_eq!(vec!["file", "model", "response_format", "language"], names(&fields));
    let file = field(&fields, "file");
    assert_eq!(Some("audio.wav"), file.file_name.as_deref());
    assert_eq!(Some("audio/wav"), file.content_type.as_deref());
    assert_eq!(wav_bytes(&[chunk_a(), chunk_b()], 16_000), file.body);
    assert_eq!("custom-transcribe", value(&fields, "model"));
    assert_eq!("json", value(&fields, "response_format"));
    assert_eq!("fr", value(&fields, "language"));
}

#[tokio::test]
async fn gpt_transcribe_takes_the_languages_list() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"text":"Hi"}"#).await;

    run(&default_stt(&server), request("gpt-transcribe", "en", TIMEOUT, &[])).await.unwrap();

    let fields = fields(&server, 0).await;
    assert_eq!("en", value(&fields, "languages[]"));
    assert!(!names(&fields).contains(&"language"));
}

#[tokio::test]
async fn blank_language_sends_none() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"text":"Hi"}"#).await;

    run(&default_stt(&server), custom(" ")).await.unwrap();

    assert_eq!(vec!["file", "model", "response_format"], names(&fields(&server, 0).await));
}

/// The form's fields in order: name to text.
async fn form_fields(form: Form) -> Vec<(String, String)> {
    let boundary = form.boundary().to_string();
    let body: Vec<u8> = form.into_stream().map(|chunk| chunk.unwrap().to_vec()).concat().await;
    parse_form(&body, &boundary).into_iter().map(|f| (f.name, String::from_utf8(f.body).unwrap())).collect()
}

async fn form(model: &str, words: &[String]) -> Vec<(String, String)> {
    form_fields(transcription_form(vec![1], model, "", words, watch::channel(false).0)).await
}

#[tokio::test]
async fn custom_words_go_as_keywords_a_prompt_or_nothing() {
    let words = vec!["Deepgram".to_string(), "Jamius Siam".to_string()];
    let pair = |name: &str, value: &str| (name.to_string(), value.to_string());
    assert_eq!(
        vec![pair("keywords[]", "Deepgram"), pair("keywords[]", "Jamius Siam")],
        form("gpt-transcribe", &words).await[3..]
    );
    for model in ["whisper-1", "gpt-4o-transcribe", "gpt-4o-mini-transcribe"] {
        assert_eq!(vec![pair("prompt", "Glossary: Deepgram, Jamius Siam")], form(model, &words).await[3..]);
    }
    for model in ["gpt-4o-transcribe-diarize", "custom-transcribe"] {
        assert_eq!(3, form(model, &words).await.len());
    }
    assert_eq!(3, form("whisper-1", &[]).await.len());
}

#[tokio::test]
async fn transcribe_sends_the_custom_words() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"text":"Hi"}"#).await;

    run(&default_stt(&server), request("gpt-4o-transcribe-2025-03-20", "", TIMEOUT, &["Soniox"])).await.unwrap();

    assert_eq!("Glossary: Soniox", value(&fields(&server, 0).await, "prompt"));
}

#[tokio::test]
async fn uploads_after_stop_at_16k_hz_without_live_text() {
    let server = MockServer::start().await;
    let stt = default_stt(&server);
    assert!(!stt.streams());
    assert!(!stt.live_text("custom-transcribe"));
    assert_eq!(16_000, stt.sample_rate("custom-transcribe"));
}

#[tokio::test]
async fn takes_custom_words_where_the_form_sends_them() {
    let server = MockServer::start().await;
    let stt = default_stt(&server);
    for model in ["gpt-transcribe", "gpt-live-transcribe", "whisper-1", "gpt-4o-transcribe"] {
        assert!(stt.takes_custom_words(model), "{model}");
    }
    assert!(stt.takes_custom_words("gpt-4o-mini-transcribe-2025-12-15"));
    assert!(!stt.takes_custom_words("gpt-realtime-whisper"));
    assert!(!stt.takes_custom_words("gpt-4o-transcribe-diarize"));
    assert!(!stt.takes_custom_words("custom-transcribe"));
}

#[tokio::test]
async fn over_the_limit_uploads_parts_in_order_and_joins_the_texts() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"text":"One."}"#).await;
    respond(&server, 200, r#"{"text":"Two."}"#).await;
    let chunks: Vec<Vec<u8>> = (0..4).map(|_| vec![1; 32_000]).collect(); // 1 s each.
    let audio = Box::new(Chunks(chunks.iter().cloned().collect()));

    let text = run_with(&stt(&server, "sk-test", 64_000), audio, custom("")).await;

    assert_eq!(Ok("One. Two.".into()), text);
    assert_eq!(2, server.received_requests().await.unwrap().len());
    for index in 0..2 {
        let file = &field(&fields(&server, index).await, "file").body.clone();
        assert_eq!(&wav_bytes(&chunks[2 * index..2 * index + 2], 16_000), file);
    }
}

#[tokio::test]
async fn missing_api_key_fails_before_recording() {
    struct Flagged(Arc<AtomicBool>);
    impl AudioSource for Flagged {
        fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
            self.0.store(true, Ordering::SeqCst);
            Box::pin(async { Ok(None) })
        }
    }
    let server = MockServer::start().await;
    let collected = Arc::new(AtomicBool::new(false));

    let error = run_with(&stt(&server, " ", 20_000_000), Box::new(Flagged(collected.clone())), custom("")).await;

    assert_eq!("Add your OpenAI API key in Iris settings", failure(error));
    assert!(!collected.load(Ordering::SeqCst));
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn http_errors_map_to_messages() {
    let server = MockServer::start().await;
    let stt = default_stt(&server);
    respond(&server, 401, "").await;
    assert_eq!("OpenAI rejected the API key. Check it in Iris settings.", failure(run(&stt, custom("")).await));
    respond(&server, 429, "").await;
    assert_eq!("OpenAI rate limit or quota reached", failure(run(&stt, custom("")).await));
    respond(&server, 500, "").await;
    assert_eq!("OpenAI server error (HTTP 500)", failure(run(&stt, custom("")).await));
    respond(&server, 400, r#"{"error":{"message":"Audio file is too short."}}"#).await;
    assert_eq!("OpenAI error: Audio file is too short.", failure(run(&stt, custom("")).await));
}

#[tokio::test]
async fn response_without_text_is_unexpected() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"error":null}"#).await;
    assert_eq!("Unexpected response from OpenAI", failure(run(&default_stt(&server), custom("")).await));
}

#[tokio::test]
async fn slow_transcript_times_out() {
    let server = MockServer::start().await;
    // 1.25 s of padded audio: a read timeout of 200 ms + 312 ms.
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"text":"late"}"#).set_delay(Duration::from_secs(3)),
        )
        .mount(&server)
        .await;

    let error = run(&default_stt(&server), request("custom-transcribe", "", Duration::from_millis(200), &[])).await;

    assert_eq!("Timed out waiting for the transcript", failure(error));
}

#[tokio::test]
async fn cancelling_cancels_the_upload() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(200).set_body_string(r#"{"text":"late"}"#).set_delay(Duration::from_secs(10)),
        )
        .mount(&server)
        .await;
    let stt = Arc::new(default_stt(&server));

    let job = tokio::spawn(async move { run(&stt, custom("")).await });
    // The upload is in flight.
    tokio::time::timeout(Duration::from_secs(5), async {
        while server.received_requests().await.unwrap().is_empty() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("upload sent");
    job.abort();

    let ended = tokio::time::timeout(Duration::from_secs(1), job).await.expect("cancelled promptly");
    assert!(ended.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn warm_up_is_a_keyless_head_throttled() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    let stt = default_stt(&server);

    stt.warm_up();
    stt.warm_up(); // Within the window: skipped.
    sleep(Duration::from_millis(500)).await;

    let requests = server.received_requests().await.unwrap();
    assert_eq!(1, requests.len());
    assert_eq!("HEAD", requests[0].method.as_str());
    assert_eq!("/v1/models", requests[0].url.path());
    assert!(!requests[0].headers.contains_key("authorization"));
}

// Desktop: the request marks when it has taken the whole file (Android `SentBody`).

#[tokio::test]
async fn sent_is_marked_once_the_file_is_taken() {
    let (sent, sent_rx) = watch::channel(false);
    let mut stream = Box::pin(transcription_form(vec![1; 100], "whisper-1", "", &[], sent).into_stream());

    // The file part: boundary, headers, the WAV.
    for _ in 0..3 {
        stream.next().await.unwrap().unwrap();
    }
    assert!(!*sent_rx.borrow());
    while stream.next().await.is_some() {}
    assert!(*sent_rx.borrow());
}
