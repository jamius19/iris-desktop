/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Vercel AI Gateway streaming transcription (beta) over a WebSocket (Android `VercelGatewaySpeechToText.kt`): a start
//! frame, then the recording as raw PCM16 binary frames, then `audio-done`; the server streams transcript parts and
//! ends with `finish`. A stream that fails after connecting (dropped, error part, closed without `finish`, stalled) is
//! reconnected once and sent all audio again, within a deadline of the reconnect timeout plus time to resend the audio.
//! Each socket is a task (OkHttp's socket: a send queue written in order, a reader and pings) that is aborted when the
//! socket is replaced or the session ends.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use futures_util::StreamExt;
use log::{debug, warn};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio::time::{Instant, MissedTickBehavior, sleep, sleep_until, timeout};
use tokio_tungstenite::tungstenite::protocol::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Bytes, Message};

use super::protocol::{
    AUDIO_DONE, GatewayPart, LiveTranscript, PROTOCOL_VERSION, SPECIFICATION_VERSION, SUBPROTOCOL, TRANSCRIPTION_URL,
    gateway_closed_message, gateway_network_error_message, gateway_sample_rate, http_error_message, is_header_value,
    only_trims, parse_gateway_part, provider_options, start_frame, stream_url,
};
use crate::net::{self, KeySource, NetError, PING_INTERVAL, WsConnectError, WsStream};
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest};

const TAG: &str = "VercelGatewayStt";
const NO_KEY: &str = "Add your Vercel AI Gateway API key in Iris settings";
/// A key that can't be a header value (Android has no text for it).
const INVALID_KEY: &str = "Invalid Vercel AI Gateway API key";
const STALL_MESSAGE: &str = "Timed out waiting for the transcript";
const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);
const DRAIN_POLL: Duration = Duration::from_millis(10);
const SEND_TIMEOUT: &str = "Timed out sending audio to AI Gateway";

/// A reconnect's base budget (as Handy's), plus [`RESEND_BYTES_PER_SECOND`] for the audio it resends.
const RECONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RESEND_BYTES_PER_SECOND: u64 = 200_000;
const RECONNECT_DELAY: Duration = Duration::from_secs(1);
const RECONNECT_TIMED_OUT: &str = "Reconnecting to AI Gateway timed out";

/// Resent audio waits while more than this is queued (bounds the copies in the send queue).
const RESEND_QUEUE_LIMIT: usize = 1 << 20;

/// How long a normal end waits for its close frame to be written (OkHttp writes it in the background).
const CLOSE_TIMEOUT: Duration = Duration::from_secs(1);

pub struct VercelGatewaySpeechToText {
    api_key: KeySource,
    url: String,
    reconnect_timeout: Duration,
    reconnect_delay: Duration,
}

impl VercelGatewaySpeechToText {
    /// `client` is unused: the WebSocket opens its own connection (no warm-up to reuse).
    pub fn new(_client: reqwest::Client, api_key: KeySource) -> Self {
        Self::with_urls(api_key, TRANSCRIPTION_URL, RECONNECT_TIMEOUT, RECONNECT_DELAY)
    }

    pub(crate) fn with_urls(
        api_key: KeySource,
        url: &str,
        reconnect_timeout: Duration,
        reconnect_delay: Duration,
    ) -> Self {
        VercelGatewaySpeechToText { api_key, url: url.to_string(), reconnect_timeout, reconnect_delay }
    }
}

impl SpeechToTextProvider for VercelGatewaySpeechToText {
    fn sample_rate(&self, model: &str) -> u32 {
        gateway_sample_rate(model)
    }

    // Custom words are not sent: whether the Gateway forwards a biasing option is unverified.
    fn transcribe<'a>(
        &'a self,
        audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async move {
            let key = match net::api_key(&self.api_key).await {
                Ok(Some(key)) => key,
                Ok(None) => return Err(SttError(NO_KEY.into())),
                Err(message) => return Err(SttError(message.into())),
            };
            let model = request.model;
            let authorization = format!("Bearer {key}");
            // Checked before any audio is pulled, so the microphone never opens.
            if !is_header_value(&authorization) {
                return Err(SttError(INVALID_KEY.into()));
            }
            if !is_header_value(&model) {
                return Err(SttError(format!("Invalid model id: {model}")));
            }
            let target = Arc::new(Target {
                url: stream_url(&self.url, &model),
                headers: vec![
                    ("authorization", authorization),
                    // Only the marker: the key rides the header above, never a subprotocol the server may echo.
                    ("sec-websocket-protocol", SUBPROTOCOL.into()),
                    ("ai-gateway-protocol-version", PROTOCOL_VERSION.into()),
                    ("ai-gateway-auth-method", "api-key".into()),
                    ("ai-transcription-model-specification-version", SPECIFICATION_VERSION.into()),
                    ("ai-model-id", model.clone()),
                ],
                model: model.clone(),
            });
            let rate = gateway_sample_rate(&model);
            let start = start_frame(rate, provider_options(&model, &request.language, true).as_ref());
            let (events, mut received) = mpsc::unbounded_channel();
            let mut session = Session::new(self, target, rate, start, request.timeout, on_partial, events);
            // An error or a dropped future drops the session: its socket task is aborted (OkHttp `cancel()`).
            let text = session.run(audio, &mut received).await?;
            session.socket.close().await;
            Ok(text)
        })
    }
}

/// Where and how every socket of a session connects.
struct Target {
    url: String,
    headers: Vec<(&'static str, String)>,
    model: String,
}

/// What the socket tasks report, tagged with their attempt so a replaced socket's late events are ignored.
enum Event {
    Server(usize, GatewayPart),
    /// `audio-done` and everything before it was written.
    Drained(usize),
    /// The message, and whether a reconnect may help (false for an HTTP rejection of the handshake: key, model,
    /// quota, server).
    Failed(usize, String, bool),
}

/// One session's inputs, in the order the loop handles them.
enum Input {
    Audio(Vec<u8>),
    AudioEnd,
    Event(Event),
    Stall,
    DrainTimeout(&'static str),
}

/// The next audio chunk; owns the source meanwhile, so the loop never cancels a pull.
type Pull<'a> = BoxFuture<'a, (Box<dyn AudioSource + 'a>, Result<Option<Vec<u8>>, SttError>)>;

fn pull<'a>(mut audio: Box<dyn AudioSource + 'a>) -> Pull<'a> {
    Box::pin(async move {
        let chunk = audio.next().await;
        (audio, chunk)
    })
}

/// `future`'s output, or never when there is none.
async fn maybe<F: Future + Unpin>(future: Option<&mut F>) -> F::Output {
    match future {
        Some(future) => future.await,
        None => std::future::pending().await,
    }
}

/// One transcription: a single loop over the audio, the socket tasks' events and the timers.
struct Session<'p> {
    provider: &'p VercelGatewaySpeechToText,
    target: Arc<Target>,
    rate: u32,
    start: String,
    /// The stall timeout: how long a drained stream may go without any input.
    stall_timeout: Duration,
    on_partial: OnPartial,
    events: mpsc::UnboundedSender<Event>,
    started_at: Instant,
    /// Every chunk sent so far, to send again to a new stream.
    sent: Vec<Bytes>,
    sent_bytes: usize,
    attempt: usize,
    socket: Socket,
    stopped: bool,
    stopped_at: Instant,
    /// The current stream has sent all audio and `audio-done`: the stall timeout runs.
    drained: bool,
    /// Until when the current stream may take to drain, and the error after that.
    drain_deadline: Option<(Instant, &'static str)>,
    retried: bool,
    /// By when the reconnect (handshake retries, resend, drain if stopped) must be done.
    reconnect_deadline: Instant,
    transcript: LiveTranscript,
    /// The text last passed to `on_partial`.
    shown: String,
}

impl<'p> Session<'p> {
    fn new(
        provider: &'p VercelGatewaySpeechToText,
        target: Arc<Target>,
        rate: u32,
        start: String,
        stall_timeout: Duration,
        on_partial: OnPartial,
        events: mpsc::UnboundedSender<Event>,
    ) -> Self {
        let started_at = Instant::now();
        let socket = connect(0, &target, &start, &events, started_at);
        Session {
            provider,
            target,
            rate,
            start,
            stall_timeout,
            on_partial,
            events,
            started_at,
            sent: Vec::new(),
            sent_bytes: 0,
            attempt: 0,
            socket,
            stopped: false,
            stopped_at: started_at,
            drained: false,
            drain_deadline: None,
            retried: false,
            reconnect_deadline: started_at,
            transcript: LiveTranscript::new(),
            shown: String::new(),
        }
    }

    async fn run<'a>(
        &mut self,
        audio: Box<dyn AudioSource + 'a>,
        events: &mut mpsc::UnboundedReceiver<Event>,
    ) -> Result<String, SttError> {
        let mut audio = Some(pull(audio));
        let mut last_input = Instant::now();
        loop {
            let stall_at = last_input + self.stall_timeout;
            let (drain_at, drain_message) = self.drain_deadline.unwrap_or((stall_at, ""));
            let input = tokio::select! {
                (source, chunk) = maybe(audio.as_mut()) => match chunk? {
                    Some(chunk) => {
                        audio = Some(pull(source));
                        Input::Audio(chunk)
                    }
                    None => {
                        audio = None;
                        Input::AudioEnd
                    }
                },
                // The session holds a sender: never closed.
                Some(event) = events.recv() => Input::Event(event),
                // A silently dead connection (no read timeout on WebSockets) shows up here or by a missed pong.
                _ = sleep_until(stall_at), if self.drained => Input::Stall,
                _ = sleep_until(drain_at), if self.drain_deadline.is_some() => Input::DrainTimeout(drain_message),
            };
            if !matches!(input, Input::Stall) {
                last_input = Instant::now();
            }
            match input {
                Input::Audio(chunk) => {
                    let chunk = Bytes::from(chunk);
                    self.sent_bytes += chunk.len();
                    self.sent.push(chunk.clone());
                    self.socket.send(Message::Binary(chunk));
                }
                Input::AudioEnd => {
                    self.stopped = true;
                    self.stopped_at = Instant::now();
                    self.finish_audio(false);
                }
                Input::Event(Event::Drained(attempt)) if attempt == self.attempt => {
                    self.drained = true;
                    self.drain_deadline = None;
                }
                Input::Event(Event::Server(attempt, part)) if attempt == self.attempt => {
                    if let Some(text) = self.on_part(part).await? {
                        return Ok(text);
                    }
                }
                Input::Event(Event::Failed(attempt, message, retryable)) if attempt == self.attempt => {
                    self.fail(message, retryable).await?
                }
                Input::Event(_) => {}
                Input::Stall => self.fail(STALL_MESSAGE.into(), true).await?,
                Input::DrainTimeout(message) => return Err(SttError(message.into())),
            }
        }
    }

    /// Handles a part of the current stream; returns the final text on `finish`.
    async fn on_part(&mut self, part: GatewayPart) -> Result<Option<String>, SttError> {
        match part {
            GatewayPart::StreamStart { warnings } => warnings.iter().for_each(|w| warn!("{TAG}: Stream warning: {w}")),
            GatewayPart::Delta(delta) => {
                self.transcript.on_delta(&delta);
                self.show(self.transcript.text());
            }
            GatewayPart::Partial { text, start_second, duration_in_seconds } => {
                self.transcript.on_partial(&text, start_second, duration_in_seconds);
                self.show(self.transcript.text());
            }
            GatewayPart::Final(text) => {
                self.transcript.on_final(&text);
                self.shown = self.transcript.text();
                (self.on_partial)(self.shown.clone());
            }
            GatewayPart::Finish(text) => {
                if self.stopped {
                    debug!("{TAG}: Timing: audio-done->finish {} ms", self.stopped_at.elapsed().as_millis());
                } else {
                    warn!("{TAG}: finish before audio-done");
                }
                return Ok(Some(if text.trim().is_empty() { self.transcript.text() } else { text }));
            }
            GatewayPart::Error(message) => self.fail(message, true).await?,
        }
        Ok(None)
    }

    /// Live text that only cuts words off the end isn't shown: they come straight back.
    fn show(&mut self, text: String) {
        if only_trims(&self.shown, &text) {
            return;
        }
        self.shown = text.clone();
        (self.on_partial)(text);
    }

    /// Pads short audio, sends `audio-done` and starts the wait for the current stream's queue to drain: until the
    /// reconnect deadline if `after_reconnect`, else for [`DRAIN_TIMEOUT`].
    fn finish_audio(&mut self, after_reconnect: bool) {
        // OpenAI's upstream rejects under 100 ms of audio (e.g. an instant stop); pad it with silence.
        let min_bytes = (self.rate * 2 / 10) as usize;
        if self.sent_bytes < min_bytes {
            self.socket.send(Message::binary(vec![0u8; min_bytes - self.sent_bytes]));
        }
        self.socket.send_audio_done();
        // On a slow uplink audio may still be queued; only time the server once it's sent.
        self.drain_deadline = Some(if after_reconnect {
            (self.reconnect_deadline, RECONNECT_TIMED_OUT)
        } else {
            (Instant::now() + DRAIN_TIMEOUT, SEND_TIMEOUT)
        });
    }

    /// Reconnects once for a retryable failure and resends the audio; fails otherwise. After that, only a handshake
    /// that failed without a response (DNS/connect error: nothing was transcribed or billed) is tried again, every
    /// reconnect delay until the reconnect deadline, so a brief network drop recovers.
    async fn fail(&mut self, message: String, retryable: bool) -> Result<(), SttError> {
        let handshake_retry = self.retried && !self.socket.opened();
        if !retryable || (self.retried && !handshake_retry) {
            return Err(SttError(message));
        }
        let delay = self.provider.reconnect_delay;
        if handshake_retry && self.reconnect_remaining() <= delay {
            return Err(SttError(RECONNECT_TIMED_OUT.into()));
        }
        self.socket.cancel();
        self.drain_deadline = None;
        self.drained = false;
        if handshake_retry {
            warn!("{TAG}: Reconnect failed ({message}); trying again in {} ms", delay.as_millis());
            sleep(delay).await; // Audio waits in its source.
        } else {
            self.retried = true;
            let resend = Duration::from_millis(self.sent_bytes as u64 * 1_000 / RESEND_BYTES_PER_SECOND);
            self.reconnect_deadline = Instant::now() + self.provider.reconnect_timeout + resend;
            let seconds = self.sent_bytes as f64 / (self.rate as f64 * 2.0);
            warn!(
                "{TAG}: Stream for {} failed ({message}); reconnecting once and resending {seconds:.1} s",
                self.target.model
            );
        }
        self.attempt += 1;
        // The new stream transcribes the recording from its start. `shown` stays, so only prefix shrinks are hidden
        // (only_trims); the live text may jump back once while the new stream catches up.
        self.transcript = LiveTranscript::new();
        self.socket = connect(self.attempt, &self.target, &self.start, &self.events, self.started_at);
        let remaining = self.reconnect_remaining();
        let resend = async {
            for chunk in &self.sent {
                while self.socket.queued() > RESEND_QUEUE_LIMIT && !self.socket.failed() {
                    sleep(DRAIN_POLL).await;
                }
                self.socket.send(Message::Binary(chunk.clone()));
            }
        };
        if remaining.is_zero() || timeout(remaining, resend).await.is_err() {
            return Err(SttError(RECONNECT_TIMED_OUT.into()));
        }
        if self.stopped {
            self.finish_audio(true);
        }
        Ok(())
    }

    fn reconnect_remaining(&self) -> Duration {
        self.reconnect_deadline.saturating_duration_since(Instant::now())
    }
}

/// Opens a socket for `attempt` with the start frame queued (sent once the handshake completes).
fn connect(
    attempt: usize,
    target: &Arc<Target>,
    start: &str,
    events: &mpsc::UnboundedSender<Event>,
    started_at: Instant,
) -> Socket {
    let socket = Socket::open(attempt, target.clone(), events.clone(), started_at);
    socket.send(Message::text(start));
    socket
}

/// What the session queues for a socket.
enum Out {
    Frame(Message),
    /// `audio-done`, reported as [`Event::Drained`] once written.
    AudioDone,
    /// A normal close (1000); the task ends once it is written.
    Close,
}

/// A WebSocket run by its own task: frames are queued and written in order, the reader forwards parts as events.
/// Dropping it aborts the task, which drops the connection.
struct Socket {
    out: mpsc::UnboundedSender<Out>,
    /// Bytes of data queued and not written yet (OkHttp `queueSize()`).
    queued: Arc<AtomicUsize>,
    opened: Arc<AtomicBool>,
    failed: Arc<AtomicBool>,
    task: JoinHandle<()>,
}

impl Socket {
    fn open(attempt: usize, target: Arc<Target>, events: mpsc::UnboundedSender<Event>, started_at: Instant) -> Self {
        let (out, queue) = mpsc::unbounded_channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let opened = Arc::new(AtomicBool::new(false));
        let failed = Arc::new(AtomicBool::new(false));
        let (task_queued, task_opened, task_failed) = (queued.clone(), opened.clone(), failed.clone());
        let task = tokio::spawn(async move {
            let failure = match net::connect_ws(&target.url, &target.headers).await {
                Err(error) => Some(connect_failure(error, &target.model)),
                Ok(socket) => {
                    task_opened.store(true, Ordering::SeqCst);
                    if attempt == 0 {
                        debug!("{TAG}: Timing: transcribe->ws open {} ms", started_at.elapsed().as_millis());
                    }
                    run_socket(socket, attempt, queue, &task_queued, &events).await
                }
            };
            if let Some((message, retryable)) = failure {
                task_failed.store(true, Ordering::SeqCst);
                let _ = events.send(Event::Failed(attempt, message, retryable));
            }
        });
        Socket { out, queued, opened, failed, task }
    }

    fn send(&self, message: Message) {
        self.queued.fetch_add(message.len(), Ordering::SeqCst);
        let _ = self.out.send(Out::Frame(message));
    }

    fn send_audio_done(&self) {
        self.queued.fetch_add(AUDIO_DONE.len(), Ordering::SeqCst);
        let _ = self.out.send(Out::AudioDone);
    }

    fn queued(&self) -> usize {
        self.queued.load(Ordering::SeqCst)
    }

    fn opened(&self) -> bool {
        self.opened.load(Ordering::SeqCst)
    }

    fn failed(&self) -> bool {
        self.failed.load(Ordering::SeqCst)
    }

    /// Drops the connection and anything queued at once.
    fn cancel(&self) {
        self.task.abort();
    }

    /// Queues a normal close and gives it [`CLOSE_TIMEOUT`] to be written; the closing handshake isn't awaited.
    async fn close(mut self) {
        let _ = self.out.send(Out::Close);
        let _ = timeout(CLOSE_TIMEOUT, &mut self.task).await;
    }
}

impl Drop for Socket {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The failure of a handshake: an HTTP rejection isn't retried.
fn connect_failure(error: WsConnectError, model: &str) -> (String, bool) {
    match error {
        WsConnectError::Http { status, body, .. } => (http_error_message(status, &body), false),
        WsConnectError::InvalidHeader("ai-model-id") => (format!("Invalid model id: {model}"), false),
        WsConnectError::InvalidHeader(_) => (INVALID_KEY.into(), false),
        WsConnectError::Network(error) => (gateway_network_error_message(&error), true),
    }
}

/// Writes the queue in order and pings every [`PING_INTERVAL`], while reading parts; returns how the socket failed,
/// or None after a normal close.
async fn run_socket(
    socket: WsStream,
    attempt: usize,
    mut queue: mpsc::UnboundedReceiver<Out>,
    queued: &AtomicUsize,
    events: &mpsc::UnboundedSender<Event>,
) -> Option<(String, bool)> {
    let (mut sink, mut stream) = socket.split();
    let awaiting_pong = AtomicBool::new(false);
    let write = async {
        let mut ping = tokio::time::interval_at(Instant::now() + PING_INTERVAL, PING_INTERVAL);
        ping.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                out = queue.recv() => {
                    let (message, audio_done) = match out {
                        Some(Out::Frame(message)) => (message, false),
                        Some(Out::AudioDone) => (Message::text(AUDIO_DONE), true),
                        Some(Out::Close) | None => {
                            let frame = CloseFrame { code: CloseCode::Normal, reason: "".into() };
                            let _ = net::write_ws(&mut sink, Message::Close(Some(frame)), net::WRITE_TIMEOUT).await;
                            return None;
                        }
                    };
                    let len = message.len();
                    if let Err(error) = net::write_ws(&mut sink, message, net::WRITE_TIMEOUT).await {
                        return Some((gateway_network_error_message(&error), true));
                    }
                    queued.fetch_sub(len, Ordering::SeqCst);
                    if audio_done {
                        let _ = events.send(Event::Drained(attempt));
                    }
                }
                _ = ping.tick() => {
                    if awaiting_pong.swap(true, Ordering::SeqCst) {
                        return Some((gateway_network_error_message(&NetError::ping_timeout()), true));
                    }
                    let ping = Message::Ping(Bytes::new());
                    if let Err(error) = net::write_ws(&mut sink, ping, net::WRITE_TIMEOUT).await {
                        return Some((gateway_network_error_message(&error), true));
                    }
                }
            }
        }
    };
    let read = async {
        while let Some(message) = stream.next().await {
            match message {
                Ok(Message::Text(text)) => {
                    if let Some(part) = parse_gateway_part(&text) {
                        let _ = events.send(Event::Server(attempt, part));
                    }
                }
                Ok(Message::Pong(_)) => awaiting_pong.store(false, Ordering::SeqCst),
                Ok(Message::Close(frame)) => {
                    let reason = frame.as_ref().map_or("", |frame| frame.reason.as_str());
                    return Some((gateway_closed_message(reason), true));
                }
                Ok(_) => {}
                Err(error) => return Some((gateway_network_error_message(&error.into()), true)),
            }
        }
        Some((gateway_closed_message(""), true))
    };
    tokio::select! {
        failure = write => failure,
        failure = read => failure,
    }
}

#[cfg(test)]
mod tests;
