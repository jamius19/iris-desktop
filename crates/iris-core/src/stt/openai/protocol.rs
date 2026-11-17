/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Message building and parsing for OpenAI Realtime transcription-only sessions, free of I/O (Android
//! `RealtimeProtocol.kt`).
//! <https://developers.openai.com/api/docs/guides/realtime-transcription>
//! <https://developers.openai.com/api/reference/resources/realtime/client-events>
//! <https://developers.openai.com/api/reference/resources/realtime/server-events>

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

/// Transcription intent, no `model` query param: a model in the URL would create a conversation session.
pub const REALTIME_URL: &str = "wss://api.openai.com/v1/realtime?intent=transcription";

/// Warm-up target: answers a keyless `HEAD` with a quick 401 and no body.
pub const WARM_UP_URL: &str = "https://api.openai.com/v1/models";

/// The least audio the server accepts in a commit: 100 ms of PCM16 mono 24 kHz.
pub const MIN_COMMIT_BYTES: usize = 24_000 * 2 / 10;

pub const COMMIT_AUDIO: &str = r#"{"type":"input_audio_buffer.commit"}"#;

/// The models a Realtime transcription session accepts (`audio.input.transcription.model`, client events reference,
/// checked 2026-11-12). Only gpt-live-transcribe and gpt-realtime-whisper stream text before the commit; the others
/// transcribe the committed audio.
pub const REALTIME_TRANSCRIPTION_MODELS: [&str; 8] = [
    "whisper-1",
    "gpt-transcribe",
    "gpt-live-transcribe",
    "gpt-4o-mini-transcribe",
    "gpt-4o-mini-transcribe-2025-12-15",
    "gpt-4o-transcribe",
    "gpt-4o-transcribe-diarize",
    "gpt-realtime-whisper",
];

/// `model` streams over a Realtime session; any other id is uploaded after stop.
pub fn is_realtime_transcription_model(model: &str) -> bool {
    REALTIME_TRANSCRIPTION_MODELS.contains(&model.trim())
}

/// `model` reports text while recording; the other Realtime models transcribe the committed buffer at stop.
pub fn has_live_text(model: &str) -> bool {
    matches!(model.trim(), "gpt-live-transcribe" | "gpt-realtime-whisper")
}

/// `model` takes the `languages` array; the others take singular `language` (never both).
pub fn uses_language_list(model: &str) -> bool {
    matches!(model.trim(), "gpt-transcribe" | "gpt-live-transcribe")
}

/// `model` takes custom words as `keywords` (an array; Realtime and file API, checked 2026-11-15).
pub fn uses_keywords(model: &str) -> bool {
    matches!(model.trim(), "gpt-transcribe" | "gpt-live-transcribe")
}

/// `model` takes custom words as a `prompt` ([`glossary_prompt`]): whisper-1 and the gpt-4o(-mini)-transcribe models,
/// not diarize (rejects a prompt) nor gpt-realtime-whisper (no prompt in Realtime sessions). Unknown ids get nothing.
pub fn takes_prompt(model: &str) -> bool {
    let id = model.trim();
    if id == "whisper-1" {
        return true;
    }
    let gpt4o = id.starts_with("gpt-4o-transcribe") || id.starts_with("gpt-4o-mini-transcribe");
    gpt4o && !id.contains("diarize")
}

/// The `prompt` carrying custom words (whisper-1 reads a prompt as a list of terms).
pub fn glossary_prompt(words: &[String]) -> String {
    format!("Glossary: {}", words.join(", "))
}

/// PCM16 mono 24 kHz input, no turn detection (gpt-live-transcribe supports no VAD; we commit on stop). `words` go in
/// as `keywords` or a `prompt`, as the model takes them ([`uses_keywords`], [`takes_prompt`]).
pub fn session_update(model: &str, language: &str, words: &[String]) -> String {
    let mut transcription = Map::new();
    transcription.insert("model".into(), json!(model));
    if !language.trim().is_empty() {
        if uses_language_list(model) {
            transcription.insert("languages".into(), json!([language]));
        } else {
            transcription.insert("language".into(), json!(language));
        }
    }
    if !words.is_empty() {
        if uses_keywords(model) {
            transcription.insert("keywords".into(), json!(words));
        } else if takes_prompt(model) {
            transcription.insert("prompt".into(), json!(glossary_prompt(words)));
        }
    }
    json!({
        "type": "session.update",
        "session": {
            "type": "transcription",
            "audio": {
                "input": {
                    "format": { "type": "audio/pcm", "rate": 24_000 },
                    "transcription": transcription,
                    "turn_detection": null,
                },
            },
        },
    })
    .to_string()
}

pub fn append_audio(chunk: &[u8]) -> String {
    json!({ "type": "input_audio_buffer.append", "audio": STANDARD.encode(chunk) }).to_string()
}

/// The server events (and socket outcomes) the client acts on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RealtimeEvent {
    Delta {
        item_id: String,
        delta: String,
    },
    Completed {
        item_id: String,
        transcript: String,
    },
    Committed {
        item_id: String,
    },
    /// Ends the session; the message is user-readable.
    Failed(String),
}

/// None for events the client ignores (session.created, session.updated, ...) and malformed ones.
pub fn parse_server_event(text: &str) -> Option<RealtimeEvent> {
    let event = serde_json::from_str::<Value>(text).ok()?;
    let event = event.as_object()?;
    let item_id = string(event, "item_id");
    match string(event, "type")?.as_str() {
        "conversation.item.input_audio_transcription.delta" => {
            item_id.map(|item_id| RealtimeEvent::Delta { item_id, delta: string(event, "delta").unwrap_or_default() })
        }
        "conversation.item.input_audio_transcription.completed" => item_id.map(|item_id| RealtimeEvent::Completed {
            item_id,
            transcript: string(event, "transcript").unwrap_or_default(),
        }),
        "input_audio_buffer.committed" => item_id.map(|item_id| RealtimeEvent::Committed { item_id }),
        "conversation.item.input_audio_transcription.failed" => {
            Some(RealtimeEvent::Failed(format!("Transcription failed: {}", error_message(event))))
        }
        "error" => Some(RealtimeEvent::Failed(format!("OpenAI error: {}", error_message(event)))),
        _ => None,
    }
}

/// For a WebSocket failure; `http_code` is set when the handshake got an HTTP error response, `cause` is the
/// failure's message.
pub fn connection_failure_message(http_code: Option<u16>, cause: &str) -> String {
    match http_code {
        None => format!("Network error: {cause}"),
        Some(401) => API_KEY_REJECTED.into(),
        Some(code) => format!("Couldn't connect to OpenAI (HTTP {code})"),
    }
}

/// For a non-2xx response; `body` may carry `{ "error": { "message": ... } }`.
pub fn openai_http_error_message(code: u16, body: &str) -> String {
    match code {
        401 | 403 => API_KEY_REJECTED.into(),
        429 => "OpenAI rate limit or quota reached".into(),
        500.. => format!("OpenAI server error (HTTP {code})"),
        _ => match error_message_of(body) {
            Some(message) => format!("OpenAI error: {message}"),
            None => format!("OpenAI request failed (HTTP {code})"),
        },
    }
}

fn error_message_of(body: &str) -> Option<String> {
    let body = serde_json::from_str::<Value>(body).ok()?;
    string(body.as_object()?.get("error")?.as_object()?, "message")
}

const API_KEY_REJECTED: &str = "OpenAI rejected the API key. Check it in Iris settings.";

pub fn closed_message(reason: &str) -> String {
    if reason.trim().is_empty() {
        "OpenAI closed the connection".into()
    } else {
        format!("OpenAI closed the connection: {reason}")
    }
}

/// A JSON primitive's content (strings, numbers, booleans; null is None), as kotlinx's `contentOrNull`.
fn string(object: &Map<String, Value>, key: &str) -> Option<String> {
    match object.get(key)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn error_message(event: &Map<String, Value>) -> String {
    event
        .get("error")
        .and_then(Value::as_object)
        .and_then(|error| string(error, "message"))
        .unwrap_or_else(|| "unknown error".into())
}

/// Assembles transcript text from per-item events. Deltas are appended per `item_id`; a completed transcript replaces
/// that item's deltas. Items are joined with spaces in first-seen order (completion order across items isn't
/// guaranteed). We commit once, on stop, so normally there is a single item.
#[derive(Debug, Default)]
pub struct TranscriptAssembler {
    items: Vec<(String, Item)>,
    committed_item_id: Option<String>,
}

#[derive(Debug, Default)]
struct Item {
    text: String,
    completed: bool,
}

impl TranscriptAssembler {
    /// Everything heard so far, including items still streaming deltas.
    pub fn live_text(&self) -> String {
        join(self.items.iter().map(|(_, item)| item))
    }

    /// Completed items once the committed item has completed, else None. Only completed items count, so text from an
    /// item that never completes can't end up in the final transcript.
    pub fn final_text(&self) -> Option<String> {
        let committed = self.committed_item_id.as_deref()?;
        let (_, item) = self.items.iter().find(|(id, _)| id == committed)?;
        item.completed.then(|| join(self.items.iter().map(|(_, item)| item).filter(|item| item.completed)))
    }

    pub fn on_delta(&mut self, item_id: &str, delta: &str) {
        let item = self.item(item_id);
        if !item.completed {
            item.text.push_str(delta);
        }
    }

    pub fn on_completed(&mut self, item_id: &str, transcript: &str) {
        let item = self.item(item_id);
        item.text = transcript.to_string();
        item.completed = true;
    }

    pub fn on_committed(&mut self, item_id: &str) {
        self.committed_item_id = Some(item_id.to_string());
    }

    fn item(&mut self, item_id: &str) -> &mut Item {
        let index = match self.items.iter().position(|(id, _)| id == item_id) {
            Some(index) => index,
            None => {
                self.items.push((item_id.to_string(), Item::default()));
                self.items.len() - 1
            }
        };
        &mut self.items[index].1
    }
}

fn join<'a>(items: impl Iterator<Item = &'a Item>) -> String {
    items.map(|item| item.text.trim()).filter(|text| !text.is_empty()).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests;
