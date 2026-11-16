/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! OpenAI Realtime transcription-only session over a WebSocket (Android `OpenAiRealtimeSpeechToText.kt`). Audio is
//! appended as it is recorded; when the audio ends the buffer is committed and the session waits for that item's final
//! transcript.

use std::time::Duration;

use futures_util::{FutureExt, SinkExt, StreamExt};
use log::debug;
use tokio::time::{Instant, MissedTickBehavior, interval_at, sleep_until, timeout};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

use super::protocol::{
    COMMIT_AUDIO, MIN_COMMIT_BYTES, REALTIME_URL, RealtimeEvent, TranscriptAssembler, append_audio, closed_message,
    connection_failure_message, has_live_text, parse_server_event, session_update, takes_prompt, uses_keywords,
};
use super::{INVALID_API_KEY, api_key, bearer};
use crate::net::{self, KeySource, NetError, WsConnectError, WsStream};
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest};

/// OkHttp's default write timeout: a message that can't be written for this long fails the socket.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the last audio and the commit may take to send after stop.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

const TRANSCRIPT_TIMEOUT: &str = "Timed out waiting for the transcript";

pub struct OpenAiRealtimeSpeechToText {
    api_key: KeySource,
    url: String,
    drain_timeout: Duration,
    ping_interval: Duration,
}

impl OpenAiRealtimeSpeechToText {
    /// The WebSocket opens its own connection (no warm-up on desktop), so no HTTP client.
    pub fn new(api_key: KeySource) -> Self {
        OpenAiRealtimeSpeechToText {
            api_key,
            url: REALTIME_URL.into(),
            drain_timeout: DRAIN_TIMEOUT,
            ping_interval: net::PING_INTERVAL,
        }
    }

    #[cfg(test)]
    fn with_url(api_key: KeySource, url: String, ping_interval: Duration) -> Self {
        OpenAiRealtimeSpeechToText { api_key, url, drain_timeout: DRAIN_TIMEOUT, ping_interval }
    }

    async fn session(
        &self,
        mut audio: Box<dyn AudioSource + '_>,
        request: SttRequest,
        on_partial: OnPartial,
    ) -> Result<String, SttError> {
        let key = api_key(&self.api_key).await?;
        let authorization = bearer(&key).ok_or_else(|| SttError(INVALID_API_KEY.into()))?;
        let started = Instant::now();
        // Recording starts at once and its audio waits for the handshake (OkHttp queues messages until then).
        let headers = [("authorization", authorization)];
        let connect = net::connect_ws(&self.url, &headers);
        tokio::pin!(connect);
        let (mut queued, mut recording) = (Vec::new(), true);
        let mut socket = loop {
            tokio::select! {
                result = &mut connect => break result.map_err(connect_failure)?,
                chunk = audio.next(), if recording => match chunk? {
                    Some(chunk) => queued.push(chunk),
                    None => recording = false,
                },
            }
        };
        debug!("Timing: transcribe->ws open {} ms", started.elapsed().as_millis());

        send(&mut socket, session_update(&request.model, &request.language, &request.custom_words)).await?;
        let mut sent_bytes = 0;
        for chunk in queued {
            send(&mut socket, append_audio(&chunk)).await?;
            sent_bytes += chunk.len();
        }
        // Set once the commit is sent: the transcript must arrive by then.
        let mut deadline = None;
        if !recording {
            deadline = Some(self.commit(&mut socket, sent_bytes, request.timeout).await?);
        }
        let mut ping = interval_at(Instant::now() + self.ping_interval, self.ping_interval);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut awaiting_pong = false;
        let mut transcript = TranscriptAssembler::default();
        let text = loop {
            tokio::select! {
                biased;
                message = socket.next() => match message {
                    Some(Ok(Message::Text(text))) => {
                        let Some(event) = parse_server_event(text.as_str()) else { continue };
                        match event {
                            RealtimeEvent::Delta { item_id, delta } => {
                                transcript.on_delta(&item_id, &delta);
                                on_partial(transcript.live_text());
                            }
                            RealtimeEvent::Completed { item_id, transcript: text } => {
                                transcript.on_completed(&item_id, &text);
                                on_partial(transcript.live_text());
                            }
                            RealtimeEvent::Committed { item_id } => transcript.on_committed(&item_id),
                            RealtimeEvent::Failed(message) => return Err(SttError(message)),
                        }
                        if let Some(text) = transcript.final_text() {
                            break text;
                        }
                    }
                    Some(Ok(Message::Pong(_))) => awaiting_pong = false,
                    Some(Ok(Message::Close(frame))) => {
                        return Err(SttError(closed_message(frame.as_ref().map_or("", |f| f.reason.as_str()))));
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(network_failure(&NetError::from(e))),
                    None => return Err(SttError(connection_failure_message(None, "connection closed"))),
                },
                _ = ping.tick() => {
                    // OkHttp's pingInterval: the previous ping's pong should have come back by now.
                    if awaiting_pong {
                        return Err(network_failure(&NetError::ping_timeout()));
                    }
                    awaiting_pong = true;
                    write(&mut socket, Message::Ping(Default::default())).await?;
                }
                chunk = audio.next(), if recording => match chunk? {
                    Some(chunk) => {
                        send(&mut socket, append_audio(&chunk)).await?;
                        sent_bytes += chunk.len();
                    }
                    None => {
                        recording = false;
                        deadline = Some(self.commit(&mut socket, sent_bytes, request.timeout).await?);
                    }
                },
                _ = until(deadline) => return Err(SttError(TRANSCRIPT_TIMEOUT.into())),
            }
        };
        // Doesn't wait: writes the close frame if the socket takes it now, and drops the socket without the handshake.
        let close = CloseFrame { code: CloseCode::Normal, reason: "".into() };
        let _ = socket.send(Message::Close(Some(close))).now_or_never();
        Ok(text)
    }

    /// Pads the audio to the 100 ms the server accepts in a commit (e.g. an instant stop), commits it, and returns when
    /// the transcript is due. On a slow uplink sending may take a while; only the server's time counts.
    async fn commit(&self, socket: &mut WsStream, sent_bytes: usize, wait: Duration) -> Result<Instant, SttError> {
        let messages = async {
            if sent_bytes < MIN_COMMIT_BYTES {
                send(socket, append_audio(&vec![0; MIN_COMMIT_BYTES - sent_bytes])).await?;
            }
            send(socket, COMMIT_AUDIO.into()).await
        };
        match timeout(self.drain_timeout, messages).await {
            Ok(result) => result.map(|()| Instant::now() + wait),
            Err(_) => Err(SttError("Timed out sending audio to OpenAI".into())),
        }
    }
}

impl SpeechToTextProvider for OpenAiRealtimeSpeechToText {
    fn live_text(&self, model: &str) -> bool {
        has_live_text(model)
    }

    fn takes_custom_words(&self, model: &str) -> bool {
        uses_keywords(model) || takes_prompt(model)
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

async fn send(socket: &mut WsStream, text: String) -> Result<(), SttError> {
    write(socket, Message::text(text)).await
}

/// Sends and flushes `message` within [`WRITE_TIMEOUT`].
async fn write(socket: &mut WsStream, message: Message) -> Result<(), SttError> {
    match timeout(WRITE_TIMEOUT, socket.send(message)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => Err(network_failure(&NetError::from(e))),
        Err(_) => Err(network_failure(&NetError::timeout())),
    }
}

fn network_failure(error: &NetError) -> SttError {
    SttError(connection_failure_message(None, &error.message))
}

fn connect_failure(error: WsConnectError) -> SttError {
    match error {
        WsConnectError::InvalidHeader(_) => SttError(INVALID_API_KEY.into()),
        WsConnectError::Http { status, .. } => SttError(connection_failure_message(Some(status), "")),
        WsConnectError::NoSubProtocol => unreachable!("no subprotocol is asked for"),
        WsConnectError::Network(e) => network_failure(&e),
    }
}

/// Completes at `deadline`; never without one.
async fn until(deadline: Option<Instant>) {
    match deadline {
        Some(deadline) => sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests;
