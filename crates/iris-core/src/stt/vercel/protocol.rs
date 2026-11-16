/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Frame building and parsing for Vercel AI Gateway's streaming transcription WebSocket (beta), free of I/O for unit
//! tests (Android `GatewayTranscriptionProtocol.kt`). No public docs: the protocol follows the AI SDK source
//! (`@ai-sdk/gateway` 4.0.143 `gateway-transcription-model.ts`, `gateway-realtime-auth.ts`; `@ai-sdk/provider-utils`
//! `transcription-stream-envelope.ts`) and Handy's `vercel_live_transcription.rs`. Uploads after stop:
//! https://vercel.com/docs/ai-gateway/modalities/speech-to-text and Handy's `vercel_transcription.rs`.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

use crate::cleanup::GatewayModel;
use crate::net::NetError;
use crate::stt::joined;

/// Upgraded to a WebSocket (`https` here is `wss` on the wire); a plain POST for an upload.
pub const TRANSCRIPTION_URL: &str = "https://ai-gateway.vercel.sh/v4/ai/transcription-model";

/// Warm-up target: the public model catalog answers a keyless `HEAD` quickly with no body.
pub const GATEWAY_WARM_UP_URL: &str = "https://ai-gateway.vercel.sh/v1/models";

/// Marker subprotocol of a transcription stream handshake.
pub const SUBPROTOCOL: &str = "ai-gateway-transcription.v1";

/// `ai-gateway-protocol-version` the AI SDK sends.
pub const PROTOCOL_VERSION: &str = "0.0.1";

/// `ai-transcription-model-specification-version` the AI SDK sends.
pub const SPECIFICATION_VERSION: &str = "4";

pub const AUDIO_DONE: &str = r#"{"type":"transcription-stream.audio-done"}"#;

/// The catalog tag of models that stream over the WebSocket.
pub const STREAMING_TAG: &str = "websocket-transcription";

/// A catalog transcription model: live ones stream over this WebSocket, the others take an upload after stop.
pub fn is_transcription_model(model: &GatewayModel) -> bool {
    model.r#type.as_deref() == Some("transcription")
}

/// A catalog model that streams over this WebSocket.
pub fn is_streaming_transcription_model(model: &GatewayModel) -> bool {
    is_transcription_model(model) && model.tags.iter().any(|tag| tag == STREAMING_TAG)
}

/// Whether `model` is transcribed live (streamed while recording) rather than uploaded after stop, from its catalog
/// entry's tags. An id not in `catalog` (typed by hand, or the catalog isn't loaded) counts as live: the WebSocket
/// reports an unknown or non-streaming model clearly.
pub fn gateway_stt_live(model: &str, catalog: Option<&[GatewayModel]>) -> bool {
    catalog
        .and_then(|catalog| catalog.iter().find(|m| m.id == model.trim()))
        .is_none_or(is_streaming_transcription_model)
}

/// `base_url` with the model in `?ai-model-id=` (its `/` encoded).
pub fn stream_url(base_url: &str, model: &str) -> String {
    let mut url = url::Url::parse(base_url).expect("transcription URL");
    url.query_pairs_mut().append_pair("ai-model-id", model);
    url.to_string()
}

/// `model`'s provider: its id prefix; the Gateway qualifies a bare id as `openai/<id>`.
fn gateway_provider(model: &str) -> &str {
    model.split_once('/').map_or("openai", |(provider, _)| provider)
}

/// OpenAI's realtime models take 24 kHz PCM16; the others get 16 kHz (as Handy sends them).
pub fn gateway_sample_rate(model: &str) -> u32 {
    if gateway_provider(model) == "openai" { 24_000 } else { 16_000 }
}

/// The language option for `model`'s provider (its id prefix; the AI SDK provider names the Gateway passes options
/// on to), and Grok's interim results when `streaming`. None when there is nothing to send; a provider without a known
/// language option detects the language itself. As Handy's: an upload to gpt-transcribe takes a `languages` list.
pub fn provider_options(model: &str, language: &str, streaming: bool) -> Option<Map<String, Value>> {
    let provider = gateway_provider(model);
    let code = language.trim();
    let mut options = Map::new();
    if !code.is_empty() {
        let id = model.split_once('/').map_or(model, |(_, id)| id);
        let option = match provider {
            "openai" if !streaming && id == "gpt-transcribe" => Some(json!({ "languages": [code] })),
            "openai" | "mistral" => Some(json!({ "language": code })),
            "elevenlabs" => Some(json!({ "languageCode": code })),
            "google" => Some(json!({ "languageCodes": [code] })),
            _ => None,
        };
        if let Some(option) = option {
            options.insert(provider.to_string(), option);
        }
    }
    // Grok STT streams only ~3 s chunks unless asked for interim results. The Gateway names the model
    // `spacexai/...` while the AI SDK adapter reads `xai`, so it goes under both.
    if streaming && (provider == "xai" || provider == "spacexai") {
        for key in ["xai", "spacexai"] {
            options.insert(key.to_string(), json!({ "streaming": { "interimResults": true } }));
        }
    }
    (!options.is_empty()).then_some(options)
}

/// The first frame: raw PCM16 at `rate`, plus `options` when set.
pub fn start_frame(rate: u32, options: Option<&Map<String, Value>>) -> String {
    let mut frame = json!({
        "type": "transcription-stream.start",
        "inputAudioFormat": { "type": "audio/pcm", "rate": rate },
    });
    if let Some(options) = options {
        frame["providerOptions"] = Value::Object(options.clone());
    }
    frame.to_string()
}

/// The server parts the client acts on.
#[derive(Clone, Debug, PartialEq)]
pub enum GatewayPart {
    /// `warnings`: e.g. unsupported provider options, as JSON.
    StreamStart { warnings: Vec<String> },
    /// Append-only text of the current segment.
    Delta(String),
    /// The current segment's text so far; later parts may revise it.
    Partial { text: String, start_second: Option<f64>, duration_in_seconds: Option<f64> },
    /// A settled segment.
    Final(String),
    /// The whole transcript; the server closes after it.
    Finish(String),
    /// Ends the stream; the message is user-readable.
    Error(String),
}

/// One server text frame (a JSON part). None for unknown types (`response-metadata`, `raw`, ...), malformed JSON and
/// parts missing a required field, which the envelope says to ignore.
pub fn parse_gateway_part(text: &str) -> Option<GatewayPart> {
    let part = parse_object(text)?;
    match string(&part, "type")? {
        "stream-start" => Some(GatewayPart::StreamStart { warnings: warnings(&part) }),
        "transcript-delta" => string(&part, "delta").map(|delta| GatewayPart::Delta(delta.to_string())),
        "transcript-partial" => string(&part, "text").map(|text| GatewayPart::Partial {
            text: text.to_string(),
            start_second: number(&part, "startSecond"),
            duration_in_seconds: number(&part, "durationInSeconds"),
        }),
        "transcript-final" => string(&part, "text").map(|text| GatewayPart::Final(text.to_string())),
        // Lenient: a dropped finish would fail the stream; a blank text falls back to the live text.
        "finish" => Some(GatewayPart::Finish(string(&part, "text").unwrap_or_default().to_string())),
        "error" => part.get("error").map(|error| GatewayPart::Error(error_part_message(error))),
        _ => None,
    }
}

/// An `error` part's payload: `{ name, message }` or a string.
pub fn error_part_message(error: &Value) -> String {
    let message = match error {
        Value::Object(error) => string(error, "message"),
        Value::String(message) => Some(message.as_str()),
        _ => None,
    };
    match message {
        Some(message) if !message.trim().is_empty() => format!("AI Gateway error: {message}"),
        _ => "AI Gateway error".to_string(),
    }
}

/// The JSON body of an upload, `{"audio":"<base64 WAV>","mediaType":"audio/wav","providerOptions":...}`, encoded
/// straight into one buffer.
pub fn upload_body(wav: &[u8], options: Option<&Map<String, Value>>) -> Vec<u8> {
    let suffix = match options {
        Some(options) => format!(r#"","mediaType":"audio/wav","providerOptions":{}}}"#, Value::Object(options.clone())),
        None => r#"","mediaType":"audio/wav"}"#.to_string(),
    };
    let prefix = r#"{"audio":""#;
    let mut body = String::with_capacity(prefix.len() + wav.len().div_ceil(3) * 4 + suffix.len());
    body.push_str(prefix);
    STANDARD.encode_string(wav, &mut body);
    body.push_str(&suffix);
    body.into_bytes()
}

/// The `text` of an upload's response and its `warnings` (as JSON); None without a `text`.
pub fn parse_upload_response(body: &str) -> Option<(String, Vec<String>)> {
    let response = parse_object(body)?;
    let text = string(&response, "text")?.to_string();
    Some((text, warnings(&response)))
}

/// Whether `value` can be sent as a header value as OkHttp checks it: tab and printable ASCII only.
pub fn is_header_value(value: &str) -> bool {
    crate::net::is_header_value(value)
}

pub fn gateway_closed_message(reason: &str) -> String {
    if reason.trim().is_empty() {
        "AI Gateway closed the connection".to_string()
    } else {
        format!("AI Gateway closed the connection: {reason}")
    }
}

/// For a socket or upload failure without an HTTP response.
pub fn gateway_network_error_message(error: &NetError) -> String {
    format!("Network error: {}", error.message)
}

/// The message for an HTTP error `code` with `body` (Android `cleanup.vercel.httpErrorMessage`, service "AI
/// Gateway").
pub fn http_error_message(code: u16, body: &str) -> String {
    crate::cleanup::protocol::http_error_message(code, body, crate::cleanup::protocol::AI_GATEWAY)
}

fn parse_object(text: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str(text) {
        Ok(Value::Object(object)) => Some(object),
        _ => None,
    }
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key)?.as_str()
}

fn number(object: &Map<String, Value>, key: &str) -> Option<f64> {
    object.get(key)?.as_f64()
}

fn warnings(object: &Map<String, Value>) -> Vec<String> {
    match object.get("warnings") {
        Some(Value::Array(warnings)) => warnings.iter().map(Value::to_string).collect(),
        _ => Vec::new(),
    }
}

/// How far apart two reported times may be and still count as the same.
const CHUNK_TOLERANCE_SECONDS: f64 = 0.05;

/// The live text of a stream (ported from Handy): the settled segments, and the segment still being heard as its
/// deltas, its earlier partial chunks and the latest partial. Providers differ: Gemini's partial is the whole segment
/// so far (its deltas then repeat it), Microsoft MAI's only what follows the deltas, and Grok STT sends each ~3 s
/// chunk of an utterance as its own partial.
#[derive(Default)]
pub struct LiveTranscript {
    settled: String,
    deltas: String,
    /// Partials a later one followed on from rather than revised.
    chunks: String,
    partial: String,
    /// Where the latest partial starts and ends in the audio, when the part says.
    partial_span: Option<(f64, f64)>,
}

impl LiveTranscript {
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything heard so far.
    pub fn text(&self) -> String {
        let current = if self.partial.trim_start().starts_with(self.deltas.trim()) {
            self.partial.clone()
        } else {
            format!("{}{}", self.deltas, self.partial)
        };
        joined(&self.settled, joined(&self.chunks, current.trim()).trim())
    }

    /// A delta supersedes the partial before it.
    pub fn on_delta(&mut self, delta: &str) {
        self.deltas.push_str(delta);
        self.partial.clear();
    }

    /// A partial revises the one before it, unless it starts later, where that one ended (a new Grok chunk): then the
    /// earlier one is kept until the segment's final.
    pub fn on_partial(&mut self, text: &str, start_second: Option<f64>, duration_in_seconds: Option<f64>) {
        if let (Some(start), Some((span_start, span_end))) = (start_second, self.partial_span)
            && start > span_start + CHUNK_TOLERANCE_SECONDS
            && start >= span_end - CHUNK_TOLERANCE_SECONDS
        {
            self.chunks = joined(&self.chunks, self.partial.trim());
        }
        self.partial = text.to_string();
        self.partial_span = match (start_second, duration_in_seconds) {
            (Some(start), Some(duration)) => Some((start, start + duration)),
            _ => None,
        };
    }

    /// Settles the current segment as `text`.
    pub fn on_final(&mut self, text: &str) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            self.settled = joined(&self.settled, trimmed);
        }
        self.deltas.clear();
        self.chunks.clear();
        self.partial.clear();
        self.partial_span = None;
    }
}

/// Whether `next` only cuts words off the end of `shown`. Such text isn't shown while streaming, as the cut words
/// come straight back (Grok ends a chunk short of its earlier guess; MAI sends settled words as a delta just before
/// the partial with the rest).
pub fn only_trims(shown: &str, next: &str) -> bool {
    next.len() < shown.len() && shown.starts_with(next)
}

#[cfg(test)]
mod tests;
