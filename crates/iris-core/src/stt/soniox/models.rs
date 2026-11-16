/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Soniox's model catalogue (Android `SonioxModels.kt`): the settings' model menu, the mode (stream or upload) and
//! the strict-hints check. https://docs.soniox.com/api-reference/stt/get_models

use std::time::Duration;

use serde_json::Value;

use super::protocol::{soniox_authorization, soniox_http_error_message};
use crate::cleanup::protocol::models_network_error_message;
use crate::net;
use crate::stt::SttError;

/// A catalogue entry. `transcription_mode`: `real_time` (WebSocket) or `async` (REST job).
/// `supports_language_hints_strict`: `language_hints_strict` may be sent (else the server answers 400).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SonioxModel {
    pub id: String,
    pub transcription_mode: Option<String>,
    pub supports_language_hints_strict: bool,
}

/// `GET` (with a key) lists the models; relative to a region's REST URL.
pub(crate) const SONIOX_MODELS_PATH: &str = "v1/models";

/// OkHttp's default read timeout, the Android call's limit.
const LIST_TIMEOUT: Duration = Duration::from_secs(10);

/// The models `api_key` can use (`GET <base_url>/v1/models`), sorted by id. Without a key, or with one that can't be
/// sent as a header, nothing is requested. The error is a short message for the settings screen.
pub async fn list_soniox_models(
    client: &reqwest::Client,
    api_key: Option<String>,
    base_url: &str,
) -> Result<Vec<SonioxModel>, SttError> {
    list_with_timeout(client, api_key, base_url, LIST_TIMEOUT).await
}

pub(crate) async fn list_with_timeout(
    client: &reqwest::Client,
    api_key: Option<String>,
    base_url: &str,
    timeout: Duration,
) -> Result<Vec<SonioxModel>, SttError> {
    let key = api_key
        .filter(|key| !key.trim().is_empty())
        .ok_or_else(|| SttError("Set your Soniox API key to load models".into()))?;
    let authorization = soniox_authorization(&key)?;
    let url = format!("{}/{SONIOX_MODELS_PATH}", base_url.trim_end_matches('/'));
    let response = client
        .get(url)
        .header(reqwest::header::AUTHORIZATION, authorization)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| SttError(models_network_error_message(&e.into())))?;
    let (code, body) = net::read_response(response).await.map_err(|e| SttError(models_network_error_message(&e)))?;
    if !(200..300).contains(&code) {
        return Err(SttError(soniox_http_error_message(code, &body)));
    }
    parse_soniox_models(&body).ok_or_else(|| SttError("Unexpected response from Soniox".into()))
}

/// `{models: [{id, transcription_mode, supports_language_hints_strict, ...}]}`, sorted by id; entries without a string
/// id are skipped, a missing or non-boolean flag is false. None if the body has no `models` array.
pub(crate) fn parse_soniox_models(body: &str) -> Option<Vec<SonioxModel>> {
    let body: Value = serde_json::from_str(body).ok()?;
    let mut models: Vec<SonioxModel> = body
        .as_object()?
        .get("models")?
        .as_array()?
        .iter()
        .filter_map(|element| {
            let model = element.as_object()?;
            Some(SonioxModel {
                id: model.get("id")?.as_str()?.to_string(),
                transcription_mode: model.get("transcription_mode").and_then(Value::as_str).map(str::to_string),
                supports_language_hints_strict: model.get("supports_language_hints_strict").and_then(Value::as_bool)
                    == Some(true),
            })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Some(models)
}

/// `model` streams over the WebSocket: its catalogue entry's `transcription_mode` is `real_time`; without an entry
/// (catalogue not loaded, or a typed id it doesn't list), its id starts with `stt-rt-`.
pub fn soniox_streams(model: &str, catalog: Option<&[SonioxModel]>) -> bool {
    match catalog.and_then(|models| models.iter().find(|entry| entry.id == model)) {
        Some(SonioxModel { transcription_mode: Some(mode), .. }) => mode == "real_time",
        _ => model.starts_with("stt-rt-"),
    }
}

#[cfg(test)]
mod tests;
