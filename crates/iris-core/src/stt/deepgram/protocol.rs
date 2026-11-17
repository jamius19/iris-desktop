/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Request building and message parsing for Deepgram's listen API, free of I/O for unit tests (Android
//! `DeepgramProtocol.kt`). Checked 2026-11-13:
//! <https://developers.deepgram.com/reference/speech-to-text/listen-streaming>,
//! <https://developers.deepgram.com/reference/speech-to-text/listen-pre-recorded>,
//! <https://developers.deepgram.com/docs/close-stream>, <https://developers.deepgram.com/docs/errors>.
//! Keyterms checked 2026-11-15: <https://developers.deepgram.com/docs/keyterm>, `/docs/redact-usage`.

use serde_json::{Map, Value};

use crate::net::NetError;
use crate::stt::joined;

/// Streaming (`https` is taken as `wss`) and pre-recorded upload (`POST`) share it. A keyless `HEAD` gets a quick 405
/// with no body: the warm-up target.
pub const LISTEN_URL: &str = "https://api.deepgram.com/v1/listen";

/// The model menu's list; any other id can be typed.
pub const DEEPGRAM_MODELS: [&str; 3] = ["nova-3", "nova-3-medical", "nova-2"];

/// PCM16 mono rate of both modes: half the upload of 24 kHz, and the VAD's own rate.
pub const DEEPGRAM_SAMPLE_RATE: u32 = 16_000;

/// Flushes the stream: the server sends the remaining Results, then Metadata, then closes.
pub const CLOSE_STREAM: &str = r#"{"type":"CloseStream"}"#;

/// Keyterm prompting (`keyterm`) is Nova-3 only (and Flux, which we don't use); nova-2 is not biased.
pub fn takes_keyterms(model: &str) -> bool {
    model.trim().starts_with("nova-3")
}

/// Our budget under Deepgram's 500 tokens across all keyterms (over it the request fails); the count is estimated.
const KEYTERM_TOKEN_BUDGET: usize = 450;

/// The leading `words` whose estimated tokens fit [`KEYTERM_TOKEN_BUDGET`]; the rest are dropped. Deepgram doesn't
/// say how it counts, so a term is estimated at one token per 2 characters (English averages ~4; names and brands
/// split into more).
pub fn deepgram_keyterms(words: &[String]) -> &[String] {
    let mut tokens = 0;
    let fit = words
        .iter()
        .take_while(|word| {
            tokens += word.chars().count().div_ceil(2);
            tokens <= KEYTERM_TOKEN_BUDGET
        })
        .count();
    &words[..fit]
}

/// How many of `words` [`listen_url`] leaves out over the token budget for `model` (0 if it sends none at all).
pub fn dropped_keyterms(model: &str, words: &[String]) -> usize {
    if takes_keyterms(model) { words.len() - deepgram_keyterms(words).len() } else { 0 }
}

/// `base_url` with the query for `model` and `language` (blank = `multi`: Nova-3 code-switching auto-detect). A
/// `streaming` session also states the raw audio format and asks for interim results; `multi` gets the 100 ms
/// endpointing the docs recommend for code-switching. An upload is a WAV file, whose header states the format.
/// `keyterms` (custom words) go in as one `keyterm` each, Nova-3 only, within the token budget
/// ([`deepgram_keyterms`]), with `redact_usage` so Deepgram's usage records show `[REDACTED]` instead of them
/// (transcription is unchanged). Values are percent-encoded (spaces as `%20`).
pub fn listen_url(base_url: &str, model: &str, language: &str, streaming: bool, keyterms: &[String]) -> String {
    let multi = language.trim().is_empty();
    let mut query = Vec::new();
    let mut add = |name: &str, value: &str| query.push(format!("{name}={}", encode(value)));
    add("model", model);
    add("language", if multi { "multi" } else { language });
    add("smart_format", "true");
    add("punctuate", "true");
    let sent = if takes_keyterms(model) { deepgram_keyterms(keyterms) } else { &[] };
    sent.iter().for_each(|term| add("keyterm", term));
    if !sent.is_empty() {
        add("redact_usage", "true");
    }
    if streaming {
        add("encoding", "linear16");
        add("sample_rate", &DEEPGRAM_SAMPLE_RATE.to_string());
        add("channels", "1");
        add("interim_results", "true");
        if multi {
            add("endpointing", "100");
        }
    }
    format!("{base_url}?{}", query.join("&"))
}

/// A query value with everything but `A-Z a-z 0-9 * - . _` percent-encoded, a space as `%20` (not `+`).
fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect::<String>().replace('+', "%20")
}

/// The server messages (and socket outcomes) the streaming client acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeepgramEvent {
    /// `is_final`: a settled segment; else an interim guess the next Result replaces.
    Result { is_final: bool, transcript: String },
    /// Sent last, after CloseStream.
    Metadata,
    /// Non-fatal per the docs (e.g. an unparseable client message): fatal failures close the socket.
    Error(String),
    /// Ends the session; the message is user-readable. Not parsed: from the socket's failure or close.
    Failed(String),
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

fn first_object<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Map<String, Value>> {
    object.get(key)?.as_array()?.first()?.as_object()
}

/// The first alternative's transcript of a `{channel: {alternatives: [...]}}`-shaped object.
fn transcript(object: &Map<String, Value>) -> Option<&str> {
    string(first_object(object.get("channel")?.as_object()?, "alternatives")?, "transcript")
}

/// None for messages the client ignores (SpeechStarted, UtteranceEnd, ...) and malformed ones.
pub fn parse_deepgram_event(text: &str) -> Option<DeepgramEvent> {
    let message = parse_object(text)?;
    match string(&message, "type")? {
        "Results" => transcript(&message).map(|transcript| DeepgramEvent::Result {
            is_final: message.get("is_final").and_then(Value::as_bool) == Some(true),
            transcript: transcript.to_string(),
        }),
        "Metadata" => Some(DeepgramEvent::Metadata),
        "Error" => Some(DeepgramEvent::Error(
            string(&message, "description").or_else(|| string(&message, "message")).unwrap_or("unknown error").into(),
        )),
        _ => None,
    }
}

/// `results.channels[0].alternatives[0].transcript` of a pre-recorded response; None if the body isn't that shape.
pub fn parse_listen_response(body: &str) -> Option<String> {
    let results = parse_object(body)?.remove("results")?;
    let alternative = first_object(first_object(results.as_object()?, "channels")?, "alternatives")?;
    string(alternative, "transcript").map(str::to_string)
}

/// The streamed transcript: settled segments (`is_final`) joined, plus the latest interim for the live text.
#[derive(Default)]
pub struct DeepgramTranscript {
    final_text: String,
    interim: String,
}

impl DeepgramTranscript {
    /// The settled segments only: the final transcript.
    pub fn final_text(&self) -> &str {
        &self.final_text
    }

    /// Everything heard so far.
    pub fn live_text(&self) -> String {
        joined(&self.final_text, &self.interim)
    }

    pub fn on_result(&mut self, is_final: bool, transcript: &str) {
        let text = transcript.trim();
        if is_final {
            if !text.is_empty() {
                self.final_text = joined(&self.final_text, text);
            }
            self.interim.clear();
        } else {
            self.interim = text.to_string();
        }
    }
}

/// For a non-2xx response (a rejected handshake or upload). `body` may carry `{err_code, err_msg}` or, from newer
/// endpoints, `{category, message}`; `dg_error` is the `dg-error` header with the same message.
pub fn deepgram_http_error_message(code: u16, body: &str, dg_error: Option<&str>) -> String {
    match code {
        401 => "Deepgram rejected the API key. Check it in Iris settings.".into(),
        402 => "Deepgram account is out of credits".into(),
        429 => "Deepgram rate limit reached".into(),
        500.. => format!("Deepgram server error (HTTP {code})"),
        _ => {
            let parsed = parse_object(body)
                .and_then(|object| string(&object, "err_msg").or_else(|| string(&object, "message")).map(String::from));
            match parsed.or_else(|| dg_error.filter(|e| !e.trim().is_empty()).map(String::from)) {
                Some(message) => format!("Deepgram error: {message}"),
                None => format!("Deepgram request failed (HTTP {code})"),
            }
        }
    }
}

/// For a server close that isn't the end of a finished stream (e.g. `1011` with `NET-0001`).
pub fn deepgram_closed_message(code: u16, reason: &str) -> String {
    if reason.trim().is_empty() {
        format!("Deepgram closed the connection (code {code})")
    } else {
        format!("Deepgram closed the connection: {reason}")
    }
}

/// For a failure without an HTTP response.
pub fn deepgram_network_error_message(error: &NetError) -> String {
    format!("Network error: {}", error.message)
}

#[cfg(test)]
mod tests;
