/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Request building and message parsing for Soniox's speech-to-text APIs, free of I/O (Android `SonioxProtocol.kt`).
//! Checked 2026-11-13:
//! https://docs.soniox.com/api-reference/stt/websocket-api
//! https://docs.soniox.com/authentication/websocket-authentication
//! https://docs.soniox.com/stt/rt/connection-keepalive, https://docs.soniox.com/stt/rt/error-handling
//! https://docs.soniox.com/api-reference/stt/files/upload_file
//! https://docs.soniox.com/api-reference/stt/files/delete_file
//! https://docs.soniox.com/api-reference/stt/transcriptions/create_transcription (also get_transcription,
//! get_transcription_transcript, delete_transcription)
//! https://docs.soniox.com/api-reference/stt/get_models, https://docs.soniox.com/data-residency
//! https://docs.soniox.com/api-reference/errors

use reqwest::header::HeaderValue;
use serde_json::{Map, Value, json};

use crate::net::NetError;
use crate::stt::SttError;

/// A Soniox project's data-residency region: its key works only on that region's domains. The WebSocket URL is
/// `https` (`net::connect_ws` takes it as `wss`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SonioxRegion {
    #[default]
    Us,
    Eu,
    Jp,
    In,
}

impl SonioxRegion {
    pub const ALL: [SonioxRegion; 4] = [SonioxRegion::Us, SonioxRegion::Eu, SonioxRegion::Jp, SonioxRegion::In];

    pub fn id(self) -> &'static str {
        match self {
            SonioxRegion::Us => "us",
            SonioxRegion::Eu => "eu",
            SonioxRegion::Jp => "jp",
            SonioxRegion::In => "in",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SonioxRegion::Us => "United States",
            SonioxRegion::Eu => "European Union",
            SonioxRegion::Jp => "Japan",
            SonioxRegion::In => "India",
        }
    }

    pub fn rest_url(self) -> &'static str {
        match self {
            SonioxRegion::Us => "https://api.soniox.com",
            SonioxRegion::Eu => "https://api.eu.soniox.com",
            SonioxRegion::Jp => "https://api.jp.soniox.com",
            SonioxRegion::In => "https://api.in.soniox.com",
        }
    }

    pub fn web_socket_url(self) -> &'static str {
        match self {
            SonioxRegion::Us => "https://stt-rt.soniox.com/transcribe-websocket",
            SonioxRegion::Eu => "https://stt-rt.eu.soniox.com/transcribe-websocket",
            SonioxRegion::Jp => "https://stt-rt.jp.soniox.com/transcribe-websocket",
            SonioxRegion::In => "https://stt-rt.in.soniox.com/transcribe-websocket",
        }
    }
}

/// The region with `id`; an unknown id means [`SonioxRegion::Us`].
pub fn soniox_region(id: &str) -> SonioxRegion {
    SonioxRegion::ALL.into_iter().find(|region| region.id() == id).unwrap_or_default()
}

/// PCM16 mono rate of both modes: the only rate the docs show, and the VAD's own rate.
pub(crate) const SONIOX_SAMPLE_RATE: u32 = 16_000;

/// An empty TEXT frame ends the stream (an empty binary frame is just an empty audio chunk: never sent).
pub(crate) const END_OF_AUDIO: &str = "";

/// The codes in `language` (comma-separated): trimmed, lowercased, unique; blank = none (auto-detect).
pub(crate) fn soniox_language_hints(language: &str) -> Vec<String> {
    let mut hints = Vec::new();
    for hint in language.split(',').map(|code| code.trim().to_lowercase()) {
        if !hint.is_empty() && !hints.contains(&hint) {
            hints.push(hint);
        }
    }
    hints
}

/// The WebSocket's first (text) frame: raw PCM16 mono at 16 kHz. `strict` (`language_hints_strict`) only with hints,
/// and only for a model whose catalogue entry supports it (else the server answers 400). The key goes in the
/// handshake's header, never in `api_key`. `terms` (custom words) go in as `context.terms`, only when there are any.
pub(crate) fn soniox_start_message(model: &str, hints: &[String], strict: bool, terms: &[String]) -> String {
    let mut message = Map::new();
    message.insert("model".into(), json!(model));
    message.insert("audio_format".into(), json!("pcm_s16le"));
    message.insert("sample_rate".into(), json!(SONIOX_SAMPLE_RATE));
    message.insert("num_channels".into(), json!(1));
    put_hints(&mut message, hints, strict);
    put_context(&mut message, terms);
    Value::Object(message).to_string()
}

/// `POST /v1/transcriptions` body for the uploaded file `file_id`; hints and terms as in [`soniox_start_message`].
pub(crate) fn soniox_create_transcription_body(
    model: &str,
    file_id: &str,
    hints: &[String],
    strict: bool,
    terms: &[String],
) -> String {
    let mut body = Map::new();
    body.insert("model".into(), json!(model));
    body.insert("file_id".into(), json!(file_id));
    put_hints(&mut body, hints, strict);
    put_context(&mut body, terms);
    Value::Object(body).to_string()
}

fn put_hints(object: &mut Map<String, Value>, hints: &[String], strict: bool) {
    if hints.is_empty() {
        return;
    }
    object.insert("language_hints".into(), json!(hints));
    if strict {
        object.insert("language_hints_strict".into(), json!(true));
    }
}

/// `context: {terms: [...]}` (https://soniox.com/docs/stt/concepts/context, checked 2026-11-15).
fn put_context(object: &mut Map<String, Value>, terms: &[String]) {
    if !terms.is_empty() {
        object.insert("context".into(), json!({ "terms": terms }));
    }
}

/// A transcript piece; tokens carry their own spacing. `is_final`: never repeated or changed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SonioxToken {
    pub text: String,
    pub is_final: bool,
}

/// The server messages the streaming client acts on (socket failures and closes are handled where they happen).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SonioxEvent {
    /// This response's tokens (finals once, then the whole non-final tail); `finished`: the transcript is complete.
    Tokens { tokens: Vec<SonioxToken>, finished: bool },
    /// An error frame (the server closes the socket right after it).
    Error { code: i32, message: Option<String> },
}

fn parse_object(text: &str) -> Option<Map<String, Value>> {
    match serde_json::from_str(text) {
        Ok(Value::Object(object)) => Some(object),
        _ => None,
    }
}

fn string<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

fn boolean(object: &Map<String, Value>, key: &str) -> Option<bool> {
    object.get(key).and_then(Value::as_bool)
}

/// Marker tokens (`<fin>` finalize, `<end>` endpoint): signals, not text.
const MARKER_TOKENS: [&str; 2] = ["<fin>", "<end>"];

/// None for malformed messages and any without an `error_code` or a `tokens` array.
pub(crate) fn parse_soniox_event(text: &str) -> Option<SonioxEvent> {
    let message = parse_object(text)?;
    if let Some(code) = message.get("error_code").and_then(Value::as_i64).and_then(|code| i32::try_from(code).ok()) {
        return Some(SonioxEvent::Error { code, message: string(&message, "error_message").map(str::to_string) });
    }
    let tokens = message.get("tokens")?.as_array()?;
    Some(SonioxEvent::Tokens {
        tokens: tokens
            .iter()
            .filter_map(|element| {
                let token = element.as_object()?;
                let text = string(token, "text").filter(|text| !MARKER_TOKENS.contains(text))?;
                Some(SonioxToken { text: text.to_string(), is_final: boolean(token, "is_final") == Some(true) })
            })
            .collect(),
        finished: boolean(&message, "finished") == Some(true),
    })
}

/// The streamed transcript: final tokens appended once, plus the latest response's non-final tail.
#[derive(Debug, Default)]
pub(crate) struct SonioxTranscript {
    finals: String,
    tail: String,
}

impl SonioxTranscript {
    /// Everything heard so far.
    pub fn live_text(&self) -> String {
        format!("{}{}", self.finals, self.tail).trim().to_string()
    }

    /// The final tokens only: the transcript once the stream finished.
    pub fn final_text(&self) -> String {
        self.finals.trim().to_string()
    }

    pub fn on_tokens(&mut self, tokens: &[SonioxToken]) {
        let mut non_final = String::new();
        for token in tokens {
            if token.is_final { self.finals.push_str(&token.text) } else { non_final.push_str(&token.text) }
        }
        self.tail = non_final;
    }
}

/// The `id` of a created file or transcription; None if missing.
pub(crate) fn parse_soniox_id(body: &str) -> Option<String> {
    string(&parse_object(body)?, "id").map(str::to_string)
}

/// A transcription job's state: `queued`, `processing`, `completed` or a failure ([`is_soniox_job_failed`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SonioxJob {
    pub status: String,
    pub error_message: Option<String>,
}

/// `GET /v1/transcriptions/{id}`; None without a string `status`.
pub(crate) fn parse_soniox_job(body: &str) -> Option<SonioxJob> {
    let job = parse_object(body)?;
    let status = string(&job, "status")?.to_string();
    let error_message = string(&job, "error_message").or_else(|| string(&job, "error_type")).map(str::to_string);
    Some(SonioxJob { status, error_message })
}

/// `GET /v1/transcriptions/{id}/transcript`'s `text`; None if missing.
pub(crate) fn parse_soniox_transcript_text(body: &str) -> Option<String> {
    string(&parse_object(body)?, "text").map(str::to_string)
}

/// The API schema says `error`, the errors page `failed`: both end the job.
pub(crate) fn is_soniox_job_failed(status: &str) -> bool {
    status == "error" || status == "failed"
}

/// For an error frame's or a REST error's `code` and `message`.
pub(crate) fn soniox_error_message(code: i32, message: Option<&str>) -> String {
    match code {
        401 => "Soniox rejected the API key. Check it in Iris settings.".into(),
        402 => "Soniox account is out of credits or over budget".into(),
        403 => "Soniox API key doesn't have permission for this".into(),
        429 => format!("Soniox limit reached{}", message.map(|m| format!(": {m}")).unwrap_or_default()),
        500.. => format!("Soniox server error ({code})"),
        _ => match message {
            Some(message) => format!("Soniox error: {message}"),
            None => format!("Soniox request failed ({code})"),
        },
    }
}

/// For a non-2xx response (a REST call or a rejected handshake). `body` may be `{status_code, error_type, message,
/// validation_errors: [{error_type, location, message}]}`.
pub(crate) fn soniox_http_error_message(code: u16, body: &str) -> String {
    let error = parse_object(body);
    let message = error.as_ref().and_then(|error| {
        string(error, "message").filter(|m| !m.trim().is_empty()).or_else(|| {
            let first = error.get("validation_errors")?.as_array()?.first()?.as_object()?;
            string(first, "message")
        })
    });
    soniox_error_message(code as i32, message)
}

/// The stream got no audio before it ended (e.g. stopped at once): blank, so the controller shows "No speech
/// detected". The one error told apart by its message (no `error_type` of its own).
pub(crate) fn is_no_audio_error(code: i32, message: Option<&str>) -> bool {
    code == 400 && message.is_some_and(|m| m.starts_with("No audio received"))
}

/// For a server close before the stream finished.
pub(crate) fn soniox_closed_message(code: u16, reason: &str) -> String {
    if reason.trim().is_empty() {
        format!("Soniox closed the connection (code {code})")
    } else {
        format!("Soniox closed the connection: {reason}")
    }
}

/// The `Authorization` header for `key`, or "Invalid Soniox API key" for one OkHttp can't send (control or non-ASCII
/// characters; `http` lets non-ASCII through).
pub(crate) fn soniox_authorization(key: &str) -> Result<HeaderValue, SttError> {
    let invalid = || SttError("Invalid Soniox API key".into());
    if !crate::net::is_header_value(key) {
        return Err(invalid());
    }
    HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| invalid())
}

/// For a failure without an HTTP response.
pub(crate) fn soniox_network_error_message(error: &NetError) -> String {
    format!("Network error: {}", error.message)
}

#[cfg(test)]
mod tests;
