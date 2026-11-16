/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `OpenRouterCleanupTest.kt` against wiremock.

use std::sync::Arc;

use serde_json::json;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::*;
use crate::cleanup::protocol::TRANSCRIPT_FRAMING;
use crate::cleanup::protocol::test_support::*;
use crate::net::http_client;

fn cleanup_with(
    server: &MockServer,
    api_key: Option<&str>,
    call_timeout_ms: u64,
    models_url: &str,
) -> OpenRouterCleanup {
    OpenRouterCleanup::with_urls(
        http_client(),
        key(api_key),
        &format!("{}/api/v1/chat/completions", server.uri()),
        models_url,
        Duration::from_millis(call_timeout_ms),
    )
}

fn cleanup(server: &MockServer) -> OpenRouterCleanup {
    cleanup_with(server, Some("sk-or-test"), 10_000, &models_url(server))
}

fn models_url(server: &MockServer) -> String {
    format!("{}/api/v1/models", server.uri())
}

fn request(text: &str, model: &str, effort: Option<&str>, options: Option<Value>) -> CleanupRequest {
    CleanupRequest {
        text: text.into(),
        system_prompt: "Clean it.".into(),
        model: model.into(),
        reasoning_effort: effort.map(String::from),
        provider_options: options.and_then(|o| o.as_object().cloned()),
    }
}

async fn clean(provider: &OpenRouterCleanup) -> Result<String, CleanupError> {
    provider.cleanup(request("hello", "m", Some("low"), None)).await
}

async fn failure(provider: &OpenRouterCleanup) -> String {
    clean(provider).await.unwrap_err().0
}

fn text(object: &Map<String, Value>, key: &str) -> String {
    object[key].as_str().unwrap().to_string()
}

async fn sent_body(server: &MockServer) -> Map<String, Value> {
    body(&requests(server, 1).await[0])
}

#[tokio::test]
async fn returns_the_parsed_text() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"  Hello, world.  "}"#)).await;

    let text = cleanup(&server).cleanup(request("um hello world", "openai/gpt-6-luna", Some("low"), None)).await;

    assert_eq!(Ok("Hello, world.".to_string()), text);
}

#[tokio::test]
async fn sends_structured_request() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;

    cleanup(&server).cleanup(request("um hello world", "openai/gpt-6-luna", Some("low"), None)).await.unwrap();

    let request = &requests(&server, 1).await[0];
    assert_eq!("POST", request.method.as_str());
    assert_eq!("/api/v1/chat/completions", target(request));
    assert_eq!(Some("Bearer sk-or-test".into()), header(request, "Authorization"));
    assert_eq!(None, header(request, "HTTP-Referer"));
    assert_eq!(None, header(request, "X-OpenRouter-Title"));
    let body = body(request);
    assert_eq!("openai/gpt-6-luna", text(&body, "model"));
    assert!(!body.contains_key("stream"));
    assert_eq!("low", text(&body, "reasoning_effort"));
    let messages: Vec<&Map<String, Value>> =
        body["messages"].as_array().unwrap().iter().map(|m| m.as_object().unwrap()).collect();
    assert_eq!(vec!["system", "user"], messages.iter().map(|m| text(m, "role")).collect::<Vec<_>>());
    assert_eq!(format!("Clean it.\n\n{TRANSCRIPT_FRAMING}"), text(messages[0], "content"));
    assert_eq!("um hello world", text(messages[1], "content"));
    let expected_format = json!({"type":"json_schema","json_schema":{"name":"cleaned_text","strict":true,"schema":{
        "type":"object","properties":{"text":{"type":"string"}},"required":["text"],
        "additionalProperties":false}}});
    assert_eq!(expected_format, body["response_format"]);
    assert!(!body.contains_key("provider"));
}

#[tokio::test]
async fn omits_reasoning_effort_for_model_default() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;

    cleanup(&server).cleanup(request("um hello world", "openai/gpt-6-luna", None, None)).await.unwrap();

    assert!(!sent_body(&server).await.contains_key("reasoning_effort"));
}

#[tokio::test]
async fn sends_provider_options_under_provider() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;
    let options = json!({"order":["openai"],"allow_fallbacks":false});

    cleanup(&server).cleanup(request("hello", "m", Some("low"), Some(options.clone()))).await.unwrap();

    let body = sent_body(&server).await;
    assert_eq!(options, body["provider"]);
    assert!(!body.contains_key("providerOptions"));
}

#[tokio::test]
async fn content_that_is_not_the_schema_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, &completed("Hello, world.")).await;

    assert_eq!("Unexpected response from OpenRouter", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn malformed_responses_fail() {
    let server = MockServer::start().await;
    let bodies = [
        completion(None, None, Some("stop")),
        completed(r#"{"text":"  "}"#),
        r#"{"id":"gen-1","choices":[]}"#.into(),
        r#"{"id":"gen-1","choices":[],"error":null}"#.into(),
        "<html>oops</html>".into(),
        String::new(),
    ];
    for body in bodies {
        respond(&server, 200, &body).await;
        assert_eq!("Unexpected response from OpenRouter", failure(&cleanup(&server)).await, "{body}");
    }
}

#[tokio::test]
async fn http_errors_map_to_readable_messages() {
    let server = MockServer::start().await;
    let cases = [
        (401, "OpenRouter rejected the API key. Check it in Iris settings."),
        (402, "OpenRouter account is out of credits"),
        (429, "OpenRouter rate limit reached"),
        (502, "OpenRouter: the model's provider failed (HTTP 502)"),
        (503, "No OpenRouter provider available for this model (check provider options)"),
        (500, "OpenRouter server error (HTTP 500)"),
    ];
    for (code, message) in cases {
        respond(&server, code, &format!(r#"{{"error":{{"code":{code},"message":"ignored"}}}}"#)).await;
        assert_eq!(message, failure(&cleanup(&server)).await);
    }
}

#[tokio::test]
async fn client_error_shows_open_router_message_when_present() {
    let server = MockServer::start().await;
    respond(&server, 400, r#"{"error":{"code":400,"message":"Model not found"}}"#).await;
    assert_eq!("OpenRouter error: Model not found", failure(&cleanup(&server)).await);

    // 403 is moderation or forbidden on OpenRouter, not a key problem.
    respond(&server, 403, r#"{"error":{"code":403,"message":"Input was flagged"}}"#).await;
    assert_eq!("OpenRouter error: Input was flagged", failure(&cleanup(&server)).await);

    respond(&server, 404, "not json").await;
    assert_eq!("OpenRouter request failed (HTTP 404)", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn error_in_a_200_response_fails() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"error":{"code":502,"message":"Provider returned error"}}"#).await;
    assert_eq!("OpenRouter error: Provider returned error", failure(&cleanup(&server)).await);

    respond(
        &server,
        200,
        r#"{"id":"gen-1","choices":[{"index":0,"message":{"role":"assistant","content":""},
           "finish_reason":"error","error":{"code":502,"message":"Upstream failed"}}]}"#,
    )
    .await;
    assert_eq!("OpenRouter error: Upstream failed", failure(&cleanup(&server)).await);

    respond(&server, 200, &completion(Some(r#"{"text":"Hel"#), None, Some("error"))).await;
    assert_eq!("OpenRouter error: generation failed", failure(&cleanup(&server)).await);
}

#[tokio::test]
async fn slow_response_times_out() {
    let server = MockServer::start().await;
    let late = ResponseTemplate::new(200).set_body_string(completed(r#"{"text":"late"}"#));
    respond_with(&server, late.set_delay(Duration::from_secs(2))).await;

    let provider = cleanup_with(&server, Some("sk-or-test"), 200, &models_url(&server));
    assert_eq!("Cleanup timed out", failure(&provider).await);
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

    // OpenRouter reports a refusal with finish_reason content_filter: the refusal wins.
    respond(&server, 200, &completion(None, Some("No."), Some("content_filter"))).await;
    assert_eq!("Model refused: No.", failure(&cleanup(&server)).await);
}

/// Android checks OkHttp's `canceled` event; here the server sees the connection close once the future is dropped.
#[tokio::test]
async fn cancelling_before_the_response_cancels_the_call() {
    let (url, received, closed) = hanging_server("/api/v1/chat/completions").await;
    let provider =
        Arc::new(OpenRouterCleanup::with_urls(http_client(), key(Some("sk-or-test")), &url, &url, CALL_TIMEOUT));

    let call = tokio::spawn(async move { clean(&provider).await });
    within_5s(received).await; // The call is in flight.
    call.abort();

    within_5s(closed).await;
}

#[tokio::test]
async fn missing_api_key_fails_without_calling() {
    let server = MockServer::start().await;

    let error = failure(&cleanup_with(&server, None, 10_000, &models_url(&server))).await;

    assert_eq!("Add your OpenRouter API key in Iris settings", error);
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn unsendable_api_key_fails_without_calling() {
    let server = MockServer::start().await;

    for key in ["sk-or\u{a0}test", "sk-or-test\n"] {
        let error = failure(&cleanup_with(&server, Some(key), 10_000, &models_url(&server))).await;
        assert_eq!("Invalid OpenRouter API key", error);
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn list_models_requests_structured_output_models_sorted_without_key() {
    let server = MockServer::start().await;
    let body = r#"{"data":[
        {"id":"openai/gpt-6-luna","name":"Luna"},
        {"id":"anthropic/claude-haiku-5"},
        {"id":"openai/gpt-6-luna:batch"},
        {"id":" "},
        {"name":"no id"},
        "odd"
    ]}"#;
    respond(&server, 200, body).await;

    let models = cleanup_with(&server, None, 10_000, &models_url(&server)).list_models().await.unwrap();

    let model = |id: &str| GatewayModel { id: id.into(), r#type: None, tags: Vec::new(), reasoning_efforts: None };
    assert_eq!(vec![model("anthropic/claude-haiku-5"), model("openai/gpt-6-luna")], models);
    let request = &requests(&server, 1).await[0];
    assert_eq!("GET", request.method.as_str());
    assert_eq!("/api/v1/models?supported_parameters=structured_outputs", target(request));
    assert_eq!(None, header(request, "Authorization"));
}

#[tokio::test]
async fn list_models_orders_reasoning_efforts_low_to_high() {
    let server = MockServer::start().await;
    let body = r#"{"data":[
        {"id":"a/luna","reasoning":{"mandatory":false,"default_enabled":true,
            "supported_efforts":["max","xhigh","high","medium","low","none"],"default_effort":"medium"}},
        {"id":"b/odd","reasoning":{"supported_efforts":["turbo","high","minimal"]}},
        {"id":"c/no-reasoning"},
        {"id":"d/no-efforts","reasoning":{"mandatory":true}},
        {"id":"e/null-efforts","reasoning":{"supported_efforts":null}},
        {"id":"f/empty-efforts","reasoning":{"supported_efforts":[]}},
        {"id":"g/string-efforts","reasoning":{"supported_efforts":"low"}}
    ]}"#;
    respond(&server, 200, body).await;

    let efforts = |values: &[&str]| Some(values.iter().map(|v| v.to_string()).collect::<Vec<_>>());
    let expected = vec![
        ("a/luna", efforts(&["none", "low", "medium", "high", "xhigh", "max"])),
        ("b/odd", efforts(&["minimal", "high", "turbo"])),
        ("c/no-reasoning", None),
        ("d/no-efforts", None),
        ("e/null-efforts", None),
        ("f/empty-efforts", None),
        ("g/string-efforts", None),
    ];
    let models = cleanup(&server).list_models().await.unwrap();
    assert_eq!(expected, models.iter().map(|m| (m.id.as_str(), m.reasoning_efforts.clone())).collect::<Vec<_>>());
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
        (429, "OpenRouter rate limit reached"),
        (500, "OpenRouter server error (HTTP 500)"),
        (404, "OpenRouter request failed (HTTP 404)"),
    ];
    for (code, message) in cases {
        respond(&server, code, "not json").await;
        assert_eq!(Err(CleanupError(message.into())), cleanup(&server).list_models().await);
    }
}

#[tokio::test]
async fn list_models_timeout_has_its_own_message() {
    let server = MockServer::start().await;
    let late = ResponseTemplate::new(200).set_body_string(r#"{"data":[]}"#);
    respond_with(&server, late.set_delay(Duration::from_secs(2))).await;

    let error = cleanup_with(&server, Some("sk-or-test"), 200, &models_url(&server)).list_models().await;

    assert_eq!(Err(CleanupError("Loading models timed out".into())), error);
}

#[tokio::test]
async fn warm_up_sends_head_of_models_without_key() {
    let server = MockServer::start().await;
    respond(&server, 200, "").await;

    cleanup(&server).warm_up();

    let request = &requests(&server, 1).await[0];
    assert_eq!("HEAD", request.method.as_str());
    assert_eq!("/api/v1/models", target(request));
    assert_eq!(None, header(request, "Authorization"));
}

/// Reduced: Android also asserts the cleanup reuses the warm-up's connection (OkHttp's `connectionIndex`).
#[tokio::test]
async fn cleanup_reuses_the_warmed_up_connection() {
    let server = MockServer::start().await;
    let completion = ResponseTemplate::new(200).set_body_string(completed(r#"{"text":"Hi."}"#));
    Mock::given(method("HEAD")).respond_with(ResponseTemplate::new(200)).mount(&server).await;
    Mock::given(method("POST")).respond_with(completion).mount(&server).await;
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
    let provider = cleanup_with(&server, Some("sk-or-test"), 10_000, &failing_url("/api/v1/models").await);

    provider.warm_up();
    tokio::time::sleep(Duration::from_millis(300)).await;
    respond(&server, 200, &completed(r#"{"text":"Hi."}"#)).await;

    assert_eq!(Ok("Hi.".to_string()), clean(&provider).await);
}
