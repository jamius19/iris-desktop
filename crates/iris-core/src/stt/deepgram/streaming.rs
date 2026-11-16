/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Deepgram live transcription over a WebSocket (Android `DeepgramSpeechToText.kt`): one socket, no reconnect. Audio
//! is sent as raw PCM16 binary frames while recording; when the audio ends, CloseStream flushes the stream and the
//! settled segments are the final transcript once Metadata (or the server's normal close) arrives. A writer task sends
//! the queued frames (OkHttp's send queue), so recording never waits for the network.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::SplitSink;
use log::{debug, warn};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{self, Message};

use super::protocol::{
    CLOSE_STREAM, DEEPGRAM_SAMPLE_RATE, DeepgramEvent, DeepgramTranscript, LISTEN_URL, deepgram_closed_message,
    deepgram_http_error_message, deepgram_network_error_message, dropped_keyterms, listen_url, parse_deepgram_event,
    takes_keyterms,
};
use crate::net::{self, KeySource, NetError, WsConnectError, WsStream};
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest};

pub(super) const MISSING_KEY: &str = "Add your Deepgram API key in Iris settings";
pub(super) const INVALID_KEY: &str = "Invalid Deepgram API key";
pub(super) const TIMED_OUT: &str = "Timed out waiting for the transcript";
const DRAIN_TIMED_OUT: &str = "Timed out sending audio to Deepgram";

const NORMAL_CLOSURE: u16 = 1000;

/// How long the queued audio and CloseStream may take to send once the recording ends.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

/// How long a normal end waits for its Close frame to be written (not for the server's reply).
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

pub struct DeepgramSpeechToText {
    api_key: KeySource,
    url: String,
    ping_interval: Duration,
    /// A frame (audio or ping) not written within this fails the socket.
    write_timeout: Duration,
}

impl DeepgramSpeechToText {
    /// `_client` is unused: the WebSocket opens its own connection, so there's no warm-up to share it with.
    pub fn new(_client: reqwest::Client, api_key: KeySource) -> Self {
        Self::with_url(api_key, LISTEN_URL.into(), net::PING_INTERVAL)
    }

    pub(crate) fn with_url(api_key: KeySource, url: String, ping_interval: Duration) -> Self {
        DeepgramSpeechToText { api_key, url, ping_interval, write_timeout: net::WRITE_TIMEOUT }
    }
}

impl SpeechToTextProvider for DeepgramSpeechToText {
    fn sample_rate(&self, _model: &str) -> u32 {
        DEEPGRAM_SAMPLE_RATE
    }

    fn takes_custom_words(&self, model: &str) -> bool {
        takes_keyterms(model)
    }

    fn transcribe<'a>(
        &'a self,
        audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async move {
            let key = match net::api_key(&self.api_key).await {
                Ok(Some(key)) => key,
                Ok(None) => return Err(SttError(MISSING_KEY.into())),
                Err(message) => return Err(SttError(message.into())),
            };
            // Before any audio is pulled: the microphone never opens for a key that can't be sent.
            if !net::is_header_value(&key) {
                return Err(SttError(INVALID_KEY.into()));
            }
            let dropped = dropped_keyterms(&request.model, &request.custom_words);
            if dropped > 0 {
                debug!("Keyterms: dropped {dropped} over the token budget");
            }
            let url = listen_url(&self.url, &request.model, &request.language, true, &request.custom_words);
            self.session(&url, &key, audio, request.timeout, on_partial).await
        })
    }
}

impl DeepgramSpeechToText {
    async fn session(
        &self,
        url: &str,
        key: &str,
        mut audio: Box<dyn AudioSource + '_>,
        timeout: Duration,
        on_partial: OnPartial,
    ) -> Result<String, SttError> {
        let started_at = Instant::now();
        let (queue, queued) = mpsc::unbounded_channel();
        let (drained_signal, mut drained) = oneshot::channel();
        let mut session = Session {
            queue,
            drained_signal: Some(drained_signal),
            close_stream_sent_at: None,
            draining: false,
            deadline: None,
        };

        // Audio queues while the handshake runs (OkHttp queues messages until it completes). The connect is bounded
        // well within the drain timeout.
        let headers = [("authorization", format!("Token {key}"))];
        let mut connect = std::pin::pin!(net::connect_ws(url, &headers));
        let socket = loop {
            tokio::select! {
                socket = &mut connect => break socket.map_err(connect_error)?,
                chunk = audio.next(), if session.close_stream_sent_at.is_none() => session.on_audio(chunk?),
            }
        };
        debug!("Timing: transcribe->ws open {} ms", started_at.elapsed().as_millis());

        let (sink, mut stream) = socket.split();
        // Set when the writer writes a ping, cleared by its pong.
        let awaiting_pong = Arc::new(AtomicBool::new(false));
        let writer = write(sink, queued, self.ping_interval, self.write_timeout, awaiting_pong.clone());
        let mut writer = AbortOnDrop(tokio::spawn(writer));
        let mut transcript = DeepgramTranscript::default();
        loop {
            let (deadline, timed_out) = session.deadline.unwrap_or((Instant::now(), TIMED_OUT));
            // A socket failure is checked first, so it wins over a write error or the audio.
            let event = tokio::select! {
                biased;
                message = stream.next() => match message {
                    Some(Ok(Message::Pong(_))) => {
                        awaiting_pong.store(false, Ordering::SeqCst);
                        None
                    }
                    message => on_message(message, session.close_stream_sent_at),
                },
                result = &mut writer.0 => {
                    let error = match result {
                        Ok(Err(error)) => error,
                        _ => connection_closed(),
                    };
                    Some(DeepgramEvent::Failed(deepgram_network_error_message(&error)))
                }
                chunk = audio.next(), if session.close_stream_sent_at.is_none() => {
                    session.on_audio(chunk?);
                    None
                }
                // An error means the writer ended: its branch reports why.
                result = &mut drained, if session.draining => {
                    session.draining = false;
                    if result.is_ok() {
                        session.deadline = Some((Instant::now() + timeout, TIMED_OUT));
                    }
                    None
                }
                _ = tokio::time::sleep_until(deadline), if session.deadline.is_some() => {
                    Some(DeepgramEvent::Failed(timed_out.into()))
                }
            };
            match event {
                Some(DeepgramEvent::Result { is_final, transcript: text }) => {
                    transcript.on_result(is_final, &text);
                    on_partial(transcript.live_text());
                }
                Some(DeepgramEvent::Metadata) => break,
                Some(DeepgramEvent::Error(message)) => warn!("Deepgram error: {message}"),
                Some(DeepgramEvent::Failed(message)) => return Err(SttError(message)),
                None => {}
            }
        }
        // Doesn't wait for the server's close reply (OkHttp `close()`).
        session.send(Message::Close(Some(CloseFrame { code: CloseCode::Normal, reason: "".into() })));
        drop(session);
        let _ = tokio::time::timeout(CLOSE_TIMEOUT, &mut writer.0).await;
        Ok(transcript.final_text().to_string())
    }
}

/// The send side of a session.
struct Session {
    queue: mpsc::UnboundedSender<Out>,
    drained_signal: Option<oneshot::Sender<()>>,
    /// Only after CloseStream do Metadata or a normal close end the session.
    close_stream_sent_at: Option<Instant>,
    /// CloseStream is queued but not written yet.
    draining: bool,
    /// Once the audio ended: when (and with which message) the drain, then the wait for the transcript, times out.
    deadline: Option<(Instant, &'static str)>,
}

impl Session {
    /// Queues an audio chunk, or CloseStream once the audio ended (None).
    fn on_audio(&mut self, chunk: Option<Vec<u8>>) {
        match chunk {
            // An empty binary frame would close the stream.
            Some(chunk) if chunk.is_empty() => {}
            Some(chunk) => self.send(Message::Binary(chunk.into())),
            None => {
                self.send(Message::text(CLOSE_STREAM));
                if let Some(signal) = self.drained_signal.take() {
                    let _ = self.queue.send(Out::Drained(signal));
                }
                let now = Instant::now();
                self.close_stream_sent_at = Some(now);
                self.draining = true;
                self.deadline = Some((now + DRAIN_TIMEOUT, DRAIN_TIMED_OUT));
            }
        }
    }

    fn send(&self, message: Message) {
        let _ = self.queue.send(Out::Frame(message));
    }
}

/// What the writer task sends, in order.
enum Out {
    Frame(Message),
    /// Everything queued before it has been written.
    Drained(oneshot::Sender<()>),
}

/// Writes the queued frames until the queue closes (the session ended) or a write fails, with a ping every
/// `ping_interval` between frames (as OkHttp writes them ahead of queued data); a ping whose pong hasn't come back by
/// the next one fails the socket, as does a frame not written within `write_timeout`.
async fn write(
    mut sink: SplitSink<WsStream, Message>,
    mut queued: mpsc::UnboundedReceiver<Out>,
    ping_interval: Duration,
    write_timeout: Duration,
    awaiting_pong: Arc<AtomicBool>,
) -> Result<(), NetError> {
    let mut ping = tokio::time::interval_at(Instant::now() + ping_interval, ping_interval);
    ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            out = queued.recv() => match out {
                Some(Out::Frame(message)) => net::write_ws(&mut sink, message, write_timeout).await?,
                Some(Out::Drained(signal)) => {
                    let _ = signal.send(());
                }
                None => return Ok(()),
            },
            _ = ping.tick() => {
                if awaiting_pong.swap(true, Ordering::SeqCst) {
                    return Err(NetError::ping_timeout());
                }
                net::write_ws(&mut sink, Message::Ping(Default::default()), write_timeout).await?;
            }
        }
    }
}

/// Aborts the writer when dropped, so an error or a dropped session drops the socket at once (OkHttp `cancel()`).
struct AbortOnDrop(JoinHandle<Result<(), NetError>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The event a socket message means (Android's `Listener`); None for ones the session ignores.
fn on_message(
    message: Option<Result<Message, tungstenite::Error>>,
    close_stream_sent_at: Option<Instant>,
) -> Option<DeepgramEvent> {
    match message {
        Some(Ok(Message::Text(text))) => {
            let event = parse_deepgram_event(&text)?;
            if event == DeepgramEvent::Metadata {
                let sent_at = close_stream_sent_at?;
                debug!("Timing: CloseStream->Metadata {} ms", sent_at.elapsed().as_millis());
            }
            Some(event)
        }
        Some(Ok(Message::Close(frame))) => {
            // After CloseStream the server closes normally once everything is sent (Metadata normally came first).
            let (code, reason) = frame.map_or((1005, String::new()), |f| (u16::from(f.code), f.reason.to_string()));
            Some(if code == NORMAL_CLOSURE && close_stream_sent_at.is_some() {
                DeepgramEvent::Metadata
            } else {
                DeepgramEvent::Failed(deepgram_closed_message(code, &reason))
            })
        }
        Some(Ok(_)) => None,
        Some(Err(error)) => Some(DeepgramEvent::Failed(deepgram_network_error_message(&error.into()))),
        None => Some(DeepgramEvent::Failed(deepgram_network_error_message(&connection_closed()))),
    }
}

fn connection_closed() -> NetError {
    NetError { timeout: false, message: "connection closed".into() }
}

fn connect_error(error: WsConnectError) -> SttError {
    SttError(match error {
        WsConnectError::Http { status, headers, body } => {
            deepgram_http_error_message(status, &body, headers.get("dg-error").and_then(|v| v.to_str().ok()))
        }
        WsConnectError::InvalidHeader(_) => INVALID_KEY.into(),
        WsConnectError::Network(error) => deepgram_network_error_message(&error),
    })
}

#[cfg(test)]
mod tests;
