/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Custom (OpenAI-compatible) cleanup providers: the provider form's validation (Android's `SettingsText.kt` and
//! `PostProcessingPage.kt`).

use url::Url;

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
