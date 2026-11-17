/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `BackupTest.kt`, a backup made by the Android app's code, and the export / restore orchestration.

use std::cell::RefCell;

use super::*;
use crate::settings::model::{CleanupPrompt, CustomCleanupProvider, keys, to_settings};
use crate::settings::secrets::{DEEPGRAM_API_KEY, GATEWAY_API_KEY, OPENAI_API_KEY, SONIOX_API_KEY};
use crate::settings::{Store, Value};

const FAST: u32 = 1_000;
const PASSWORD: &str = "correct horse";
const PLAIN: &[u8] = b"payload { \"a\": 1 }";

fn encrypt(bytes: &[u8], password: &str) -> Vec<u8> {
    encrypt_backup(bytes, password, FAST)
}

fn fast() -> Vec<u8> {
    encrypt(PLAIN, PASSWORD)
}

/// Expects [result] to fail with [message].
fn failure<T: fmt::Debug>(message: &'static str, result: Result<T, BackupError>) {
    assert_eq!(BackupError::Message(message), result.expect_err(message));
}

fn iterations_of(file: &[u8]) -> i32 {
    i32::from_be_bytes(file[9..13].try_into().unwrap())
}

// Crypto

#[test]
fn encrypt_then_decrypt_round_trips() {
    let file = fast();

    assert_eq!(b"IRISBKUP", &file[..8]);
    assert_eq!(1, file[8]);
    assert_eq!(FAST as i32, iterations_of(&file));
    assert_eq!(41 + PLAIN.len() + 16, file.len());
    assert_eq!(PLAIN, decrypt_backup(&file, PASSWORD).unwrap());
}

#[test]
fn default_iterations_are_600000() {
    let file = encrypt_backup(PLAIN, PASSWORD, BACKUP_KDF_ITERATIONS);

    assert_eq!(600_000, iterations_of(&file));
    assert_eq!(PLAIN, decrypt_backup(&file, PASSWORD).unwrap());
}

#[test]
fn non_ascii_password_round_trips() {
    let password = "pässwörd 🔑";

    assert_eq!(PLAIN, decrypt_backup(&encrypt(PLAIN, password), password).unwrap());
}

#[test]
fn two_backups_of_the_same_payload_differ() {
    let a = fast();
    let b = fast();

    assert_ne!(a[13..29], b[13..29]); // salt
    assert_ne!(a[29..41], b[29..41]); // nonce
    assert_ne!(a, b);
}

#[test]
fn wrong_password_fails() {
    failure(WRONG_PASSWORD_OR_DAMAGED, decrypt_backup(&fast(), "wrong"));
}

#[test]
fn edited_header_fails() {
    let mut salt = fast();
    salt[13] ^= 1;
    failure(WRONG_PASSWORD_OR_DAMAGED, decrypt_backup(&salt, PASSWORD));

    // FAST + 1: still in range, so only the AAD tag can catch it.
    let mut iterations = fast();
    iterations[12] ^= 1;
    assert_eq!(FAST as i32 + 1, iterations_of(&iterations));
    failure(WRONG_PASSWORD_OR_DAMAGED, decrypt_backup(&iterations, PASSWORD));
}

#[test]
fn edited_ciphertext_fails() {
    let mut file = fast();
    file[45] ^= 1;

    failure(WRONG_PASSWORD_OR_DAMAGED, decrypt_backup(&file, PASSWORD));
}

#[test]
fn dropped_last_byte_fails() {
    let file = fast();

    failure(WRONG_PASSWORD_OR_DAMAGED, decrypt_backup(&file[..file.len() - 1], PASSWORD));
}

#[test]
fn too_short_file_is_not_a_backup() {
    failure(NOT_A_BACKUP, decrypt_backup(&[], PASSWORD));
    failure(NOT_A_BACKUP, decrypt_backup(&encrypt(&[], PASSWORD)[..56], PASSWORD));
}

#[test]
fn wrong_magic_is_not_a_backup() {
    let mut file = fast();
    file[0] = b'X';

    failure(NOT_A_BACKUP, decrypt_backup(&file, PASSWORD));
}

#[test]
fn newer_format_version_is_rejected() {
    let mut file = fast();
    file[8] = 2;

    failure(NEWER_VERSION, decrypt_backup(&file, PASSWORD));
}

#[test]
fn zero_format_version_is_not_a_backup() {
    let mut file = fast();
    file[8] = 0;

    failure(NOT_A_BACKUP, decrypt_backup(&file, PASSWORD));
}

#[test]
fn out_of_range_iterations_are_rejected() {
    for iterations in [0, -1, i32::MAX] {
        let mut file = fast();
        file[9..13].copy_from_slice(&iterations.to_be_bytes());

        failure(WRONG_PASSWORD_OR_DAMAGED, decrypt_backup(&file, PASSWORD));
    }
}

// Mapping and validation

fn s(value: &str) -> Value {
    Value::String(value.to_string())
}

fn values<const N: usize>(entries: [(&str, Value); N]) -> Values {
    entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

fn secrets<const N: usize>(entries: [(&str, &str); N]) -> BTreeMap<String, String> {
    entries.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn json<T: Serialize>(value: &T) -> Value {
    Value::String(serde_json::to_string(value).unwrap())
}

fn prompts() -> Vec<CleanupPrompt> {
    vec![CleanupPrompt { id: "p".into(), name: "Formal".into(), system_prompt: "Make it \"formal\".".into() }]
}

fn providers() -> Vec<CustomCleanupProvider> {
    vec![CustomCleanupProvider::new("c1", "Groq", "https://x.example/v1")]
}

fn round_trip(contents: &BackupContents) -> BackupContents {
    decode_backup(&encode_backup(contents)).unwrap()
}

/// A payload with the given settings entries (raw JSON) and secrets.
fn payload(settings: &str, secrets: &str, version: i32) -> Vec<u8> {
    format!(r#"{{"version":{version},"settings":{{{settings}}},"secrets":{{{secrets}}}}}"#).into_bytes()
}

fn settings_payload(settings: &str) -> Vec<u8> {
    payload(settings, "", 1)
}

#[test]
fn contents_round_trip_all_types() {
    let values = values([
        (keys::STT_PROVIDER_ID, s("deepgram")),
        (keys::CLEANUP_REASONING_EFFORT, s("")),
        (keys::PANEL_LINES, Value::Int(5)),
        (keys::VAD_ENABLED, Value::Boolean(false)),
        (keys::BUBBLE_Y_FRACTION, Value::Float(0.42)),
        (keys::PROMPTS, json(&prompts())),
        (keys::CUSTOM_CLEANUP_PROVIDERS, json(&providers())),
    ]);
    let stored =
        secrets([(OPENAI_API_KEY, "sk-1"), (SONIOX_API_KEY, "sx-\"quoted\" ✓"), (&custom_cleanup_key("c1"), "gsk_2")]);
    let contents = BackupContents { values, secrets: stored };

    let restored = round_trip(&contents);

    assert_eq!(contents, restored);
    let settings = to_settings(&restored.values, &restored.secrets.keys().cloned().collect()).unwrap();
    assert_eq!(0.42, settings.bubble_y_fraction);
    assert_eq!(5, settings.panel_lines);
    assert_eq!(prompts(), settings.prompts);
    assert_eq!(providers(), settings.custom_cleanup_providers);
    assert_eq!(HashSet::from(["c1".to_string()]), settings.custom_cleanup_key_ids);
}

#[test]
fn missing_prefs_stay_missing() {
    let empty = round_trip(&BackupContents { values: Values::new(), secrets: BTreeMap::new() });
    assert!(empty.values.is_empty());
    assert!(empty.secrets.is_empty());

    let blank = round_trip(&BackupContents {
        values: values([(keys::CLEANUP_REASONING_EFFORT, s(""))]),
        secrets: BTreeMap::new(),
    });
    assert_eq!(None, empty.values.get(keys::CLEANUP_REASONING_EFFORT));
    assert_eq!(Some(&s("")), blank.values.get(keys::CLEANUP_REASONING_EFFORT));
    assert_eq!(Some("low"), to_settings(&empty.values, &HashSet::new()).unwrap().cleanup_reasoning_effort.as_deref());
    assert_eq!(None, to_settings(&blank.values, &HashSet::new()).unwrap().cleanup_reasoning_effort);
}

#[test]
fn unknown_pref_keys_are_kept() {
    let restored = decode_backup(&settings_payload(r#""future_setting":{"type":"int","value":"7"}"#)).unwrap();

    assert_eq!(Some(&Value::Int(7)), restored.values.get("future_setting"));
}

#[test]
fn orphaned_custom_key_is_dropped() {
    let values = values([(keys::CUSTOM_CLEANUP_PROVIDERS, json(&providers()))]);
    let stored = secrets([
        (&custom_cleanup_key("c1"), "kept"),
        (&custom_cleanup_key("gone"), "dropped"),
        (GATEWAY_API_KEY, "gw"),
    ]);

    let restored = round_trip(&BackupContents { values, secrets: stored });

    assert_eq!(secrets([(&custom_cleanup_key("c1"), "kept"), (GATEWAY_API_KEY, "gw")]), restored.secrets);
}

#[test]
fn newer_payload_version_is_rejected() {
    failure(NEWER_VERSION, decode_backup(&payload("", "", 2)));
    // Even when the newer payload no longer decodes as this version's.
    failure(NEWER_VERSION, decode_backup(br#"{"version":2,"settings":[]}"#));
}

#[test]
fn missing_section_is_rejected() {
    failure(DAMAGED, decode_backup(br#"{"version":1,"settings":{}}"#));
}

#[test]
fn full_pipeline_round_trips() {
    let contents = BackupContents {
        values: values([(keys::STT_PROVIDER_ID, s("deepgram"))]),
        secrets: secrets([(OPENAI_API_KEY, "sk-1")]),
    };

    let file = encrypt_backup(&encode_backup(&contents), PASSWORD, FAST);
    let restored = decode_backup(&decrypt_backup(&file, PASSWORD).unwrap()).unwrap();

    assert_eq!(contents, restored);
}

#[test]
fn malformed_json_is_rejected_without_leaking_it() {
    let full = payload("", r#""openai_api_key":"sk-secret-123""#, 1);
    let truncated = &full[..full.len() - 3];

    let e = decode_backup(truncated).unwrap_err();

    assert_eq!(BackupError::Message(DAMAGED), e);
    assert!(!e.to_string().contains("sk-secret-123"));
}

#[test]
fn unknown_type_is_rejected() {
    failure(DAMAGED, decode_backup(&settings_payload(r#""panel_lines":{"type":"long","value":"3"}"#)));
}

#[test]
fn unparsable_value_is_rejected() {
    failure(DAMAGED, decode_backup(&settings_payload(r#""panel_lines":{"type":"int","value":"abc"}"#)));
    failure(DAMAGED, decode_backup(&settings_payload(r#""vad_enabled":{"type":"boolean","value":"yes"}"#)));
}

#[test]
fn blank_names_are_rejected() {
    failure(DAMAGED, decode_backup(&settings_payload(r#"" ":{"type":"string","value":"x"}"#)));
    failure(DAMAGED, decode_backup(&payload("", r#""":"x""#, 1)));
}

#[test]
fn wrongly_typed_known_key_is_rejected() {
    failure(DAMAGED, decode_backup(&settings_payload(r#""panel_lines":{"type":"string","value":"3"}"#)));
}

#[test]
fn invalid_bubble_side_is_rejected() {
    failure(DAMAGED, decode_backup(&settings_payload(r#""bubble_side":{"type":"string","value":"TOP"}"#)));
}

#[test]
fn custom_words_round_trip() {
    let values = values([
        (keys::CUSTOM_WORDS_ENABLED, Value::Boolean(true)),
        (keys::CUSTOM_WORDS, json(&["Deepgram", "Jamius Siam"])),
        (keys::CORRECT_SIMILAR_WORDS, Value::Boolean(true)),
    ]);
    let contents = BackupContents { values, secrets: BTreeMap::new() };

    let restored = round_trip(&contents);

    assert_eq!(contents, restored);
    let settings = to_settings(&restored.values, &HashSet::new()).unwrap();
    assert!(settings.custom_words_enabled);
    assert_eq!(vec!["Deepgram", "Jamius Siam"], settings.custom_words);
    assert!(settings.correct_similar_words);
}

#[test]
fn malformed_custom_words_are_rejected() {
    failure(DAMAGED, decode_backup(&settings_payload(r#""custom_words":{"type":"string","value":"[1,"}"#)));
}

#[test]
fn malformed_prompts_are_rejected() {
    failure(DAMAGED, decode_backup(&settings_payload(r#""cleanup_prompts":{"type":"string","value":"[{"}"#)));
}

#[test]
fn malformed_custom_providers_are_rejected() {
    failure(
        DAMAGED,
        decode_backup(&settings_payload(r#""custom_cleanup_providers":{"type":"string","value":"{\"id\":1}"}"#)),
    );
}

// A backup made by the Android app's own `encodeBackup` / `encryptBackup` (run in a JVM unit test, 600,000
// iterations), with Android-only keys, a float, Kotlin's default-omitting provider JSON and non-ASCII secrets.

const ANDROID_FIXTURE: &[u8] = include_bytes!("android.irisbackup");
const ANDROID_FIXTURE_PASSWORD: &str = "correct horse ✓ pässwörd";

#[test]
fn android_backup_decrypts_and_decodes() {
    let contents = decode_backup(&decrypt_backup(ANDROID_FIXTURE, ANDROID_FIXTURE_PASSWORD).unwrap()).unwrap();

    let expected = values([
        (keys::STT_PROVIDER_ID, s("deepgram")),
        (keys::DEEPGRAM_STT_MODEL, s("nova-3")),
        (keys::DEEPGRAM_UPLOAD, Value::Boolean(true)),
        (keys::STT_LANGUAGE, s("de")),
        (keys::PANEL_LINES, Value::Int(5)),
        (keys::STT_SPEED_PERCENT, Value::Int(150)),
        (keys::VAD_ENABLED, Value::Boolean(false)),
        (keys::BUBBLE_SIDE, s("LEFT")),
        (keys::BUBBLE_Y_FRACTION, Value::Float(0.42)),
        (keys::ON_DEVICE_STT_MODEL, s("moonshine-base-en")),
        (keys::PROMPTS, s(r#"[{"id":"p1","name":"Formal","systemPrompt":"Make it \"formal\"."}]"#)),
        (keys::ACTIVE_PROMPT_ID, s("p1")),
        (
            keys::CUSTOM_CLEANUP_PROVIDERS,
            s(r#"[{"id":"c1","name":"Groq","baseUrl":"https://api.groq.com/openai/v1","model":"llama-3.3-70b"}]"#),
        ),
        (keys::CLEANUP_PROVIDER_ID, s("c1")),
        (keys::CLEANUP_ENABLED, Value::Boolean(true)),
        (keys::CUSTOM_WORDS, s(r#"["Deepgram","Jamius Siam"]"#)),
        (keys::CLEANUP_REASONING_EFFORT, s("")),
    ]);
    assert_eq!(expected, contents.values);
    assert_eq!(
        secrets([
            (OPENAI_API_KEY, "sk-fixture-openai"),
            (DEEPGRAM_API_KEY, "dg-fixture \"quoted\" ✓"),
            (&custom_cleanup_key("c1"), "gsk_fixture"),
        ]),
        contents.secrets
    );

    let settings = to_settings(&contents.values, &contents.secrets.keys().cloned().collect()).unwrap();
    assert_eq!("LEFT", settings.bubble_side);
    assert_eq!(0.42, settings.bubble_y_fraction);
    assert_eq!("moonshine-base-en", settings.on_device_stt_model);
    assert_eq!(Some("Formal"), settings.active_prompt.as_ref().map(|p| p.name.as_str()));
    let groq = CustomCleanupProvider {
        model: "llama-3.3-70b".into(),
        ..CustomCleanupProvider::new("c1", "Groq", "https://api.groq.com/openai/v1")
    };
    assert_eq!(vec![groq], settings.custom_cleanup_providers);
    assert_eq!("c1", settings.cleanup_provider_id);
    assert_eq!(None, settings.cleanup_reasoning_effort);
    assert!(settings.deepgram_key_set && settings.open_ai_key_set);
    assert_eq!(HashSet::from(["c1".to_string()]), settings.custom_cleanup_key_ids);
}

// Export and restore (Android `SettingsRepository.exportBackup` / `restoreBackup`, `SecretStore.replaceAll`)

/// A keychain in memory that logs every write; reading [unreadable] fails.
#[derive(Default)]
struct FakeKeychain {
    keys: RefCell<BTreeMap<String, String>>,
    log: RefCell<Vec<String>>,
    unreadable: Option<String>,
    fail_put: Option<String>,
}

impl FakeKeychain {
    fn with<const N: usize>(entries: [(&str, &str); N]) -> Self {
        FakeKeychain { keys: RefCell::new(secrets(entries)), ..Default::default() }
    }
}

impl Keychain for FakeKeychain {
    fn get(&self, name: &str) -> Result<Option<String>, SecretError> {
        if self.unreadable.as_deref() == Some(name) {
            return Err(SecretError::new("locked"));
        }
        Ok(self.keys.borrow().get(name).cloned())
    }

    fn put(&self, name: &str, value: &str) -> Result<(), SecretError> {
        self.log.borrow_mut().push(format!("put {name}"));
        if self.fail_put.as_deref() == Some(name) {
            return Err(SecretError::new("locked"));
        }
        self.keys.borrow_mut().insert(name.to_string(), value.to_string());
        Ok(())
    }

    fn clear(&self, name: &str) -> Result<(), SecretError> {
        self.log.borrow_mut().push(format!("clear {name}"));
        self.keys.borrow_mut().remove(name);
        Ok(())
    }
}

fn store() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("settings.json"));
    (dir, store)
}

#[test]
fn export_holds_the_settings_and_every_stored_key() {
    let (_dir, mut store) = store();
    store.save_custom_cleanup_provider("c1", "Groq", "https://x.example/v1").unwrap();
    store.set_shortcut_transcribe("ctrl+alt+d").unwrap();
    let keychain = FakeKeychain::with([
        (OPENAI_API_KEY, "sk-1"),
        (&custom_cleanup_key("c1"), "gsk"),
        // Not a known name for these settings: keychains can't be listed.
        (&custom_cleanup_key("gone"), "orphan"),
    ]);

    let file = export_backup(store.values(), &keychain, PASSWORD).unwrap();

    assert_eq!(600_000, iterations_of(&file));
    let contents = decode_backup(&decrypt_backup(&file, PASSWORD).unwrap()).unwrap();
    assert_eq!(store.values(), &contents.values);
    assert_eq!(secrets([(OPENAI_API_KEY, "sk-1"), (&custom_cleanup_key("c1"), "gsk")]), contents.secrets);
}

#[test]
fn export_fails_when_a_key_is_unreadable() {
    let keychain = FakeKeychain { unreadable: Some(SONIOX_API_KEY.to_string()), ..FakeKeychain::with([]) };

    failure(KEY_UNREADABLE, export_backup(&Values::new(), &keychain, PASSWORD));
}

fn backup_file() -> Vec<u8> {
    let contents = BackupContents {
        values: values([
            (keys::CUSTOM_CLEANUP_PROVIDERS, json(&providers())),
            (keys::BUBBLE_SIDE, s("LEFT")),
            (keys::PANEL_LINES, Value::Int(5)),
        ]),
        secrets: secrets([(GATEWAY_API_KEY, "gw"), (&custom_cleanup_key("c1"), "gsk")]),
    };
    encrypt_backup(&encode_backup(&contents), PASSWORD, FAST)
}

#[test]
fn restore_replaces_all_settings_and_keys() {
    let (dir, mut store) = store();
    store.save_custom_cleanup_provider("old", "Old", "https://old.example/v1").unwrap();
    store.set_shortcut_transcribe("ctrl+alt+d").unwrap();
    let keychain = FakeKeychain::with([(OPENAI_API_KEY, "sk-old"), (&custom_cleanup_key("old"), "old-key")]);
    let current = store.values().clone();

    restore_backup(&keychain, &current, &backup_file(), PASSWORD, |values| {
        keychain.log.borrow_mut().push("write settings".into());
        store.replace_all(values)
    })
    .unwrap();

    // Anything not in the backup is removed, desktop-only keys too.
    let expected = values([
        (keys::CUSTOM_CLEANUP_PROVIDERS, json(&providers())),
        (keys::BUBBLE_SIDE, s("LEFT")),
        (keys::PANEL_LINES, Value::Int(5)),
    ]);
    assert_eq!(&expected, store.values());
    assert_eq!(&expected, Store::open(dir.path().join("settings.json")).values());
    assert_eq!(secrets([(GATEWAY_API_KEY, "gw"), (&custom_cleanup_key("c1"), "gsk")]), *keychain.keys.borrow());
    // Every key known for the current settings is cleared first, then the settings, then the backup's keys.
    let mut order: Vec<String> = SecretStore::names(["old"]).iter().map(|name| format!("clear {name}")).collect();
    order.push("write settings".into());
    order.extend([format!("put {}", custom_cleanup_key("c1")), format!("put {GATEWAY_API_KEY}")]);
    assert_eq!(order, *keychain.log.borrow());
}

#[test]
fn failed_decrypt_or_decode_changes_nothing() {
    let keychain = FakeKeychain::with([(OPENAI_API_KEY, "sk-old")]);
    let mut written = false;

    failure(
        WRONG_PASSWORD_OR_DAMAGED,
        restore_backup(&keychain, &Values::new(), &backup_file(), "wrong", |_| {
            written = true;
            Ok(())
        }),
    );
    let damaged = encrypt_backup(&settings_payload(r#""panel_lines":{"type":"int","value":"x"}"#), PASSWORD, FAST);
    failure(
        DAMAGED,
        restore_backup(&keychain, &Values::new(), &damaged, PASSWORD, |_| {
            written = true;
            Ok(())
        }),
    );

    assert!(!written);
    assert!(keychain.log.borrow().is_empty());
}

#[test]
fn failed_settings_write_leaves_keyless_providers() {
    let keychain = FakeKeychain::with([(OPENAI_API_KEY, "sk-old")]);

    let result = restore_backup(&keychain, &Values::new(), &backup_file(), PASSWORD, |_| Err(io::Error::other("disk")));

    assert_eq!(Err(BackupError::Other("disk".into())), result);
    assert_eq!(RESTORE_FAILED, result.unwrap_err().user_message(RESTORE_FAILED));
    assert!(keychain.keys.borrow().is_empty());
}

#[test]
fn restore_skips_unknown_secret_names() {
    let contents = BackupContents {
        values: values([(keys::CUSTOM_CLEANUP_PROVIDERS, json(&providers()))]),
        secrets: secrets([(GATEWAY_API_KEY, "gw"), ("not_a_known_key", "x")]),
    };
    let file = encrypt_backup(&encode_backup(&contents), PASSWORD, FAST);
    let keychain = FakeKeychain::with([]);

    restore_backup(&keychain, &Values::new(), &file, PASSWORD, |_| Ok(())).unwrap();

    assert_eq!(secrets([(GATEWAY_API_KEY, "gw")]), *keychain.keys.borrow());
    assert!(!keychain.log.borrow().contains(&"put not_a_known_key".to_string()));
}

#[test]
fn restore_put_failure_leaves_the_settings_and_earlier_keys() {
    let keychain = FakeKeychain { fail_put: Some(GATEWAY_API_KEY.to_string()), ..FakeKeychain::with([]) };
    let mut written = false;

    let result = restore_backup(&keychain, &Values::new(), &backup_file(), PASSWORD, |_| {
        written = true;
        Ok(())
    });

    assert!(matches!(result, Err(BackupError::Other(_))));
    assert!(written);
    assert_eq!(secrets([(&custom_cleanup_key("c1"), "gsk")]), *keychain.keys.borrow());
}
