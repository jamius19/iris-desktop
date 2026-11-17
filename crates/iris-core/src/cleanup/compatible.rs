/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Custom (OpenAI-compatible) cleanup providers: the provider (Android `OpenAiCompatibleCleanup.kt`,
//! `OpenAiCompatibleProtocol.kt`) and the provider form's validation (Android's `SettingsText.kt` and
//! `PostProcessingPage.kt`).

use std::sync::LazyLock;
use std::time::Duration;

use regex::Regex;
use url::Url;

use super::protocol::{
    api_key, chat_request, execute, http_error_message, models_network_error_message, network_error_message,
    parse_cleaned_text, parse_completion, parse_models_response, post_chat, warm_up,
};
use super::{CleanupError, CleanupProvider, CleanupRequest, GatewayModel};
use crate::net::KeySource;
use crate::stt::BoxFuture;

const TAG: &str = "OpenAiCompatibleCleanup";

/// The limit of every call (the max of the cleanup timeout setting). The response arrives only once the model is
/// done, so a cleanup call has no read timeout: this is its only limit.
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

/// Cleanup through a user-added OpenAI-compatible Chat Completions server (`name` in messages) at `base_url`. With
/// `send_json_schema` the request asks for a strict `{ "text": string }` structured output; without it the reply's
/// content is the cleaned text (minus a leading `<think>` block). No provider options.
pub struct OpenAiCompatibleCleanup {
    client: reqwest::Client,
    name: String,
    base_url: String,
    send_json_schema: bool,
    api_key: KeySource,
    call_timeout: Duration,
    /// Desktop: why `base_url` is refused ([`base_url_error`]: https only); no request goes out while set.
    refused: Option<&'static str>,
}

impl OpenAiCompatibleCleanup {
    /// A base URL that fails [`base_url_error`] is refused: every call fails without a request.
    pub fn new(
        client: reqwest::Client,
        name: &str,
        base_url: &str,
        send_json_schema: bool,
        api_key: KeySource,
    ) -> Self {
        let refused = base_url_error(base_url);
        Self { refused, ..Self::with_timeout(client, name, base_url, send_json_schema, api_key, CALL_TIMEOUT) }
    }

    /// Any base URL (tests: an http server).
    pub(crate) fn with_timeout(
        client: reqwest::Client,
        name: &str,
        base_url: &str,
        send_json_schema: bool,
        api_key: KeySource,
        call_timeout: Duration,
    ) -> Self {
        Self {
            client,
            name: name.into(),
            base_url: base_url.into(),
            send_json_schema,
            api_key,
            call_timeout,
            refused: None,
        }
    }

    fn check_base_url(&self) -> Result<(), CleanupError> {
        match self.refused {
            Some(error) => Err(CleanupError(format!("{}: {error}", self.name))),
            None => Ok(()),
        }
    }

    fn unexpected_response(&self) -> CleanupError {
        CleanupError(format!("Unexpected response from {}", self.name))
    }
}

impl CleanupProvider for OpenAiCompatibleCleanup {
    fn cleanup(&self, request: CleanupRequest) -> BoxFuture<'_, Result<String, CleanupError>> {
        Box::pin(async move {
            self.check_base_url()?;
            let name = &self.name;
            if request.model.trim().is_empty() {
                return Err(CleanupError(format!("Set a cleanup model for {name} in Iris settings")));
            }
            let key = api_key(&self.api_key, &format!("Add your {name} API key in Iris settings"), name).await?;
            let body = chat_request(
                &request.text,
                &request.system_prompt,
                &request.model,
                request.reasoning_effort.as_deref(),
                "",
                None,
                self.send_json_schema,
            );
            let call = post_chat(&self.client, &chat_completions_url(&self.base_url), &key, body, self.call_timeout);
            let (code, response) = execute(call, network_error_message).await?;
            if !(200..=299).contains(&code) {
                return Err(CleanupError(http_error_message(code, &response, name)));
            }
            let completion = parse_completion(&response).ok_or_else(|| self.unexpected_response())?;
            match completion.finish_reason.as_deref() {
                Some("length") => return Err(CleanupError("Cleanup output was cut off".into())),
                Some("content_filter") => return Err(CleanupError("Cleanup blocked by content filter".into())),
                _ => {}
            }
            if let Some(refusal) = completion.refused() {
                return Err(CleanupError(format!("Model refused: {refusal}")));
            }
            let content = completion.content.as_deref();
            let cleaned = if self.send_json_schema {
                content.and_then(parse_cleaned_text)
            } else {
                content.map(without_think_block).filter(|text| !text.is_empty())
            };
            cleaned.ok_or_else(|| self.unexpected_response())
        })
    }

    /// Keyless `HEAD` of the models list: only opens the connection that [`cleanup`](CleanupProvider::cleanup) then
    /// reuses (a 401 is fine). Skipped for a refused base URL.
    fn warm_up(&self) {
        if self.refused.is_none() {
            warm_up(&self.client, &models_url(&self.base_url), TAG);
        }
    }

    /// Every model the key can list (no type filter: some servers use other types, e.g. `chat`).
    fn list_models(&self) -> BoxFuture<'_, Result<Vec<GatewayModel>, CleanupError>> {
        Box::pin(async move {
            self.check_base_url()?;
            let name = &self.name;
            let key = api_key(&self.api_key, &format!("Set your {name} API key to load models"), name).await?;
            let call = self.client.get(models_url(&self.base_url)).bearer_auth(key).timeout(self.call_timeout);
            let (code, body) = execute(call, models_network_error_message).await?;
            if !(200..=299).contains(&code) {
                return Err(CleanupError(http_error_message(code, &body, name)));
            }
            parse_models_response(&body).ok_or_else(|| self.unexpected_response())
        })
    }
}

// URLs of a user-added OpenAI-compatible server. The base URL includes the version path (e.g.
// `https://api.groq.com/openai/v1`); `/v1` is never added.

pub fn chat_completions_url(base_url: &str) -> String {
    format!("{}/chat/completions", base_url.trim_end_matches('/'))
}

pub fn models_url(base_url: &str) -> String {
    format!("{}/models", base_url.trim_end_matches('/'))
}

static LEADING_THINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?is)^\s*<think>.*?</think>").unwrap());

/// `content` without a leading `<think>…</think>` block (reasoning models that inline it), trimmed.
pub fn without_think_block(content: &str) -> String {
    LEADING_THINK.replace(content, "").trim().to_string()
}

/// Why a custom provider's Save can't go ahead, per field; all None = [`Self::ok`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderDraftErrors {
    pub name: Option<&'static str>,
    pub base_url: Option<&'static str>,
    pub api_key: Option<&'static str>,
}

impl ProviderDraftErrors {
    pub fn ok(&self) -> bool {
        self.name.is_none() && self.base_url.is_none() && self.api_key.is_none()
    }
}

/// Checks a custom provider draft: a name, an https base URL, and a key ([key_input], or one [key_stored] and usable:
/// see [`key_usable`]).
pub fn provider_draft_errors(name: &str, base_url: &str, key_input: &str, key_stored: bool) -> ProviderDraftErrors {
    ProviderDraftErrors {
        name: name.trim().is_empty().then_some("Enter a provider name."),
        base_url: base_url_error(base_url),
        api_key: (key_input.trim().is_empty() && !key_stored).then_some("Enter an API key."),
    }
}

/// The stored key may be kept: it is shown (stored and not marked for removal) and the host is unchanged. A new host
/// needs a new key: the stored one must never be sent to another server. [stored_base_url]: None for a new provider.
pub fn key_usable(key_shown: bool, base_url: &str, stored_base_url: Option<&str>) -> bool {
    key_shown && stored_base_url.is_none_or(|stored| same_host(base_url, stored))
}

/// Both URLs parse (http or https) and have the same host (a provider's stored key is kept only for its own host).
pub fn same_host(a: &str, b: &str) -> bool {
    match (http_url(a), http_url(b)) {
        (Some(a), Some(b)) => a.host_str().is_some() && a.host_str() == b.host_str(),
        _ => false,
    }
}

/// Why [base_url] can't be a custom provider's base URL (blank, not https, unparseable), or None.
pub fn base_url_error(base_url: &str) -> Option<&'static str> {
    let url = base_url.trim();
    if url.is_empty() {
        Some("Enter a base URL.")
    } else if !url.get(..8).is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://")) {
        Some("Base URL must start with https://")
    } else if http_url(url).is_none() {
        Some("Not a valid URL.")
    } else {
        None
    }
}

/// [text] as an http(s) URL with a host, like OkHttp's `toHttpUrlOrNull`.
fn http_url(text: &str) -> Option<Url> {
    Url::parse(text.trim()).ok().filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some())
}

#[cfg(test)]
mod provider_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_errors() {
        assert_eq!(Some("Enter a base URL."), base_url_error("  "));
        assert_eq!(Some("Base URL must start with https://"), base_url_error("http://localhost:11434/v1"));
        assert_eq!(Some("Base URL must start with https://"), base_url_error("api.groq.com/openai/v1"));
        assert_eq!(Some("Not a valid URL."), base_url_error("https://"));
        assert_eq!(Some("Not a valid URL."), base_url_error("https://exa mple.com"));
        assert_eq!(None, base_url_error(" https://api.groq.com/openai/v1/ "));
    }

    #[test]
    fn provider_draft_errors_check_every_field() {
        assert!(provider_draft_errors("Groq", "https://api.groq.com/openai/v1", "gsk-1", false).ok());

        let blank = provider_draft_errors(" ", "", "", false);
        assert!(!blank.ok());
        assert_eq!(Some("Enter a provider name."), blank.name);
        assert_eq!(Some("Enter a base URL."), blank.base_url);
        assert_eq!(Some("Enter an API key."), blank.api_key);

        // A stored key needs no input; one marked for removal counts as not stored.
        assert_eq!(None, provider_draft_errors("Groq", "https://a.example", "", true).api_key);
        assert_eq!(Some("Enter an API key."), provider_draft_errors("Groq", "https://a.example", " ", false).api_key);
    }

    #[test]
    fn same_hosts() {
        assert!(same_host("https://api.groq.com/openai/v1", " https://API.groq.com/v2/ "));
        assert!(!same_host("https://api.groq.com/openai/v1", "https://evil.example/openai/v1"));
        assert!(!same_host("https://api.groq.com/v1", "https://groq.com/v1"));
        assert!(!same_host("not a url", "not a url"));
    }

    /// PostProcessingPage's Save rule (no Android unit test).
    #[test]
    fn a_new_host_needs_a_new_key() {
        assert!(key_usable(true, "https://api.groq.com/v1", None));
        assert!(key_usable(true, "https://api.groq.com/v2", Some("https://api.groq.com/openai/v1")));
        assert!(!key_usable(true, "https://evil.example/v1", Some("https://api.groq.com/openai/v1")));
        assert!(!key_usable(false, "https://api.groq.com/v1", Some("https://api.groq.com/v1")));
        assert_eq!(
            Some("Enter an API key."),
            provider_draft_errors(
                "Groq",
                "https://evil.example/v1",
                "",
                key_usable(true, "https://evil.example/v1", Some("https://api.groq.com/v1"))
            )
            .api_key
        );
    }
}
