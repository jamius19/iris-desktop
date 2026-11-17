/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! `Settings` with defaults on read: port of Android's `Settings.kt` and `SettingsRepository.toSettings`.

use std::collections::HashSet;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use super::secrets;
use super::store::{Value, Values};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupPrompt {
    pub id: String,
    pub name: String,
    pub system_prompt: String,
}

pub const TYPE_OPENAI_COMPATIBLE: &str = "openai-compatible";

/// A user-added cleanup provider (OpenAI-compatible Chat Completions). Its key is the secret
/// [`secrets::custom_cleanup_key`] of its id.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomCleanupProvider {
    /// A UUID; also [`Settings::cleanup_provider_id`] when selected.
    pub id: String,
    #[serde(default = "openai_compatible")]
    pub r#type: String,
    pub name: String,
    /// Trimmed, no trailing '/', https; `/chat/completions` and `/models` are appended.
    pub base_url: String,
    #[serde(default)]
    pub model: String,
    /// None = model default (not sent): many servers reject `reasoning_effort`.
    pub reasoning_effort: Option<String>,
    /// Requests structured output (strict JSON schema); false = the reply is inserted as-is.
    #[serde(default = "enabled")]
    pub send_json_schema: bool,
}

impl CustomCleanupProvider {
    /// A provider with the default type and model settings.
    pub fn new(id: &str, name: &str, base_url: &str) -> Self {
        CustomCleanupProvider {
            id: id.to_string(),
            r#type: openai_compatible(),
            name: name.to_string(),
            base_url: base_url.to_string(),
            model: String::new(),
            reasoning_effort: None,
            send_json_schema: true,
        }
    }
}

fn openai_compatible() -> String {
    TYPE_OPENAI_COMPATIBLE.to_string()
}

fn enabled() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub stt_provider_id: String,
    /// The OpenAI (direct) model.
    pub stt_model: String,
    /// The Vercel AI Gateway transcription model.
    pub gateway_stt_model: String,
    /// [`Self::gateway_stt_model`] streams while recording (live text); false = uploaded after stop.
    pub gateway_stt_live: bool,
    /// The Deepgram model.
    pub deepgram_stt_model: String,
    /// Deepgram uploads the recording after stop (pre-recorded API, cheaper, no live text) instead of streaming.
    pub deepgram_upload: bool,
    /// The Soniox model: `real_time` models stream, `async` ones upload after stop.
    pub soniox_stt_model: String,
    /// The Soniox project's region id (us, eu, jp, in; unknown = us).
    pub soniox_region: String,
    /// Soniox language hints: comma-separated codes; blank means auto-detect. Soniox only.
    pub soniox_languages: String,
    /// The on-device model id; an unknown id means the default model (`stt::local::models::by_id`).
    pub on_device_stt_model: String,
    /// Minutes the on-device model stays loaded unused: -1 never unloads, 0 at once. One of
    /// [`ON_DEVICE_UNLOAD_MINUTES`]; any other stored value means the default.
    pub on_device_unload_minutes: i32,
    /// ISO-639-1 code; blank means auto-detect.
    pub stt_language: String,
    /// How long to wait for the final transcript once the audio is sent; past it the session fails.
    pub stt_timeout_seconds: i32,
    /// Send only audio around detected speech (Silero VAD).
    pub vad_enabled: bool,
    /// Speed-up of the audio sent to the STT service, in percent (100 = as recorded).
    pub stt_speed_percent: i32,
    /// Remove filler words from the final transcript.
    pub remove_filler_words: bool,
    /// Master toggle of Custom Words (see [`Self::active_custom_words`]).
    pub custom_words_enabled: bool,
    /// The Custom Words list, normalised ([`normalize_custom_words`]), in the user's order.
    pub custom_words: Vec<String>,
    /// Fix words in the final transcript that sound like a custom word.
    pub correct_similar_words: bool,
    pub cleanup_enabled: bool,
    pub cleanup_provider_id: String,
    /// The Vercel AI Gateway cleanup model.
    pub cleanup_model: String,
    /// Gateway `reasoning_effort`; None = model default (not sent).
    pub cleanup_reasoning_effort: Option<String>,
    /// Gateway provider options as typed (a JSON object, sent as `providerOptions`); blank = not sent.
    pub cleanup_provider_options: String,
    /// The OpenRouter cleanup model.
    pub open_router_cleanup_model: String,
    /// OpenRouter `reasoning_effort`; None = model default (not sent).
    pub open_router_cleanup_reasoning_effort: Option<String>,
    /// OpenRouter provider routing options as typed (a JSON object, sent as `provider`); blank = not sent.
    pub open_router_cleanup_provider_options: String,
    /// Overall cleanup deadline; past it the original transcript is inserted.
    pub cleanup_timeout_seconds: i32,
    pub prompts: Vec<CleanupPrompt>,
    /// The selected prompt, falling back to the first one if the selection no longer exists.
    pub active_prompt: Option<CleanupPrompt>,
    pub open_ai_key_set: bool,
    pub gateway_key_set: bool,
    pub deepgram_key_set: bool,
    pub soniox_key_set: bool,
    pub open_router_key_set: bool,
    /// User-added cleanup providers, in creation order.
    pub custom_cleanup_providers: Vec<CustomCleanupProvider>,
    /// Ids of [`Self::custom_cleanup_providers`] that have a stored key.
    pub custom_cleanup_key_ids: HashSet<String>,
    /// Android only (bubble edge, "LEFT" or "RIGHT"): kept, unused.
    pub bubble_side: String,
    /// Android only: kept, unused.
    pub bubble_y_fraction: f32,
    /// Lines of live text the panel shows (its last ones); notices show up to max(this, 4).
    pub panel_lines: i32,
    /// Panel's max width as a percent of the display width. Kept, unused on the desktop (`overlay_width_percent`).
    pub panel_width_percent: i32,
    /// Desktop: the overlay's screen edge, `top` or `bottom`.
    pub overlay_position: String,
    /// Desktop: the overlay's width once live text shows, as a percent of the display width.
    pub overlay_width_percent: i32,
    /// Desktop: the capture device id; "" = system default.
    pub microphone_device: String,
    /// Desktop: `auto` (GPU when it works) or `cpu`.
    pub local_accelerator: String,
    pub shortcut_transcribe: String,
    pub shortcut_cancel: String,
    /// `hold_or_toggle`, `toggle` or `push_to_talk`.
    pub shortcut_activation: String,
    /// `ctrl_v`, `direct`, `shift_insert` or `ctrl_shift_v`.
    pub paste_method: String,
    pub append_trailing_space: bool,
    pub restore_clipboard: bool,
    pub paste_delay_ms: i32,
    pub paste_delay_after_ms: i32,
    pub feedback_sounds: bool,
    pub feedback_volume_percent: i32,
    /// Desktop: mute the default output while listening (wins over [`Self::duck_while_recording`]).
    pub mute_while_recording: bool,
    /// Desktop: lower the default output by [`Self::duck_amount`] percent while listening.
    pub duck_while_recording: bool,
    pub duck_amount: i32,
    pub launch_at_startup: bool,
}

impl Settings {
    /// Speech-to-text goes through Vercel AI Gateway.
    pub fn stt_gateway(&self) -> bool {
        self.stt_provider_id == STT_PROVIDER_VERCEL_GATEWAY
    }

    pub fn stt_deepgram(&self) -> bool {
        self.stt_provider_id == STT_PROVIDER_DEEPGRAM
    }

    pub fn stt_soniox(&self) -> bool {
        self.stt_provider_id == STT_PROVIDER_SONIOX
    }

    /// Speech-to-text runs on this computer (a downloaded model).
    pub fn stt_on_device(&self) -> bool {
        self.stt_provider_id == STT_PROVIDER_ON_DEVICE
    }

    /// The selected provider's model (an unknown provider id means OpenAI).
    pub fn active_stt_model(&self) -> &str {
        if self.stt_on_device() {
            &self.on_device_stt_model
        } else if self.stt_gateway() {
            &self.gateway_stt_model
        } else if self.stt_deepgram() {
            &self.deepgram_stt_model
        } else if self.stt_soniox() {
            &self.soniox_stt_model
        } else {
            &self.stt_model
        }
    }

    /// The language setting the selected provider uses: Soniox has its own ([`Self::soniox_languages`]).
    pub fn active_stt_language(&self) -> &str {
        if self.stt_soniox() { &self.soniox_languages } else { &self.stt_language }
    }

    /// The custom words providers and the correction get: [`Self::custom_words`] while enabled, else none.
    pub fn active_custom_words(&self) -> &[String] {
        if self.custom_words_enabled { &self.custom_words } else { &[] }
    }

    /// The selected speech-to-text provider's API key is set (On device has none: always true).
    pub fn stt_key_set(&self) -> bool {
        if self.stt_on_device() {
            true
        } else if self.stt_gateway() {
            self.gateway_key_set
        } else if self.stt_deepgram() {
            self.deepgram_key_set
        } else if self.stt_soniox() {
            self.soniox_key_set
        } else {
            self.open_ai_key_set
        }
    }

    /// Cleanup goes through OpenRouter (else a custom provider or Vercel AI Gateway).
    pub fn cleanup_open_router(&self) -> bool {
        self.cleanup_provider_id == CLEANUP_PROVIDER_OPENROUTER
    }

    /// The selected custom cleanup provider, or None for a built-in one.
    pub fn active_custom_cleanup(&self) -> Option<&CustomCleanupProvider> {
        self.custom_cleanup_providers.iter().find(|p| p.id == self.cleanup_provider_id)
    }

    pub fn active_cleanup_model(&self) -> &str {
        match self.active_custom_cleanup() {
            Some(custom) => &custom.model,
            None if self.cleanup_open_router() => &self.open_router_cleanup_model,
            None => &self.cleanup_model,
        }
    }

    pub fn active_cleanup_reasoning_effort(&self) -> Option<&str> {
        match self.active_custom_cleanup() {
            Some(custom) => custom.reasoning_effort.as_deref(),
            None if self.cleanup_open_router() => self.open_router_cleanup_reasoning_effort.as_deref(),
            None => self.cleanup_reasoning_effort.as_deref(),
        }
    }

    /// Custom providers have no provider options ("").
    pub fn active_cleanup_provider_options(&self) -> &str {
        match self.active_custom_cleanup() {
            Some(_) => "",
            None if self.cleanup_open_router() => &self.open_router_cleanup_provider_options,
            None => &self.cleanup_provider_options,
        }
    }
}

pub const STT_PROVIDER_OPENAI: &str = "openai";
pub const STT_PROVIDER_VERCEL_GATEWAY: &str = "vercel-gateway";
pub const STT_PROVIDER_DEEPGRAM: &str = "deepgram";
pub const STT_PROVIDER_SONIOX: &str = "soniox";
pub const STT_PROVIDER_ON_DEVICE: &str = "on-device";
pub const CLEANUP_PROVIDER_VERCEL_GATEWAY: &str = "vercel-gateway";
pub const CLEANUP_PROVIDER_OPENROUTER: &str = "openrouter";

pub const DEFAULT_STT_MODEL: &str = "gpt-live-transcribe";
pub const DEFAULT_GATEWAY_STT_MODEL: &str = "openai/gpt-live-transcribe";
pub const DEFAULT_DEEPGRAM_STT_MODEL: &str = "nova-3";
pub const DEFAULT_SONIOX_STT_MODEL: &str = "stt-rt-v6";
pub const DEFAULT_SONIOX_REGION: &str = "us";
pub const DEFAULT_ON_DEVICE_STT_MODEL: &str = "nemotron-streaming-en";
/// Both cleanup providers (same id on OpenRouter).
pub const DEFAULT_CLEANUP_MODEL: &str = "openai/gpt-6-luna";

pub const DEFAULT_STT_TIMEOUT_SECONDS: i32 = 10;
pub const STT_TIMEOUT_RANGE: RangeInclusive<i32> = 5..=30;

pub const DEFAULT_ON_DEVICE_UNLOAD_MINUTES: i32 = 5;
/// The Unload Model options in menu order: -1 = never, 0 = immediately.
pub const ON_DEVICE_UNLOAD_MINUTES: [i32; 7] = [-1, 0, 2, 5, 10, 15, 60];

pub const DEFAULT_STT_SPEED_PERCENT: i32 = 100;
pub const STT_SPEED_PERCENT_RANGE: RangeInclusive<i32> = 100..=200;

pub const DEFAULT_REASONING_EFFORT: &str = "low";
/// The values both cleanup APIs accept, for when a model's own list is unknown (also the display order).
pub const REASONING_EFFORTS: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

pub const DEFAULT_CLEANUP_TIMEOUT_SECONDS: i32 = 5;
pub const CLEANUP_TIMEOUT_RANGE: RangeInclusive<i32> = 1..=30;

pub const DEFAULT_BUBBLE_SIDE: &str = "RIGHT";
pub const DEFAULT_BUBBLE_Y_FRACTION: f32 = 0.4;

pub const MAX_CUSTOM_WORDS: usize = 100;
pub const MAX_CUSTOM_WORD_LENGTH: usize = 60;

pub const DEFAULT_PANEL_LINES: i32 = 3;
pub const PANEL_LINES_RANGE: RangeInclusive<i32> = 1..=6;
pub const DEFAULT_PANEL_WIDTH_PERCENT: i32 = 50;
pub const PANEL_WIDTH_PERCENT_RANGE: RangeInclusive<i32> = 30..=90;

pub const DEFAULT_OVERLAY_POSITION: &str = "bottom";
/// About a seventh of the display.
pub const DEFAULT_OVERLAY_WIDTH_PERCENT: i32 = 14;
pub const OVERLAY_WIDTH_PERCENT_RANGE: RangeInclusive<i32> = 10..=30;
pub const DEFAULT_LOCAL_ACCELERATOR: &str = "auto";
#[cfg(windows)]
pub const DEFAULT_SHORTCUT_TRANSCRIBE: &str = "ctrl+space";
#[cfg(not(windows))]
pub const DEFAULT_SHORTCUT_TRANSCRIBE: &str = "ctrl+alt+space";
pub const DEFAULT_SHORTCUT_CANCEL: &str = "escape";
pub const DEFAULT_SHORTCUT_ACTIVATION: &str = "hold_or_toggle";
pub const DEFAULT_PASTE_METHOD: &str = "ctrl_v";
pub const DEFAULT_PASTE_DELAY_MS: i32 = 60;
pub const PASTE_DELAY_MS_RANGE: RangeInclusive<i32> = 0..=500;
pub const DEFAULT_FEEDBACK_VOLUME_PERCENT: i32 = 100;
pub const FEEDBACK_VOLUME_PERCENT_RANGE: RangeInclusive<i32> = 0..=100;
pub const DEFAULT_DUCK_AMOUNT: i32 = 50;
pub const DUCK_AMOUNT_RANGE: RangeInclusive<i32> = 0..=100;

pub const DEFAULT_PROMPT_ID: &str = "default";
pub const DEFAULT_PROMPT_NAME: &str = "Clean up dictation";
pub const DEFAULT_PROMPT_SYSTEM_PROMPT: &str = "You clean up dictated text. Remove filler words (um, uh, like, you \
    know), false starts and repetitions. Fix punctuation, capitalization and grammar. Keep the speaker's meaning, \
    wording, tone and language; do not translate, summarize, add content or answer questions in the text. Output only \
    the cleaned text.";

/// The seeded prompt (Android's `Settings.DEFAULT_PROMPT`).
pub fn default_prompt() -> CleanupPrompt {
    CleanupPrompt {
        id: DEFAULT_PROMPT_ID.to_string(),
        name: DEFAULT_PROMPT_NAME.to_string(),
        system_prompt: DEFAULT_PROMPT_SYSTEM_PROMPT.to_string(),
    }
}

pub const UNNAMED_PROVIDER: &str = "Unnamed Provider";

/// The stored key names (Android's preference names).
pub mod keys {
    pub const STT_PROVIDER_ID: &str = "stt_provider_id";
    pub const STT_MODEL: &str = "stt_model";
    pub const GATEWAY_STT_MODEL: &str = "gateway_stt_model";
    pub const GATEWAY_STT_LIVE: &str = "gateway_stt_live";
    pub const DEEPGRAM_STT_MODEL: &str = "deepgram_stt_model";
    pub const DEEPGRAM_UPLOAD: &str = "deepgram_upload";
    pub const SONIOX_STT_MODEL: &str = "soniox_stt_model";
    pub const SONIOX_REGION: &str = "soniox_region";
    pub const SONIOX_LANGUAGES: &str = "soniox_languages";
    pub const ON_DEVICE_STT_MODEL: &str = "on_device_stt_model";
    pub const ON_DEVICE_UNLOAD_MINUTES: &str = "on_device_unload_minutes";
    pub const STT_LANGUAGE: &str = "stt_language";
    pub const STT_TIMEOUT_SECONDS: &str = "stt_timeout_seconds";
    pub const VAD_ENABLED: &str = "vad_enabled";
    pub const STT_SPEED_PERCENT: &str = "stt_speed_percent";
    pub const REMOVE_FILLER_WORDS: &str = "remove_filler_words";
    pub const CUSTOM_WORDS_ENABLED: &str = "custom_words_enabled";
    pub const CUSTOM_WORDS: &str = "custom_words";
    pub const CORRECT_SIMILAR_WORDS: &str = "correct_similar_words";
    pub const CLEANUP_ENABLED: &str = "cleanup_enabled";
    pub const CLEANUP_PROVIDER_ID: &str = "cleanup_provider_id";
    pub const CLEANUP_MODEL: &str = "cleanup_model";
    pub const CLEANUP_REASONING_EFFORT: &str = "cleanup_reasoning_effort";
    pub const CLEANUP_PROVIDER_OPTIONS: &str = "cleanup_provider_options";
    pub const OPENROUTER_CLEANUP_MODEL: &str = "openrouter_cleanup_model";
    pub const OPENROUTER_CLEANUP_REASONING_EFFORT: &str = "openrouter_cleanup_reasoning_effort";
    pub const OPENROUTER_CLEANUP_PROVIDER_OPTIONS: &str = "openrouter_cleanup_provider_options";
    pub const CLEANUP_TIMEOUT_SECONDS: &str = "cleanup_timeout_seconds";
    pub const PROMPTS: &str = "cleanup_prompts";
    pub const ACTIVE_PROMPT_ID: &str = "cleanup_active_prompt_id";
    pub const CUSTOM_CLEANUP_PROVIDERS: &str = "custom_cleanup_providers";
    pub const BUBBLE_SIDE: &str = "bubble_side";
    pub const BUBBLE_Y_FRACTION: &str = "bubble_y_fraction";
    pub const PANEL_LINES: &str = "panel_lines";
    pub const PANEL_WIDTH_PERCENT: &str = "panel_width_percent";
    pub const OVERLAY_POSITION: &str = "overlay_position";
    pub const OVERLAY_WIDTH_PERCENT: &str = "overlay_width_percent";
    pub const MICROPHONE_DEVICE: &str = "microphone_device";
    pub const LOCAL_ACCELERATOR: &str = "local_accelerator";
    pub const SHORTCUT_TRANSCRIBE: &str = "shortcut_transcribe";
    pub const SHORTCUT_CANCEL: &str = "shortcut_cancel";
    pub const SHORTCUT_ACTIVATION: &str = "shortcut_activation";
    pub const PASTE_METHOD: &str = "paste_method";
    pub const APPEND_TRAILING_SPACE: &str = "append_trailing_space";
    pub const RESTORE_CLIPBOARD: &str = "restore_clipboard";
    pub const PASTE_DELAY_MS: &str = "paste_delay_ms";
    pub const PASTE_DELAY_AFTER_MS: &str = "paste_delay_after_ms";
    pub const FEEDBACK_SOUNDS: &str = "feedback_sounds";
    pub const FEEDBACK_VOLUME_PERCENT: &str = "feedback_volume_percent";
    pub const MUTE_WHILE_RECORDING: &str = "mute_while_recording";
    pub const DUCK_WHILE_RECORDING: &str = "duck_while_recording";
    pub const DUCK_AMOUNT: &str = "duck_amount";
    pub const LAUNCH_AT_STARTUP: &str = "launch_at_startup";
}

/// Custom words as providers may receive them: `<`, `>`, CR and LF removed, whitespace collapsed, trimmed; blanks,
/// entries over [`MAX_CUSTOM_WORD_LENGTH`] and case-insensitive duplicates (first kept) dropped; at most
/// [`MAX_CUSTOM_WORDS`].
pub fn normalize_custom_words(words: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    words
        .iter()
        .map(|word| clean_custom_word(word))
        // UTF-16 units, as Android's String.length.
        .filter(|word| !word.is_empty() && word.encode_utf16().count() <= MAX_CUSTOM_WORD_LENGTH)
        .filter(|word| seen.insert(word.to_lowercase()))
        .take(MAX_CUSTOM_WORDS)
        .collect()
}

/// Android's `[<>\r\n]` removed, then `\s+` (Java: ASCII whitespace incl. VT) as one space, then trimmed.
fn clean_custom_word(word: &str) -> String {
    let mut cleaned = String::with_capacity(word.len());
    let mut in_space = false;
    for c in word.chars().filter(|c| !matches!(c, '<' | '>' | '\r' | '\n')) {
        if matches!(c, ' ' | '\t' | '\x0B' | '\x0C') {
            if !in_space {
                cleaned.push(' ');
            }
            in_space = true;
        } else {
            cleaned.push(c);
            in_space = false;
        }
    }
    cleaned.trim().to_string()
}

pub fn stt_provider_label(id: &str) -> &'static str {
    match id {
        STT_PROVIDER_ON_DEVICE => "On device",
        STT_PROVIDER_VERCEL_GATEWAY => "Vercel AI Gateway",
        STT_PROVIDER_DEEPGRAM => "Deepgram",
        STT_PROVIDER_SONIOX => "Soniox",
        _ => "OpenAI",
    }
}

/// Built-ins by name, else the saved custom provider's name; a draft (or a blank name) is [`UNNAMED_PROVIDER`].
pub fn cleanup_provider_label(id: &str, custom: &[CustomCleanupProvider]) -> String {
    match id {
        CLEANUP_PROVIDER_VERCEL_GATEWAY => "Vercel AI Gateway".to_string(),
        CLEANUP_PROVIDER_OPENROUTER => "OpenRouter".to_string(),
        _ => custom
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.as_str())
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(UNNAMED_PROVIDER)
            .to_string(),
    }
}

/// [`Settings`] from stored [values]: defaults for missing keys. Errs (naming the key) on a wrongly typed known key or
/// malformed JSON, as Android's `toSettings` throws (backup restore relies on it to reject a damaged file).
pub fn to_settings(values: &Values, stored_secrets: &HashSet<String>) -> Result<Settings, String> {
    let prompts = prompts(values)?;
    let active_prompt_id = string(values, keys::ACTIVE_PROMPT_ID)?.unwrap_or(DEFAULT_PROMPT_ID);
    let custom_providers = custom_cleanup_providers(values)?;
    // A stored id that is neither built in nor a saved custom provider (e.g. deleted) means the Gateway.
    let cleanup_provider_id = match string(values, keys::CLEANUP_PROVIDER_ID)? {
        Some(id) if id == CLEANUP_PROVIDER_OPENROUTER || custom_providers.iter().any(|p| p.id == id) => id,
        _ => CLEANUP_PROVIDER_VERCEL_GATEWAY,
    }
    .to_string();
    let active_prompt = prompts.iter().find(|p| p.id == active_prompt_id).or(prompts.first()).cloned();
    let custom_cleanup_key_ids = custom_providers
        .iter()
        .filter(|p| stored_secrets.contains(&secrets::custom_cleanup_key(&p.id)))
        .map(|p| p.id.clone())
        .collect();
    Ok(Settings {
        stt_provider_id: string_or(values, keys::STT_PROVIDER_ID, STT_PROVIDER_ON_DEVICE)?,
        stt_model: string_or(values, keys::STT_MODEL, DEFAULT_STT_MODEL)?,
        gateway_stt_model: string_or(values, keys::GATEWAY_STT_MODEL, DEFAULT_GATEWAY_STT_MODEL)?,
        gateway_stt_live: boolean(values, keys::GATEWAY_STT_LIVE)?.unwrap_or(true),
        deepgram_stt_model: string_or(values, keys::DEEPGRAM_STT_MODEL, DEFAULT_DEEPGRAM_STT_MODEL)?,
        deepgram_upload: boolean(values, keys::DEEPGRAM_UPLOAD)?.unwrap_or(false),
        soniox_stt_model: string_or(values, keys::SONIOX_STT_MODEL, DEFAULT_SONIOX_STT_MODEL)?,
        soniox_region: string_or(values, keys::SONIOX_REGION, DEFAULT_SONIOX_REGION)?,
        soniox_languages: string_or(values, keys::SONIOX_LANGUAGES, "")?,
        on_device_stt_model: string_or(values, keys::ON_DEVICE_STT_MODEL, DEFAULT_ON_DEVICE_STT_MODEL)?,
        on_device_unload_minutes: int(values, keys::ON_DEVICE_UNLOAD_MINUTES)?
            .filter(|minutes| ON_DEVICE_UNLOAD_MINUTES.contains(minutes))
            .unwrap_or(DEFAULT_ON_DEVICE_UNLOAD_MINUTES),
        stt_language: string_or(values, keys::STT_LANGUAGE, "")?,
        stt_timeout_seconds: int(values, keys::STT_TIMEOUT_SECONDS)?.unwrap_or(DEFAULT_STT_TIMEOUT_SECONDS),
        vad_enabled: boolean(values, keys::VAD_ENABLED)?.unwrap_or(true),
        stt_speed_percent: int(values, keys::STT_SPEED_PERCENT)?.unwrap_or(DEFAULT_STT_SPEED_PERCENT),
        remove_filler_words: boolean(values, keys::REMOVE_FILLER_WORDS)?.unwrap_or(true),
        custom_words_enabled: boolean(values, keys::CUSTOM_WORDS_ENABLED)?.unwrap_or(false),
        // Normalised on read too: a restored backup may break the rules providers rely on.
        custom_words: normalize_custom_words(&custom_words(values)?),
        correct_similar_words: boolean(values, keys::CORRECT_SIMILAR_WORDS)?.unwrap_or(false),
        cleanup_enabled: boolean(values, keys::CLEANUP_ENABLED)?.unwrap_or(false),
        cleanup_provider_id,
        cleanup_model: string_or(values, keys::CLEANUP_MODEL, DEFAULT_CLEANUP_MODEL)?,
        cleanup_reasoning_effort: reasoning_effort(values, keys::CLEANUP_REASONING_EFFORT)?,
        cleanup_provider_options: string_or(values, keys::CLEANUP_PROVIDER_OPTIONS, "")?,
        open_router_cleanup_model: string_or(values, keys::OPENROUTER_CLEANUP_MODEL, DEFAULT_CLEANUP_MODEL)?,
        open_router_cleanup_reasoning_effort: reasoning_effort(values, keys::OPENROUTER_CLEANUP_REASONING_EFFORT)?,
        open_router_cleanup_provider_options: string_or(values, keys::OPENROUTER_CLEANUP_PROVIDER_OPTIONS, "")?,
        cleanup_timeout_seconds: int(values, keys::CLEANUP_TIMEOUT_SECONDS)?.unwrap_or(DEFAULT_CLEANUP_TIMEOUT_SECONDS),
        prompts,
        active_prompt,
        open_ai_key_set: stored_secrets.contains(secrets::OPENAI_API_KEY),
        gateway_key_set: stored_secrets.contains(secrets::GATEWAY_API_KEY),
        deepgram_key_set: stored_secrets.contains(secrets::DEEPGRAM_API_KEY),
        soniox_key_set: stored_secrets.contains(secrets::SONIOX_API_KEY),
        open_router_key_set: stored_secrets.contains(secrets::OPENROUTER_API_KEY),
        custom_cleanup_providers: custom_providers,
        custom_cleanup_key_ids,
        // Android's `BubbleSide.valueOf` throws on anything else.
        bubble_side: match string(values, keys::BUBBLE_SIDE)? {
            Some(side @ ("LEFT" | "RIGHT")) => side.to_string(),
            Some(_) => return Err(format!("Invalid {}", keys::BUBBLE_SIDE)),
            None => DEFAULT_BUBBLE_SIDE.to_string(),
        },
        bubble_y_fraction: float(values, keys::BUBBLE_Y_FRACTION)?.unwrap_or(DEFAULT_BUBBLE_Y_FRACTION),
        panel_lines: int(values, keys::PANEL_LINES)?.unwrap_or(DEFAULT_PANEL_LINES),
        panel_width_percent: int(values, keys::PANEL_WIDTH_PERCENT)?.unwrap_or(DEFAULT_PANEL_WIDTH_PERCENT),
        overlay_position: string_or(values, keys::OVERLAY_POSITION, DEFAULT_OVERLAY_POSITION)?,
        overlay_width_percent: int(values, keys::OVERLAY_WIDTH_PERCENT)?.unwrap_or(DEFAULT_OVERLAY_WIDTH_PERCENT),
        microphone_device: string_or(values, keys::MICROPHONE_DEVICE, "")?,
        local_accelerator: string_or(values, keys::LOCAL_ACCELERATOR, DEFAULT_LOCAL_ACCELERATOR)?,
        shortcut_transcribe: string_or(values, keys::SHORTCUT_TRANSCRIBE, DEFAULT_SHORTCUT_TRANSCRIBE)?,
        shortcut_cancel: string_or(values, keys::SHORTCUT_CANCEL, DEFAULT_SHORTCUT_CANCEL)?,
        shortcut_activation: string_or(values, keys::SHORTCUT_ACTIVATION, DEFAULT_SHORTCUT_ACTIVATION)?,
        paste_method: string_or(values, keys::PASTE_METHOD, DEFAULT_PASTE_METHOD)?,
        append_trailing_space: boolean(values, keys::APPEND_TRAILING_SPACE)?.unwrap_or(false),
        restore_clipboard: boolean(values, keys::RESTORE_CLIPBOARD)?.unwrap_or(true),
        paste_delay_ms: int(values, keys::PASTE_DELAY_MS)?.unwrap_or(DEFAULT_PASTE_DELAY_MS),
        paste_delay_after_ms: int(values, keys::PASTE_DELAY_AFTER_MS)?.unwrap_or(DEFAULT_PASTE_DELAY_MS),
        feedback_sounds: boolean(values, keys::FEEDBACK_SOUNDS)?.unwrap_or(false),
        feedback_volume_percent: int(values, keys::FEEDBACK_VOLUME_PERCENT)?.unwrap_or(DEFAULT_FEEDBACK_VOLUME_PERCENT),
        mute_while_recording: boolean(values, keys::MUTE_WHILE_RECORDING)?.unwrap_or(false),
        duck_while_recording: boolean(values, keys::DUCK_WHILE_RECORDING)?.unwrap_or(false),
        duck_amount: int(values, keys::DUCK_AMOUNT)?.unwrap_or(DEFAULT_DUCK_AMOUNT),
        launch_at_startup: boolean(values, keys::LAUNCH_AT_STARTUP)?.unwrap_or(false),
    })
}

/// Stored prompts, or the seeded default list if prompts were never saved.
pub(crate) fn prompts(values: &Values) -> Result<Vec<CleanupPrompt>, String> {
    match string(values, keys::PROMPTS)? {
        Some(encoded) => decode(keys::PROMPTS, encoded),
        None => Ok(vec![default_prompt()]),
    }
}

/// Stored custom cleanup providers; none if never saved.
pub(crate) fn custom_cleanup_providers(values: &Values) -> Result<Vec<CustomCleanupProvider>, String> {
    string(values, keys::CUSTOM_CLEANUP_PROVIDERS)?
        .map_or(Ok(Vec::new()), |e| decode(keys::CUSTOM_CLEANUP_PROVIDERS, e))
}

/// Stored custom words as saved (normalised on read by [`to_settings`]); none if never saved.
pub(crate) fn custom_words(values: &Values) -> Result<Vec<String>, String> {
    string(values, keys::CUSTOM_WORDS)?.map_or(Ok(Vec::new()), |e| decode(keys::CUSTOM_WORDS, e))
}

fn decode<T: for<'de> Deserialize<'de>>(key: &str, encoded: &str) -> Result<T, String> {
    serde_json::from_str(encoded).map_err(|e| format!("Malformed {key}: {e}"))
}

/// A stored reasoning effort: missing = the default level, "" = model default (None).
fn reasoning_effort(values: &Values, key: &str) -> Result<Option<String>, String> {
    Ok(match string(values, key)? {
        None => Some(DEFAULT_REASONING_EFFORT.to_string()),
        Some("") => None,
        Some(effort) => Some(effort.to_string()),
    })
}

fn string<'a>(values: &'a Values, key: &str) -> Result<Option<&'a str>, String> {
    match values.get(key) {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(wrong_type(key)),
    }
}

fn string_or(values: &Values, key: &str, default: &str) -> Result<String, String> {
    Ok(string(values, key)?.unwrap_or(default).to_string())
}

fn int(values: &Values, key: &str) -> Result<Option<i32>, String> {
    match values.get(key) {
        None => Ok(None),
        Some(Value::Int(i)) => Ok(Some(*i)),
        Some(_) => Err(wrong_type(key)),
    }
}

fn boolean(values: &Values, key: &str) -> Result<Option<bool>, String> {
    match values.get(key) {
        None => Ok(None),
        Some(Value::Boolean(b)) => Ok(Some(*b)),
        Some(_) => Err(wrong_type(key)),
    }
}

fn float(values: &Values, key: &str) -> Result<Option<f32>, String> {
    match values.get(key) {
        None => Ok(None),
        Some(Value::Float(f)) => Ok(Some(*f)),
        Some(_) => Err(wrong_type(key)),
    }
}

fn wrong_type(key: &str) -> String {
    format!("Wrong type for {key}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::secrets::{
        DEEPGRAM_API_KEY, GATEWAY_API_KEY, OPENAI_API_KEY, OPENROUTER_API_KEY, SONIOX_API_KEY, custom_cleanup_key,
    };

    fn s(value: &str) -> Value {
        Value::String(value.to_string())
    }

    fn prefs<const N: usize>(entries: [(&str, Value); N]) -> Values {
        entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn secrets<const N: usize>(names: [&str; N]) -> HashSet<String> {
        names.into_iter().map(str::to_string).collect()
    }

    fn settings(values: &Values) -> Settings {
        to_settings(values, &HashSet::new()).unwrap()
    }

    fn words(list: &[&str]) -> Vec<String> {
        list.iter().map(|w| w.to_string()).collect()
    }

    fn test_prompts() -> Vec<CleanupPrompt> {
        vec![
            CleanupPrompt {
                id: "a".into(),
                name: "Formal".into(),
                system_prompt: "Make it \"formal\".\nKeep émojis 👍".into(),
            },
            CleanupPrompt { id: "b".into(), name: "Casual".into(), system_prompt: "Make it casual.".into() },
        ]
    }

    fn custom_providers() -> Vec<CustomCleanupProvider> {
        vec![
            CustomCleanupProvider {
                model: "llama-4".into(),
                reasoning_effort: Some("high".into()),
                send_json_schema: false,
                ..CustomCleanupProvider::new("c1", "Groq", "https://api.groq.com/openai/v1")
            },
            CustomCleanupProvider::new("c2", "Local", "https://llm.example.com/v1"),
        ]
    }

    fn encode<T: Serialize>(value: &T) -> Value {
        Value::String(serde_json::to_string(value).unwrap())
    }

    #[test]
    fn prompts_round_trip() {
        let encoded = serde_json::to_string(&test_prompts()).unwrap();
        assert_eq!(test_prompts(), decode::<Vec<CleanupPrompt>>("p", &encoded).unwrap());
    }

    #[test]
    fn empty_prompt_list_round_trip() {
        let encoded = serde_json::to_string(&Vec::<CleanupPrompt>::new()).unwrap();
        assert_eq!(Vec::<CleanupPrompt>::new(), decode::<Vec<CleanupPrompt>>("p", &encoded).unwrap());
    }

    #[test]
    fn decode_ignores_unknown_fields() {
        let decoded: Vec<CleanupPrompt> =
            decode("p", r#"[{"id":"a","name":"n","systemPrompt":"s","extra":1}]"#).unwrap();
        let expected = CleanupPrompt { id: "a".into(), name: "n".into(), system_prompt: "s".into() };
        assert_eq!(vec![expected], decoded);
    }

    #[test]
    fn custom_providers_round_trip() {
        let encoded = serde_json::to_string(&custom_providers()).unwrap();
        assert_eq!(custom_providers(), decode::<Vec<CustomCleanupProvider>>("c", &encoded).unwrap());
    }

    #[test]
    fn custom_provider_decode_ignores_unknown_keys_and_fills_defaults() {
        let decoded: Vec<CustomCleanupProvider> =
            decode("c", r#"[{"id":"c","name":"n","baseUrl":"https://x.example","extra":1}]"#).unwrap();

        assert_eq!(vec![CustomCleanupProvider::new("c", "n", "https://x.example")], decoded);
        assert_eq!(TYPE_OPENAI_COMPATIBLE, decoded[0].r#type);
        assert_eq!("", decoded[0].model);
        assert_eq!(None, decoded[0].reasoning_effort);
        assert!(decoded[0].send_json_schema);
    }

    #[test]
    fn empty_preferences_map_to_defaults() {
        let settings = settings(&Values::new());

        assert_eq!(STT_PROVIDER_ON_DEVICE, settings.stt_provider_id);
        assert!(settings.stt_on_device());
        assert_eq!("nemotron-streaming-en", settings.on_device_stt_model);
        assert_eq!(5, settings.on_device_unload_minutes);
        assert_eq!(DEFAULT_STT_MODEL, settings.stt_model);
        assert_eq!(DEFAULT_GATEWAY_STT_MODEL, settings.gateway_stt_model);
        assert!(settings.gateway_stt_live);
        assert_eq!(DEFAULT_DEEPGRAM_STT_MODEL, settings.deepgram_stt_model);
        assert_eq!("nova-3", settings.deepgram_stt_model);
        assert!(!settings.deepgram_upload);
        assert_eq!(DEFAULT_SONIOX_STT_MODEL, settings.soniox_stt_model);
        assert_eq!("stt-rt-v6", settings.soniox_stt_model);
        assert_eq!(DEFAULT_SONIOX_REGION, settings.soniox_region);
        assert_eq!("us", settings.soniox_region);
        assert_eq!("", settings.soniox_languages);
        assert!(!settings.stt_gateway());
        assert!(!settings.stt_deepgram());
        assert!(!settings.stt_soniox());
        assert_eq!(DEFAULT_ON_DEVICE_STT_MODEL, settings.active_stt_model());
        assert_eq!("", settings.stt_language);
        assert_eq!(DEFAULT_STT_TIMEOUT_SECONDS, settings.stt_timeout_seconds);
        assert!(settings.vad_enabled);
        assert_eq!(DEFAULT_STT_SPEED_PERCENT, settings.stt_speed_percent);
        assert!(settings.remove_filler_words);
        assert!(!settings.cleanup_enabled);
        assert_eq!(CLEANUP_PROVIDER_VERCEL_GATEWAY, settings.cleanup_provider_id);
        assert_eq!(DEFAULT_CLEANUP_MODEL, settings.cleanup_model);
        assert_eq!(Some(DEFAULT_REASONING_EFFORT), settings.cleanup_reasoning_effort.as_deref());
        assert_eq!("", settings.cleanup_provider_options);
        assert!(!settings.cleanup_open_router());
        assert_eq!(DEFAULT_CLEANUP_MODEL, settings.open_router_cleanup_model);
        assert_eq!(Some("low"), settings.open_router_cleanup_reasoning_effort.as_deref());
        assert_eq!("", settings.open_router_cleanup_provider_options);
        assert_eq!(DEFAULT_CLEANUP_TIMEOUT_SECONDS, settings.cleanup_timeout_seconds);
        assert_eq!(vec![default_prompt()], settings.prompts);
        assert_eq!(Some(default_prompt()), settings.active_prompt);
        assert!(!settings.open_ai_key_set);
        assert!(!settings.gateway_key_set);
        assert!(!settings.deepgram_key_set);
        assert!(!settings.soniox_key_set);
        assert!(!settings.open_router_key_set);
        assert_eq!(Vec::<CustomCleanupProvider>::new(), settings.custom_cleanup_providers);
        assert_eq!(HashSet::<String>::new(), settings.custom_cleanup_key_ids);
        assert_eq!(None, settings.active_custom_cleanup());
        assert_eq!(DEFAULT_BUBBLE_SIDE, settings.bubble_side);
        assert_eq!(DEFAULT_BUBBLE_Y_FRACTION, settings.bubble_y_fraction);
        assert_eq!(DEFAULT_PANEL_LINES, settings.panel_lines);
        assert_eq!(DEFAULT_PANEL_WIDTH_PERCENT, settings.panel_width_percent);
    }

    /// Desktop keys (no Android test): defaults on read.
    #[test]
    fn empty_preferences_map_to_desktop_defaults() {
        let settings = settings(&Values::new());

        assert_eq!("bottom", settings.overlay_position);
        assert_eq!(14, settings.overlay_width_percent);
        assert_eq!("", settings.microphone_device);
        assert_eq!("auto", settings.local_accelerator);
        assert_eq!(if cfg!(windows) { "ctrl+space" } else { "ctrl+alt+space" }, settings.shortcut_transcribe);
        assert_eq!("escape", settings.shortcut_cancel);
        assert_eq!("hold_or_toggle", settings.shortcut_activation);
        assert_eq!("ctrl_v", settings.paste_method);
        assert!(!settings.append_trailing_space);
        assert!(settings.restore_clipboard);
        assert_eq!(60, settings.paste_delay_ms);
        assert_eq!(60, settings.paste_delay_after_ms);
        assert!(!settings.feedback_sounds);
        assert_eq!(100, settings.feedback_volume_percent);
        assert!(!settings.mute_while_recording);
        assert!(!settings.duck_while_recording);
        assert_eq!(50, settings.duck_amount);
        assert!(!settings.launch_at_startup);
    }

    #[test]
    fn stored_values_are_mapped() {
        let values = prefs([
            (keys::STT_PROVIDER_ID, s(STT_PROVIDER_VERCEL_GATEWAY)),
            (keys::STT_MODEL, s("gpt-transcribe")),
            (keys::GATEWAY_STT_MODEL, s("google/gemini-3.5-transcribe-live")),
            (keys::GATEWAY_STT_LIVE, Value::Boolean(false)),
            (keys::DEEPGRAM_STT_MODEL, s("nova-3-medical")),
            (keys::DEEPGRAM_UPLOAD, Value::Boolean(true)),
            (keys::SONIOX_STT_MODEL, s("stt-async-v6")),
            (keys::SONIOX_REGION, s("eu")),
            (keys::SONIOX_LANGUAGES, s("en, bn")),
            (keys::ON_DEVICE_UNLOAD_MINUTES, Value::Int(-1)),
            (keys::STT_LANGUAGE, s("fr")),
            (keys::STT_TIMEOUT_SECONDS, Value::Int(20)),
            (keys::VAD_ENABLED, Value::Boolean(false)),
            (keys::STT_SPEED_PERCENT, Value::Int(150)),
            (keys::REMOVE_FILLER_WORDS, Value::Boolean(false)),
            (keys::CLEANUP_ENABLED, Value::Boolean(true)),
            (keys::CLEANUP_MODEL, s("google/gemini-2.5-flash")),
            (keys::CLEANUP_PROVIDER_ID, s(CLEANUP_PROVIDER_OPENROUTER)),
            (keys::CLEANUP_REASONING_EFFORT, s("high")),
            (keys::CLEANUP_PROVIDER_OPTIONS, s(r#"{"gateway": {"order": ["azure"]}}"#)),
            (keys::OPENROUTER_CLEANUP_MODEL, s("anthropic/claude-haiku-5")),
            (keys::OPENROUTER_CLEANUP_REASONING_EFFORT, s("medium")),
            (keys::OPENROUTER_CLEANUP_PROVIDER_OPTIONS, s(r#"{"order": ["openai"]}"#)),
            (keys::CLEANUP_TIMEOUT_SECONDS, Value::Int(12)),
            (keys::PROMPTS, encode(&test_prompts())),
            (keys::ACTIVE_PROMPT_ID, s("b")),
            (keys::BUBBLE_SIDE, s("LEFT")),
            (keys::BUBBLE_Y_FRACTION, Value::Float(0.75)),
            (keys::PANEL_LINES, Value::Int(5)),
            (keys::PANEL_WIDTH_PERCENT, Value::Int(70)),
            (keys::CUSTOM_CLEANUP_PROVIDERS, encode(&custom_providers())),
        ]);

        let settings = to_settings(
            &values,
            &secrets([
                GATEWAY_API_KEY,
                DEEPGRAM_API_KEY,
                SONIOX_API_KEY,
                OPENROUTER_API_KEY,
                &custom_cleanup_key("c2"),
                &custom_cleanup_key("deleted"),
            ]),
        )
        .unwrap();

        assert_eq!(STT_PROVIDER_VERCEL_GATEWAY, settings.stt_provider_id);
        assert_eq!("gpt-transcribe", settings.stt_model);
        assert_eq!("google/gemini-3.5-transcribe-live", settings.gateway_stt_model);
        assert!(!settings.gateway_stt_live);
        assert_eq!("nova-3-medical", settings.deepgram_stt_model);
        assert!(settings.deepgram_upload);
        assert_eq!("stt-async-v6", settings.soniox_stt_model);
        assert_eq!("eu", settings.soniox_region);
        assert_eq!("en, bn", settings.soniox_languages);
        assert_eq!(-1, settings.on_device_unload_minutes);
        assert_eq!("fr", settings.stt_language);
        assert_eq!(20, settings.stt_timeout_seconds);
        assert!(!settings.vad_enabled);
        assert_eq!(150, settings.stt_speed_percent);
        assert!(!settings.remove_filler_words);
        assert!(settings.cleanup_enabled);
        assert_eq!("google/gemini-2.5-flash", settings.cleanup_model);
        assert_eq!(Some("high"), settings.cleanup_reasoning_effort.as_deref());
        assert_eq!(CLEANUP_PROVIDER_OPENROUTER, settings.cleanup_provider_id);
        assert_eq!(r#"{"gateway": {"order": ["azure"]}}"#, settings.cleanup_provider_options);
        assert_eq!("anthropic/claude-haiku-5", settings.open_router_cleanup_model);
        assert_eq!(Some("medium"), settings.open_router_cleanup_reasoning_effort.as_deref());
        assert_eq!(r#"{"order": ["openai"]}"#, settings.open_router_cleanup_provider_options);
        assert_eq!(12, settings.cleanup_timeout_seconds);
        assert_eq!(test_prompts(), settings.prompts);
        assert_eq!(Some(test_prompts()[1].clone()), settings.active_prompt);
        assert!(!settings.open_ai_key_set);
        assert!(settings.gateway_key_set);
        assert!(settings.deepgram_key_set);
        assert!(settings.soniox_key_set);
        assert!(settings.open_router_key_set);
        assert_eq!(custom_providers(), settings.custom_cleanup_providers);
        assert_eq!(secrets(["c2"]), settings.custom_cleanup_key_ids);
        assert_eq!("LEFT", settings.bubble_side);
        assert_eq!(0.75, settings.bubble_y_fraction);
        assert_eq!(5, settings.panel_lines);
        assert_eq!(70, settings.panel_width_percent);
    }

    #[test]
    fn unload_minutes_outside_the_options_mean_the_default() {
        let settings = settings(&prefs([(keys::ON_DEVICE_UNLOAD_MINUTES, Value::Int(7))]));

        assert_eq!(DEFAULT_ON_DEVICE_UNLOAD_MINUTES, settings.on_device_unload_minutes);
    }

    #[test]
    fn active_stt_model_and_key_follow_the_provider() {
        let mut values = prefs([
            (keys::STT_PROVIDER_ID, s(STT_PROVIDER_OPENAI)),
            (keys::STT_MODEL, s("gpt-transcribe")),
            (keys::GATEWAY_STT_MODEL, s("spacexai/grok-stt")),
            (keys::DEEPGRAM_STT_MODEL, s("nova-2")),
            (keys::SONIOX_STT_MODEL, s("stt-async-v6")),
            (keys::STT_LANGUAGE, s("fr")),
            (keys::SONIOX_LANGUAGES, s("en, bn")),
        ]);
        let with = |values: &Values, names: &HashSet<String>| to_settings(values, names).unwrap();

        let open_ai = with(&values, &secrets([OPENAI_API_KEY]));
        assert!(!open_ai.stt_gateway());
        assert_eq!("gpt-transcribe", open_ai.active_stt_model());
        assert!(open_ai.stt_key_set());
        assert_eq!("fr", open_ai.active_stt_language());

        values.insert(keys::STT_PROVIDER_ID.into(), s(STT_PROVIDER_VERCEL_GATEWAY));
        let gateway = with(&values, &secrets([OPENAI_API_KEY]));
        assert!(gateway.stt_gateway());
        assert_eq!("spacexai/grok-stt", gateway.active_stt_model());
        assert!(!gateway.stt_key_set());
        assert!(with(&values, &secrets([GATEWAY_API_KEY])).stt_key_set());

        values.insert(keys::STT_PROVIDER_ID.into(), s(STT_PROVIDER_DEEPGRAM));
        let deepgram = with(&values, &secrets([OPENAI_API_KEY, GATEWAY_API_KEY]));
        assert!(deepgram.stt_deepgram());
        assert!(!deepgram.stt_gateway());
        assert_eq!("nova-2", deepgram.active_stt_model());
        assert!(!deepgram.stt_key_set());
        assert!(with(&values, &secrets([DEEPGRAM_API_KEY])).stt_key_set());
        assert_eq!("fr", deepgram.active_stt_language());

        values.insert(keys::STT_PROVIDER_ID.into(), s(STT_PROVIDER_SONIOX));
        let soniox = with(&values, &secrets([OPENAI_API_KEY, DEEPGRAM_API_KEY]));
        assert!(soniox.stt_soniox());
        assert!(!soniox.stt_deepgram());
        assert_eq!("stt-async-v6", soniox.active_stt_model());
        assert!(!soniox.stt_key_set());
        assert!(with(&values, &secrets([SONIOX_API_KEY])).stt_key_set());
        assert_eq!("en, bn", soniox.active_stt_language());

        values.insert(keys::STT_PROVIDER_ID.into(), s(STT_PROVIDER_ON_DEVICE));
        let on_device = settings(&values);
        assert!(on_device.stt_on_device());
        assert!(!on_device.stt_soniox());
        assert_eq!(DEFAULT_ON_DEVICE_STT_MODEL, on_device.active_stt_model());
        assert!(on_device.stt_key_set()); // No key.
        assert_eq!("fr", on_device.active_stt_language());
        values.insert(keys::ON_DEVICE_STT_MODEL.into(), s("whisper-small"));
        assert_eq!("whisper-small", settings(&values).active_stt_model());

        values.insert(keys::STT_PROVIDER_ID.into(), s("unknown"));
        let unknown = with(&values, &secrets([OPENAI_API_KEY]));
        assert_eq!("gpt-transcribe", unknown.active_stt_model());
        assert!(unknown.stt_key_set());
    }

    #[test]
    fn active_cleanup_fields_follow_the_provider() {
        let mut values = prefs([
            (keys::CLEANUP_MODEL, s("openai/gpt-6-luna")),
            (keys::CLEANUP_REASONING_EFFORT, s("high")),
            (keys::CLEANUP_PROVIDER_OPTIONS, s(r#"{"gateway":{}}"#)),
            (keys::OPENROUTER_CLEANUP_MODEL, s("google/gemini-3-flash")),
            (keys::OPENROUTER_CLEANUP_REASONING_EFFORT, s("")),
            (keys::OPENROUTER_CLEANUP_PROVIDER_OPTIONS, s(r#"{"order":["google"]}"#)),
        ]);

        let gateway = settings(&values);
        assert!(!gateway.cleanup_open_router());
        assert_eq!("openai/gpt-6-luna", gateway.active_cleanup_model());
        assert_eq!(Some("high"), gateway.active_cleanup_reasoning_effort());
        assert_eq!(r#"{"gateway":{}}"#, gateway.active_cleanup_provider_options());

        values.insert(keys::CLEANUP_PROVIDER_ID.into(), s(CLEANUP_PROVIDER_OPENROUTER));
        let open_router = settings(&values);
        assert!(open_router.cleanup_open_router());
        assert_eq!("google/gemini-3-flash", open_router.active_cleanup_model());
        assert_eq!(None, open_router.active_cleanup_reasoning_effort());
        assert_eq!(r#"{"order":["google"]}"#, open_router.active_cleanup_provider_options());

        values.insert(keys::CLEANUP_PROVIDER_ID.into(), s("unknown"));
        let unknown = settings(&values);
        assert_eq!(CLEANUP_PROVIDER_VERCEL_GATEWAY, unknown.cleanup_provider_id);
        assert!(!unknown.cleanup_open_router());
        assert_eq!("openai/gpt-6-luna", unknown.active_cleanup_model());
        assert_eq!(Some("high"), unknown.active_cleanup_reasoning_effort());
        assert_eq!(r#"{"gateway":{}}"#, unknown.active_cleanup_provider_options());

        values.insert(keys::CUSTOM_CLEANUP_PROVIDERS.into(), encode(&custom_providers()));
        values.insert(keys::CLEANUP_PROVIDER_ID.into(), s("c1"));
        let custom = settings(&values);
        assert_eq!("c1", custom.cleanup_provider_id);
        assert_eq!(Some(&custom_providers()[0]), custom.active_custom_cleanup());
        assert!(!custom.cleanup_open_router());
        assert_eq!("llama-4", custom.active_cleanup_model());
        assert_eq!(Some("high"), custom.active_cleanup_reasoning_effort());
        assert_eq!("", custom.active_cleanup_provider_options());

        values.insert(keys::CLEANUP_PROVIDER_ID.into(), s("c2"));
        let defaults = settings(&values);
        assert_eq!("", defaults.active_cleanup_model());
        assert_eq!(None, defaults.active_cleanup_reasoning_effort());
    }

    #[test]
    fn dangling_custom_provider_id_means_the_gateway() {
        let settings = settings(&prefs([
            (keys::CUSTOM_CLEANUP_PROVIDERS, encode(&custom_providers())),
            (keys::CLEANUP_PROVIDER_ID, s("deleted")),
            (keys::CLEANUP_MODEL, s("openai/gpt-6-luna")),
            (keys::CLEANUP_PROVIDER_OPTIONS, s(r#"{"gateway":{}}"#)),
        ]));

        assert_eq!(CLEANUP_PROVIDER_VERCEL_GATEWAY, settings.cleanup_provider_id);
        assert_eq!(None, settings.active_custom_cleanup());
        assert_eq!("openai/gpt-6-luna", settings.active_cleanup_model());
        assert_eq!(Some("low"), settings.active_cleanup_reasoning_effort());
        assert_eq!(r#"{"gateway":{}}"#, settings.active_cleanup_provider_options());
    }

    #[test]
    fn empty_reasoning_effort_is_model_default() {
        let settings = settings(&prefs([
            (keys::CLEANUP_REASONING_EFFORT, s("")),
            (keys::OPENROUTER_CLEANUP_REASONING_EFFORT, s("")),
        ]));

        assert_eq!(None, settings.cleanup_reasoning_effort);
        assert_eq!(None, settings.open_router_cleanup_reasoning_effort);
    }

    #[test]
    fn saved_empty_prompt_list_is_not_reseeded() {
        let settings = settings(&prefs([(keys::PROMPTS, s("[]"))]));

        assert_eq!(Vec::<CleanupPrompt>::new(), settings.prompts);
        assert_eq!(None, settings.active_prompt);
    }

    #[test]
    fn missing_active_prompt_falls_back_to_first() {
        let values = prefs([(keys::PROMPTS, encode(&test_prompts())), (keys::ACTIVE_PROMPT_ID, s("deleted"))]);

        assert_eq!(Some(test_prompts()[0].clone()), settings(&values).active_prompt);
    }

    #[test]
    fn custom_words_defaults() {
        let settings = settings(&Values::new());

        assert!(!settings.custom_words_enabled);
        assert_eq!(Vec::<String>::new(), settings.custom_words);
        assert!(!settings.correct_similar_words);
    }

    #[test]
    fn stored_custom_words_are_mapped() {
        let settings = settings(&prefs([
            (keys::CUSTOM_WORDS_ENABLED, Value::Boolean(true)),
            (keys::CUSTOM_WORDS, encode(&words(&["Deepgram", "Jamius Siam"]))),
            (keys::CORRECT_SIMILAR_WORDS, Value::Boolean(true)),
        ]));

        assert!(settings.custom_words_enabled);
        assert_eq!(words(&["Deepgram", "Jamius Siam"]), settings.custom_words);
        assert!(settings.correct_similar_words);
    }

    #[test]
    fn normalize_custom_words_cleans_the_list() {
        let x61 = "x".repeat(61);
        let input = words(&[" Deepgram ", "", "   ", "deepgram", "<Soniox>", "a\r\nb", "Jamius \t  Siam", &x61]);

        assert_eq!(words(&["Deepgram", "Soniox", "ab", "Jamius Siam"]), normalize_custom_words(&input));
        assert_eq!(vec!["x".repeat(60)], normalize_custom_words(&["x".repeat(60)]));
        let w = |range: std::ops::RangeInclusive<i32>| range.map(|i| format!("w{i}")).collect::<Vec<_>>();
        assert_eq!(w(1..=100), normalize_custom_words(&w(1..=101)));
    }

    #[test]
    fn stored_custom_words_are_normalised_on_read() {
        let mut stored = words(&[" <a> ", "A", &"y".repeat(61)]);
        stored.extend((1..=120).map(|i| format!("w{i}")));
        let words = settings(&prefs([(keys::CUSTOM_WORDS, encode(&stored))])).custom_words;

        assert_eq!(100, words.len());
        assert_eq!(["a", "w1"], words[..2]);
    }

    #[test]
    fn malformed_custom_words_throw() {
        assert!(to_settings(&prefs([(keys::CUSTOM_WORDS, s("not json"))]), &HashSet::new()).is_err());
    }

    /// Desktop (Android throws ClassCastException): a known key of the wrong type is an error.
    #[test]
    fn wrongly_typed_known_key_is_an_error() {
        assert!(to_settings(&prefs([(keys::PANEL_LINES, s("3"))]), &HashSet::new()).is_err());
    }

    #[test]
    fn invalid_bubble_side_is_an_error() {
        assert!(to_settings(&prefs([(keys::BUBBLE_SIDE, s("TOP"))]), &HashSet::new()).is_err());
    }

    #[test]
    fn active_custom_words_need_the_toggle() {
        let words = words(&["Deepgram"]);
        let settings = Settings { custom_words: words.clone(), ..settings(&Values::new()) };

        assert!(settings.active_custom_words().is_empty());
        assert_eq!(words, Settings { custom_words_enabled: true, ..settings }.active_custom_words());
    }
}
