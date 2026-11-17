/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! OpenAI's model list for the settings' model menu (Android `OpenAiModels.kt`).
//! <https://developers.openai.com/api/reference/resources/models/methods/list>

use std::time::Duration;

use reqwest::header::AUTHORIZATION;
use serde_json::Value;

use super::protocol::openai_http_error_message;
use super::{INVALID_API_KEY, bearer};
use crate::net::{self, NetError};
use crate::stt::SttError;

/// `GET` lists the models the key can use.
pub const MODELS_URL: &str = "https://api.openai.com/v1/models";

/// OkHttp's default connect, write and read timeouts (10 s each) as one bound for the whole call.
const MODELS_TIMEOUT: Duration = Duration::from_secs(30);

/// The transcription models `api_key` can use (`GET /v1/models`), sorted. Without a key, or with one that can't be
/// sent as a header, nothing is requested. Fails with a short message for the settings screen.
pub async fn list_openai_transcription_models(
    client: &reqwest::Client,
    api_key: Option<String>,
) -> Result<Vec<String>, SttError> {
    list_models_at(client, api_key, MODELS_URL, MODELS_TIMEOUT).await
}

pub(crate) async fn list_models_at(
    client: &reqwest::Client,
    api_key: Option<String>,
    url: &str,
    timeout: Duration,
) -> Result<Vec<String>, SttError> {
    let error = |message: &str| SttError(message.into());
    let key =
        api_key.filter(|k| !k.trim().is_empty()).ok_or_else(|| error("Set your OpenAI API key to load models"))?;
    let authorization = bearer(&key).ok_or_else(|| error(INVALID_API_KEY))?;
    let request = client.get(url).header(AUTHORIZATION, authorization).timeout(timeout);
    let (code, body) = match request.send().await {
        Ok(response) => net::read_response(response).await,
        Err(e) => Err(NetError::from(e)),
    }
    .map_err(|e| error(if e.timeout { "Loading models timed out" } else { "Couldn't load models (network error)" }))?;
    if !(200..300).contains(&code) {
        return Err(SttError(openai_http_error_message(code, &body)));
    }
    parse_transcription_model_ids(&body).ok_or_else(|| error("Unexpected response from OpenAI"))
}

/// The ids in `{ "data": [{ "id": ... }] }` that contain "transcribe" or "whisper" (any case), sorted; None if the
/// body has no `data` array.
pub(crate) fn parse_transcription_model_ids(body: &str) -> Option<Vec<String>> {
    let body = serde_json::from_str::<Value>(body).ok()?;
    let data = body.as_object()?.get("data")?.as_array()?;
    let mut ids: Vec<String> = data
        .iter()
        .filter_map(|model| model.as_object()?.get("id")?.as_str())
        .filter(|id| {
            let id = id.to_lowercase();
            id.contains("transcribe") || id.contains("whisper")
        })
        .map(String::from)
        .collect();
    ids.sort();
    Some(ids)
}

#[cfg(test)]
mod tests;
