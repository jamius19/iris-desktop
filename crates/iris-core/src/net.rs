/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Plumbing the cloud providers share (Android: the app's one `OkHttpClient`): the HTTP client, bounded response
//! reads, WebSocket connects with headers, API keys from the keychain and the throttled keyless warm-up.
//! Nothing here logs a key, a transcript or a request body.

use std::error::Error as StdError;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use futures_util::{Sink, SinkExt};
use log::{debug, warn};
use reqwest::header::HeaderMap;
use tokio::net::TcpStream;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderName, HeaderValue};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream, tungstenite};

use crate::settings::secrets::SecretError;

/// OkHttp's default connect timeout.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// How long an idle pooled connection is kept (OkHttp's 5 min pool keep-alive), so a warm-up's connection lasts.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// The most a response body (HTTP) or message (WebSocket) may hold; anything larger fails the call. Model catalogues
/// are the largest (a few hundred KB).
pub const MAX_RESPONSE_BYTES: usize = 8 << 20;

/// The app's HTTP client: one connection pool (HTTP/2 where offered) for every provider, so a warm-up's connection is
/// reused. Calls set their own overall timeout. Redirects are followed only within the same origin (stricter than
/// OkHttp), so a key header never goes to another host.
pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() < 10 && attempt.url().origin() == attempt.previous()[0].origin() {
                attempt.follow()
            } else {
                attempt.error("redirect to another host refused")
            }
        }))
        .build()
        .expect("HTTP client")
}

/// A transport failure: a timeout, or anything else (DNS, refused, TLS, reset, a body over the size cap).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NetError {
    pub timeout: bool,
    /// The innermost cause, short and without URLs (Android shows `e.message`): e.g. "connection refused".
    pub message: String,
}

impl NetError {
    pub fn timeout() -> Self {
        NetError { timeout: true, message: "timeout".into() }
    }

    /// No pong came back within a [`PING_INTERVAL`] (OkHttp's message for a missed pong).
    pub fn ping_timeout() -> Self {
        NetError {
            timeout: true,
            message: format!("sent ping but didn't receive pong within {}ms", PING_INTERVAL.as_millis()),
        }
    }

    fn other(message: impl Into<String>) -> Self {
        NetError { timeout: false, message: message.into() }
    }
}

impl From<reqwest::Error> for NetError {
    fn from(error: reqwest::Error) -> Self {
        if error.is_timeout() { NetError::timeout() } else { NetError::other(innermost(&error)) }
    }
}

impl From<tungstenite::Error> for NetError {
    fn from(error: tungstenite::Error) -> Self {
        match &error {
            tungstenite::Error::Io(e) if e.kind() == std::io::ErrorKind::TimedOut => NetError::timeout(),
            _ => NetError::other(innermost(&error)),
        }
    }
}

/// The deepest source's message: reqwest's own Display names the URL (which can hold custom words).
fn innermost(error: &(dyn StdError + 'static)) -> String {
    let mut current = error;
    while let Some(source) = current.source() {
        current = source;
    }
    current.to_string()
}

/// Reads a response's status and body (lossy UTF-8), failing once the body passes [`MAX_RESPONSE_BYTES`]. The
/// caller bounds the time (the request's timeout covers the body too).
pub async fn read_response(mut response: reqwest::Response) -> Result<(u16, String), NetError> {
    let status = response.status().as_u16();
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(NetError::other("response too large"));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, String::from_utf8_lossy(&body).into_owned()))
}

/// WebSocket providers ping this often; a ping whose pong hasn't arrived by the next one fails the socket (OkHttp's
/// `pingInterval`: keeps the socket alive through pauses the VAD doesn't send, and detects a dead connection).
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// A WebSocket frame that can't be written within this fails the socket (OkHttp's default write timeout).
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

pub type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Writes `message` to `sink`, failing with [`NetError::timeout`] if it takes longer than `limit`.
pub async fn write_ws<S>(sink: &mut S, message: Message, limit: Duration) -> Result<(), NetError>
where
    S: Sink<Message, Error = tungstenite::Error> + Unpin,
{
    match tokio::time::timeout(limit, sink.send(message)).await {
        Ok(result) => result.map_err(NetError::from),
        Err(_) => Err(NetError::timeout()),
    }
}

/// Why a WebSocket couldn't open.
#[derive(Debug)]
pub enum WsConnectError {
    /// A header value can't be sent (e.g. a key with control or non-ASCII characters; Android: "Invalid … API key").
    /// Holds the header's name only.
    InvalidHeader(&'static str),
    /// The server answered the upgrade with this HTTP status (e.g. 401) and body (capped).
    Http {
        status: u16,
        headers: HeaderMap,
        body: String,
    },
    Network(NetError),
}

/// Opens a WebSocket to `url` (`https`/`http` are taken as `wss`/`ws`, like OkHttp) with `headers`, within
/// [`CONNECT_TIMEOUT`] for the TCP, TLS and upgrade together. Messages are capped at [`MAX_RESPONSE_BYTES`]. The
/// caller sends its own pings (OkHttp's `pingInterval`).
pub async fn connect_ws(url: &str, headers: &[(&'static str, String)]) -> Result<WsStream, WsConnectError> {
    let url = match url.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some(("http", rest)) => format!("ws://{rest}"),
        _ => url.to_string(),
    };
    let mut request =
        url.as_str().into_client_request().map_err(|e| WsConnectError::Network(NetError::other(innermost(&e))))?;
    for (name, value) in headers {
        let value = Some(value)
            .filter(|v| is_header_value(v))
            .and_then(|v| HeaderValue::from_str(v).ok())
            .ok_or(WsConnectError::InvalidHeader(name))?;
        request.headers_mut().insert(HeaderName::from_static(name), value);
    }
    let config =
        WebSocketConfig::default().max_message_size(Some(MAX_RESPONSE_BYTES)).max_frame_size(Some(MAX_RESPONSE_BYTES));
    let connect = tokio_tungstenite::connect_async_tls_with_config(request, Some(config), true, tls_connector());
    match tokio::time::timeout(CONNECT_TIMEOUT, connect).await {
        Err(_) => Err(WsConnectError::Network(NetError::timeout())),
        Ok(Ok((socket, _))) => Ok(socket),
        Ok(Err(tungstenite::Error::Http(response))) => {
            let status = response.status().as_u16();
            let mut headers = HeaderMap::new();
            for (name, value) in response.headers() {
                if let (Ok(name), Ok(value)) = (
                    reqwest::header::HeaderName::from_bytes(name.as_ref()),
                    reqwest::header::HeaderValue::from_bytes(value.as_bytes()),
                ) {
                    headers.append(name, value);
                }
            }
            let body = response.body().as_deref().unwrap_or_default();
            let body = String::from_utf8_lossy(&body[..body.len().min(MAX_RESPONSE_BYTES)]).into_owned();
            Err(WsConnectError::Http { status, headers, body })
        }
        Ok(Err(e)) => Err(WsConnectError::Network(e.into())),
    }
}

/// `value` may be sent as a header value: tab and printable ASCII only (OkHttp's rule; the `http` crate also lets
/// non-ASCII bytes through).
pub fn is_header_value(value: &str) -> bool {
    value.chars().all(|c| c == '\t' || (' '..='~').contains(&c))
}

/// TLS for WebSockets with the OS verifier, as reqwest does; None (the crate's bundled roots) if that can't be set up.
fn tls_connector() -> Option<Connector> {
    static CONFIG: OnceLock<Option<Arc<rustls::ClientConfig>>> = OnceLock::new();
    CONFIG
        .get_or_init(|| {
            use rustls_platform_verifier::BuilderVerifierExt;
            let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .and_then(|builder| builder.with_platform_verifier())
                .map(|builder| builder.with_no_client_auth());
            match config {
                Ok(config) => Some(Arc::new(config)),
                Err(e) => {
                    warn!("WebSocket TLS: the system verifier failed ({e}); using bundled roots");
                    None
                }
            }
        })
        .clone()
        .map(Connector::Rustls)
}

/// Reads a provider's API key from the keychain (a blocking call). Never logged.
pub type KeySource = Arc<dyn Fn() -> Result<Option<String>, SecretError> + Send + Sync>;

/// A keychain read failed (locked, no Secret Service): not the same as a missing key.
pub const KEYCHAIN_READ_ERROR: &str = "Couldn't read the keychain.";

/// The key `source` holds, read off the async workers: None when unset or blank; Err([`KEYCHAIN_READ_ERROR`]) when
/// the keychain can't be read.
pub async fn api_key(source: &KeySource) -> Result<Option<String>, &'static str> {
    let source = source.clone();
    match tokio::task::spawn_blocking(move || source()).await {
        Ok(Ok(key)) => Ok(key.filter(|k| !k.trim().is_empty())),
        Ok(Err(e)) => {
            warn!("Reading an API key failed: {e}");
            Err(KEYCHAIN_READ_ERROR)
        }
        Err(e) => {
            warn!("Reading an API key failed: {e}");
            Err(KEYCHAIN_READ_ERROR)
        }
    }
}

/// How long a warm-up's connection counts as fresh: inside the 5 min pool keep-alive (Android's 3 min).
pub const WARM_UP_WINDOW: Duration = Duration::from_secs(180);

/// Android's `warmedUpAt`: a provider's last warm-up whose connection no call has taken yet, so repeats within
/// [`WARM_UP_WINDOW`] are skipped.
#[derive(Default)]
pub struct WarmUp {
    at: Mutex<Option<Instant>>,
}

impl WarmUp {
    /// Sends a keyless `HEAD url` in the background (the response is ignored), leaving an open connection in
    /// `client`'s pool for the next call, unless an earlier warm-up is still fresh. A failure clears the mark, so the
    /// next call tries again. Needs a tokio runtime (skipped without one).
    pub fn head(self: &Arc<Self>, client: &reqwest::Client, url: &str, tag: &'static str) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let now = Instant::now();
        {
            let mut at = self.lock();
            if at.is_some_and(|at| now.duration_since(at) < WARM_UP_WINDOW) {
                return;
            }
            *at = Some(now);
        }
        let (state, request) = (self.clone(), client.head(url).timeout(WARM_UP_TIMEOUT));
        runtime.spawn(async move {
            match request.send().await {
                Ok(response) => debug!("{tag}: warm-up {:?} {}", response.version(), response.status().as_u16()),
                Err(e) => {
                    debug!("{tag}: warm-up failed: {}", NetError::from(e).message);
                    let mut at = state.lock();
                    if *at == Some(now) {
                        *at = None;
                    }
                }
            }
        });
    }

    /// A call took the warmed connection: the next warm-up opens a new one.
    pub fn taken(&self) {
        *self.lock() = None;
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        self.at.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A warm-up that hangs is dropped after this long.
const WARM_UP_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn read_response_caps_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![b'a'; MAX_RESPONSE_BYTES + 1]))
            .mount(&server)
            .await;

        let response = http_client().get(server.uri()).send().await.unwrap();

        assert_eq!(Err(NetError::other("response too large")), read_response(response).await);
    }

    #[tokio::test]
    async fn redirects_are_followed_only_within_the_same_origin() {
        let (a, b) = (MockServer::start().await, MockServer::start().await);
        Mock::given(method("POST"))
            .and(path("/cross"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", format!("{}/x", b.uri())))
            .mount(&a)
            .await;
        Mock::given(method("POST"))
            .and(path("/same"))
            .respond_with(ResponseTemplate::new(307).insert_header("location", format!("{}/other", a.uri())))
            .mount(&a)
            .await;
        Mock::given(method("POST")).and(path("/other")).respond_with(ResponseTemplate::new(200)).mount(&a).await;
        Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).mount(&b).await;
        let client = http_client();

        let error = client.post(format!("{}/cross", a.uri())).send().await.unwrap_err();
        assert_eq!(NetError::other("redirect to another host refused"), NetError::from(error));
        assert!(b.received_requests().await.unwrap().is_empty());

        let response = client.post(format!("{}/same", a.uri())).send().await.unwrap();
        assert_eq!(200, response.status().as_u16());
        assert_eq!("/other", response.url().path());
    }

    #[tokio::test]
    async fn warm_up_is_skipped_while_fresh_and_again_once_taken() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD")).respond_with(ResponseTemplate::new(405)).mount(&server).await;
        let (client, warm_up) = (http_client(), Arc::new(WarmUp::default()));

        warm_up.head(&client, &server.uri(), "test");
        warm_up.head(&client, &server.uri(), "test");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(1, server.received_requests().await.unwrap().len());

        warm_up.taken();
        warm_up.head(&client, &server.uri(), "test");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(2, server.received_requests().await.unwrap().len());
    }

    #[tokio::test]
    async fn api_key_reads_blank_as_unset_and_maps_keychain_errors() {
        let set: KeySource = Arc::new(|| Ok(Some("sk-1".into())));
        let blank: KeySource = Arc::new(|| Ok(Some(" ".into())));
        let failing: KeySource = Arc::new(|| Err(SecretError::new("locked")));

        assert_eq!(Ok(Some("sk-1".into())), api_key(&set).await);
        assert_eq!(Ok(None), api_key(&blank).await);
        assert_eq!(Err(KEYCHAIN_READ_ERROR), api_key(&failing).await);
    }

    #[tokio::test]
    async fn connect_ws_reports_the_rejected_upgrade_and_bad_header_values() {
        let server = test_support::WsServer::start(|_request, _socket| async {}).await;
        server.reject_next(401, r#"{"error":"bad key"}"#);

        match connect_ws(&server.url("/listen"), &[("authorization", "Token k".into())]).await {
            Err(WsConnectError::Http { status, body, .. }) => {
                assert_eq!(401, status);
                assert_eq!(r#"{"error":"bad key"}"#, body);
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(
            connect_ws(&server.url("/listen"), &[("authorization", "Token \n".into())]).await,
            Err(WsConnectError::InvalidHeader("authorization"))
        ));
    }
}
