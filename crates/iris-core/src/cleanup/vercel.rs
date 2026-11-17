/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Cleanup through Vercel AI Gateway's OpenAI-compatible Chat Completions API: a structured output (Android
//! `VercelGatewayCleanup.kt`).

use std::time::Duration;

use super::protocol::{
    AI_GATEWAY, CHAT_COMPLETIONS_URL, GATEWAY_OPTIONS_KEY, MODELS_URL, api_key, chat_request, execute,
    http_error_message, models_network_error_message, network_error_message, parse_cleaned_text, parse_completion,
    parse_models_response, post_chat, warm_up,
};
use super::{CleanupError, CleanupProvider, CleanupRequest, GatewayModel};
use crate::net::KeySource;
use crate::stt::BoxFuture;

const TAG: &str = "VercelGatewayCleanup";

/// The limit of every call (the max of the cleanup timeout setting). The response arrives only once the model is
/// done, so a cleanup call has no read timeout: this is its only limit.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

const UNEXPECTED_RESPONSE: &str = "Unexpected response from AI Gateway";

pub struct VercelGatewayCleanup {
    client: reqwest::Client,
    api_key: KeySource,
    url: String,
    models_url: String,
    call_timeout: Duration,
}

impl VercelGatewayCleanup {
    pub fn new(client: reqwest::Client, api_key: KeySource) -> Self {
        Self::with_urls(client, api_key, CHAT_COMPLETIONS_URL, MODELS_URL, CALL_TIMEOUT)
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

impl CleanupProvider for VercelGatewayCleanup {
    fn cleanup(&self, request: CleanupRequest) -> BoxFuture<'_, Result<String, CleanupError>> {
        Box::pin(async move {
            let key =
                api_key(&self.api_key, "Add your Vercel AI Gateway API key in Iris settings", "Vercel AI Gateway")
                    .await?;
            let body = chat_request(
                &request.text,
                &request.system_prompt,
                &request.model,
                request.reasoning_effort.as_deref(),
                GATEWAY_OPTIONS_KEY,
                request.provider_options.as_ref(),
                true,
            );
            let call = post_chat(&self.client, &self.url, &key, body, self.call_timeout);
            let (code, body) = execute(call, network_error_message).await?;
            if !(200..=299).contains(&code) {
                return Err(CleanupError(http_error_message(code, &body, AI_GATEWAY)));
            }
            let completion = parse_completion(&body).ok_or_else(unexpected_response)?;
            match completion.finish_reason.as_deref() {
                Some("length") => return Err(CleanupError("Cleanup output was cut off".into())),
                Some("content_filter") => return Err(CleanupError("Cleanup blocked by content filter".into())),
                _ => {}
            }
            if let Some(refusal) = completion.refused() {
                return Err(CleanupError(format!("Model refused: {refusal}")));
            }
            completion.content.as_deref().and_then(parse_cleaned_text).ok_or_else(unexpected_response)
        })
    }

    /// `HEAD` of the public model catalogue: answered from Vercel's edge cache with no body and no key needed, so it
    /// only opens the (HTTP/2) connection that [`cleanup`](CleanupProvider::cleanup) then reuses from the pool.
    fn warm_up(&self) {
        warm_up(&self.client, &self.models_url, TAG);
    }

    /// The catalogue is public, so no key is sent (an invalid one would get a 401).
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<GatewayModel>, CleanupError>> {
        Box::pin(async move {
            let call = self.client.get(&self.models_url).timeout(self.call_timeout);
            let (code, body) = execute(call, models_network_error_message).await?;
            if !(200..=299).contains(&code) {
                return Err(CleanupError(http_error_message(code, &body, AI_GATEWAY)));
            }
            parse_models_response(&body).ok_or_else(unexpected_response)
        })
    }
}

fn unexpected_response() -> CleanupError {
    CleanupError(UNEXPECTED_RESPONSE.into())
}

#[cfg(test)]
mod tests;
