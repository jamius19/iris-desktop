/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `SonioxModelsTest.kt` against wiremock.

use super::*;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

fn model(id: &str, mode: Option<&str>, strict: bool) -> SonioxModel {
    SonioxModel { id: id.into(), transcription_mode: mode.map(str::to_string), supports_language_hints_strict: strict }
}

async fn list(server: &MockServer, api_key: Option<&str>) -> Result<Vec<SonioxModel>, SttError> {
    list_soniox_models(&net::http_client(), api_key.map(str::to_string), &format!("{}/", server.uri())).await
}

async fn respond(server: &MockServer, code: u16, body: &str) {
    Mock::given(any())
        .respond_with(ResponseTemplate::new(code).set_body_string(body))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

fn message(result: Result<Vec<SonioxModel>, SttError>) -> String {
    result.expect_err("an error").0
}

#[tokio::test]
async fn sends_get_with_bearer_key() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"models":[]}"#).await;

    list(&server, Some("sx-test")).await.unwrap();

    let request = &server.received_requests().await.unwrap()[0];
    assert_eq!("GET", request.method.as_str());
    assert_eq!("/v1/models", request.url.path());
    assert_eq!("Bearer sx-test", request.headers.get("authorization").unwrap());
}

#[tokio::test]
async fn parses_mode_and_strict_sorted_by_id() {
    let server = MockServer::start().await;
    respond(
        &server,
        200,
        r#"{"models":[
            {"id":"stt-rt-v6","aliased_model_id":null,"transcription_mode":"real_time","languages":[],
             "supports_language_hints_strict":true,"supports_max_endpoint_delay":true},
            {"id":"stt-async-v6","transcription_mode":"async","supports_language_hints_strict":false}
        ]}"#,
    )
    .await;

    assert_eq!(
        vec![model("stt-async-v6", Some("async"), false), model("stt-rt-v6", Some("real_time"), true)],
        list(&server, Some("sx-test")).await.unwrap()
    );
}

#[test]
fn skips_entries_without_string_id_and_missing_flag_is_false() {
    assert_eq!(
        Some(vec![model("a", None, false), model("b", Some("async"), false)]),
        parse_soniox_models(
            r#"{"models":[{"id":"b","transcription_mode":"async","supports_language_hints_strict":"yes"},
               {"id":7},{"transcription_mode":"async"},"x",{"id":"a"}]}"#
        )
    );
}

#[tokio::test]
async fn without_key_nothing_is_requested() {
    let server = MockServer::start().await;
    for key in [None, Some(" ")] {
        assert_eq!("Set your Soniox API key to load models", message(list(&server, key).await));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn key_that_cannot_be_a_header_is_rejected_without_request() {
    let server = MockServer::start().await;
    for key in ["sx-tést", "sx-test\n"] {
        assert_eq!("Invalid Soniox API key", message(list(&server, Some(key)).await));
    }
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn http_errors_map_to_readable_messages() {
    let server = MockServer::start().await;
    let cases = [
        (401, "Soniox rejected the API key. Check it in Iris settings."),
        (402, "Soniox account is out of credits or over budget"),
        (429, "Soniox limit reached: Slow down"),
        (503, "Soniox server error (503)"),
        (404, "Soniox error: Slow down"),
    ];
    for (code, expected) in cases {
        let body = format!(r#"{{"status_code":{code},"error_type":"x","message":"Slow down","validation_errors":[]}}"#);
        respond(&server, code, &body).await;
        assert_eq!(expected, message(list(&server, Some("sx-test")).await));
    }
}

#[tokio::test]
async fn malformed_body_is_unexpected() {
    let server = MockServer::start().await;
    for body in ["not json", r#"{"object":"list"}"#, r#"{"models":{}}"#] {
        respond(&server, 200, body).await;
        assert_eq!("Unexpected response from Soniox", message(list(&server, Some("sx-test")).await));
    }
}

#[tokio::test]
async fn timeout_has_its_own_message() {
    let server = MockServer::start().await;
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"models":[]}"#).set_delay(Duration::from_secs(2)))
        .mount(&server)
        .await;

    let result =
        list_with_timeout(&net::http_client(), Some("sx-test".into()), &server.uri(), Duration::from_millis(200)).await;

    assert_eq!("Loading models timed out", message(result));
}

#[tokio::test]
async fn network_error_is_short() {
    // A port nothing listens on (wiremock pools its servers: a dropped one keeps listening).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    let result = list_soniox_models(&net::http_client(), Some("sx-test".into()), &url).await;

    assert_eq!("Couldn't load models (network error)", message(result));
}

#[test]
fn catalogue_entry_decides_the_mode_else_the_id_rule() {
    let catalog = [
        model("stt-rt-v6", Some("real_time"), true),
        model("stt-rt-legacy", Some("async"), false),
        model("custom-live", Some("real_time"), false),
    ];

    assert!(soniox_streams("stt-rt-v6", Some(&catalog)));
    assert!(!soniox_streams("stt-rt-legacy", Some(&catalog))); // The entry wins over the id.
    assert!(soniox_streams("custom-live", Some(&catalog)));
    assert!(soniox_streams("stt-rt-v7", Some(&catalog))); // Not listed: the id rule.
    assert!(!soniox_streams("stt-async-v6", Some(&catalog)));
    assert!(soniox_streams("stt-rt-v6", None));
    assert!(!soniox_streams("stt-async-v6", None));
}
