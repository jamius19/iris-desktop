/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Cleanup through OpenRouter's OpenAI-compatible Chat Completions API: a structured output (Android
//! `OpenRouterCleanup.kt`, `OpenRouterProtocol.kt`). The request body and completion parsing are the Gateway's
//! ([`super::protocol`]).
//! <https://openrouter.ai/docs/api_reference/overview>, `/guides/features/structured-outputs`,
//! `/guides/best-practices/reasoning-tokens`, `/api_reference/errors-and-debugging`,
//! `/guides/routing/provider-selection`

use std::time::Duration;

use serde_json::{Map, Value};

use super::protocol::{
    api_key, chat_request, error_message, execute, models_network_error_message, network_error_message,
    parse_cleaned_text, parse_completion, parse_object, post_chat, string, strings, warm_up,
};
use super::{CleanupError, CleanupProvider, CleanupRequest, GatewayModel};
use crate::net::KeySource;
use crate::settings::REASONING_EFFORTS;
use crate::stt::BoxFuture;

pub const OPENROUTER_CHAT_COMPLETIONS_URL: &str = "https://openrouter.ai/api/v1/chat/completions";

/// Public catalogue: listed and warmed up without a key.
pub const OPENROUTER_MODELS_URL: &str = "https://openrouter.ai/api/v1/models";

/// The list request's query: only models that take a json_schema `response_format`.
pub const STRUCTURED_OUTPUTS_QUERY: &str = "supported_parameters=structured_outputs";

/// OpenRouter's top-level key for the user's provider (routing) options.
pub const OPENROUTER_OPTIONS_KEY: &str = "provider";

const BATCH_SUFFIX: &str = ":batch";

const TAG: &str = "OpenRouterCleanup";

/// The limit of every call (the max of the cleanup timeout setting). The response arrives only once the model is
/// done, so a cleanup call has no read timeout: this is its only limit.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

const UNEXPECTED_RESPONSE: &str = "Unexpected response from OpenRouter";

pub struct OpenRouterCleanup {
    client: reqwest::Client,
    api_key: KeySource,
    url: String,
    models_url: String,
    call_timeout: Duration,
}

impl OpenRouterCleanup {
    pub fn new(client: reqwest::Client, api_key: KeySource) -> Self {
        Self::with_urls(client, api_key, OPENROUTER_CHAT_COMPLETIONS_URL, OPENROUTER_MODELS_URL, CALL_TIMEOUT)
    }

    pub(crate) fn with_urls(
        client: reqwest::Client,
        api_key: KeySource,
        url: &str,
        models_url: &str,
        call_timeout: Duration,
    ) -> Self {
        Self { client, api_key, url: url.into(), models_url: models_url.into(), call_timeout }
    }
}

impl CleanupProvider for OpenRouterCleanup {
    fn cleanup(&self, request: CleanupRequest) -> BoxFuture<'_, Result<String, CleanupError>> {
        Box::pin(async move {
            let key = api_key(&self.api_key, "Add your OpenRouter API key in Iris settings", "OpenRouter").await?;
            let body = chat_request(
                &request.text,
                &request.system_prompt,
                &request.model,
                request.reasoning_effort.as_deref(),
                OPENROUTER_OPTIONS_KEY,
                request.provider_options.as_ref(),
                true,
            );
            let call = post_chat(&self.client, &self.url, &key, body, self.call_timeout);
            let (code, body) = execute(call, network_error_message).await?;
            if !(200..=299).contains(&code) {
                return Err(CleanupError(open_router_http_error_message(code, &body)));
            }
            if let Some(error) = parse_open_router_error(&body) {
                return Err(CleanupError(format!("OpenRouter error: {error}")));
            }
            let completion = parse_completion(&body).ok_or_else(unexpected_response)?;
            if completion.finish_reason.as_deref() == Some("length") {
                return Err(CleanupError("Cleanup output was cut off".into()));
            }
            // Before content_filter: OpenRouter reports a refusal with finish_reason content_filter.
            if let Some(refusal) = completion.refused() {
                return Err(CleanupError(format!("Model refused: {refusal}")));
            }
            if completion.finish_reason.as_deref() == Some("content_filter") {
                return Err(CleanupError("Cleanup blocked by content filter".into()));
            }
            completion.content.as_deref().and_then(parse_cleaned_text).ok_or_else(unexpected_response)
        })
    }

    /// `HEAD` of the public model catalogue (no key): only opens the connection that
    /// [`cleanup`](CleanupProvider::cleanup) then reuses.
    fn warm_up(&self) {
        warm_up(&self.client, &self.models_url, TAG);
    }

    /// The catalogue is public, so no key is sent. Only models with structured output are requested.
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<GatewayModel>, CleanupError>> {
        Box::pin(async move {
            let url = format!("{}?{STRUCTURED_OUTPUTS_QUERY}", self.models_url);
            let call = self.client.get(url).timeout(self.call_timeout);
            let (code, body) = execute(call, models_network_error_message).await?;
            if !(200..=299).contains(&code) {
                return Err(CleanupError(open_router_http_error_message(code, &body)));
            }
            parse_open_router_models(&body).ok_or_else(unexpected_response)
        })
    }
}

fn unexpected_response() -> CleanupError {
    CleanupError(UNEXPECTED_RESPONSE.into())
}

/// Models with a non-blank id from a `GET /api/v1/models` body (`{data:[{id, reasoning?:{supported_efforts:[..]}}]}`),
/// sorted by id, without the `:batch` variants (batch API only). Efforts are put in [`REASONING_EFFORTS`] order
/// (OpenRouter lists them high to low; unknown values last); None if missing or empty. None if not that shape.
pub fn parse_open_router_models(body: &str) -> Option<Vec<GatewayModel>> {
    let root = parse_object(body)?;
    let mut models: Vec<GatewayModel> = root
        .get("data")?
        .as_array()?
        .iter()
        .filter_map(Value::as_object)
        .filter_map(|model| {
            let id = string(model, "id").filter(|id| !id.trim().is_empty() && !id.ends_with(BATCH_SUFFIX))?;
            Some(GatewayModel { id, r#type: None, tags: Vec::new(), reasoning_efforts: reasoning_efforts(model) })
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Some(models)
}

/// `reasoning.supported_efforts`, low to high; None if missing, not that shape or empty.
fn reasoning_efforts(model: &Map<String, Value>) -> Option<Vec<String>> {
    let mut efforts = strings(model.get("reasoning")?.get("supported_efforts")?.as_array()?);
    efforts.sort_by_key(|effort| REASONING_EFFORTS.iter().position(|e| e == effort).unwrap_or(usize::MAX));
    Some(efforts).filter(|efforts| !efforts.is_empty())
}

/// The error a 200 response can carry instead of a completion: a top-level `error` (no `choices`), or
/// `choices[0].error` / `choices[0].finish_reason == "error"`. Its `message` (else "generation failed"); None if none.
pub fn parse_open_router_error(body: &str) -> Option<String> {
    let root = parse_object(body)?;
    let choice = root.get("choices").and_then(Value::as_array).and_then(|c| c.first()).and_then(Value::as_object);
    let error = match choice {
        None => Some(present(&root, "error")?),
        Some(choice) => match present(choice, "error") {
            Some(error) => Some(error),
            None if string(choice, "finish_reason").as_deref() == Some("error") => None,
            None => return None,
        },
    };
    let message = error.and_then(Value::as_object).and_then(|error| string(error, "message"));
    Some(message.filter(|m| !m.trim().is_empty()).unwrap_or_else(|| "generation failed".into()))
}

/// `key`'s value unless it is missing or JSON `null`.
fn present<'a>(object: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    object.get(key).filter(|value| !value.is_null())
}

/// For a non-2xx response; `body` may carry `{ "error": { "code", "message" } }`.
pub fn open_router_http_error_message(code: u16, body: &str) -> String {
    match code {
        401 => "OpenRouter rejected the API key. Check it in Iris settings.".into(),
        402 => "OpenRouter account is out of credits".into(),
        429 => "OpenRouter rate limit reached".into(),
        502 => "OpenRouter: the model's provider failed (HTTP 502)".into(),
        503 => "No OpenRouter provider available for this model (check provider options)".into(),
        500.. => format!("OpenRouter server error (HTTP {code})"),
        _ => error_message(body)
            .map(|message| format!("OpenRouter error: {message}"))
            .unwrap_or_else(|| format!("OpenRouter request failed (HTTP {code})")),
    }
}

#[cfg(test)]
mod tests;
