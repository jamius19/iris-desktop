/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! A local WebSocket server for provider tests (Android's `MockWebServer` + `webSocketUpgrade`). HTTP tests use
//! `wiremock`. Nothing here reaches the network.

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::{HeaderMap, StatusCode};

/// One upgrade request the server saw (rejected ones too).
#[derive(Clone, Debug)]
pub struct Recorded {
    /// 0 for the first connection, then 1, …
    pub index: usize,
    /// Path and query, e.g. `/v1/listen?model=nova-3`.
    pub uri: String,
    pub headers: HeaderMap,
}

impl Recorded {
    /// Every value of the query parameter `name`, decoded.
    pub fn query(&self, name: &str) -> Vec<String> {
        let url = url::Url::parse(&format!("http://localhost{}", self.uri)).expect("request URI");
        url.query_pairs().filter(|(k, _)| k == name).map(|(_, v)| v.into_owned()).collect()
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.headers.get(name).map(|v| v.to_str().expect("ASCII header").to_string())
    }
}

/// Accepts WebSocket connections on 127.0.0.1 and runs `handler` on each in its own task. The upgrade answer echoes
/// the client's first `Sec-WebSocket-Protocol` unless [`WsServer::no_subprotocol_echo`]. Stops when dropped.
pub struct WsServer {
    port: u16,
    echo_subprotocol: Arc<AtomicBool>,
    rejects: Arc<Mutex<VecDeque<(u16, String)>>>,
    requests: Arc<Mutex<Vec<Recorded>>>,
    task: tokio::task::JoinHandle<()>,
}

impl WsServer {
    pub async fn start<F, Fut>(handler: F) -> Self
    where
        F: Fn(Recorded, WebSocketStream<TcpStream>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("address").port();
        let echo_subprotocol = Arc::new(AtomicBool::new(true));
        let rejects: Arc<Mutex<VecDeque<(u16, String)>>> = Arc::default();
        let requests: Arc<Mutex<Vec<Recorded>>> = Arc::default();
        let handler = Arc::new(handler);
        let (task_echo, task_rejects, task_requests) = (echo_subprotocol.clone(), rejects.clone(), requests.clone());
        let task = tokio::spawn(async move {
            let mut index = 0;
            while let Ok((stream, _)) = listener.accept().await {
                let reject = task_rejects.lock().unwrap().pop_front();
                let echo = task_echo.load(Ordering::SeqCst);
                let (requests, handler) = (task_requests.clone(), handler.clone());
                let this = index;
                index += 1;
                tokio::spawn(async move {
                    let mut seen = None;
                    #[allow(clippy::result_large_err)] // tungstenite's callback signature.
                    let callback = |request: &Request, mut response: Response| -> Result<Response, ErrorResponse> {
                        let recorded = Recorded {
                            index: this,
                            uri: request.uri().to_string(),
                            headers: request.headers().clone(),
                        };
                        requests.lock().unwrap().push(recorded.clone());
                        seen = Some(recorded);
                        if let Some((status, body)) = reject {
                            let mut error = ErrorResponse::new(Some(body));
                            *error.status_mut() = StatusCode::from_u16(status).expect("status");
                            return Err(error);
                        }
                        if let Some(protocol) = request.headers().get("sec-websocket-protocol").filter(|_| echo) {
                            let first = protocol.to_str().unwrap_or_default().split(',').next().unwrap_or_default();
                            response.headers_mut().insert("sec-websocket-protocol", first.trim().parse().unwrap());
                        }
                        Ok(response)
                    };
                    if let Ok(socket) = tokio_tungstenite::accept_hdr_async(stream, callback).await {
                        handler(seen.expect("request seen"), socket).await;
                    }
                });
            }
        });
        WsServer { port, echo_subprotocol, rejects, requests, task }
    }

    /// Upgrade answers no longer echo the client's `Sec-WebSocket-Protocol`.
    pub fn no_subprotocol_echo(&self) {
        self.echo_subprotocol.store(false, Ordering::SeqCst);
    }

    /// `http://127.0.0.1:<port><path>`: the providers take `http`/`https` URLs, as on Android.
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }

    /// The next connection's upgrade is answered with this HTTP status and body instead.
    pub fn reject_next(&self, status: u16, body: &str) {
        self.rejects.lock().unwrap().push_back((status, body.to_string()));
    }

    /// The upgrade requests so far.
    pub fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for WsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}
