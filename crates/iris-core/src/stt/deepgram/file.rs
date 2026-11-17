/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Deepgram pre-recorded transcription (`POST /v1/listen` with a WAV body) of the recording after stop (Android
//! `DeepgramFileTranscription.kt`): no live text, cheaper than streaming. The 2 GB file limit is far beyond a
//! dictation, so it is one upload.

use std::sync::Arc;
use std::time::Duration;

use log::debug;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use tokio::time::Instant;

use super::protocol::{
    DEEPGRAM_SAMPLE_RATE, LISTEN_URL, deepgram_http_error_message, deepgram_network_error_message, dropped_keyterms,
    listen_url, parse_listen_response, takes_keyterms,
};
use super::streaming::{INVALID_KEY, MISSING_KEY, TIMED_OUT};
use crate::net::{self, KeySource, NetError, WarmUp};
use crate::stt::wav::{pcm16_duration, upload_read_timeout, upload_send_allowance, wav_bytes};
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest};

const TAG: &str = "DeepgramFileStt";

pub struct DeepgramFileTranscription {
    client: reqwest::Client,
    api_key: KeySource,
    url: String,
    warm_up_url: String,
    warm_up: Arc<WarmUp>,
    /// Sends the upload in time ([`upload_send_allowance`]); off in tests.
    send_allowance: fn(Duration) -> Duration,
}

impl DeepgramFileTranscription {
    pub fn new(client: reqwest::Client, api_key: KeySource) -> Self {
        Self {
            send_allowance: upload_send_allowance,
            ..Self::with_urls(client, api_key, LISTEN_URL.into(), LISTEN_URL.into())
        }
    }

    pub(crate) fn with_urls(client: reqwest::Client, api_key: KeySource, url: String, warm_up_url: String) -> Self {
        DeepgramFileTranscription {
            client,
            api_key,
            url,
            warm_up_url,
            warm_up: Arc::default(),
            send_allowance: |_| Duration::ZERO,
        }
    }
}

impl SpeechToTextProvider for DeepgramFileTranscription {
    fn sample_rate(&self, _model: &str) -> u32 {
        DEEPGRAM_SAMPLE_RATE
    }

    fn streams(&self) -> bool {
        false
    }

    fn takes_custom_words(&self, model: &str) -> bool {
        takes_keyterms(model)
    }

    fn transcribe<'a>(
        &'a self,
        mut audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        _on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async move {
            let key = match net::api_key(&self.api_key).await {
                Ok(Some(key)) => key,
                Ok(None) => return Err(SttError(MISSING_KEY.into())),
                Err(message) => return Err(SttError(message.into())),
            };
            // Before any audio is pulled: the microphone never opens for a key that can't be sent.
            let mut authorization = Some(format!("Token {key}"))
                .filter(|value| net::is_header_value(value))
                .and_then(|value| HeaderValue::from_str(&value).ok())
                .ok_or_else(|| SttError(INVALID_KEY.into()))?;
            authorization.set_sensitive(true);
            let dropped = dropped_keyterms(&request.model, &request.custom_words);
            if dropped > 0 {
                debug!("Keyterms: dropped {dropped} over the token budget");
            }
            let mut chunks = Vec::new();
            while let Some(chunk) = audio.next().await? {
                chunks.push(chunk);
            }
            let stopped_at = Instant::now();
            let wav = wav_bytes(&chunks, DEEPGRAM_SAMPLE_RATE);
            let wav_size = wav.len();
            let audio_bytes = chunks.iter().map(Vec::len).sum();
            let audio_duration = pcm16_duration(audio_bytes, DEEPGRAM_SAMPLE_RATE);
            let timeout = (self.send_allowance)(audio_duration) + upload_read_timeout(request.timeout, audio_duration);
            let url = listen_url(&self.url, &request.model, &request.language, false, &request.custom_words);
            let response = self
                .client
                .post(url)
                .header(AUTHORIZATION, authorization)
                .header(CONTENT_TYPE, "audio/wav")
                .body(wav)
                .timeout(timeout)
                .send()
                .await;
            let (code, body) = match response {
                Ok(response) => net::read_response(response).await,
                Err(error) => Err(error.into()),
            }
            .map_err(network_error)?;
            if !(200..300).contains(&code) {
                return Err(SttError(deepgram_http_error_message(code, &body, None)));
            }
            let text =
                parse_listen_response(&body).ok_or_else(|| SttError("Unexpected response from Deepgram".into()))?;
            debug!("Timing: upload {} KB, stop->response {} ms", wav_size / 1024, stopped_at.elapsed().as_millis());
            Ok(text.trim().to_string())
        })
    }

    /// `HEAD /v1/listen` without a key: a quick 405 that leaves an idle (HTTP/2) connection in the shared pool for the
    /// upload. Skipped within the warm-up window of an earlier one (an HTTP/2 connection returns to the pool).
    fn warm_up(&self) {
        self.warm_up.head(&self.client, &self.warm_up_url, TAG);
    }
}

/// The request's one overall timeout covers the upload too: a timeout is reported as the transcript's.
fn network_error(error: NetError) -> SttError {
    SttError(if error.timeout { TIMED_OUT.into() } else { deepgram_network_error_message(&error) })
}

#[cfg(test)]
mod tests;
