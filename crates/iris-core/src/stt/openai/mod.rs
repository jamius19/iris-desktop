/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! OpenAI speech-to-text: Realtime WebSocket, file upload and the model list.

mod file;
mod models;
pub mod protocol;
mod realtime;

pub use file::OpenAiFileTranscription;
pub use models::list_openai_transcription_models;
pub use realtime::OpenAiRealtimeSpeechToText;

use crate::net::{self, KeySource};
use crate::stt::SttError;

/// The stored key, or the error to show without one.
async fn api_key(source: &KeySource) -> Result<String, SttError> {
    match net::api_key(source).await {
        Ok(Some(key)) => Ok(key),
        Ok(None) => Err(SttError("Add your OpenAI API key in Iris settings".into())),
        Err(message) => Err(SttError(message.into())),
    }
}

/// The `Authorization` value for `key`, or None when it can't be a header value: OkHttp's rule (tab or printable
/// ASCII only), so a key with control or non-ASCII characters fails without a request. The key is never shown.
fn bearer(key: &str) -> Option<String> {
    crate::net::is_header_value(key).then(|| format!("Bearer {key}"))
}

const INVALID_API_KEY: &str = "Invalid OpenAI API key";
