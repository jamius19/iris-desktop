/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! API keys in the OS keychain (Windows Credential Manager, Linux Secret Service): service [`SERVICE`], account = the
//! Android pref name. Blocking calls: run them off the UI thread. Values are never logged.

use std::collections::HashSet;
use std::fmt;

use keyring::Entry;

pub const SERVICE: &str = "dev.jamius.iris";

pub const OPENAI_API_KEY: &str = "openai_api_key";
pub const GATEWAY_API_KEY: &str = "gateway_api_key";
pub const DEEPGRAM_API_KEY: &str = "deepgram_api_key";
pub const SONIOX_API_KEY: &str = "soniox_api_key";
pub const OPENROUTER_API_KEY: &str = "openrouter_api_key";

const CUSTOM_CLEANUP_KEY_PREFIX: &str = "custom_cleanup_key_";

/// The key of the custom cleanup provider [id].
pub fn custom_cleanup_key(id: &str) -> String {
    format!("{CUSTOM_CLEANUP_KEY_PREFIX}{id}")
}

/// A keychain failure; its message never contains a secret.
#[derive(Debug)]
pub struct SecretError(String);

impl fmt::Display for SecretError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SecretError {}

impl From<keyring::Error> for SecretError {
    fn from(error: keyring::Error) -> Self {
        // Display, not Debug: Debug of BadEncoding/BadDataFormat holds the secret's bytes.
        SecretError(error.to_string())
    }
}

#[derive(Default)]
pub struct SecretStore;

impl SecretStore {
    pub fn new() -> Self {
        SecretStore
    }

    /// Stores [value] trimmed (Android trims every key before storing it).
    pub fn put(&self, name: &str, value: &str) -> Result<(), SecretError> {
        Ok(Entry::new(SERVICE, name)?.set_password(value.trim())?)
    }

    pub fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        match Entry::new(SERVICE, name)?.get_password() {
            Ok(value) => Ok(Some(value)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Removes [name]; nothing stored is no error.
    pub fn clear(&self, name: &str) -> Result<(), SecretError> {
        match Entry::new(SERVICE, name)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    /// Which of [names] are stored, probing each (keychains can't be listed); one that can't be read counts as not
    /// stored (logged).
    pub fn stored(&self, names: &[String]) -> HashSet<String> {
        stored_by(names, |name| self.get(name))
    }

    /// The five fixed key names, then the custom cleanup providers' ones.
    pub fn names<'a>(custom_provider_ids: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        [OPENAI_API_KEY, GATEWAY_API_KEY, DEEPGRAM_API_KEY, SONIOX_API_KEY, OPENROUTER_API_KEY]
            .into_iter()
            .map(str::to_string)
            .chain(custom_provider_ids.into_iter().map(custom_cleanup_key))
            .collect()
    }
}

fn stored_by(names: &[String], get: impl Fn(&str) -> Result<Option<String>, SecretError>) -> HashSet<String> {
    names
        .iter()
        .filter(|name| match get(name) {
            Ok(value) => value.is_some(),
            Err(e) => {
                log::warn!("Couldn't read the key {name}: {e}");
                false
            }
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn built_ins() -> Vec<String> {
        SecretStore::names([])
    }

    fn stored_in(keychain: &HashMap<String, String>, names: &[String]) -> HashSet<String> {
        stored_by(names, |name| Ok(keychain.get(name).cloned()))
    }

    fn keychain(names: &[String]) -> HashMap<String, String> {
        names.iter().map(|name| (name.clone(), "secret".to_string())).collect()
    }

    #[test]
    fn stored_secrets_reports_every_stored_pref() {
        let all = SecretStore::names(["c1"]);
        assert_eq!(HashSet::new(), stored_in(&HashMap::new(), &all));

        assert_eq!(all.iter().cloned().collect::<HashSet<_>>(), stored_in(&keychain(&all), &all));
    }

    #[test]
    fn stored_secrets_leaves_out_absent_ones() {
        let some = vec![GATEWAY_API_KEY.to_string(), custom_cleanup_key("c2")];
        let names = SecretStore::names(["c1", "c2"]);

        let stored = stored_in(&keychain(&some), &names);
        assert_eq!(some.into_iter().collect::<HashSet<_>>(), stored);
        for name in built_ins().iter().filter(|n| *n != GATEWAY_API_KEY) {
            assert!(!stored.contains(name));
        }
        assert!(!stored.contains(&custom_cleanup_key("c1")));
    }

    /// Desktop: a keychain error reads as not stored.
    #[test]
    fn unreadable_secret_is_not_stored() {
        let names = SecretStore::names([]);
        let stored = stored_by(&names, |name| {
            if name == OPENAI_API_KEY { Err(SecretError("locked".into())) } else { Ok(Some("k".into())) }
        });

        assert!(!stored.contains(OPENAI_API_KEY));
        assert_eq!(4, stored.len());
    }

    #[test]
    fn names_are_the_android_pref_names() {
        assert_eq!(
            vec![
                "openai_api_key",
                "gateway_api_key",
                "deepgram_api_key",
                "soniox_api_key",
                "openrouter_api_key",
                "custom_cleanup_key_c1"
            ],
            SecretStore::names(["c1"])
        );
    }
}
