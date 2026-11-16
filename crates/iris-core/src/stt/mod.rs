/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Speech-to-text: the provider interface (Android `SpeechToTextProvider.kt`) and the providers: [`local`] (on
//! device), [`openai`], [`vercel`] (Vercel AI Gateway), [`deepgram`], [`soniox`].

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::settings::Settings;

pub mod deepgram;
pub mod local;
pub mod openai;
pub mod soniox;
pub mod vercel;
pub mod wav;

/// A boxed, sendable future (the provider methods' return type; `dyn` providers can't use `async fn`).
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A user-readable speech-to-text failure (missing model or key, auth, network or server error).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SttError(pub String);

impl fmt::Display for SttError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SttError {}

/// A session's audio for [`SpeechToTextProvider::transcribe`]: PCM16 LE mono chunks at the provider's
/// [`sample_rate`](SpeechToTextProvider::sample_rate). The first [`next`](AudioSource::next) opens the microphone.
pub trait AudioSource: Send {
    /// The next chunk; `Ok(None)` once the recording ended (stop). An empty chunk is a segment end (only for
    /// providers that [want them](SpeechToTextProvider::wants_segment_ends)). Errors (microphone, "No speech
    /// detected") are the session's: pass them on unchanged. Must be cancel-safe: dropping a pending `next()`
    /// (providers poll it inside `select!`) loses no audio.
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>>;
}

/// What one [`SpeechToTextProvider::transcribe`] call transcribes.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SttRequest {
    pub model: String,
    /// ISO-639-1 code, or blank to auto-detect (Soniox: a comma-separated list).
    pub language: String,
    /// How long to wait for the final transcript once all audio is sent.
    pub timeout: Duration,
    /// Terms to bias recognition toward (normalised Custom Words); providers without an option for them ignore them.
    pub custom_words: Vec<String>,
}

/// Reports the whole live transcript whenever it changes.
pub type OnPartial = Box<dyn Fn(String) + Send + Sync>;

/// Streaming speech-to-text. A new provider is just a new implementation.
pub trait SpeechToTextProvider: Send + Sync {
    /// The rate (Hz) [`transcribe`](Self::transcribe) takes `model`'s audio at.
    fn sample_rate(&self, _model: &str) -> u32 {
        24_000
    }

    /// Sends audio while recording (holds a socket: gets VAD keep-alive audio); false = no socket while recording:
    /// upload providers and the on-device one.
    fn streams(&self) -> bool {
        true
    }

    /// `model` reports text while recording. Picks the VAD hangover.
    fn live_text(&self, _model: &str) -> bool {
        self.streams()
    }

    /// The audio may be sped up (Audio Speed); false = always sent as recorded (Soniox real-time).
    fn allows_speed_up(&self) -> bool {
        true
    }

    /// The audio carries an empty chunk at the end of each speech segment the VAD gate detects (Skip Silence on);
    /// for providers that decode per segment.
    fn wants_segment_ends(&self) -> bool {
        false
    }

    /// [`transcribe`](Self::transcribe) sends `model` the custom words; false = it ignores them (the settings' Custom
    /// Words notes).
    fn takes_custom_words(&self, _model: &str) -> bool {
        false
    }

    /// Streams `audio` to the service, reports the live transcript through `on_partial`, and once `audio` ends
    /// returns the final transcript. Dropping the future aborts and discards the session.
    fn transcribe<'a>(
        &'a self,
        audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>>;

    /// Gets ready for a [`transcribe`](Self::transcribe) call (a warm connection, a loaded model). Returns at once
    /// and never fails.
    fn warm_up(&self) {}
}

/// `a` and `b` joined with a space, unless either is empty or the boundary is CJK/kana/full-width (Android
/// `GatewayTranscriptionProtocol.joined`).
pub fn joined(a: &str, b: &str) -> String {
    let unspaced = |c: char| {
        matches!(
            c,
            '\u{3000}'..='\u{30ff}'
                | '\u{3400}'..='\u{4dbf}'
                | '\u{4e00}'..='\u{9fff}'
                | '\u{f900}'..='\u{faff}'
                | '\u{ff00}'..='\u{ffef}'
        )
    };
    match (a.chars().last(), b.chars().next()) {
        (Some(last), Some(first)) if !unspaced(last) && !unspaced(first) => format!("{a} {b}"),
        _ => format!("{a}{b}"),
    }
}

/// The provider and model `settings` select get the custom words (Android `AppContainer.sttTakesCustomWords`, from the
/// same predicates the providers' `takes_custom_words` use).
pub fn stt_takes_custom_words(settings: &Settings) -> bool {
    let model = settings.active_stt_model();
    if settings.stt_on_device() || settings.stt_gateway() {
        false
    } else if settings.stt_deepgram() {
        deepgram::protocol::takes_keyterms(model)
    } else if settings.stt_soniox() {
        true
    } else {
        openai::protocol::uses_keywords(model) || openai::protocol::takes_prompt(model)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_spaces_words_but_not_cjk() {
        assert_eq!("a b", joined("a", "b"));
        assert_eq!("b", joined("", "b"));
        assert_eq!("a", joined("a", ""));
        assert_eq!("你好世界", joined("你好", "世界"));
    }
}
