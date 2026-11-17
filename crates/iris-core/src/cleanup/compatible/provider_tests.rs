/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `OpenAiCompatibleCleanupTest.kt` against wiremock, and the desktop https-only rule.

use serde_json::{Map, Value};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::cleanup::protocol::test_support::*;
use crate::cleanup::protocol::{PLAIN_TRANSCRIPT_FRAMING, TRANSCRIPT_FRAMING};
use crate::net::http_client;

fn cleanup_with(server: &MockServer, api_key: Option<&str>, send_json_schema: bool) -> OpenAiCompatibleCleanup {
    OpenAiCompatibleCleanup::with_timeout(
        http_client(),
        "Groq",
        // A trailing '/' is dropped: no double slash, and no /v1 is added.
        &format!("{}/openai/v1/", server.uri()),
        send_json_schema,
        key(api_key),
        Duration::from_secs(10),
    )
}

fn cleanup(server: &MockServer) -> OpenAiCompatibleCleanup {
    cleanup_with(server, Some("gsk-test"), true)
}

fn plain(server: &MockServer) -> OpenAiCompatibleCleanup {
    cleanup_with(server, Some("gsk-test"), false)
}

async fn clean_with(
    provider: &OpenAiCompatibleCleanup,
    model: &str,
    effort: Option<&str>,
) -> Result<String, CleanupError> {
    let request = CleanupRequest {
        text: "hello".into(),
        system_prompt: "Clean it.".into(),
        model: model.into(),
        reasoning_effort: effort.map(String::from),
        provider_options: None,
    };
    provider.cleanup(request).await
}

async fn clean(provider: &OpenAiCompatibleCleanup) -> Result<String, CleanupError> {
    clean_with(provider, "m", Some("low")).await
}

async fn failure(provider: &OpenAiCompatibleCleanup) -> String {
    clean(provider).await.unwrap_err().0
}

fn text(object: &Map<String, Value>, key: &str) -> String {
    object[key].as_str().unwrap().to_string()
}

#[test]
fn urls() {
    assert_eq!("https://a.example/v1/chat/completions", chat_completions_url("https://a.example/v1/"));
    assert_eq!("https://a.example/v1/models", models_url("https://a.example/v1"));
    assert_eq!("https://a.example/chat/completions", chat_completions_url("https://a.example"));
}

#[tokio::test]
async fn schema_on_sends_structured_request_and_parses_text() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"  Hello, world.  "}"#)).await;

    assert_eq!(Ok("Hello, world.".to_string()), clean_with(&cleanup(&server), "llama-4", Some("low")).await);

    let request = &requests(&server, 1).await[0];
    assert_eq!("POST", request.method.as_str());
    assert_eq!("/openai/v1/chat/completions", target(request));
    assert_eq!(Some("Bearer gsk-test".into()), header(request, "Authorization"));
    let body = body(request);
    assert_eq!("llama-4", text(&body, "model"));
    assert_eq!("low", text(&body, "reasoning_effort"));
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(format!("Clean it.\n\n{TRANSCRIPT_FRAMING}"), text(messages[0].as_object().unwrap(), "content"));
    assert_eq!("hello", text(messages[1].as_object().unwrap(), "content"));
    assert_eq!("json_schema", text(body["response_format"].as_object().unwrap(), "type"));
    assert!(!body.contains_key("provider"));
    assert!(!body.contains_key("providerOptions"));
    assert!(!body.contains_key(""));
}

#[tokio::test]
async fn schema_off_sends_plain_request_and_returns_trimmed_content() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed("  Hello, world.\n")).await;

    assert_eq!(Ok("Hello, world.".to_string()), clean(&plain(&server)).await);

    let body = body(&requests(&server, 1).await[0]);
    assert!(!body.contains_key("response_format"));
    let system = text(body["messages"][0].as_object().unwrap(), "content");
    assert_eq!(format!("Clean it.\n\n{PLAIN_TRANSCRIPT_FRAMING}"), system);
}

#[tokio::test]
async fn schema_off_strips_leading_think_block() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed("\n<THINK>Plan:\n<b>fix</b> commas</think>\n\n Hello, world. ")).await;

    assert_eq!(Ok("Hello, world.".to_string()), clean(&plain(&server)).await);
}

#[test]
fn think_block_stripping() {
    assert_eq!("Hi.", without_think_block("  <think>a\nb</think> Hi. "));
    assert_eq!("", without_think_block("<think>only thoughts</think>"));
    // Only a leading block: text without one, or a later one, is untouched (just trimmed).
    assert_eq!("Hello, world.", without_think_block(" Hello, world.\n"));
    assert_eq!("Hi <think>x</think>", without_think_block("Hi <think>x</think>"));
}

#[tokio::test]
async fn schema_off_think_only_content_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed("<think>thoughts</think>  ")).await;

    assert_eq!("Unexpected response from Groq", failure(&plain(&server)).await);
}

#[tokio::test]
async fn schema_off_blank_content_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed("   ")).await;

    assert_eq!("Unexpected response from Groq", failure(&plain(&server)).await);
}

#[tokio::test]
async fn schema_on_content_that_is_not_the_schema_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed("Hello, world.")).await;

    assert_eq!("Unexpected response from Groq", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn omits_reasoning_effort_for_model_default() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;

    clean_with(&cleanup(&server), "m", None).await.unwrap();

    assert!(!body(&requests(&server, 1).await[0]).contains_key("reasoning_effort"));
}

#[tokio::test]
async fn missing_key_or_blank_model_fails_without_calling() {
    let server = MockServer::start().await;

    assert_eq!("Add your Groq API key in Iris settings", failure(&cleanup_with(&server, None, true)).await);
    assert_eq!(
        Err(CleanupError("Set a cleanup model for Groq in Iris settings".into())),
        clean_with(&cleanup(&server), " ", Some("low")).await
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unsendable_api_key_fails_without_calling() {
    let server = MockServer::start().await;

    for key in ["gsk\u{a0}test", "gsk-test\n"] {
        assert_eq!("Invalid Groq API key", failure(&cleanup_with(&server, Some(key), true)).await);
        assert_eq!(
            Err(CleanupError("Invalid Groq API key".into())),
            cleanup_with(&server, Some(key), true).list_models().await
        );
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn http_errors_name_the_provider() {
    let server = MockServer::start().await;
    let cases = [
        (401, "Groq rejected the API key. Check it in Iris settings."),
        (429, "Groq rate limit or quota reached"),
        (500, "Groq server error (HTTP 500)"),
    ];
    for (code, message) in cases {
        respond(&server, code, r#"{"error":{"message":"ignored"}}"#).await;
        assert_eq!(message, failure(&cleanup(&server)).await);
    }
    respond(&server, 400, r#"{"error":{"message":"response_format is not supported"}}"#).await;
    assert_eq!("Groq error: response_format is not supported", failure(&cleanup(&server)).await);
    // A plain-string error (some local servers): the generic message.
    respond(&server, 400, r#"{"error":"bad request"}"#).await;
    assert_eq!("Groq request failed (HTTP 400)", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn cut_off_and_refusal_fail() {
    let server = MockServer::start().await;
    respond(&server, 200, &completion(Some(r#"{"text":"Hel"#), None, Some("length"))).await;
    assert_eq!("Cleanup output was cut off", failure(&cleanup(&server)).await);

    respond(&server, 200, &completion(None, Some("I can't help with that."), Some("stop"))).await;
    assert_eq!("Model refused: I can't help with that.", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn list_models_sends_key_and_keeps_every_id() {
    let server = MockServer::start().await;
    let body = r#"{"object":"list","data":[
        {"id":"llama-4","object":"model","type":"chat"},
        {"id":"whisper-large-v3","object":"model"},
        {"id":" "}
    ]}"#;
    respond(&server, 200, body).await;

    let models = cleanup(&server).list_models().await.unwrap();

    let model = |id: &str, r#type: Option<&str>| GatewayModel {
        id: id.into(),
        r#type: r#type.map(String::from),
        tags: Vec::new(),
        reasoning_efforts: None,
    };
    assert_eq!(vec![model("llama-4", Some("chat")), model("whisper-large-v3", None)], models);
    let request = &requests(&server, 1).await[0];
    assert_eq!("GET", request.method.as_str());
    assert_eq!("/openai/v1/models", target(request));
    assert_eq!(Some("Bearer gsk-test".into()), header(request, "Authorization"));
}

#[tokio::test]
async fn list_models_without_key_fails_without_calling() {
    let server = MockServer::start().await;

    assert_eq!(
        Err(CleanupError("Set your Groq API key to load models".into())),
        cleanup_with(&server, None, true).list_models().await
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// Reduced: Android also asserts the cleanup reuses the warm-up's connection (OkHttp's `connectionIndex`).
#[tokio::test]
async fn warm_up_is_a_keyless_head_and_cleanup_reuses_its_connection() {
    let server = MockServer::start().await;
    let completion = ResponseTemplate::new(200).set_body_string(completed(r#"{"text":"Hi."}"#));
    Mock::given(method("HEAD")).respond_with(ResponseTemplate::new(401)).mount(&server).await;
    Mock::given(method("POST")).respond_with(completion).mount(&server).await;
    let provider = cleanup(&server);

    provider.warm_up();
    let warm_up = &requests(&server, 1).await[0];
    assert_eq!("HEAD", warm_up.method.as_str());
    assert_eq!("/openai/v1/models", target(warm_up));
    assert_eq!(None, header(warm_up, "Authorization"));
    assert_eq!(Ok("Hi.".to_string()), clean(&provider).await);

    assert_eq!("POST", requests(&server, 2).await[1].method.as_str());
}

/// Desktop: custom base URLs are https only; the production constructor refuses any other without a request.
#[tokio::test]
async fn an_http_base_url_is_refused_without_a_request() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;
    let provider =
        OpenAiCompatibleCleanup::new(http_client(), "Groq", &format!("{}/v1", server.uri()), true, key(Some("gsk")));

    let refused = CleanupError("Groq: Base URL must start with https://".into());
    assert_eq!(refused, clean(&provider).await.unwrap_err());
    assert_eq!(refused, provider.list_models().await.unwrap_err());
    provider.warm_up();
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert!(server.received_requests().await.unwrap().is_empty());
}
