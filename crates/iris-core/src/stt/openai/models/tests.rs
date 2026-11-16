/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `OpenAiModelsTest.kt` against wiremock.

use super::*;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

async fn list_at(server: &MockServer, api_key: Option<&str>, timeout: Duration) -> Result<Vec<String>, SttError> {
    let url = format!("{}/v1/models", server.uri());
    list_models_at(&net::http_client(), api_key.map(String::from), &url, timeout).await
}

async fn list(server: &MockServer, api_key: Option<&str>) -> Result<Vec<String>, SttError> {
    list_at(server, api_key, MODELS_TIMEOUT).await
}

/// Answers the next request (only) with `code` and `body`.
async fn respond(server: &MockServer, code: u16, body: &str) {
    Mock::given(any())
        .respond_with(ResponseTemplate::new(code).set_body_string(body))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

fn failure(result: Result<Vec<String>, SttError>) -> String {
    result.expect_err("should fail").0
}

fn models(ids: &[&str]) -> String {
    let models: Vec<String> = ids
        .iter()
        .map(|id| {
            format!(r#"{{"id":"{id}","object":"model","created":1686935002,"owned_by":"openai","shutdown_date":null}}"#)
        })
        .collect();
    format!(r#"{{"object":"list","data":[{}]}}"#, models.join(","))
}

#[tokio::test]
async fn sends_get_with_bearer_key() {
    let server = MockServer::start().await;
    respond(&server, 200, &models(&[])).await;

    list(&server, Some("sk-test")).await.unwrap();

    let requests = server.received_requests().await.unwrap();
    assert_eq!("GET", requests[0].method.as_str());
    assert_eq!("/v1/models", requests[0].url.path());
    assert_eq!("Bearer sk-test", requests[0].headers["authorization"]);
}

#[tokio::test]
async fn keeps_transcription_models_sorted_by_id() {
    let server = MockServer::start().await;
    respond(&server, 200, &models(&["gpt-live-transcribe", "gpt-6-luna", "whisper-1", "GPT-4o-Transcribe", "tts-1"]))
        .await;

    assert_eq!(
        vec!["GPT-4o-Transcribe", "gpt-live-transcribe", "whisper-1"],
        list(&server, Some("sk-test")).await.unwrap()
    );
}

#[test]
fn skips_entries_without_string_id() {
    assert_eq!(
        Some(vec!["whisper-1".to_string()]),
        parse_transcription_model_ids(r#"{"data":[{"id":"whisper-1"},{"id":7},{"object":"model"},"x"]}"#)
    );
}

#[tokio::test]
async fn without_key_nothing_is_requested() {
    let server = MockServer::start().await;
    for key in [None, Some(" ")] {
        assert_eq!("Set your OpenAI API key to load models", failure(list(&server, key).await));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn key_that_cannot_be_a_header_is_rejected_without_request() {
    let server = MockServer::start().await;
    for key in ["sk-tést", "sk-test\n"] {
        assert_eq!("Invalid OpenAI API key", failure(list(&server, Some(key)).await));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn http_errors_map_to_readable_messages() {
    let server = MockServer::start().await;
    let cases = [
        (401, "OpenAI rejected the API key. Check it in Iris settings."),
        (403, "OpenAI rejected the API key. Check it in Iris settings."),
        (429, "OpenAI rate limit or quota reached"),
        (503, "OpenAI server error (HTTP 503)"),
        (404, "OpenAI error: Not found"),
    ];
    for (code, message) in cases {
        respond(&server, code, r#"{"error":{"message":"Not found"}}"#).await;
        assert_eq!(message, failure(list(&server, Some("sk-test")).await));
    }
}

#[tokio::test]
async fn malformed_body_is_unexpected() {
    let server = MockServer::start().await;
    for body in ["not json", r#"{"object":"list"}"#, r#"{"data":{}}"#] {
        respond(&server, 200, body).await;
        assert_eq!("Unexpected response from OpenAI", failure(list(&server, Some("sk-test")).await));
    }
}

#[tokio::test]
async fn timeout_has_its_own_message() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string(models(&[])).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;

    let result = list_at(&server, Some("sk-test"), Duration::from_millis(200)).await;

    assert_eq!("Loading models timed out", failure(result));
}

#[tokio::test]
async fn network_error_is_short() {
    // A port nothing listens on any more.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/models", listener.local_addr().unwrap());
    drop(listener);

    let result = list_models_at(&net::http_client(), Some("sk-test".into()), &url, MODELS_TIMEOUT).await;

    assert_eq!("Couldn't load models (network error)", failure(result));
}
