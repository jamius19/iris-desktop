/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The settings file: a typed key/value map (Android's preferences) as `{"version":1,"settings":{name:{"type":…,
//! "value":…}}}`, each value a string parsed by its type, the same object as Android's backup payload `settings`.
//! Setters port `SettingsRepository`'s: same trimming and normalising; each writes the file atomically.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};

use super::model::{
    self, CLEANUP_PROVIDER_VERCEL_GATEWAY, CleanupPrompt, CustomCleanupProvider, Settings, keys, normalize_custom_words,
};

/// A stored value; Android's preference types.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    String(String),
    Int(i32),
    Boolean(bool),
    Float(f32),
}

pub type Values = BTreeMap<String, Value>;

const FILE_VERSION: i32 = 1;

pub struct Store {
    path: PathBuf,
    values: Values,
    // The file couldn't be read or set aside: saving would overwrite it, so every edit fails.
    unwritable: bool,
}

impl Store {
    /// `settings.json` in the config dir: Windows `%APPDATA%\jamius\Iris\config`, Linux `~/.config/iris`.
    pub fn default_path() -> PathBuf {
        ProjectDirs::from("dev", "jamius", "Iris").expect("no home directory").config_dir().join("settings.json")
    }

    /// Loads [path]; a missing file is empty. A corrupt (or newer-version) file is moved aside to
    /// `settings.json.corrupt-<unix seconds>` and reads as empty; an invalid entry is dropped (the file copied aside
    /// first). An unreadable file, or one that couldn't be set aside, reads as empty and makes every edit fail, so it
    /// is never overwritten. All logged.
    pub fn open(path: PathBuf) -> Store {
        let mut unwritable = false;
        let values = match fs::read(&path) {
            Ok(bytes) => match decode_file(&bytes) {
                Ok(values) => {
                    let (values, ok) = drop_invalid(&path, values);
                    unwritable = !ok;
                    values
                }
                Err(e) => {
                    log::warn!("Settings file {} is corrupt, using defaults: {e}", path.display());
                    unwritable = !set_aside(&path, false);
                    Values::new()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Values::new(),
            Err(e) => {
                log::warn!("Couldn't read the settings file {}, using defaults: {e}", path.display());
                unwritable = true;
                Values::new()
            }
        };
        Store { path, values, unwritable }
    }

    /// The settings with defaults ([`model::to_settings`]). [`Store::open`] drops each invalid entry on its own, so a
    /// wrongly typed or malformed key falls back to its default only; all defaults (logged) is a last resort.
    pub fn settings(&self, stored_secrets: &HashSet<String>) -> Settings {
        model::to_settings(&self.values, stored_secrets).unwrap_or_else(|e| {
            log::warn!("Invalid settings, using defaults: {e}");
            model::to_settings(&Values::new(), stored_secrets).expect("defaults are valid")
        })
    }

    /// The stored values as saved (unknown keys included).
    pub fn values(&self) -> &Values {
        &self.values
    }

    /// Replaces every stored value with [values] (backup restore: anything not in [values] is removed).
    pub fn replace_all(&mut self, values: Values) -> io::Result<()> {
        self.edit(|current| {
            *current = values;
            Ok(())
        })
    }

    pub fn set_stt_provider(&mut self, id: &str) -> io::Result<()> {
        self.set(keys::STT_PROVIDER_ID, string(id))
    }

    pub fn set_stt_model(&mut self, model: &str) -> io::Result<()> {
        self.set(keys::STT_MODEL, string(model.trim()))
    }

    /// [live]: the model streams (see [`Settings::gateway_stt_live`]).
    pub fn set_gateway_stt_model(&mut self, model: &str, live: bool) -> io::Result<()> {
        self.edit(|values| {
            put(values, keys::GATEWAY_STT_MODEL, string(model.trim()));
            put(values, keys::GATEWAY_STT_LIVE, Value::Boolean(live));
            Ok(())
        })
    }

    pub fn set_deepgram_stt_model(&mut self, model: &str) -> io::Result<()> {
        self.set(keys::DEEPGRAM_STT_MODEL, string(model.trim()))
    }

    pub fn set_deepgram_upload(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::DEEPGRAM_UPLOAD, Value::Boolean(enabled))
    }

    pub fn set_soniox_stt_model(&mut self, model: &str) -> io::Result<()> {
        self.set(keys::SONIOX_STT_MODEL, string(model.trim()))
    }

    pub fn set_soniox_region(&mut self, id: &str) -> io::Result<()> {
        self.set(keys::SONIOX_REGION, string(id))
    }

    pub fn set_soniox_languages(&mut self, languages: &str) -> io::Result<()> {
        self.set(keys::SONIOX_LANGUAGES, string(languages.trim()))
    }

    pub fn set_on_device_stt_model(&mut self, id: &str) -> io::Result<()> {
        self.set(keys::ON_DEVICE_STT_MODEL, string(id))
    }

    pub fn set_on_device_unload_minutes(&mut self, minutes: i32) -> io::Result<()> {
        self.set(keys::ON_DEVICE_UNLOAD_MINUTES, Value::Int(minutes))
    }

    pub fn set_stt_language(&mut self, language: &str) -> io::Result<()> {
        self.set(keys::STT_LANGUAGE, string(language.trim()))
    }

    pub fn set_stt_timeout_seconds(&mut self, seconds: i32) -> io::Result<()> {
        self.set(keys::STT_TIMEOUT_SECONDS, Value::Int(seconds))
    }

    pub fn set_vad_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::VAD_ENABLED, Value::Boolean(enabled))
    }

    pub fn set_stt_speed_percent(&mut self, percent: i32) -> io::Result<()> {
        self.set(keys::STT_SPEED_PERCENT, Value::Int(percent))
    }

    pub fn set_remove_filler_words(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::REMOVE_FILLER_WORDS, Value::Boolean(enabled))
    }

    pub fn set_custom_words_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::CUSTOM_WORDS_ENABLED, Value::Boolean(enabled))
    }

    pub fn set_correct_similar_words(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::CORRECT_SIMILAR_WORDS, Value::Boolean(enabled))
    }

    /// Adds [word] at the end; the list is stored normalised ([`normalize_custom_words`]: drops a blank or duplicate).
    pub fn add_custom_word(&mut self, word: &str) -> io::Result<()> {
        self.edit(|values| {
            let mut words = model::custom_words(values).map_err(invalid)?;
            words.push(word.to_string());
            put(values, keys::CUSTOM_WORDS, json(&normalize_custom_words(&words)));
            Ok(())
        })
    }

    /// Removes [word] (as [`Settings::custom_words`] shows it).
    pub fn remove_custom_word(&mut self, word: &str) -> io::Result<()> {
        self.edit(|values| {
            let mut words = normalize_custom_words(&model::custom_words(values).map_err(invalid)?);
            words.retain(|w| w != word);
            put(values, keys::CUSTOM_WORDS, json(&words));
            Ok(())
        })
    }

    pub fn set_cleanup_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::CLEANUP_ENABLED, Value::Boolean(enabled))
    }

    pub fn set_cleanup_provider(&mut self, id: &str) -> io::Result<()> {
        self.set(keys::CLEANUP_PROVIDER_ID, string(id))
    }

    pub fn set_cleanup_model(&mut self, model: &str) -> io::Result<()> {
        self.set(keys::CLEANUP_MODEL, string(model.trim()))
    }

    /// None = model default, stored as "" (a missing key means the default level).
    pub fn set_cleanup_reasoning_effort(&mut self, effort: Option<&str>) -> io::Result<()> {
        self.set(keys::CLEANUP_REASONING_EFFORT, string(effort.unwrap_or("")))
    }

    /// Stored as typed (trimmed); "" clears.
    pub fn set_cleanup_provider_options(&mut self, options: &str) -> io::Result<()> {
        self.set(keys::CLEANUP_PROVIDER_OPTIONS, string(options.trim()))
    }

    pub fn set_open_router_cleanup_model(&mut self, model: &str) -> io::Result<()> {
        self.set(keys::OPENROUTER_CLEANUP_MODEL, string(model.trim()))
    }

    /// None = model default, stored as "".
    pub fn set_open_router_cleanup_reasoning_effort(&mut self, effort: Option<&str>) -> io::Result<()> {
        self.set(keys::OPENROUTER_CLEANUP_REASONING_EFFORT, string(effort.unwrap_or("")))
    }

    /// Stored as typed (trimmed); "" clears.
    pub fn set_open_router_cleanup_provider_options(&mut self, options: &str) -> io::Result<()> {
        self.set(keys::OPENROUTER_CLEANUP_PROVIDER_OPTIONS, string(options.trim()))
    }

    /// Adds the prompt, or replaces the existing prompt with the same id.
    pub fn save_prompt(&mut self, prompt: CleanupPrompt) -> io::Result<()> {
        self.edit(|values| {
            let mut prompts = model::prompts(values).map_err(invalid)?;
            match prompts.iter_mut().find(|p| p.id == prompt.id) {
                Some(existing) => *existing = prompt,
                None => prompts.push(prompt),
            }
            put(values, keys::PROMPTS, json(&prompts));
            Ok(())
        })
    }

    pub fn delete_prompt(&mut self, id: &str) -> io::Result<()> {
        self.edit(|values| {
            let mut prompts = model::prompts(values).map_err(invalid)?;
            prompts.retain(|p| p.id != id);
            put(values, keys::PROMPTS, json(&prompts));
            Ok(())
        })
    }

    pub fn set_active_prompt(&mut self, id: &str) -> io::Result<()> {
        self.set(keys::ACTIVE_PROMPT_ID, string(id))
    }

    pub fn set_cleanup_timeout_seconds(&mut self, seconds: i32) -> io::Result<()> {
        self.set(keys::CLEANUP_TIMEOUT_SECONDS, Value::Int(seconds))
    }

    /// Android only (kept for parity): [side] is "LEFT" or "RIGHT".
    pub fn set_bubble_position(&mut self, side: &str, y_fraction: f32) -> io::Result<()> {
        self.edit(|values| {
            put(values, keys::BUBBLE_SIDE, string(side));
            put(values, keys::BUBBLE_Y_FRACTION, Value::Float(y_fraction));
            Ok(())
        })
    }

    pub fn set_panel_lines(&mut self, lines: i32) -> io::Result<()> {
        self.set(keys::PANEL_LINES, Value::Int(lines))
    }

    /// Kept for parity: the desktop reads `overlay_width_percent` instead.
    pub fn set_panel_width_percent(&mut self, percent: i32) -> io::Result<()> {
        self.set(keys::PANEL_WIDTH_PERCENT, Value::Int(percent))
    }

    /// Saves the custom cleanup provider [id] (adds it, or updates the name and base URL of the existing one, keeping
    /// its model settings) and selects it. Its key goes to the `SecretStore` first, by the caller.
    pub fn save_custom_cleanup_provider(&mut self, id: &str, name: &str, base_url: &str) -> io::Result<()> {
        let name = name.trim();
        let url = base_url.trim().trim_end_matches('/');
        self.edit(|values| {
            let mut providers = model::custom_cleanup_providers(values).map_err(invalid)?;
            match providers.iter_mut().find(|p| p.id == id) {
                Some(existing) => {
                    existing.name = name.to_string();
                    existing.base_url = url.to_string();
                }
                None => providers.push(CustomCleanupProvider::new(id, name, url)),
            }
            put(values, keys::CUSTOM_CLEANUP_PROVIDERS, json(&providers));
            put(values, keys::CLEANUP_PROVIDER_ID, string(id));
            Ok(())
        })
    }

    /// Applies [update] to the custom cleanup provider [id] (its model settings); nothing if it is gone.
    pub fn update_custom_cleanup_provider(
        &mut self,
        id: &str,
        update: impl FnOnce(&mut CustomCleanupProvider),
    ) -> io::Result<()> {
        self.edit(|values| {
            let mut providers = model::custom_cleanup_providers(values).map_err(invalid)?;
            if let Some(provider) = providers.iter_mut().find(|p| p.id == id) {
                update(provider);
                put(values, keys::CUSTOM_CLEANUP_PROVIDERS, json(&providers));
            }
            Ok(())
        })
    }

    /// Deletes the custom cleanup provider [id]; if it was selected, the Gateway is. Its key is cleared first, by the
    /// caller, so a failure leaves a keyless provider (retryable), never an orphaned key.
    pub fn delete_custom_cleanup_provider(&mut self, id: &str) -> io::Result<()> {
        self.edit(|values| {
            let mut providers = model::custom_cleanup_providers(values).map_err(invalid)?;
            providers.retain(|p| p.id != id);
            put(values, keys::CUSTOM_CLEANUP_PROVIDERS, json(&providers));
            if values.get(keys::CLEANUP_PROVIDER_ID) == Some(&string(id)) {
                put(values, keys::CLEANUP_PROVIDER_ID, string(CLEANUP_PROVIDER_VERCEL_GATEWAY));
            }
            Ok(())
        })
    }

    pub fn set_overlay_position(&mut self, position: &str) -> io::Result<()> {
        self.set(keys::OVERLAY_POSITION, string(position))
    }

    pub fn set_overlay_width_percent(&mut self, percent: i32) -> io::Result<()> {
        self.set(keys::OVERLAY_WIDTH_PERCENT, Value::Int(percent))
    }

    pub fn set_microphone_device(&mut self, device: &str) -> io::Result<()> {
        self.set(keys::MICROPHONE_DEVICE, string(device))
    }

    pub fn set_local_accelerator(&mut self, accelerator: &str) -> io::Result<()> {
        self.set(keys::LOCAL_ACCELERATOR, string(accelerator))
    }

    pub fn set_shortcut_transcribe(&mut self, shortcut: &str) -> io::Result<()> {
        self.set(keys::SHORTCUT_TRANSCRIBE, string(shortcut))
    }

    pub fn set_shortcut_cancel(&mut self, shortcut: &str) -> io::Result<()> {
        self.set(keys::SHORTCUT_CANCEL, string(shortcut))
    }

    pub fn set_shortcut_activation(&mut self, activation: &str) -> io::Result<()> {
        self.set(keys::SHORTCUT_ACTIVATION, string(activation))
    }

    pub fn set_paste_method(&mut self, method: &str) -> io::Result<()> {
        self.set(keys::PASTE_METHOD, string(method))
    }

    pub fn set_append_trailing_space(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::APPEND_TRAILING_SPACE, Value::Boolean(enabled))
    }

    pub fn set_restore_clipboard(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::RESTORE_CLIPBOARD, Value::Boolean(enabled))
    }

    pub fn set_paste_delay_ms(&mut self, ms: i32) -> io::Result<()> {
        self.set(keys::PASTE_DELAY_MS, Value::Int(ms))
    }

    pub fn set_paste_delay_after_ms(&mut self, ms: i32) -> io::Result<()> {
        self.set(keys::PASTE_DELAY_AFTER_MS, Value::Int(ms))
    }

    pub fn set_feedback_sounds(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::FEEDBACK_SOUNDS, Value::Boolean(enabled))
    }

    pub fn set_feedback_volume_percent(&mut self, percent: i32) -> io::Result<()> {
        self.set(keys::FEEDBACK_VOLUME_PERCENT, Value::Int(percent))
    }

    /// Mute and Lower are exclusive: turning one on turns the other off.
    pub fn set_mute_while_recording(&mut self, enabled: bool) -> io::Result<()> {
        self.edit(|values| {
            put(values, keys::MUTE_WHILE_RECORDING, Value::Boolean(enabled));
            if enabled {
                put(values, keys::DUCK_WHILE_RECORDING, Value::Boolean(false));
            }
            Ok(())
        })
    }

    pub fn set_duck_while_recording(&mut self, enabled: bool) -> io::Result<()> {
        self.edit(|values| {
            put(values, keys::DUCK_WHILE_RECORDING, Value::Boolean(enabled));
            if enabled {
                put(values, keys::MUTE_WHILE_RECORDING, Value::Boolean(false));
            }
            Ok(())
        })
    }

    pub fn set_duck_amount(&mut self, percent: i32) -> io::Result<()> {
        self.set(keys::DUCK_AMOUNT, Value::Int(percent))
    }

    pub fn set_launch_at_startup(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::LAUNCH_AT_STARTUP, Value::Boolean(enabled))
    }

    pub fn set_start_hidden(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::START_HIDDEN, Value::Boolean(enabled))
    }

    pub fn set_history_enabled(&mut self, enabled: bool) -> io::Result<()> {
        self.set(keys::HISTORY_ENABLED, Value::Boolean(enabled))
    }

    pub fn set_history_limit(&mut self, limit: i32) -> io::Result<()> {
        self.set(keys::HISTORY_LIMIT, Value::Int(limit))
    }

    fn set(&mut self, key: &str, value: Value) -> io::Result<()> {
        self.edit(|values| {
            put(values, key, value);
            Ok(())
        })
    }

    /// Applies [change] to a copy and writes it; kept in memory only once written (like a DataStore edit).
    fn edit(&mut self, change: impl FnOnce(&mut Values) -> io::Result<()>) -> io::Result<()> {
        if self.unwritable {
            return Err(io::Error::other("the settings file couldn't be read or set aside; not overwriting it"));
        }
        let mut values = self.values.clone();
        change(&mut values)?;
        self.write(&values)?;
        self.values = values;
        Ok(())
    }

    /// Writes a temp file next to the settings file, then renames it over it.
    fn write(&self, values: &Values) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut tmp_name = self.path.file_name().unwrap_or_default().to_os_string();
        tmp_name.push(".tmp");
        let tmp = self.path.with_file_name(tmp_name);
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&encode_file(values))?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, &self.path)
    }
}

/// [values] without the entries [`model::to_settings`] rejects, each checked on its own (logged by key, never the
/// value); the file is copied aside first if any is dropped. False if that copy failed.
fn drop_invalid(path: &Path, mut values: Values) -> (Values, bool) {
    let invalid: Vec<String> = values
        .iter()
        .filter(|(k, v)| model::to_settings(&Values::from([((*k).clone(), (*v).clone())]), &HashSet::new()).is_err())
        .map(|(k, _)| k.clone())
        .collect();
    if invalid.is_empty() {
        return (values, true);
    }
    log::warn!("Invalid settings {invalid:?} in {}, using their defaults", path.display());
    let ok = set_aside(path, true);
    values.retain(|k, _| !invalid.contains(k));
    (values, ok)
}

/// Moves (or with [copy], copies) the settings file to `<name>.corrupt-<unix seconds>` beside it; false (logged) if
/// that failed.
fn set_aside(path: &Path, copy: bool) -> bool {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".corrupt-{secs}"));
    let target = path.with_file_name(name);
    let result = if copy { fs::copy(path, &target).map(|_| ()) } else { fs::rename(path, &target) };
    match result {
        Ok(()) => {
            log::warn!("Settings file kept as {}", target.display());
            true
        }
        Err(e) => {
            log::error!("Couldn't keep the settings file as {}, not saving settings: {e}", target.display());
            false
        }
    }
}

fn string(value: &str) -> Value {
    Value::String(value.to_string())
}

fn json<T: Serialize>(value: &T) -> Value {
    Value::String(serde_json::to_string(value).expect("serialisable"))
}

fn put(values: &mut Values, key: &str, value: Value) {
    values.insert(key.to_string(), value);
}

fn invalid(error: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, error)
}

#[derive(Serialize, Deserialize)]
struct SettingsFile {
    version: i32,
    settings: BTreeMap<String, TypedValue>,
}

/// A value as a string, parsed by [type] (Android's backup `TypedValue`).
#[derive(Serialize, Deserialize)]
pub(crate) struct TypedValue {
    r#type: String,
    value: String,
}

fn encode_file(values: &Values) -> Vec<u8> {
    serde_json::to_vec_pretty(&SettingsFile { version: FILE_VERSION, settings: encode_values(values) })
        .expect("serialisable")
}

/// [values] as typed strings (the file's and the backup payload's `settings`).
pub(crate) fn encode_values(values: &Values) -> BTreeMap<String, TypedValue> {
    values
        .iter()
        .map(|(name, value)| {
            let (r#type, value) = match value {
                Value::String(s) => ("string", s.clone()),
                Value::Int(i) => ("int", i.to_string()),
                Value::Boolean(b) => ("boolean", b.to_string()),
                // Debug keeps the ".0" Kotlin's Float.toString writes.
                Value::Float(f) => ("float", format!("{f:?}")),
            };
            (name.clone(), TypedValue { r#type: r#type.to_string(), value })
        })
        .collect()
}

/// Errs on malformed JSON, a newer version, a blank name, an unknown type or a value its type can't parse (as Android's
/// backup decode).
fn decode_file(bytes: &[u8]) -> Result<Values, String> {
    let file: SettingsFile = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if file.version > FILE_VERSION {
        return Err(format!("version {} is newer than {FILE_VERSION}", file.version));
    }
    decode_values(file.settings)
}

/// Typed strings as values; errs on a blank name, an unknown type or a value its type can't parse.
pub(crate) fn decode_values(settings: BTreeMap<String, TypedValue>) -> Result<Values, String> {
    settings
        .into_iter()
        .map(|(name, typed)| {
            let bad = || format!("bad {} value for {name}", typed.r#type);
            if name.trim().is_empty() {
                return Err("blank name".to_string());
            }
            let value = match typed.r#type.as_str() {
                "string" => Value::String(typed.value.clone()),
                "int" => Value::Int(typed.value.parse().map_err(|_| bad())?),
                "boolean" => Value::Boolean(typed.value.parse().map_err(|_| bad())?),
                "float" => Value::Float(typed.value.parse().map_err(|_| bad())?),
                _ => return Err(bad()),
            };
            Ok((name, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::model::{DEFAULT_PROMPT_ID, default_prompt};
    use serde_json::json;

    fn temp() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config").join("settings.json");
        (dir, path)
    }

    fn settings(store: &Store) -> Settings {
        store.settings(&HashSet::new())
    }

    fn file_json(path: &PathBuf) -> serde_json::Value {
        serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
    }

    #[test]
    fn missing_file_is_empty() {
        let (_dir, path) = temp();
        let store = Store::open(path.clone());

        assert_eq!(model::to_settings(&Values::new(), &HashSet::new()).unwrap(), settings(&store));
        assert!(!path.exists());
    }

    #[test]
    fn corrupt_file_is_empty() {
        let (_dir, path) = temp();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for corrupt in [
            "not json",
            r#"{"version":1,"settings":{"panel_lines":{"type":"int","value":"x"}}}"#,
            r#"{"version":1,"settings":{"panel_lines":{"type":"long","value":"3"}}}"#,
            r#"{"version":1,"settings":{"vad_enabled":{"type":"boolean","value":"yes"}}}"#,
        ] {
            fs::write(&path, corrupt).unwrap();
            assert_eq!(Store::open(path.clone()).values, Values::new(), "{corrupt}");
        }
    }

    #[test]
    fn round_trip_keeps_unknown_keys_and_int_vs_float() {
        let (_dir, path) = temp();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let android = json!({"version": 1, "settings": {
            "panel_lines": {"type": "int", "value": "5"},
            "bubble_y_fraction": {"type": "float", "value": "0.75"},
            "future_float": {"type": "float", "value": "2.0"},
            "future_int": {"type": "int", "value": "2"},
            "future_flag": {"type": "boolean", "value": "true"},
            "future_text": {"type": "string", "value": "x"},
        }});
        fs::write(&path, android.to_string()).unwrap();

        let mut store = Store::open(path.clone());
        assert_eq!(5, settings(&store).panel_lines);
        assert_eq!(0.75, settings(&store).bubble_y_fraction);
        store.set_vad_enabled(false).unwrap();

        let written = file_json(&path);
        assert_eq!(1, written["version"]);
        let typed = &written["settings"];
        assert_eq!(json!({"type": "int", "value": "5"}), typed["panel_lines"]);
        assert_eq!(json!({"type": "float", "value": "0.75"}), typed["bubble_y_fraction"]);
        assert_eq!(json!({"type": "float", "value": "2.0"}), typed["future_float"]);
        assert_eq!(json!({"type": "int", "value": "2"}), typed["future_int"]);
        assert_eq!(json!({"type": "boolean", "value": "true"}), typed["future_flag"]);
        assert_eq!(json!({"type": "string", "value": "x"}), typed["future_text"]);
        assert_eq!(json!({"type": "boolean", "value": "false"}), typed["vad_enabled"]);

        let reopened = Store::open(path.clone());
        assert_eq!(store.values, reopened.values);
        assert_eq!(Some(&Value::Float(2.0)), reopened.values.get("future_float"));
        assert_eq!(Some(&Value::Int(2)), reopened.values.get("future_int"));
        assert!(!settings(&reopened).vad_enabled);
    }

    #[test]
    fn setters_trim_and_persist() {
        let (_dir, path) = temp();
        let mut store = Store::open(path.clone());
        store.set_stt_model("  gpt-transcribe ").unwrap();
        store.set_gateway_stt_model(" google/x ", false).unwrap();
        store.set_soniox_languages(" en, bn ").unwrap();
        store.set_cleanup_reasoning_effort(None).unwrap();
        store.set_open_router_cleanup_reasoning_effort(Some("high")).unwrap();
        store.set_cleanup_provider_options("  {\"a\":1} ").unwrap();
        store.set_paste_delay_ms(120).unwrap();
        store.set_overlay_width_percent(20).unwrap();
        store.set_launch_at_startup(true).unwrap();
        store.set_start_hidden(false).unwrap();
        store.set_history_enabled(false).unwrap();
        store.set_history_limit(50).unwrap();

        let settings = settings(&Store::open(path));
        assert_eq!("gpt-transcribe", settings.stt_model);
        assert_eq!("google/x", settings.gateway_stt_model);
        assert!(!settings.gateway_stt_live);
        assert_eq!("en, bn", settings.soniox_languages);
        assert_eq!(None, settings.cleanup_reasoning_effort);
        assert_eq!(Some("high"), settings.open_router_cleanup_reasoning_effort.as_deref());
        assert_eq!("{\"a\":1}", settings.cleanup_provider_options);
        assert_eq!(120, settings.paste_delay_ms);
        assert_eq!(20, settings.overlay_width_percent);
        // Android's Max Width is left alone.
        assert_eq!(model::DEFAULT_PANEL_WIDTH_PERCENT, settings.panel_width_percent);
        assert!(settings.launch_at_startup);
        assert!(!settings.start_hidden);
        assert!(!settings.history_enabled);
        assert_eq!(50, settings.history_limit);
    }

    #[test]
    fn mute_and_lower_are_exclusive_and_persist() {
        let (_dir, path) = temp();
        let mut store = Store::open(path.clone());
        store.set_duck_amount(30).unwrap();
        store.set_duck_while_recording(true).unwrap();
        store.set_mute_while_recording(true).unwrap();
        let muted = settings(&Store::open(path.clone()));
        assert!(muted.mute_while_recording);
        assert!(!muted.duck_while_recording);
        assert_eq!(30, muted.duck_amount);

        store.set_duck_while_recording(true).unwrap();
        let lowered = settings(&Store::open(path.clone()));
        assert!(!lowered.mute_while_recording);
        assert!(lowered.duck_while_recording);

        // Turning one off leaves the other alone.
        store.set_mute_while_recording(false).unwrap();
        assert!(settings(&Store::open(path)).duck_while_recording);
    }

    #[test]
    fn custom_words_are_added_normalised_and_removed() {
        let (_dir, path) = temp();
        let mut store = Store::open(path);
        store.add_custom_word(" Deepgram ").unwrap();
        store.add_custom_word("deepgram").unwrap();
        store.add_custom_word("  ").unwrap();
        store.add_custom_word("Jamius \t Siam").unwrap();
        assert_eq!(vec!["Deepgram", "Jamius Siam"], settings(&store).custom_words);

        store.remove_custom_word("Deepgram").unwrap();
        assert_eq!(vec!["Jamius Siam"], settings(&store).custom_words);
    }

    #[test]
    fn prompts_are_saved_replaced_and_deleted() {
        let (_dir, path) = temp();
        let mut store = Store::open(path);
        let added = CleanupPrompt { id: "a".into(), name: "Formal".into(), system_prompt: "Formal.".into() };
        store.save_prompt(added.clone()).unwrap();
        assert_eq!(vec![default_prompt(), added.clone()], settings(&store).prompts);

        let renamed = CleanupPrompt { name: "Very formal".into(), ..added };
        store.save_prompt(renamed.clone()).unwrap();
        store.set_active_prompt("a").unwrap();
        assert_eq!(vec![default_prompt(), renamed.clone()], settings(&store).prompts);
        assert_eq!(Some(renamed.clone()), settings(&store).active_prompt);

        store.delete_prompt(DEFAULT_PROMPT_ID).unwrap();
        assert_eq!(vec![renamed], settings(&store).prompts);
    }

    #[test]
    fn custom_providers_are_saved_updated_and_deleted() {
        let (_dir, path) = temp();
        let mut store = Store::open(path);
        store.save_custom_cleanup_provider("c1", " Groq ", " https://api.groq.com/openai/v1// ").unwrap();
        let saved = settings(&store);
        assert_eq!("c1", saved.cleanup_provider_id);
        assert_eq!(
            vec![CustomCleanupProvider::new("c1", "Groq", "https://api.groq.com/openai/v1")],
            saved.custom_cleanup_providers
        );

        store.update_custom_cleanup_provider("c1", |p| p.model = "llama-4".into()).unwrap();
        store.update_custom_cleanup_provider("gone", |p| p.model = "x".into()).unwrap();
        store.save_custom_cleanup_provider("c1", "Groq 2", "https://api.groq.com/v2").unwrap();
        let provider = &settings(&store).custom_cleanup_providers[0];
        assert_eq!(
            ("Groq 2", "https://api.groq.com/v2", "llama-4"),
            (&*provider.name, &*provider.base_url, &*provider.model)
        );
        assert_eq!(1, settings(&store).custom_cleanup_providers.len());

        store.delete_custom_cleanup_provider("c1").unwrap();
        let deleted = settings(&store);
        assert!(deleted.custom_cleanup_providers.is_empty());
        assert_eq!(CLEANUP_PROVIDER_VERCEL_GATEWAY, deleted.cleanup_provider_id);
        assert_eq!(Some(&string(CLEANUP_PROVIDER_VERCEL_GATEWAY)), store.values.get(keys::CLEANUP_PROVIDER_ID));
    }

    #[test]
    fn failed_write_keeps_the_old_value() {
        let dir = tempfile::tempdir().unwrap();
        // The settings path is a directory: the rename fails.
        let path = dir.path().join("settings.json");
        fs::create_dir_all(path.join("x")).unwrap();
        let mut store = Store::open(path);

        assert!(store.set_panel_lines(5).is_err());
        assert_eq!(model::DEFAULT_PANEL_LINES, settings(&store).panel_lines);
    }

    /// The `settings.json.corrupt-*` files beside [path], as their contents.
    fn set_aside_files(path: &Path) -> Vec<String> {
        fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("settings.json.corrupt-"))
            .map(|p| fs::read_to_string(p).unwrap())
            .collect()
    }

    #[test]
    fn corrupt_file_is_moved_aside_and_kept_after_a_save() {
        let (_dir, path) = temp();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not json").unwrap();

        let mut store = Store::open(path.clone());
        assert!(!path.exists());
        store.set_panel_lines(5).unwrap();

        assert_eq!(5, settings(&Store::open(path.clone())).panel_lines);
        assert_eq!(vec!["not json"], set_aside_files(&path));
    }

    #[test]
    fn newer_version_file_is_not_overwritten() {
        let (_dir, path) = temp();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let newer = r#"{"version":2,"settings":{"panel_lines":{"type":"int","value":"7"}}}"#;
        fs::write(&path, newer).unwrap();

        let mut store = Store::open(path.clone());
        assert_eq!(Values::new(), store.values);
        store.set_vad_enabled(false).unwrap();

        assert_eq!(vec![newer], set_aside_files(&path));
        assert_eq!(1, file_json(&path)["version"]);
    }

    #[test]
    fn invalid_entry_falls_back_alone() {
        let (_dir, path) = temp();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let file = json!({"version": 1, "settings": {
            "panel_lines": {"type": "int", "value": "5"},
            "vad_enabled": {"type": "string", "value": "x"},
            "prompts": {"type": "string", "value": "not json"},
        }})
        .to_string();
        fs::write(&path, &file).unwrap();

        let mut store = Store::open(path.clone());
        let loaded = settings(&store);
        assert_eq!(5, loaded.panel_lines);
        assert!(loaded.vad_enabled);
        assert_eq!(vec![default_prompt()], loaded.prompts);
        assert_eq!(vec![file], set_aside_files(&path));

        store.set_panel_lines(6).unwrap();
        let written = file_json(&path);
        assert_eq!(json!({"type": "int", "value": "6"}), written["settings"]["panel_lines"]);
        assert!(written["settings"].get("vad_enabled").is_none());
    }
}
