/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Helpers the cleanup provider tests share (Android's MockWebServer helpers in each test).

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Map, Value, json};
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use wiremock::matchers::any;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

use crate::net::KeySource;

/// A key source holding `key`.
pub(crate) fn key(key: Option<&str>) -> KeySource {
    let key = key.map(String::from);
    Arc::new(move || Ok(key.clone()))
}

/// A `chat.completion` whose `choices[0].message` has `content` and `refusal`, and `finish_reason`.
pub(crate) fn completion(content: Option<&str>, refusal: Option<&str>, finish_reason: Option<&str>) -> String {
    let mut message = json!({ "role": "assistant", "content": content });
    if let Some(refusal) = refusal {
        message["refusal"] = refusal.into();
    }
    json!({
        "id": "chatcmpl-1",
        "object": "chat.completion",
        "choices": [{ "index": 0, "message": message, "finish_reason": finish_reason }],
    })
    .to_string()
}

/// A completion with `content` that stopped normally.
pub(crate) fn completed(content: &str) -> String {
    completion(Some(content), None, Some("stop"))
}

/// From now on the server answers every request with `code` and `body` (earlier answers and requests are cleared).
pub(crate) async fn respond(server: &MockServer, code: u16, body: &str) {
    respond_with(server, ResponseTemplate::new(code).set_body_string(body)).await;
}

/// Like [`respond`] with a whole response.
pub(crate) async fn respond_with(server: &MockServer, response: ResponseTemplate) {
    server.reset().await;
    Mock::given(any()).respond_with(response).mount(server).await;
}

/// The requests the server got, waiting up to 5 s for at least `count`.
pub(crate) async fn requests(server: &MockServer, count: usize) -> Vec<Request> {
    for _ in 0..500 {
        let requests = server.received_requests().await.unwrap();
        if requests.len() >= count {
            return requests;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("fewer than {count} requests");
}

/// A request's JSON body.
pub(crate) fn body(request: &Request) -> Map<String, Value> {
    serde_json::from_slice(&request.body).unwrap()
}

/// A request's path and query (OkHttp's `request.target`).
pub(crate) fn target(request: &Request) -> String {
    match request.url.query() {
        Some(query) => format!("{}?{query}", request.url.path()),
        None => request.url.path().to_string(),
    }
}

pub(crate) fn header(request: &Request, name: &str) -> Option<String> {
    request.headers.get(name).map(|value| value.to_str().unwrap().to_string())
}

/// The URL of a server that closes every connection at once (a request there fails fast on every OS).
pub(crate) async fn failing_url(path: &str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}{path}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            drop(socket);
        }
    });
    url
}

/// A server that takes one request and never answers: its URL (with `path`), a receiver that fires once a request
/// arrived and one that fires once the client dropped the connection.
pub(crate) async fn hanging_server(path: &str) -> (String, oneshot::Receiver<()>, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}{path}", listener.local_addr().unwrap());
    let (received, on_received) = oneshot::channel();
    let (closed, on_closed) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut received = Some(received);
        let mut buffer = vec![0; 64 * 1024];
        while let Ok(1..) = socket.read(&mut buffer).await {
            if let Some(received) = received.take() {
                let _ = received.send(());
            }
        }
        let _ = closed.send(());
    });
    (url, on_received, on_closed)
}

/// Waits up to 5 s for `signal`.
pub(crate) async fn within_5s(signal: oneshot::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(5), signal).await.expect("in time").unwrap();
}
