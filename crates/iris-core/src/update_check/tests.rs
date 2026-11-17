/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `UpdateCheckTest.kt` against wiremock.

use super::*;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, ResponseTemplate};

const PATH: &str = "/repos/jamius19/iris-desktop/releases/latest";

/// Answers the next request (only) with `code` and `body`.
async fn respond(server: &MockServer, code: u16, body: &str) {
    Mock::given(any())
        .respond_with(ResponseTemplate::new(code).set_body_string(body))
        .up_to_n_times(1)
        .mount(server)
        .await;
}

async fn check_at(uri: &str) -> UpdateStatus {
    check_for_update(&net::http_client(), "0.9.0", &format!("{uri}{PATH}")).await
}

#[test]
fn a_higher_tag_is_newer() {
    assert!(is_newer_version("v0.9.1", "0.9.0"));
    assert!(is_newer_version("v1.0.0", "0.9.9"));
    assert!(is_newer_version("v0.10.0", "0.9.0"));
    assert!(is_newer_version("0.9.1", "0.9.0"));
    assert!(is_newer_version("v0.9.0.1", "0.9.0"));
}

#[test]
fn the_same_or_a_lower_tag_is_not_newer() {
    assert!(!is_newer_version("v0.9.0", "0.9.0"));
    assert!(!is_newer_version("v0.9", "0.9.0"));
    assert!(!is_newer_version("v0.8.9", "0.9.0"));
    assert!(!is_newer_version("v0.9.0", "0.10.0"));
}

#[test]
fn a_version_that_isnt_numbers_is_never_newer() {
    assert!(!is_newer_version("v1.0.0-beta", "0.9.0"));
    assert!(!is_newer_version("latest", "0.9.0"));
    assert!(!is_newer_version("v1.0.0", ""));
}

#[test]
fn the_tag_is_read_from_a_release_response() {
    assert_eq!(Some("v1.2.0".to_string()), parse_release_tag(r#"{"tag_name":"v1.2.0","name":"Iris 1.2.0"}"#));
    assert_eq!(None, parse_release_tag(r#"{"name":"Iris 1.2.0"}"#));
    assert_eq!(None, parse_release_tag(r#"{"tag_name":null}"#));
    assert_eq!(None, parse_release_tag("[]"));
    assert_eq!(None, parse_release_tag("not json"));
}

#[tokio::test]
async fn a_newer_release_is_available() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"tag_name":"v0.9.1"}"#).await;

    assert_eq!(UpdateStatus::Available, check_at(&server.uri()).await);

    let requests = server.received_requests().await.unwrap();
    assert_eq!("GET", requests[0].method.as_str());
    assert_eq!(PATH, requests[0].url.path());
    // Desktop: GitHub requires a User-Agent.
    assert_eq!("application/vnd.github+json", requests[0].headers["accept"]);
    assert_eq!("iris-desktop", requests[0].headers["user-agent"]);
}

#[tokio::test]
async fn the_installed_release_is_up_to_date() {
    let server = MockServer::start().await;
    respond(&server, 200, r#"{"tag_name":"v0.9.0"}"#).await;

    assert_eq!(UpdateStatus::UpToDate, check_at(&server.uri()).await);
}

#[tokio::test]
async fn no_release_yet_is_up_to_date() {
    let server = MockServer::start().await;
    respond(&server, 404, r#"{"message":"Not Found"}"#).await;

    assert_eq!(UpdateStatus::UpToDate, check_at(&server.uri()).await);
}

#[tokio::test]
async fn an_error_or_unexpected_response_is_unknown() {
    let server = MockServer::start().await;
    respond(&server, 403, r#"{"message":"API rate limit exceeded"}"#).await;
    assert_eq!(UpdateStatus::Unknown, check_at(&server.uri()).await);

    respond(&server, 200, "not json").await;
    assert_eq!(UpdateStatus::Unknown, check_at(&server.uri()).await);
}

#[tokio::test]
async fn a_network_error_is_unknown() {
    // A port nothing listens on any more (a dropped MockServer goes back to wiremock's pool, still listening).
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let uri = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);

    assert_eq!(UpdateStatus::Unknown, check_at(&uri).await);
}
