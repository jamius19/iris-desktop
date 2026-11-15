/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The on-device models in menu order (Android's `OnDeviceModels.kt` minus Moonshine, dropped on desktop). Download
//! URLs and hashes come with the downloader.

use crate::settings::DEFAULT_ON_DEVICE_STT_MODEL;

#[derive(Debug, PartialEq, Eq)]
pub struct OnDeviceModel {
    /// The stored setting value (Android's id).
    pub id: &'static str,
    pub label: &'static str,
    /// The brand mark: `nvidia` or `openai`.
    pub maker: &'static str,
    /// As the helper shows it: "English", "25 European languages", "99 languages".
    pub languages: &'static str,
    /// The one language the model outputs (ISO 639-1); None = multilingual.
    pub output_language: Option<&'static str>,
    /// Decoded as one stream while recording (live text), not per phrase.
    pub streams: bool,
    pub licence: &'static str,
    pub licence_url: &'static str,
}

const NVIDIA_OPEN_MODEL_LICENCE: &str = "Licensed by NVIDIA Corporation under the NVIDIA Open Model License";
const NVIDIA_OPEN_MODEL_LICENCE_URL: &str =
    "https://www.nvidia.com/en-us/agreements/enterprise-software/nvidia-open-model-license/";

pub const ON_DEVICE_MODELS: [OnDeviceModel; 4] = [
    OnDeviceModel {
        id: DEFAULT_ON_DEVICE_STT_MODEL,
        label: "Nemotron Streaming English",
        maker: "nvidia",
        languages: "English",
        output_language: Some("en"),
        streams: true,
        licence: NVIDIA_OPEN_MODEL_LICENCE,
        licence_url: NVIDIA_OPEN_MODEL_LICENCE_URL,
    },
    OnDeviceModel {
        id: "parakeet-unified-en",
        label: "Parakeet Unified English",
        maker: "nvidia",
        languages: "English",
        output_language: Some("en"),
        // Offline on Android; the GGUF engine streams it.
        streams: true,
        licence: NVIDIA_OPEN_MODEL_LICENCE,
        licence_url: NVIDIA_OPEN_MODEL_LICENCE_URL,
    },
    OnDeviceModel {
        id: "parakeet-tdt-v3",
        label: "Parakeet TDT v3",
        maker: "nvidia",
        languages: "25 European languages",
        output_language: None,
        streams: false,
        licence: "Parakeet TDT 0.6B v3 by NVIDIA, CC BY 4.0",
        licence_url: "https://creativecommons.org/licenses/by/4.0/",
    },
    OnDeviceModel {
        id: "whisper-small",
        label: "Whisper Small",
        maker: "openai",
        languages: "99 languages",
        output_language: None,
        streams: false,
        licence: "Whisper by OpenAI, MIT License",
        licence_url: "https://github.com/openai/whisper/blob/main/LICENSE",
    },
];

/// The model [id]; an unknown id (e.g. Android's `moonshine-base-en`) means the default model.
pub fn by_id(id: &str) -> &'static OnDeviceModel {
    ON_DEVICE_MODELS.iter().find(|m| m.id == id).unwrap_or(&ON_DEVICE_MODELS[0])
}

/// Whisper Small's language codes; only these may reach Whisper.
pub const WHISPER_LANGUAGES: [&str; 99] = [
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it", "id", "hi", "fi",
    "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur", "hr", "bg", "lt", "la", "mi", "ml",
    "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn", "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs",
    "kk", "sq", "sw", "gl", "mr", "pa", "si", "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am",
    "yi", "lo", "uz", "fo", "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su",
];

/// The language setting as Whisper takes it: trimmed and lowercased if Whisper knows it, else "" (auto-detect).
pub fn whisper_language(setting: &str) -> String {
    let language = setting.trim().to_lowercase();
    if WHISPER_LANGUAGES.contains(&language.as_str()) { language } else { String::new() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique_and_the_default_exists() {
        let ids: HashSet<_> = ON_DEVICE_MODELS.iter().map(|m| m.id).collect();
        assert_eq!(ON_DEVICE_MODELS.len(), ids.len());
        assert_eq!(DEFAULT_ON_DEVICE_STT_MODEL, ON_DEVICE_MODELS[0].id);
        assert_eq!("nemotron-streaming-en", DEFAULT_ON_DEVICE_STT_MODEL);
    }

    /// Desktop: Parakeet Unified streams too (Android: only Nemotron).
    #[test]
    fn only_nemotron_and_parakeet_unified_stream() {
        let streaming: Vec<_> = ON_DEVICE_MODELS.iter().filter(|m| m.streams).map(|m| m.id).collect();
        assert_eq!(vec!["nemotron-streaming-en", "parakeet-unified-en"], streaming);
    }

    #[test]
    fn only_the_english_models_have_an_output_language() {
        let english: Vec<_> =
            ON_DEVICE_MODELS.iter().filter(|m| m.output_language == Some("en")).map(|m| m.id).collect();
        assert_eq!(vec!["nemotron-streaming-en", "parakeet-unified-en"], english);
        assert!(ON_DEVICE_MODELS.iter().all(|m| m.output_language.is_none() || m.output_language == Some("en")));
    }

    #[test]
    fn unknown_id_is_the_default_model() {
        assert_eq!(DEFAULT_ON_DEVICE_STT_MODEL, by_id("x").id);
        assert_eq!(DEFAULT_ON_DEVICE_STT_MODEL, by_id("moonshine-base-en").id);
        assert_eq!("whisper-small", by_id("whisper-small").id);
    }

    #[test]
    fn whisper_languages_are_validated() {
        assert_eq!(99, WHISPER_LANGUAGES.iter().collect::<HashSet<_>>().len());
        assert_eq!("en", whisper_language(" EN "));
        assert_eq!("haw", whisper_language("haw"));
        assert_eq!("", whisper_language("yue"));
        assert_eq!("", whisper_language("en-US"));
        assert_eq!("", whisper_language(""));
    }
}
