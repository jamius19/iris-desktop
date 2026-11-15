/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Custom providers' favicons (Android `Favicons.kt`): Google's favicon service for the base URL's registrable
//! domain, cached as `<cache>/favicons/<domain>.png` and in memory. Definitive failures (non-2xx, undecodable) are
//! remembered so a bad domain isn't refetched in a loop; network errors aren't, so they retry on the next refresh.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use slint::Image;
use tokio::runtime::Runtime;

use crate::ui::{refresh, with_state};

const FAVICON_URL: &str = "https://www.google.com/s2/favicons";

/// The domain whose favicon stands for `base_url`: its registrable domain (api.groq.com -> groq.com); None for a
/// non-URL, an IP, localhost or an internal name.
pub fn favicon_domain(base_url: &str) -> Option<String> {
    let url = url::Url::parse(base_url.trim()).ok()?;
    let url::Host::Domain(host) = url.host()? else {
        return None;
    };
    // Only under a known public suffix: "localhost" and internal names (llm.internal) have no registrable domain.
    let domain = psl::domain(host.as_bytes()).filter(|d| d.suffix().is_known())?;
    String::from_utf8(domain.as_bytes().to_vec()).ok()
}

#[derive(Default)]
pub struct Favicons {
    memory: HashMap<String, Option<Image>>,
    loading: HashSet<String>,
}

impl Favicons {
    pub fn new() -> Self {
        Self::default()
    }

    /// The favicon for `base_url` if loaded; otherwise starts loading it (the state refreshes when it arrives).
    pub fn get(&mut self, base_url: &str, runtime: &Runtime) -> Option<Image> {
        let domain = favicon_domain(base_url)?;
        if let Some(image) = self.memory.get(&domain) {
            return image.clone();
        }
        let file = dir().join(format!("{domain}.png"));
        if file.exists() {
            match Image::load_from_path(&file) {
                Ok(image) => {
                    self.memory.insert(domain, Some(image.clone()));
                    return Some(image);
                }
                Err(_) => {
                    let _ = fs::remove_file(&file);
                }
            }
        }
        if self.loading.insert(domain.clone()) {
            runtime.spawn(async move {
                let fetched = fetch(&domain).await;
                let _ = slint::invoke_from_event_loop(move || {
                    with_state(|state| {
                        state.favicons.loading.remove(&domain);
                        match fetched {
                            // Decoded on the main thread (Slint images aren't Send).
                            Ok(Some(path)) => {
                                let image = Image::load_from_path(&path).ok();
                                if image.is_none() {
                                    let _ = fs::remove_file(&path);
                                }
                                state.favicons.memory.insert(domain, image);
                                refresh(state);
                            }
                            Ok(None) => {
                                state.favicons.memory.insert(domain, None);
                                refresh(state);
                            }
                            // No refresh: it would refetch at once, in a loop while offline.
                            Err(e) => log::debug!("Favicon for {domain} failed: {e}"),
                        }
                    })
                });
            });
        }
        None
    }
}

fn dir() -> PathBuf {
    crate::project_dirs().cache_dir().join("favicons")
}

/// Downloads the favicon to the cache: the file, or None for a definitive failure; Err for a network error.
async fn fetch(domain: &str) -> Result<Option<PathBuf>, reqwest::Error> {
    let client = reqwest::Client::builder().timeout(Duration::from_secs(10)).build()?;
    let response = client.get(format!("{FAVICON_URL}?domain={domain}&sz=64")).send().await?;
    // Only a 2xx counts: an unknown domain gets a 404 with a generic globe.
    if !response.status().is_success() {
        return Ok(None);
    }
    let bytes = response.bytes().await?;
    let dir = dir();
    let file = dir.join(format!("{domain}.png"));
    let tmp = dir.join(format!("{domain}.tmp"));
    let written =
        fs::create_dir_all(&dir).and_then(|()| fs::write(&tmp, &bytes)).and_then(|()| fs::rename(&tmp, &file));
    Ok(written.ok().map(|()| file))
}

#[cfg(test)]
mod tests {
    use super::favicon_domain;

    // Android SettingsTextTest `faviconDomains`.
    #[test]
    fn favicon_domains() {
        assert_eq!(favicon_domain("https://api.groq.com/openai/v1").as_deref(), Some("groq.com"));
        assert_eq!(favicon_domain(" https://openrouter.ai/api/v1 ").as_deref(), Some("openrouter.ai"));
        assert_eq!(favicon_domain("https://foo.bar.co.uk/v1").as_deref(), Some("bar.co.uk"));
        assert_eq!(favicon_domain("https://127.0.0.1:8080/v1"), None);
        assert_eq!(favicon_domain("https://localhost/v1"), None);
        assert_eq!(favicon_domain("https://llm.internal/v1"), None);
        assert_eq!(favicon_domain("not a url"), None);
    }
}
