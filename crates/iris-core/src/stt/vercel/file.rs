/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Vercel AI Gateway transcription of a recording uploaded after stop (`POST` of base64 WAV as JSON; Android
//! `VercelGatewayFileTranscription.kt`), for models without streaming support: no live text. No size limit is
//! documented, so it is one upload.

use std::sync::Arc;
use std::time::Duration;

use log::{debug, warn};
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE};
use tokio::time::Instant;

use super::protocol::{
    GATEWAY_WARM_UP_URL, PROTOCOL_VERSION, SPECIFICATION_VERSION, TRANSCRIPTION_URL, gateway_network_error_message,
    http_error_message, parse_upload_response, provider_options, upload_body,
};
use crate::net::{self, KeySource, NetError, WarmUp, is_header_value};
use crate::stt::wav::{pcm16_duration, upload_read_timeout, wav_bytes};
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest};

const TAG: &str = "VercelGatewayFileStt";

/// As Handy uploads it.
const SAMPLE_RATE: u32 = 16_000;

/// Time to connect and send the upload, on top of the read timeout (OkHttp's default write timeout).
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

pub struct VercelGatewayFileTranscription {
    client: reqwest::Client,
    api_key: KeySource,
    url: String,
    warm_up_url: String,
    warm_up: Arc<WarmUp>,
    send_timeout: Duration,
}

impl VercelGatewayFileTranscription {
    pub fn new(client: reqwest::Client, api_key: KeySource) -> Self {
        Self::with_urls(client, api_key, TRANSCRIPTION_URL, GATEWAY_WARM_UP_URL, SEND_TIMEOUT)
    }

    pub(crate) fn with_urls(
        client: reqwest::Client,
        api_key: KeySource,
        url: &str,
        warm_up_url: &str,
        send_timeout: Duration,
    ) -> Self {
        VercelGatewayFileTranscription {
            client,
            api_key,
            url: url.to_string(),
            warm_up_url: warm_up_url.to_string(),
            warm_up: Arc::default(),
            send_timeout,
        }
    }
}

impl SpeechToTextProvider for VercelGatewayFileTranscription {
    fn sample_rate(&self, _model: &str) -> u32 {
        SAMPLE_RATE
    }

    fn streams(&self) -> bool {
        false
    }

    // Custom words are not sent: whether the Gateway forwards a biasing option is unverified.
    fn transcribe<'a>(
        &'a self,
        mut audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        _on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async move {
            let key = match net::api_key(&self.api_key).await {
                Ok(Some(key)) => key,
                Ok(None) => return Err(SttError("Add your Vercel AI Gateway API key in Iris settings".into())),
                Err(message) => return Err(SttError(message.into())),
            };
            let model = request.model;
            // Checked before any audio is pulled, so the microphone never opens.
            let authorization = format!("Bearer {key}");
            if !is_header_value(&authorization) {
                return Err(SttError("Invalid Vercel AI Gateway API key".into()));
            }
            if !is_header_value(&model) {
                return Err(SttError(format!("Invalid model id: {model}")));
            }
            let mut chunks = Vec::new();
            while let Some(chunk) = audio.next().await? {
                chunks.push(chunk);
            }
            let stopped_at = Instant::now();
            let wav = wav_bytes(&chunks, SAMPLE_RATE);
            let audio_duration = pcm16_duration(chunks.iter().map(Vec::len).sum(), SAMPLE_RATE);
            drop(chunks);
            let body = upload_body(&wav, provider_options(&model, &request.language, false).as_ref());
            let response = self
                .client
                .post(&self.url)
                .header(AUTHORIZATION, authorization)
                .header("ai-gateway-protocol-version", PROTOCOL_VERSION)
                .header("ai-gateway-auth-method", "api-key")
                .header("ai-transcription-model-specification-version", SPECIFICATION_VERSION)
                .header("ai-model-id", &model)
                .header(CONTENT_TYPE, "application/json")
                .body(body)
                // The send allowance at real-time pace, then the read timeout (Kotlin's starts once the body is sent).
                .timeout(self.send_timeout + audio_duration + upload_read_timeout(request.timeout, audio_duration))
                .send()
                .await;
            let (status, response) = match response {
                Ok(response) => net::read_response(response).await,
                Err(error) => Err(NetError::from(error)),
            }
            .map_err(|error| {
                // Without a streamed body the send and the wait can't be told apart: a timeout is the server's.
                SttError(if error.timeout {
                    "Timed out waiting for the transcript".into()
                } else {
                    gateway_network_error_message(&error)
                })
            })?;
            if !(200..300).contains(&status) {
                return Err(SttError(http_error_message(status, &response)));
            }
            let (text, warnings) = parse_upload_response(&response)
                .ok_or_else(|| SttError("Unexpected response from AI Gateway".into()))?;
            warnings.iter().for_each(|w| warn!("{TAG}: Upload warning: {w}"));
            debug!(
                "{TAG}: Timing: upload {} KB, stop->response {} ms",
                wav.len() / 1024,
                stopped_at.elapsed().as_millis()
            );
            Ok(text.trim().to_string())
        })
    }

    /// `HEAD` of the public model catalog without a key, leaving an idle connection in the shared pool for the upload.
    /// Skipped within the warm-up window of an earlier warm-up (the upload returns its connection to the pool).
    fn warm_up(&self) {
        self.warm_up.head(&self.client, &self.warm_up_url, TAG);
    }
}

#[cfg(test)]
mod tests;
