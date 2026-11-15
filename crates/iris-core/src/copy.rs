/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! UI copy helpers (Android's `SettingsText.kt`, with desktop wording) and the save-failure messages.

use crate::cleanup::options::parse_provider_options;
use crate::settings::{
    CLEANUP_PROVIDER_OPENROUTER, CLEANUP_PROVIDER_VERCEL_GATEWAY, MAX_CUSTOM_WORD_LENGTH, MAX_CUSTOM_WORDS,
    STT_PROVIDER_DEEPGRAM, STT_PROVIDER_ON_DEVICE, STT_PROVIDER_OPENAI, STT_PROVIDER_SONIOX,
    STT_PROVIDER_VERCEL_GATEWAY, Settings,
};
use crate::stt::local::models::OnDeviceModel;
use crate::stt::local::stats::{SpeedVerdict, round_to_tenth};

/// A failed settings write, in the control's helper slot.
pub const SAVE_ERROR: &str = "Couldn't save. Try again.";
pub const API_KEY_SAVE_ERROR: &str = "Couldn't save the API key. Try again.";
pub const API_KEY_DELETE_ERROR: &str = "Couldn't delete the API key. Try again.";

pub fn reasoning_effort_label(effort: Option<&str>) -> String {
    match effort {
        None => "Model default".to_string(),
        Some("xhigh") => "Extra high".to_string(),
        Some(effort) => {
            let mut chars = effort.chars();
            chars.next().map(|first| first.to_uppercase().chain(chars).collect()).unwrap_or_default()
        }
    }
}

/// The transcription providers in menu order; an unknown stored id shows as OpenAI.
pub const STT_PROVIDERS: [&str; 5] = [
    STT_PROVIDER_ON_DEVICE,
    STT_PROVIDER_OPENAI,
    STT_PROVIDER_VERCEL_GATEWAY,
    STT_PROVIDER_DEEPGRAM,
    STT_PROVIDER_SONIOX,
];

/// The built-in cleanup providers in menu order (the custom ones follow).
pub const CLEANUP_PROVIDERS: [&str; 2] = [CLEANUP_PROVIDER_VERCEL_GATEWAY, CLEANUP_PROVIDER_OPENROUTER];

/// "1.0x" .. "2.0x".
pub fn speed_text(percent: i32) -> String {
    // Debug keeps the ".0" Kotlin's Float.toString writes.
    format!("{:?}x", percent as f32 / 100.0)
}

pub fn seconds_text(seconds: i32) -> String {
    format!("{seconds} s")
}

/// Whole megabytes (10^6 bytes, rounded): "663 MB".
pub fn megabytes_text(bytes: i64) -> String {
    format!("{} MB", (bytes + 500_000) / 1_000_000)
}

/// A download's progress: "120 / 663 MB".
pub fn download_progress_text(done_bytes: i64, total_bytes: i64) -> String {
    format!("{} / {}", (done_bytes + 500_000) / 1_000_000, megabytes_text(total_bytes))
}

/// The helper under the on-device model menu.
pub fn on_device_model_helper(model: &OnDeviceModel) -> String {
    if model.streams {
        "This streaming model only supports English.".to_string()
    } else {
        format!("{}. Runs offline on this computer.", model.languages)
    }
}

/// An Unload Model option ([`crate::settings::ON_DEVICE_UNLOAD_MINUTES`]).
pub fn on_device_unload_label(minutes: i32) -> String {
    match minutes {
        -1 => "Never".to_string(),
        0 => "Immediately".to_string(),
        60 => "After 1 hour".to_string(),
        _ => format!("After {minutes} minutes"),
    }
}

/// The Unload Model helper's first line.
pub const UNLOAD_HELPER: &str = "Frees the model's memory if you haven't dictated for this long.";

/// The RAM guide under [`UNLOAD_HELPER`]: (label in SemiBold, advice) per line.
pub const UNLOAD_RAM_GUIDE: [(&str, &str); 3] = [
    ("16 GB+ RAM:", " Never or 15 minutes+."),
    ("8 GB:", " 5–10 minutes."),
    ("4 GB or less:", " Immediately or 2 minutes."),
];

/// The Skip Silence switch's subtitle; [on_device_model] is the selected one with On device, else None.
pub fn skip_silence_subtitle(on_device_model: Option<&OnDeviceModel>) -> &'static str {
    match on_device_model {
        None => "Detects speech on-device and sends only that, to cut cost.",
        Some(model) if model.streams => "Detects speech on-device and transcribes only that.",
        Some(_) => "Detects speech on-device and transcribes each phrase as you pause.",
    }
}

pub fn percent_text(percent: i32) -> String {
    format!("{percent}%")
}

/// An on-device model's speed against real time, rounded to one decimal: "7.1× faster than you speak".
pub fn speed_factor_text(factor: f64) -> String {
    if one_decimal(factor) == "1.0" {
        "As fast as you speak".to_string()
    } else if factor > 1.0 {
        format!("{}× faster than you speak", one_decimal(factor))
    } else {
        format!("{}× slower than you speak", one_decimal(1.0 / factor))
    }
}

/// Debug, like Kotlin's Double.toString: "2.0", not "2".
fn one_decimal(x: f64) -> String {
    format!("{:?}", round_to_tenth(x))
}

pub fn verdict_label(verdict: SpeedVerdict) -> &'static str {
    match verdict {
        SpeedVerdict::KeepsUp => "Keeps up",
        SpeedVerdict::Borderline => "Borderline",
        SpeedVerdict::FallsBehind => "Falls behind",
    }
}

/// Milliseconds as seconds, rounded to tenths: "4.8 s".
pub fn tenths_text(ms: i64) -> String {
    let tenths = (ms + 50) / 100;
    format!("{}.{} s", tenths / 10, tenths % 10)
}

/// How many dictations (or cleanups: [noun], Android's default "dictation") a section's statistics come from.
pub fn stats_basis_text(count: i32, noun: &str) -> String {
    if count == 1 { format!("From your last {noun}.") } else { format!("From your last {count} {noun}s.") }
}

/// Milliseconds as minutes, rounded to tenths: "12.4 min".
pub fn minutes_text(ms: i64) -> String {
    let tenths = (ms + 3_000) / 6_000;
    format!("{}.{} min", tenths / 10, tenths % 10)
}

/// How much less than [recorded_ms] was billed, in percent, floored: 0 only when nothing was saved (or no recording),
/// 100 only when nothing was billed, else 1..99.
pub fn saved_percent(recorded_ms: i64, billed_ms: i64) -> i32 {
    if recorded_ms <= 0 || billed_ms >= recorded_ms {
        return 0;
    }
    if billed_ms <= 0 {
        return 100;
    }
    ((recorded_ms - billed_ms) * 100 / recorded_ms).clamp(1, 99) as i32
}

/// The Audio Sent row's subtitle.
pub fn audio_saved_text(percent: i32) -> String {
    if percent > 0 {
        format!("{percent}% less than recorded.")
    } else {
        "Same as recorded duration. Nothing saved.".to_string()
    }
}

/// A duration, floored: "35 s", "47 min", "3 h 12 min", "2 h"; a negative one is "0 s".
pub fn duration_text(ms: i64) -> String {
    let seconds = ms.max(0) / 1_000;
    if seconds < 60 {
        return format!("{seconds} s");
    }
    let minutes = seconds / 60;
    if minutes < 60 {
        return format!("{minutes} min");
    }
    let hours = minutes / 60;
    if minutes % 60 == 0 { format!("{hours} h") } else { format!("{hours} h {} min", minutes % 60) }
}

/// The API key field's helper; [shared_with] names the other page using the same key (the Gateway key).
pub fn api_key_helper(is_set: bool, shared_with: Option<&str>) -> String {
    let shared = shared_with.map(|page| format!(" (shared with {page})")).unwrap_or_default();
    if is_set { format!("API key saved{shared}. Enter a new one to replace it.") } else { format!("Not set{shared}") }
}

const NEW_PROMPT: &str = "New prompt";

/// "New prompt", or "New prompt N" with the lowest N >= 2 not in [existing].
pub fn new_prompt_name(existing: &[String]) -> String {
    if !existing.iter().any(|name| name == NEW_PROMPT) {
        return NEW_PROMPT.to_string();
    }
    (2..).map(|n| format!("{NEW_PROMPT} {n}")).find(|name| !existing.contains(name)).expect("unbounded")
}

/// Why [text] can't be saved as provider options (blank is fine: it clears them), or None.
pub fn provider_options_error(text: &str) -> Option<&'static str> {
    parse_provider_options(text).err()
}

/// Why [input] can't be added to the Custom Words [words], or None: over the length cap, or the list is full. A blank
/// [input] is no error (nothing is added; nor is a case-insensitive duplicate, dropped on save).
pub fn custom_word_error(words: &[String], input: &str) -> Option<String> {
    if input.trim().is_empty() {
        None
    } else if input.trim().encode_utf16().count() > MAX_CUSTOM_WORD_LENGTH {
        Some(format!("Keep it to {MAX_CUSTOM_WORD_LENGTH} characters or fewer."))
    } else if words.len() >= MAX_CUSTOM_WORDS {
        Some(format!("The list is full ({MAX_CUSTOM_WORDS} words or phrases)."))
    } else {
        None
    }
}

/// The Add Word field's helper: the caps ([`MAX_CUSTOM_WORDS`], [`MAX_CUSTOM_WORD_LENGTH`]).
pub const CUSTOM_WORDS_HELPER: &str = "Up to 100 words or phrases, 60 characters each.";

/// The Transcription Provider note while Custom Words are in use (on, with words) and [sent] (the selected provider and
/// model get them), else None.
pub fn custom_words_stt_note(settings: &Settings, sent: bool) -> Option<&'static str> {
    (sent && !settings.active_custom_words().is_empty())
        .then_some("Custom words feature is on, so additional charges might apply.")
}

/// The Custom Words card's note: that the feature is off, or, when the selected model doesn't get the words ([sent]
/// false), what still applies; else None.
pub fn custom_words_unsent_note(
    enabled: bool,
    sent: bool,
    correction_enabled: bool,
    cleanup_enabled: bool,
) -> Option<&'static str> {
    match (enabled, sent, correction_enabled, cleanup_enabled) {
        (false, ..) => Some("Feature is disabled."),
        (_, true, ..) => None,
        (_, _, true, true) => Some("Not sent to this model. Only the correction and LLM cleanup apply."),
        (_, _, true, false) => Some("Not sent to this model. Only the correction applies."),
        (_, _, false, true) => Some("Not sent to this model. Only the LLM cleanup applies."),
        (_, _, false, false) => Some("Not sent to this model, so custom words have no effect."),
    }
}

pub const GATEWAY_OPTIONS_HINT: &str = "Sent as providerOptions, e.g. {\"gateway\": {\"order\": [\"azure\", \
    \"openai\"]}}. See the AI Gateway provider options docs. Leave blank to send none. Stored unencrypted: don't put \
    API keys here.";

pub const OPENROUTER_OPTIONS_HINT: &str = "Sent as provider (routing), e.g. {\"order\": [\"openai\"], \
    \"allow_fallbacks\": false}. See OpenRouter's provider routing docs. Leave blank to send none.";

#[cfg(test)]
mod settings_text_tests {
    use super::*;
    use crate::settings::{CustomCleanupProvider, Values, cleanup_provider_label, stt_provider_label, to_settings};
    use crate::stt::local::models::by_id;
    use crate::stt::local::stats::speed_verdict;
    use std::collections::HashSet;

    #[test]
    fn reasoning_labels() {
        assert_eq!("Model default", reasoning_effort_label(None));
        assert_eq!("Extra high", reasoning_effort_label(Some("xhigh")));
        assert_eq!("Low", reasoning_effort_label(Some("low")));
    }

    #[test]
    fn speed_seconds_and_percent() {
        assert_eq!("1.0x", speed_text(100));
        assert_eq!("1.5x", speed_text(150));
        assert_eq!("2.0x", speed_text(200));
        assert_eq!("10 s", seconds_text(10));
        assert_eq!("50%", percent_text(50));
    }

    /// Desktop: Parakeet Unified streams, so the per-phrase helper is checked on Parakeet TDT v3.
    #[test]
    fn on_device_texts() {
        assert_eq!("663 MB", megabytes_text(663_043_117));
        assert_eq!("141 MB", megabytes_text(141_300_566));
        assert_eq!("0 MB", megabytes_text(0));
        assert_eq!("120 / 663 MB", download_progress_text(120_400_000, 663_043_117));
        assert_eq!(
            "This streaming model only supports English.",
            on_device_model_helper(by_id("nemotron-streaming-en"))
        );
        assert_eq!("This streaming model only supports English.", on_device_model_helper(by_id("parakeet-unified-en")));
        assert_eq!(
            "25 European languages. Runs offline on this computer.",
            on_device_model_helper(by_id("parakeet-tdt-v3"))
        );
    }

    #[test]
    fn skip_silence_subtitles() {
        assert_eq!("Detects speech on-device and sends only that, to cut cost.", skip_silence_subtitle(None));
        assert_eq!(
            "Detects speech on-device and transcribes only that.",
            skip_silence_subtitle(Some(by_id("nemotron-streaming-en")))
        );
        assert_eq!(
            "Detects speech on-device and transcribes each phrase as you pause.",
            skip_silence_subtitle(Some(by_id("whisper-small")))
        );
    }

    #[test]
    fn api_key_helpers() {
        assert_eq!("API key saved. Enter a new one to replace it.", api_key_helper(true, None));
        assert_eq!("Not set", api_key_helper(false, None));
        assert_eq!(
            "API key saved (shared with transcription). Enter a new one to replace it.",
            api_key_helper(true, Some("transcription"))
        );
        assert_eq!("Not set (shared with post processing)", api_key_helper(false, Some("post processing")));
    }

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn new_prompt_names() {
        assert_eq!("New prompt", new_prompt_name(&[]));
        assert_eq!("New prompt", new_prompt_name(&names(&["Clean up dictation"])));
        assert_eq!("New prompt 2", new_prompt_name(&names(&["New prompt"])));
        assert_eq!("New prompt 3", new_prompt_name(&names(&["New prompt", "New prompt 2", "New prompt 4"])));
    }

    #[test]
    fn provider_options_errors() {
        assert_eq!(None, provider_options_error(""));
        assert_eq!(None, provider_options_error("{\"order\": [\"openai\"]}"));
        assert_eq!(Some("Not valid JSON"), provider_options_error("{order: 1"));
        assert_eq!(Some("Must be a JSON object: { ... }"), provider_options_error("[1]"));
    }

    #[test]
    fn provider_labels() {
        assert_eq!(STT_PROVIDER_ON_DEVICE, STT_PROVIDERS[0]);
        assert_eq!("On device", stt_provider_label(STT_PROVIDER_ON_DEVICE));
        assert_eq!("OpenAI", stt_provider_label(STT_PROVIDER_OPENAI));
        assert_eq!("Vercel AI Gateway", stt_provider_label(STT_PROVIDER_VERCEL_GATEWAY));
        assert_eq!("Deepgram", stt_provider_label(STT_PROVIDER_DEEPGRAM));
        assert_eq!("Soniox", stt_provider_label(STT_PROVIDER_SONIOX));
        assert_eq!("OpenAI", stt_provider_label("unknown"));
    }

    #[test]
    fn cleanup_provider_labels() {
        let custom = vec![
            CustomCleanupProvider::new("c1", "Groq", "https://api.groq.com/openai/v1"),
            CustomCleanupProvider::new("c2", " ", "https://x.example"),
        ];
        assert_eq!("Vercel AI Gateway", cleanup_provider_label(CLEANUP_PROVIDER_VERCEL_GATEWAY, &custom));
        assert_eq!("OpenRouter", cleanup_provider_label(CLEANUP_PROVIDER_OPENROUTER, &custom));
        assert_eq!("Groq", cleanup_provider_label("c1", &custom));
        assert_eq!("Unnamed Provider", cleanup_provider_label("c2", &custom));
        assert_eq!("Unnamed Provider", cleanup_provider_label("unknown", &custom));
    }

    #[test]
    fn speed_factor_texts() {
        assert_eq!("7.1× faster than you speak", speed_factor_text(7.098));
        assert_eq!("2.0× slower than you speak", speed_factor_text(0.5));
        assert_eq!("As fast as you speak", speed_factor_text(1.02));
        assert_eq!("As fast as you speak", speed_factor_text(0.98));
    }

    #[test]
    fn the_verdict_agrees_with_the_speed_text_at_the_thresholds() {
        let cases = [
            (0.94, ("Falls behind", "1.1× slower than you speak")),
            (0.95, ("Borderline", "As fast as you speak")),
            (0.98, ("Borderline", "As fast as you speak")),
            (1.44, ("Borderline", "1.4× faster than you speak")),
            (1.45, ("Keeps up", "1.5× faster than you speak")),
            (1.499, ("Keeps up", "1.5× faster than you speak")),
        ];
        for (factor, (verdict, text)) in cases {
            assert_eq!((verdict, text.to_string()), (verdict_label(speed_verdict(factor)), speed_factor_text(factor)));
        }
    }

    #[test]
    fn tenths_texts() {
        assert_eq!("0.2 s", tenths_text(191));
        assert_eq!("4.8 s", tenths_text(4_849));
        assert_eq!("12.3 s", tenths_text(12_340));
        assert_eq!("0.1 s", tenths_text(50));
        assert_eq!("0.0 s", tenths_text(0));
    }

    #[test]
    fn verdict_labels() {
        assert_eq!("Keeps up", verdict_label(SpeedVerdict::KeepsUp));
        assert_eq!("Borderline", verdict_label(SpeedVerdict::Borderline));
        assert_eq!("Falls behind", verdict_label(SpeedVerdict::FallsBehind));
    }

    #[test]
    fn stats_basis_texts() {
        assert_eq!("From your last dictation.", stats_basis_text(1, "dictation"));
        assert_eq!("From your last 12 dictations.", stats_basis_text(12, "dictation"));
        assert_eq!("From your last post processing request.", stats_basis_text(1, "post processing request"));
        assert_eq!("From your last 5 post processing requests.", stats_basis_text(5, "post processing request"));
    }

    #[test]
    fn minutes_texts() {
        assert_eq!("0.0 min", minutes_text(0));
        assert_eq!("12.4 min", minutes_text(744_000));
        assert_eq!("0.5 min", minutes_text(30_000));
    }

    #[test]
    fn saved_percents() {
        assert_eq!(0, saved_percent(10_000, 10_000));
        assert_eq!(38, saved_percent(10_000, 6_200));
        assert_eq!(100, saved_percent(10_000, 0));
        // Floored and capped: anything billed is under 100, any saving is at least 1.
        assert_eq!(99, saved_percent(10_000, 30));
        assert_eq!(1, saved_percent(10_000, 9_960));
        // Clamped: billed over recorded (e.g. Sonic's tail) saves nothing.
        assert_eq!(0, saved_percent(10_000, 10_500));
        assert_eq!(0, saved_percent(0, 0));
    }

    #[test]
    fn audio_saved_texts() {
        assert_eq!("38% less than recorded.", audio_saved_text(38));
        assert_eq!("Same as recorded duration. Nothing saved.", audio_saved_text(0));
    }

    #[test]
    fn duration_texts() {
        assert_eq!("0 s", duration_text(0));
        assert_eq!("35 s", duration_text(35_000));
        assert_eq!("59 s", duration_text(59_999));
        assert_eq!("1 min", duration_text(60_000));
        assert_eq!("47 min", duration_text(47 * 60_000 + 59_000));
        assert_eq!("3 h 12 min", duration_text((3 * 60 + 12) * 60_000));
        assert_eq!("2 h", duration_text(2 * 3_600_000));
        // The summary card's net Time Saved may be negative: shown as nothing saved.
        assert_eq!("0 s", duration_text(-90_000));
    }

    #[test]
    fn custom_word_errors() {
        let ninety_nine: Vec<String> = (1..=99).map(|i| format!("w{i}")).collect();
        let mut hundred = ninety_nine.clone();
        hundred.push("w100".into());
        assert_eq!(None, custom_word_error(&ninety_nine, "w100"));
        assert_eq!(None, custom_word_error(&ninety_nine, &"x".repeat(60)));
        assert_eq!(None, custom_word_error(&ninety_nine, &format!("  {}  ", "x".repeat(60))));
        assert_eq!(Some("Keep it to 60 characters or fewer.".into()), custom_word_error(&ninety_nine, &"x".repeat(61)));
        assert_eq!(Some("The list is full (100 words or phrases).".into()), custom_word_error(&hundred, "w101"));
        // Blank input adds nothing, no error.
        assert_eq!(None, custom_word_error(&hundred, "  "));
        assert_eq!(None, custom_word_error(&[], ""));
    }

    #[test]
    fn custom_words_helper_names_the_caps() {
        assert_eq!(
            format!("Up to {MAX_CUSTOM_WORDS} words or phrases, {MAX_CUSTOM_WORD_LENGTH} characters each."),
            CUSTOM_WORDS_HELPER
        );
    }

    #[test]
    fn custom_words_stt_note_only_while_in_use_and_sent() {
        let defaults = to_settings(&Values::new(), &HashSet::new()).unwrap();
        let on = Settings { custom_words_enabled: true, custom_words: names(&["Deepgram"]), ..defaults };

        assert_eq!(
            Some("Custom words feature is on, so additional charges might apply."),
            custom_words_stt_note(&on, true)
        );
        assert_eq!(None, custom_words_stt_note(&on, false));
        assert_eq!(None, custom_words_stt_note(&Settings { custom_words_enabled: false, ..on.clone() }, true));
        assert_eq!(None, custom_words_stt_note(&Settings { custom_words: Vec::new(), ..on }, true));
    }

    #[test]
    fn custom_words_unsent_note_names_what_still_applies() {
        assert_eq!(None, custom_words_unsent_note(true, true, true, true));
        assert_eq!(None, custom_words_unsent_note(true, true, false, false));
        assert_eq!(
            Some("Not sent to this model. Only the correction and LLM cleanup apply."),
            custom_words_unsent_note(true, false, true, true)
        );
        assert_eq!(
            Some("Not sent to this model. Only the correction applies."),
            custom_words_unsent_note(true, false, true, false)
        );
        assert_eq!(
            Some("Not sent to this model. Only the LLM cleanup applies."),
            custom_words_unsent_note(true, false, false, true)
        );
        assert_eq!(
            Some("Not sent to this model, so custom words have no effect."),
            custom_words_unsent_note(true, false, false, false)
        );
        assert_eq!(Some("Feature is disabled."), custom_words_unsent_note(false, false, false, false));
        assert_eq!(Some("Feature is disabled."), custom_words_unsent_note(false, true, true, true));
    }
}
