/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Speech-to-text. The model predicates the settings pages need; M3 moves them into the provider protocol modules.

use crate::settings::Settings;

pub mod local;

/// The models a Realtime transcription session accepts (Android `RealtimeProtocol.kt`); any other id is uploaded.
pub const REALTIME_TRANSCRIPTION_MODELS: [&str; 8] = [
    "whisper-1",
    "gpt-transcribe",
    "gpt-live-transcribe",
    "gpt-4o-mini-transcribe",
    "gpt-4o-mini-transcribe-2025-12-15",
    "gpt-4o-transcribe",
    "gpt-4o-transcribe-diarize",
    "gpt-realtime-whisper",
];

/// Deepgram's static model list (no catalogue).
pub const DEEPGRAM_MODELS: [&str; 3] = ["nova-3", "nova-3-medical", "nova-2"];

/// Soniox regions: id and label; an unknown id means the first (US).
pub const SONIOX_REGIONS: [(&str, &str); 4] =
    [("us", "United States"), ("eu", "European Union"), ("jp", "Japan"), ("in", "India")];

/// `model` streams over a Realtime session.
pub fn is_realtime_transcription_model(model: &str) -> bool {
    REALTIME_TRANSCRIPTION_MODELS.contains(&model.trim())
}

/// `model` takes custom words as `keywords`.
pub fn uses_keywords(model: &str) -> bool {
    matches!(model.trim(), "gpt-transcribe" | "gpt-live-transcribe")
}

/// `model` takes custom words as a `prompt`: whisper-1 and the gpt-4o(-mini)-transcribe models, not diarize.
pub fn takes_prompt(model: &str) -> bool {
    let id = model.trim();
    if id == "whisper-1" {
        return true;
    }
    let gpt4o = id.starts_with("gpt-4o-transcribe") || id.starts_with("gpt-4o-mini-transcribe");
    gpt4o && !id.contains("diarize")
}

/// Keyterm prompting is Nova-3 only.
pub fn takes_keyterms(model: &str) -> bool {
    model.trim().starts_with("nova-3")
}

/// A Soniox model streams; without the catalogue (M3) the `stt-rt-` prefix decides, as Android's fallback.
pub fn soniox_streams(model: &str) -> bool {
    model.starts_with("stt-rt-")
}

/// The provider and model `settings` select get the custom words (Android `AppContainer.sttTakesCustomWords`).
pub fn stt_takes_custom_words(settings: &Settings) -> bool {
    let model = settings.active_stt_model();
    if settings.stt_on_device() || settings.stt_gateway() {
        false
    } else if settings.stt_deepgram() {
        takes_keyterms(model)
    } else if settings.stt_soniox() {
        true
    } else {
        uses_keywords(model) || takes_prompt(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_models_take_keywords_or_a_prompt() {
        assert!(uses_keywords("gpt-live-transcribe"));
        assert!(!uses_keywords("whisper-1"));
        assert!(takes_prompt("whisper-1"));
        assert!(takes_prompt("gpt-4o-mini-transcribe-2025-12-15"));
        assert!(!takes_prompt("gpt-4o-transcribe-diarize"));
        assert!(!takes_prompt("gpt-realtime-whisper"));
    }

    #[test]
    fn only_nova_3_takes_keyterms() {
        assert!(takes_keyterms(" nova-3-medical"));
        assert!(!takes_keyterms("nova-2"));
    }

    #[test]
    fn realtime_models_and_soniox_fallback() {
        assert!(is_realtime_transcription_model(" gpt-4o-transcribe "));
        assert!(!is_realtime_transcription_model("gpt-4o-transcribe-2"));
        assert!(soniox_streams("stt-rt-v6"));
        assert!(!soniox_streams("stt-async-v6"));
    }
}
