/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Soniox real-time transcription over a WebSocket (Android `SonioxSpeechToText.kt`; one socket, no reconnect). The
//! start frame goes first, then the audio as raw PCM16 binary frames while recording; when the audio ends, an empty
//! text frame ends the stream and the final tokens are the transcript once a `finished` response arrives. Audio
//! recorded before the handshake completes is queued (OkHttp's send queue). No warm-up: tungstenite opens its own
//! connection, so a pooled one can't be reused.

use std::pin::pin;
use std::time::Duration;

use futures_util::{FutureExt, SinkExt, StreamExt};
use log::debug;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use super::ModelInfo;
use super::protocol::{
    END_OF_AUDIO, SONIOX_SAMPLE_RATE, SonioxEvent, SonioxRegion, SonioxTranscript, is_no_audio_error,
    parse_soniox_event, soniox_authorization, soniox_closed_message, soniox_error_message, soniox_http_error_message,
    soniox_language_hints, soniox_network_error_message, soniox_start_message,
};
use crate::net::{self, KeySource, NetError, PING_INTERVAL, WsConnectError};
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest};

/// How long the queued audio may take to go out once the recording ended.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

/// A close frame without a status (RFC 6455's "no status received").
const NO_STATUS: u16 = 1005;

pub struct SonioxSpeechToText {
    api_key: KeySource,
    model_info: ModelInfo,
    url: String,
    /// A frame (audio or ping) not written within this fails the socket.
    write_timeout: Duration,
}

impl SonioxSpeechToText {
    /// On `region`'s WebSocket URL. `_client` is unused: the socket opens its own connection.
    pub fn new(_client: reqwest::Client, api_key: KeySource, model_info: ModelInfo, region: SonioxRegion) -> Self {
        Self::with_url(api_key, model_info, region.web_socket_url())
    }

    pub(crate) fn with_url(api_key: KeySource, model_info: ModelInfo, url: &str) -> Self {
        SonioxSpeechToText { api_key, model_info, url: url.to_string(), write_timeout: net::WRITE_TIMEOUT }
    }

    async fn session(
        &self,
        audio: Box<dyn AudioSource + '_>,
        request: SttRequest,
        on_partial: OnPartial,
    ) -> Result<String, SttError> {
        let key = match net::api_key(&self.api_key).await {
            Ok(Some(key)) => key,
            Ok(None) => return Err(SttError("Add your Soniox API key in Iris settings".into())),
            Err(message) => return Err(SttError(message.into())),
        };
        // Before pulling audio, as OkHttp's request builder throws.
        soniox_authorization(&key)?;
        let started_at = Instant::now();
        let hints = soniox_language_hints(&request.language);
        // Only with hints: the catalogue lookup may wait for a load (the audio queues in the controller).
        let lookup = (!hints.is_empty()).then(|| (self.model_info)(request.model.clone()));
        let (outgoing, queue) = mpsc::unbounded_channel();
        let (written, mut written_rx) = watch::channel(0usize);
        let start =
            |strict: bool| Message::text(soniox_start_message(&request.model, &hints, strict, &request.custom_words));

        let mut producer = pin!(produce(audio, lookup, start, outgoing));
        let mut connection = pin!(self.connection(&key, queue, &written, &on_partial, started_at));

        let queued = tokio::select! {
            queued = &mut producer => queued?,
            text = &mut connection => {
                // Finished early: the recording still runs to its end, as the Android one does.
                let text = text?;
                producer.await?;
                return Ok(text);
            }
        };
        // On a slow uplink the queued audio may still be going out; only time the server once it's sent. A socket
        // failure meanwhile ends the session.
        let drained = tokio::time::timeout(DRAIN_TIMEOUT, written_rx.wait_for(|&sent| sent >= queued));
        tokio::select! {
            biased;
            text = &mut connection => return text,
            drained = drained => {
                if drained.is_err() {
                    return Err(SttError("Timed out sending audio to Soniox".into()));
                }
            }
        }
        tokio::time::timeout(request.timeout, connection)
            .await
            .unwrap_or_else(|_| Err(SttError("Timed out waiting for the transcript".into())))
    }

    /// Connects, sends what the producer queues (counting into `written`), pings, and returns the final transcript
    /// once `finished` arrives. Returning ends the socket: a normal end sends a close frame without waiting for the
    /// server's; an error just drops it.
    async fn connection(
        &self,
        key: &str,
        mut queue: mpsc::UnboundedReceiver<Message>,
        written: &watch::Sender<usize>,
        on_partial: &OnPartial,
        started_at: Instant,
    ) -> Result<String, SttError> {
        let socket =
            net::connect_ws(&self.url, &[("authorization", format!("Bearer {key}"))]).await.map_err(|e| match e {
                WsConnectError::InvalidHeader(_) => SttError("Invalid Soniox API key".into()),
                WsConnectError::Http { status, body, .. } => SttError(soniox_http_error_message(status, &body)),
                WsConnectError::Network(e) => network_error(&e),
            })?;
        debug!("Timing: transcribe->ws open {} ms", started_at.elapsed().as_millis());
        let (mut sink, mut stream) = socket.split();
        let mut ping = tokio::time::interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let (mut queue_open, mut awaiting_pong, mut sent) = (true, false, 0);
        let mut end_sent_at = None;
        let mut transcript = SonioxTranscript::default();
        loop {
            tokio::select! {
                message = queue.recv(), if queue_open => match message {
                    None => queue_open = false,
                    Some(message) => {
                        let end = matches!(&message, Message::Text(text) if text.as_str() == END_OF_AUDIO);
                        net::write_ws(&mut sink, message, self.write_timeout).await.map_err(|e| network_error(&e))?;
                        if end {
                            end_sent_at = Some(Instant::now());
                        }
                        sent += 1;
                        written.send_replace(sent);
                    }
                },
                _ = ping.tick() => {
                    if awaiting_pong {
                        return Err(network_error(&NetError::ping_timeout()));
                    }
                    let ping = Message::Ping(Default::default());
                    net::write_ws(&mut sink, ping, self.write_timeout).await.map_err(|e| network_error(&e))?;
                    awaiting_pong = true;
                }
                frame = stream.next() => match frame {
                    Some(Ok(Message::Text(text))) => {
                        let done = match parse_soniox_event(&text) {
                            None => None,
                            Some(SonioxEvent::Tokens { tokens, finished }) => {
                                transcript.on_tokens(&tokens);
                                on_partial(transcript.live_text());
                                if finished && let Some(at) = end_sent_at {
                                    debug!("Timing: end->finished {} ms", at.elapsed().as_millis());
                                }
                                finished.then(|| transcript.final_text())
                            }
                            Some(SonioxEvent::Error { code, message }) => {
                                if !is_no_audio_error(code, message.as_deref()) {
                                    return Err(SttError(soniox_error_message(code, message.as_deref())));
                                }
                                // Stopped before any audio: blank, so the controller shows "No speech detected".
                                Some(String::new())
                            }
                        };
                        if let Some(text) = done {
                            // Only starts the close: the closing handshake isn't awaited.
                            let close = CloseFrame { code: CloseCode::Normal, reason: Default::default() };
                            let _ = sink.send(Message::Close(Some(close))).now_or_never();
                            return Ok(text);
                        }
                    }
                    Some(Ok(Message::Pong(_))) => awaiting_pong = false,
                    // The server closes after `finished` (already handled) or after an error frame (handled first).
                    Some(Ok(Message::Close(frame))) => {
                        let (code, reason) =
                            frame.map_or((NO_STATUS, String::new()), |f| (f.code.into(), f.reason.to_string()));
                        return Err(SttError(soniox_closed_message(code, &reason)));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(network_error(&e.into())),
                    None => {
                        return Err(network_error(&NetError { timeout: false, message: "connection closed".into() }));
                    }
                }
            }
        }
    }
}

/// Pulls the audio and queues the start frame, the non-empty chunks and the end frame. The start frame goes at the
/// first chunk (so the microphone opens at once) and waits for the catalogue lookup, which starts right away. Returns
/// how many frames it queued.
async fn produce(
    mut audio: Box<dyn AudioSource + '_>,
    lookup: Option<BoxFuture<'static, Option<super::SonioxModel>>>,
    start: impl Fn(bool) -> Message,
    outgoing: mpsc::UnboundedSender<Message>,
) -> Result<usize, SttError> {
    let (strict_tx, strict_rx) = oneshot::channel();
    let lookup = async move {
        let strict = match lookup {
            Some(lookup) => lookup.await.is_some_and(|entry| entry.supports_language_hints_strict),
            None => false,
        };
        let _ = strict_tx.send(strict);
        std::future::pending().await
    };
    let frames = async move {
        let mut strict = Some(strict_rx);
        let mut queued = 0;
        // A send fails only once the connection ended, which ends the session too.
        let mut queue = |message: Message| {
            let _ = outgoing.send(message);
            queued += 1;
        };
        while let Some(chunk) = audio.next().await? {
            // An empty chunk is only an empty chunk: skipped.
            if chunk.is_empty() {
                continue;
            }
            if let Some(strict) = strict.take() {
                queue(start(strict.await.unwrap_or(false)));
            }
            queue(Message::binary(chunk));
        }
        if let Some(strict) = strict.take() {
            queue(start(strict.await.unwrap_or(false)));
        }
        queue(Message::text(END_OF_AUDIO));
        Ok(queued)
    };
    tokio::select! {
        result = frames => result,
        result = lookup => result,
    }
}

fn network_error(error: &NetError) -> SttError {
    SttError(soniox_network_error_message(error))
}

impl SpeechToTextProvider for SonioxSpeechToText {
    fn sample_rate(&self, _model: &str) -> u32 {
        SONIOX_SAMPLE_RATE
    }

    /// Billed by wall-clock stream time, and wants audio at real-time pace: a speed-up saves nothing.
    fn allows_speed_up(&self) -> bool {
        false
    }

    fn takes_custom_words(&self, _model: &str) -> bool {
        true
    }

    fn transcribe<'a>(
        &'a self,
        audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(self.session(audio, request, on_partial))
    }
}

#[cfg(test)]
mod tests;
