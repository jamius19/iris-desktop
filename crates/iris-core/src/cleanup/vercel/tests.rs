/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `VercelGatewayCleanupTest.kt` against wiremock.

use std::sync::Arc;

use serde_json::{Map, Value, json};
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::cleanup::protocol::test_support::*;
use crate::cleanup::protocol::{TRANSCRIPT_FRAMING, is_chat_model};
use crate::cleanup::with_glossary;
use crate::net::http_client;

fn cleanup_with(server: &MockServer, api_key: Option<&str>, call_timeout_ms: u64) -> VercelGatewayCleanup {
    VercelGatewayCleanup::with_urls(
        http_client(),
        key(api_key),
        &format!("{}/v1/chat/completions", server.uri()),
        &format!("{}/v1/models", server.uri()),
        Duration::from_millis(call_timeout_ms),
    )
}

fn cleanup(server: &MockServer) -> VercelGatewayCleanup {
    cleanup_with(server, Some("vck-test"), 10_000)
}

fn request(text: &str, system_prompt: &str, model: &str, effort: Option<&str>) -> CleanupRequest {
    CleanupRequest {
        text: text.into(),
        system_prompt: system_prompt.into(),
        model: model.into(),
        reasoning_effort: effort.map(String::from),
        provider_options: None,
    }
}

async fn clean(provider: &VercelGatewayCleanup) -> Result<String, CleanupError> {
    provider.cleanup(request("hello", "Clean it.", "m", Some("low"))).await
}

async fn failure(provider: &VercelGatewayCleanup) -> String {
    clean(provider).await.unwrap_err().0
}

fn string(object: &Map<String, Value>, key: &str) -> String {
    object[key].as_str().unwrap().to_string()
}

async fn sent_body(server: &MockServer) -> Map<String, Value> {
    body(&requests(server, 1).await[0])
}

#[tokio::test]
async fn returns_the_parsed_text() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"  Hello, world.  "}"#)).await;

    let text = cleanup(&server).cleanup(request("um hello world", "Clean it.", "openai/gpt-6-luna", Some("low"))).await;

    assert_eq!(Ok("Hello, world.".to_string()), text);
}

#[tokio::test]
async fn sends_structured_request() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;

    cleanup(&server).cleanup(request("um hello world", "Clean it.", "openai/gpt-6-luna", Some("low"))).await.unwrap();

    let request = &requests(&server, 1).await[0];
    assert_eq!("POST", request.method.as_str());
    assert_eq!("/v1/chat/completions", target(request));
    assert_eq!(Some("Bearer vck-test".into()), header(request, "Authorization"));
    let body = body(request);
    assert_eq!("openai/gpt-6-luna", string(&body, "model"));
    assert!(!body.contains_key("stream"));
    assert_eq!("low", string(&body, "reasoning_effort"));
    let messages: Vec<&Map<String, Value>> =
        body["messages"].as_array().unwrap().iter().map(|m| m.as_object().unwrap()).collect();
    assert_eq!(vec!["system", "user"], messages.iter().map(|m| string(m, "role")).collect::<Vec<_>>());
    assert_eq!(format!("Clean it.\n\n{TRANSCRIPT_FRAMING}"), string(messages[0], "content"));
    assert_eq!("um hello world", string(messages[1], "content"));
    let expected_format = json!({"type":"json_schema","json_schema":{"name":"cleaned_text","strict":true,"schema":{
        "type":"object","properties":{"text":{"type":"string"}},"required":["text"],
        "additionalProperties":false}}});
    assert_eq!(expected_format, body["response_format"]);
    assert!(!body.contains_key("providerOptions"));
}

#[tokio::test]
async fn glossary_goes_between_the_prompt_and_the_framing() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;
    let prompt = with_glossary("Clean it.", &["Deepgram".into()]);

    cleanup(&server).cleanup(request("um hello world", &prompt, "openai/gpt-6-luna", Some("low"))).await.unwrap();

    let body = sent_body(&server).await;
    let system = string(body["messages"][0].as_object().unwrap(), "content");
    assert_eq!(format!("{prompt}\n\n{TRANSCRIPT_FRAMING}"), system);
    assert!(system.starts_with("Clean it.\n\n"));
    assert!(system.contains(&format!("<custom_words>\nDeepgram\n</custom_words>\n\n{TRANSCRIPT_FRAMING}")));
}

#[tokio::test]
async fn omits_reasoning_effort_for_model_default() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;

    cleanup(&server).cleanup(request("um hello world", "Clean it.", "openai/gpt-6-luna", None)).await.unwrap();

    assert!(!sent_body(&server).await.contains_key("reasoning_effort"));
}

#[tokio::test]
async fn sends_provider_options_under_provider_options() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;
    let options = json!({"gateway":{"order":["azure","openai"]}});

    let mut call = request("um hello world", "Clean it.", "openai/gpt-6-luna", Some("low"));
    call.provider_options = options.as_object().cloned();
    cleanup(&server).cleanup(call).await.unwrap();

    let body = sent_body(&server).await;
    assert_eq!(options, body["providerOptions"]);
    assert!(!body.contains_key("provider"));
}

#[tokio::test]
async fn escapes_decode() {
    let server = MockServer::start().await;
    let content = r#"{"text":"Line\none \"q\" back\\slash café 😀"}"#;
    respond(&server, 200, &completed(content)).await;

    assert_eq!(Ok("Line\none \"q\" back\\slash café 😀".to_string()), clean(&cleanup(&server)).await);
}

#[tokio::test]
async fn content_that_is_not_the_schema_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed("Hello, world.")).await;

    assert_eq!("Unexpected response from AI Gateway", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn blank_cleaned_text_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"  "}"#)).await;

    assert_eq!("Unexpected response from AI Gateway", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn malformed_responses_fail() {
    let server = MockServer::start().await;
    let bodies = [
        completion(None, None, Some("stop")),
        r#"{"id":"chatcmpl-1","choices":[]}"#.into(),
        "<html>oops</html>".into(),
        String::new(),
    ];
    for body in bodies {
        respond(&server, 200, &body).await;
        assert_eq!("Unexpected response from AI Gateway", failure(&cleanup(&server)).await, "{body}");
    }
}

#[tokio::test]
async fn http_errors_map_to_readable_messages() {
    let server = MockServer::start().await;
    let cases = [
        (401, "AI Gateway rejected the API key. Check it in Iris settings."),
        (403, "AI Gateway rejected the API key. Check it in Iris settings."),
        (429, "AI Gateway rate limit or quota reached"),
        (503, "AI Gateway server error (HTTP 503)"),
    ];
    for (code, message) in cases {
        respond(&server, code, r#"{"error":{"message":"ignored"}}"#).await;
        assert_eq!(message, failure(&cleanup(&server)).await);
    }
}

#[test]
fn http_error_message_names_the_service() {
    let error = r#"{"error":{"message":"Bad model"}}"#;
    assert_eq!("Groq rejected the API key. Check it in Iris settings.", http_error_message(403, "", "Groq"));
    assert_eq!("Groq rate limit or quota reached", http_error_message(429, "", "Groq"));
    assert_eq!("Groq server error (HTTP 502)", http_error_message(502, "", "Groq"));
    assert_eq!("Groq error: Bad model", http_error_message(400, error, "Groq"));
    assert_eq!("Groq request failed (HTTP 404)", http_error_message(404, "nope", "Groq"));
    assert_eq!("AI Gateway error: Bad model", http_error_message(400, error, AI_GATEWAY));
}

#[tokio::test]
async fn client_error_shows_gateway_message_when_present() {
    let server = MockServer::start().await;
    respond(&server, 400, r#"{"error":{"message":"Model not found","type":"invalid_request_error"}}"#).await;
    assert_eq!("AI Gateway error: Model not found", failure(&cleanup(&server)).await);

    respond(&server, 404, "not json").await;
    assert_eq!("AI Gateway request failed (HTTP 404)", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn slow_response_times_out() {
    let server = MockServer::start().await;
    let late = ResponseTemplate::new(200).set_body_string(completed(r#"{"text":"late"}"#));
    respond_with(&server, late.set_delay(Duration::from_secs(2))).await;

    assert_eq!("Cleanup timed out", failure(&cleanup_with(&server, Some("vck-test"), 200)).await);
}

#[tokio::test]
async fn cut_off_output_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, &completion(Some(r#"{"text":"Hel"#), None, Some("length"))).await;

    assert_eq!("Cleanup output was cut off", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn content_filter_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, &completion(None, None, Some("content_filter"))).await;

    assert_eq!("Cleanup blocked by content filter", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn refusal_is_the_error_message() {
    let server = MockServer::start().await;
    respond(&server, 200, &completion(None, Some("I can't help with that."), Some("stop"))).await;

    assert_eq!("Model refused: I can't help with that.", failure(&cleanup(&server)).await);
}

/// Android checks OkHttp's `canceled` event; here the server sees the connection close once the future is dropped.
#[tokio::test]
async fn cancelling_before_the_response_cancels_the_call() {
    let (url, received, closed) = hanging_server("/v1/chat/completions").await;
    let provider =
        Arc::new(VercelGatewayCleanup::with_urls(http_client(), key(Some("vck-test")), &url, &url, CALL_TIMEOUT));

    let call = tokio::spawn(async move { clean(&provider).await });
    within_5s(received).await; // The call is in flight.
    call.abort();

    within_5s(closed).await;
}

#[tokio::test]
async fn missing_api_key_fails_without_calling() {
    let server = MockServer::start().await;

    let error = failure(&cleanup_with(&server, None, 10_000)).await;

    assert_eq!("Add your Vercel AI Gateway API key in Iris settings", error);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unsendable_api_key_fails_without_calling() {
    let server = MockServer::start().await;

    for key in ["vck\u{a0}test", "vck-test\n"] {
        let error = failure(&cleanup_with(&server, Some(key), 10_000)).await;
        assert_eq!("Invalid Vercel AI Gateway API key", error);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// A `GET /v1/models` body; each pair is an id and its `type` (omitted when None).
fn models(models: &[(&str, Option<&str>)]) -> String {
    let data: Vec<Value> = models
        .iter()
        .map(|(id, r#type)| {
            let mut model = json!({ "id": id, "object": "model" });
            if let Some(r#type) = r#type {
                model["type"] = (*r#type).into();
            }
            model
        })
        .collect();
    json!({ "object": "list", "data": data }).to_string()
}

fn ids(models: &[GatewayModel]) -> Vec<&str> {
    models.iter().map(|m| m.id.as_str()).collect()
}

#[tokio::test]
async fn list_models_returns_the_whole_catalog_sorted_and_sends_no_key() {
    let server = MockServer::start().await;
    let catalogue = models(&[
        ("openai/gpt-6-luna", Some("language")),
        ("openai/text-embedding-3-small", Some("embedding")),
        ("anthropic/claude-haiku", Some("language")),
        ("bfl/flux-pro-1.1", Some("image")),
        ("acme/untyped", None),
    ]);
    respond(&server, 200, &catalogue).await;

    let models = cleanup(&server).list_models().await.unwrap();

    assert_eq!(
        vec![
            "acme/untyped",
            "anthropic/claude-haiku",
            "bfl/flux-pro-1.1",
            "openai/gpt-6-luna",
            "openai/text-embedding-3-small"
        ],
        ids(&models)
    );
    assert_eq!(
        vec![None, Some("language"), Some("image"), Some("language"), Some("embedding")],
        models.iter().map(|m| m.r#type.as_deref()).collect::<Vec<_>>()
    );
    let chat: Vec<GatewayModel> = models.iter().filter(|m| is_chat_model(m)).cloned().collect();
    assert_eq!(vec!["acme/untyped", "anthropic/claude-haiku", "openai/gpt-6-luna"], ids(&chat));
    let request = &requests(&server, 1).await[0];
    assert_eq!("GET", request.method.as_str());
    assert_eq!("/v1/models", target(request));
    assert_eq!(None, header(request, "Authorization"));
}

#[tokio::test]
async fn list_models_works_without_a_key() {
    let server = MockServer::start().await;
    respond(&server, 200, &models(&[("openai/gpt-6-luna", Some("language"))])).await;

    let models = cleanup_with(&server, None, 10_000).list_models().await.unwrap();

    assert_eq!(vec!["openai/gpt-6-luna"], ids(&models));
    assert_eq!(None, header(&requests(&server, 1).await[0], "Authorization"));
}

#[tokio::test]
async fn list_models_parses_reasoning_efforts() {
    let server = MockServer::start().await;
    let body = r#"{"object":"list","data":[
        {"id":"a/luna","type":"language","reasoning_options":[
            {"type":"effort","values":["none","low","medium","high","xhigh","max"]}]},
        {"id":"b/gemini","type":"language","reasoning_options":[
            {"type":"toggle"},{"type":"effort","values":["low","high"]}]},
        {"id":"c/no-field","type":"language"},
        {"id":"d/null","type":"language","reasoning_options":null},
        {"id":"e/object","type":"language","reasoning_options":{}},
        {"id":"f/budget","type":"language","reasoning_options":[{"type":"budget_tokens","min":1024}]},
        {"id":"g/values-string","type":"language","reasoning_options":[{"type":"effort","values":"low"}]},
        {"id":"h/values-empty","type":"language","reasoning_options":[{"type":"effort","values":[]}]}
    ]}"#;
    respond(&server, 200, body).await;

    let efforts = |values: &[&str]| Some(values.iter().map(|v| v.to_string()).collect::<Vec<_>>());
    let expected = vec![
        ("a/luna", efforts(&["none", "low", "medium", "high", "xhigh", "max"])),
        ("b/gemini", efforts(&["low", "high"])),
        ("c/no-field", None),
        ("d/null", None),
        ("e/object", None),
        ("f/budget", None),
        ("g/values-string", None),
        ("h/values-empty", None),
    ];
    let models = cleanup(&server).list_models().await.unwrap();
    assert_eq!(expected, models.iter().map(|m| (m.id.as_str(), m.reasoning_efforts.clone())).collect::<Vec<_>>());
}

/// A trimmed `/v1/models` snapshot.
const CATALOG: &str = r#"{"object":"list","data":[
    {"id":"openai/gpt-6-luna","type":"language","tags":["reasoning"],
     "reasoning_options":[{"type":"effort","values":["none","low"]}]},
    {"id":"openai/whisper-1","type":"transcription","tags":["open-weights"]},
    {"id":"openai/gpt-realtime-whisper","type":"transcription",
     "tags":["websocket-realtime","websocket-transcription"]},
    {"id":"openai/gpt-live-transcribe","type":"transcription","tags":["websocket-transcription"]},
    {"id":"elevenlabs/scribe-v2","type":"transcription"},
    {"id":"x/odd-tags","type":"transcription","tags":[1,"websocket-transcription",null]}
]}"#;

fn model(id: &str, r#type: &str, tags: &[&str], efforts: Option<&[&str]>) -> GatewayModel {
    GatewayModel {
        id: id.into(),
        r#type: Some(r#type.into()),
        tags: tags.iter().map(|t| t.to_string()).collect(),
        reasoning_efforts: efforts.map(|e| e.iter().map(|v| v.to_string()).collect()),
    }
}

#[tokio::test]
async fn list_models_parses_type_and_tags() {
    let server = MockServer::start().await;
    respond(&server, 200, CATALOG).await;

    assert_eq!(
        vec![
            model("elevenlabs/scribe-v2", "transcription", &[], None),
            model("openai/gpt-6-luna", "language", &["reasoning"], Some(&["none", "low"])),
            model("openai/gpt-live-transcribe", "transcription", &["websocket-transcription"], None),
            model(
                "openai/gpt-realtime-whisper",
                "transcription",
                &["websocket-realtime", "websocket-transcription"],
                None
            ),
            model("openai/whisper-1", "transcription", &["open-weights"], None),
            model("x/odd-tags", "transcription", &["websocket-transcription"], None),
        ],
        cleanup(&server).list_models().await.unwrap()
    );
}

#[tokio::test]
async fn chat_model_filter_keeps_only_language_models() {
    let server = MockServer::start().await;
    respond(&server, 200, CATALOG).await;

    let models = cleanup(&server).list_models().await.unwrap();

    assert_eq!(
        vec!["openai/gpt-6-luna"],
        models.iter().filter(|m| is_chat_model(m)).map(|m| &m.id).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn list_models_malformed_response_fails() {
    let server = MockServer::start().await;
    for body in ["<html>oops</html>", r#"{"object":"list"}"#, r#"{"data":{}}"#] {
        respond(&server, 200, body).await;
        assert_eq!(Err(CleanupError(UNEXPECTED_RESPONSE.into())), cleanup(&server).list_models().await, "{body}");
    }
}

#[tokio::test]
async fn list_models_http_errors_map_to_readable_messages() {
    let server = MockServer::start().await;
    let cases = [
        (401, "AI Gateway rejected the API key. Check it in Iris settings."),
        (429, "AI Gateway rate limit or quota reached"),
        (502, "AI Gateway server error (HTTP 502)"),
        (404, "AI Gateway request failed (HTTP 404)"),
    ];
    for (code, message) in cases {
        respond(&server, code, "not json").await;
        assert_eq!(Err(CleanupError(message.into())), cleanup(&server).list_models().await);
    }
}

#[tokio::test]
async fn list_models_timeout_has_its_own_message() {
    let server = MockServer::start().await;
    let late = ResponseTemplate::new(200).set_body_string(models(&[("openai/gpt-6-luna", Some("language"))]));
    respond_with(&server, late.set_delay(Duration::from_secs(2))).await;

    let error = cleanup_with(&server, Some("vck-test"), 200).list_models().await;

    assert_eq!(Err(CleanupError("Loading models timed out".into())), error);
}

#[tokio::test]
async fn warm_up_sends_head_of_models_without_key() {
    let server = MockServer::start().await;
    respond(&server, 200, "").await;

    cleanup(&server).warm_up();

    let request = &requests(&server, 1).await[0];
    assert_eq!("HEAD", request.method.as_str());
    assert_eq!("/v1/models", target(request));
    assert_eq!(None, header(request, "Authorization"));
}

/// Reduced: Android also asserts the cleanup reuses the warm-up's connection (OkHttp's `connectionIndex`).
#[tokio::test]
async fn cleanup_reuses_the_warmed_up_connection() {
    let server = MockServer::start().await;
    Mock::given(method("HEAD")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_string(completed(r#"{"text":"Hi."}"#)))
        .mount(&server)
        .await;
    let provider = cleanup(&server);

    provider.warm_up();
    requests(&server, 1).await;

    assert_eq!(Ok("Hi.".to_string()), clean(&provider).await);
    let methods: Vec<String> = requests(&server, 2).await.iter().map(|r| r.method.to_string()).collect();
    assert_eq!(vec!["HEAD", "POST"], methods);
}

#[tokio::test]
async fn warm_up_failure_is_ignored() {
    let server = MockServer::start().await;
    let failing = failing_url("/v1/models").await;
    let provider = VercelGatewayCleanup::with_urls(
        http_client(),
        key(Some("vck-test")),
        &format!("{}/v1/chat/completions", server.uri()),
        &failing,
        Duration::from_secs(10),
    );

    provider.warm_up();
    tokio::time::sleep(Duration::from_millis(300)).await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;

    assert_eq!(Ok("Hi.".to_string()), clean(&provider).await);
}
