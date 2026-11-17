/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Encrypted backups of the settings and API keys: Android's `Backup.kt` (the same file format, payload and messages,
//! so a backup made by either app restores on the other) and `SettingsRepository.exportBackup` / `restoreBackup`.
//! Nothing here logs or returns the password, a key or the payload.

use std::collections::{BTreeMap, HashSet};
use std::fmt;
use std::io;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, Key, KeyInit, Nonce, Payload as AeadPayload};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroize;

use super::model;
use super::secrets::{SecretError, SecretStore, custom_cleanup_key};
use super::store::{self, TypedValue, Values};

pub const BACKUP_KDF_ITERATIONS: u32 = 600_000;

pub const NOT_A_BACKUP: &str = "This isn't an Iris backup file.";
pub const NEWER_VERSION: &str = "This backup is from a newer version of Iris. Update the app, then try again.";
pub const WRONG_PASSWORD_OR_DAMAGED: &str = "Wrong password, or the file is damaged.";
pub const DAMAGED: &str = "This backup is damaged and can't be restored.";
pub const KEY_UNREADABLE: &str = "Couldn't read a saved API key. Enter it again, then try again.";

// The Backup page's (Android `BackupPage.kt`).
/// Far above any real backup; larger files aren't read whole (the page reads at most this + 1 bytes and treats a
/// longer file as [`NOT_A_BACKUP`]).
pub const MAX_BACKUP_BYTES: usize = 4 * 1024 * 1024;
pub const PASSWORD_REQUIRED: &str = "Enter a password.";
pub const BACKUP_SAVED: &str = "Backup saved.";
pub const RESTORED: &str = "Settings restored.";
pub const BACKUP_FAILED: &str = "Couldn't save the backup. Try again.";
pub const RESTORE_FAILED: &str = "Couldn't restore the backup. Try again.";

/// A backup or restore failure (Android `BackupException`).
#[derive(Debug, PartialEq)]
pub enum BackupError {
    /// One of this module's messages, shown as-is.
    Message(&'static str),
    /// Anything else (writing the settings, the keychain during a restore): the page shows [`BACKUP_FAILED`] or
    /// [`RESTORE_FAILED`]. Never holds a secret.
    Other(String),
}

impl BackupError {
    /// What the page shows: the message, else [generic].
    pub fn user_message(&self, generic: &'static str) -> &'static str {
        match self {
            BackupError::Message(message) => message,
            BackupError::Other(_) => generic,
        }
    }
}

impl fmt::Display for BackupError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            BackupError::Message(message) => f.write_str(message),
            BackupError::Other(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for BackupError {}

fn other(error: impl fmt::Display) -> BackupError {
    BackupError::Other(error.to_string())
}

/// What a backup holds: the raw settings values and the API keys by keychain name.
#[derive(Clone, Debug, PartialEq)]
pub struct BackupContents {
    pub values: Values,
    pub secrets: BTreeMap<String, String>,
}

/// The keychain as backup and restore use it ([`SecretStore`] in the app; injected so tests can fake it).
pub trait Keychain {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError>;
    fn put(&self, name: &str, value: &str) -> Result<(), SecretError>;
    fn clear(&self, name: &str) -> Result<(), SecretError>;
}

impl Keychain for SecretStore {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        SecretStore::get(self, name)
    }

    fn put(&self, name: &str, value: &str) -> Result<(), SecretError> {
        SecretStore::put(self, name, value)
    }

    fn clear(&self, name: &str) -> Result<(), SecretError> {
        SecretStore::clear(self, name)
    }
}

/// An encrypted backup of [values] (the store's) and every stored API key (not statistics). Keychains can't be listed,
/// so the keys read are the five fixed ones and those of the custom providers in [values]. Blocking (key derivation and
/// keychain): run it off the UI thread.
pub fn export_backup(values: &Values, keychain: &impl Keychain, password: &str) -> Result<Vec<u8>, BackupError> {
    let mut secrets = BTreeMap::new();
    for name in known_secret_names(values) {
        match keychain.get(&name) {
            Ok(Some(value)) => {
                secrets.insert(name, value);
            }
            Ok(None) => {}
            Err(e) => {
                // Keychain errors never contain the key.
                log::warn!("Backup: couldn't read the key {name}: {e}");
                return Err(BackupError::Message(KEY_UNREADABLE));
            }
        }
    }
    let mut plain = encode_backup(&BackupContents { values: values.clone(), secrets });
    let file = encrypt_backup(&plain, password, BACKUP_KDF_ITERATIONS);
    plain.zeroize();
    Ok(file)
}

/// Replaces all settings and API keys with [file]'s, after decrypting and validating all of it (Android
/// `restoreBackup` + `SecretStore.replaceAll`): clears every key known for [current] (the store's values), then
/// [write_settings] replaces the whole settings map ([`super::Store::replace_all`]), then the backup's keys are stored.
/// A failure at any point leaves at most keyless providers, never an orphaned key. Only the backup's keys that the
/// restored settings know (the five fixed ones and its custom providers') are stored: keychains can't be listed, so any
/// other name would stay behind for good. Blocking: run it off the UI thread.
pub fn restore_backup(
    keychain: &impl Keychain,
    current: &Values,
    file: &[u8],
    password: &str,
    write_settings: impl FnOnce(Values) -> io::Result<()>,
) -> Result<(), BackupError> {
    let mut payload = decrypt_backup(file, password)?;
    let contents = decode_backup(&payload);
    payload.zeroize();
    let contents = contents?;
    let allowed: HashSet<String> = known_secret_names(&contents.values).into_iter().collect();
    for name in known_secret_names(current) {
        keychain.clear(&name).map_err(other)?;
    }
    write_settings(contents.values).map_err(other)?;
    for (name, value) in contents.secrets.iter().filter(|(name, _)| allowed.contains(*name)) {
        keychain.put(name, value).map_err(other)?;
    }
    Ok(())
}

/// The five fixed key names and those of [values]' custom providers.
fn known_secret_names(values: &Values) -> Vec<String> {
    // The store only holds valid providers (it drops invalid entries on load).
    let providers = model::custom_cleanup_providers(values).unwrap_or_default();
    SecretStore::names(providers.iter().map(|p| p.id.as_str()))
}

/// Payload JSON bytes of [contents].
pub fn encode_backup(contents: &BackupContents) -> Vec<u8> {
    let payload = Payload {
        version: PAYLOAD_VERSION,
        settings: store::encode_values(&contents.values),
        secrets: contents.secrets.clone(),
    };
    serde_json::to_vec(&payload).expect("serialisable")
}

/// Parses and validates a payload (Android `decodeBackup`): errors carry no detail (JSON errors quote the input).
pub fn decode_backup(payload: &[u8]) -> Result<BackupContents, BackupError> {
    let damaged = || BackupError::Message(DAMAGED);
    // Kotlin's decodeToString.
    let text = String::from_utf8_lossy(payload);
    // The version first: a newer payload may not decode as this one.
    let version = serde_json::from_str::<PayloadVersion>(&text).map_err(|_| damaged())?.version;
    if version > PAYLOAD_VERSION {
        return Err(BackupError::Message(NEWER_VERSION));
    }
    let decoded = serde_json::from_str::<Payload>(&text).map_err(|_| damaged())?;

    // Unknown keys are kept: nothing reads them (Android-only keys such as `bubble_side` survive on the desktop, and
    // desktop-only keys on Android).
    let values = store::decode_values(decoded.settings).map_err(|_| damaged())?;
    // Whatever would break reading the settings (a bad enum, malformed JSON, a wrongly typed known key) is rejected
    // here, before anything is written.
    if model::to_settings(&values, &HashSet::new()).is_err() {
        return Err(damaged());
    }

    // A custom provider's key without its provider would be orphaned.
    let provider_ids: HashSet<String> =
        model::custom_cleanup_providers(&values).map_err(|_| damaged())?.into_iter().map(|p| p.id).collect();
    let custom_prefix = custom_cleanup_key("");
    let mut secrets = BTreeMap::new();
    for (name, value) in decoded.secrets {
        if name.trim().is_empty() {
            return Err(damaged());
        }
        if name.strip_prefix(&custom_prefix).is_some_and(|id| !provider_ids.contains(id)) {
            continue;
        }
        secrets.insert(name, value);
    }
    Ok(BackupContents { values, secrets })
}

/// The file around [plain]: magic, format version, iterations, salt and nonce (the header, authenticated as AAD), then
/// AES-256-GCM with a PBKDF2-HMAC-SHA256 key from [password] (UTF-8). [iterations] is a parameter only so tests run
/// fast.
pub fn encrypt_backup(plain: &[u8], password: &str, iterations: u32) -> Vec<u8> {
    let mut salt = [0u8; SALT_SIZE];
    let mut nonce = [0u8; NONCE_SIZE];
    getrandom::fill(&mut salt).expect("OS random generator");
    getrandom::fill(&mut nonce).expect("OS random generator");
    let mut file = Vec::with_capacity(HEADER_SIZE + plain.len() + TAG_SIZE);
    file.extend_from_slice(MAGIC);
    file.push(FORMAT_VERSION);
    file.extend_from_slice(&iterations.to_be_bytes());
    file.extend_from_slice(&salt);
    file.extend_from_slice(&nonce);
    let sealed = cipher(password, &salt, iterations)
        .encrypt(&Nonce::<Aes256Gcm>::from(nonce), AeadPayload { msg: plain, aad: &file })
        .expect("AES-GCM encryption");
    file.extend_from_slice(&sealed);
    file
}

/// [file]'s payload bytes; fails with [`NOT_A_BACKUP`], [`NEWER_VERSION`] or [`WRONG_PASSWORD_OR_DAMAGED`].
pub fn decrypt_backup(file: &[u8], password: &str) -> Result<Vec<u8>, BackupError> {
    if file.len() < HEADER_SIZE + TAG_SIZE || !file.starts_with(MAGIC) {
        return Err(BackupError::Message(NOT_A_BACKUP));
    }
    let version = file[MAGIC.len()];
    if version > FORMAT_VERSION {
        return Err(BackupError::Message(NEWER_VERSION));
    }
    if version == 0 {
        return Err(BackupError::Message(NOT_A_BACKUP));
    }
    // Signed, as Java's getInt. Bounded, so a crafted value can't hang the key derivation.
    let iterations = i32::from_be_bytes(file[9..13].try_into().expect("4 bytes"));
    if !(1..=MAX_ITERATIONS).contains(&iterations) {
        return Err(BackupError::Message(WRONG_PASSWORD_OR_DAMAGED));
    }
    let salt = &file[13..13 + SALT_SIZE];
    let nonce: [u8; NONCE_SIZE] = file[29..HEADER_SIZE].try_into().expect("12 bytes");
    let (header, sealed) = file.split_at(HEADER_SIZE);
    cipher(password, salt, iterations as u32)
        .decrypt(&Nonce::<Aes256Gcm>::from(nonce), AeadPayload { msg: sealed, aad: header })
        .map_err(|_| BackupError::Message(WRONG_PASSWORD_OR_DAMAGED))
}

fn cipher(password: &str, salt: &[u8], iterations: u32) -> Aes256Gcm {
    let mut key = pbkdf2::pbkdf2_hmac_array::<Sha256, KEY_SIZE>(password.as_bytes(), salt, iterations);
    let cipher = Aes256Gcm::new(&Key::<Aes256Gcm>::from(key));
    key.zeroize();
    cipher
}

#[derive(Serialize, Deserialize)]
struct Payload {
    version: i32,
    settings: BTreeMap<String, TypedValue>,
    secrets: BTreeMap<String, String>,
}

#[derive(Deserialize)]
struct PayloadVersion {
    version: i32,
}

const MAGIC: &[u8] = b"IRISBKUP";
const FORMAT_VERSION: u8 = 1;
const PAYLOAD_VERSION: i32 = 1;
const SALT_SIZE: usize = 16;
const NONCE_SIZE: usize = 12;
const TAG_SIZE: usize = 16;
const HEADER_SIZE: usize = 41; // magic 8, version 1, iterations 4, salt 16, nonce 12
const MAX_ITERATIONS: i32 = 10_000_000;
const KEY_SIZE: usize = 32;

#[cfg(test)]
mod tests;
