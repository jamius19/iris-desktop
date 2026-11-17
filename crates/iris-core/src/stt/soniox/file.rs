/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Soniox async transcription of the recording after stop (Android `SonioxFileTranscription.kt`): upload the WAV
//! (`POST /v1/files`), create a job (`POST /v1/transcriptions`), poll it, fetch its transcript. The job and the file
//! are deleted afterwards in every case, a cancelled session too (they count against Soniox's quotas for 30 days
//! otherwise).

use std::sync::Arc;
use std::time::Duration;

use log::debug;
use reqwest::header::{AUTHORIZATION, CONTENT_TYPE, HeaderValue};
use reqwest::multipart::{Form, Part};
use tokio::time::{Instant, sleep};
use url::Url;

use super::ModelInfo;
use super::models::SONIOX_MODELS_PATH;
use super::protocol::{
    SONIOX_SAMPLE_RATE, SonioxRegion, is_soniox_job_failed, parse_soniox_id, parse_soniox_job,
    parse_soniox_transcript_text, soniox_authorization, soniox_create_transcription_body, soniox_http_error_message,
    soniox_language_hints, soniox_network_error_message,
};
use crate::net::{self, KeySource, NetError, WarmUp};
use crate::stt::wav::{pcm16_duration, upload_read_timeout, upload_send_allowance, wav_bytes};
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest};

const TAG: &str = "SonioxFileStt";
const UNEXPECTED: &str = "Unexpected response from Soniox";
const TRANSCRIPT_TIMEOUT: &str = "Timed out waiting for the transcript";
const SEND_TIMEOUT: &str = "Timed out sending audio to Soniox";

/// Polls after 250, 500, then every 1000 ms: short dictations finish fast, long ones don't hammer the API.
const FIRST_POLL: Duration = Duration::from_millis(250);
const MAX_POLL: Duration = Duration::from_millis(1_000);

/// A cleanup `DELETE` that hangs is dropped after this long (OkHttp's default read timeout).
const DELETE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct SonioxFileTranscription {
    client: reqwest::Client,
    api_key: KeySource,
    model_info: ModelInfo,
    base_url: Url,
    warm_up_url: String,
    warm_up: Arc<WarmUp>,
    /// Sends the upload in time ([`upload_send_allowance`]); off in tests.
    send_allowance: fn(Duration) -> Duration,
}

impl SonioxFileTranscription {
    /// On `region`'s REST URL.
    pub fn new(client: reqwest::Client, api_key: KeySource, model_info: ModelInfo, region: SonioxRegion) -> Self {
        let base_url = region.rest_url();
        let models_url = format!("{base_url}/{SONIOX_MODELS_PATH}");
        Self {
            send_allowance: upload_send_allowance,
            ..Self::with_urls(client, api_key, model_info, base_url, &models_url)
        }
    }

    pub(crate) fn with_urls(
        client: reqwest::Client,
        api_key: KeySource,
        model_info: ModelInfo,
        base_url: &str,
        warm_up_url: &str,
    ) -> Self {
        SonioxFileTranscription {
            client,
            api_key,
            model_info,
            base_url: Url::parse(base_url).expect("Soniox base URL"),
            warm_up_url: warm_up_url.to_string(),
            warm_up: Arc::default(),
            send_allowance: |_| Duration::ZERO,
        }
    }

    async fn session(&self, mut audio: Box<dyn AudioSource + '_>, request: SttRequest) -> Result<String, SttError> {
        let key = match net::api_key(&self.api_key).await {
            Ok(Some(key)) => key,
            Ok(None) => return Err(SttError("Add your Soniox API key in Iris settings".into())),
            Err(message) => return Err(SttError(message.into())),
        };
        // Before any audio is pulled: the microphone never opens for a key that can't be sent.
        let authorization = soniox_authorization(&key)?;
        let mut chunks = Vec::new();
        while let Some(chunk) = audio.next().await? {
            chunks.push(chunk);
        }
        let stopped_at = Instant::now();
        let hints = soniox_language_hints(&request.language);
        let strict = !hints.is_empty()
            && (self.model_info)(request.model.clone()).await.is_some_and(|entry| entry.supports_language_hints_strict);
        let wav = wav_bytes(&chunks, SONIOX_SAMPLE_RATE);
        let audio_duration = pcm16_duration(chunks.iter().map(Vec::len).sum(), SONIOX_SAMPLE_RATE);
        let budget = upload_read_timeout(request.timeout, audio_duration);
        // Deletes the job and the file once this ends: success, failure, timeout or cancellation (dropped).
        let mut cleanup =
            Cleanup { client: self.client.clone(), authorization: authorization.clone(), job: None, file: None };

        let wav_kb = wav.len() / 1024;
        let part = Part::bytes(wav).file_name("audio.wav").mime_str("audio/wav").expect("audio/wav");
        let upload = self
            .request(
                &authorization,
                reqwest::Method::POST,
                &["v1", "files"],
                budget + (self.send_allowance)(audio_duration),
            )
            .multipart(Form::new().part("file", part));
        let file_id =
            parse_soniox_id(&fetch(upload, SEND_TIMEOUT).await?).ok_or_else(|| SttError(UNEXPECTED.into()))?;
        cleanup.file = Some(self.url(&["v1", "files", &file_id]));
        let body = soniox_create_transcription_body(&request.model, &file_id, &hints, strict, &request.custom_words);
        let create = self
            .request(&authorization, reqwest::Method::POST, &["v1", "transcriptions"], budget)
            .header(CONTENT_TYPE, "application/json")
            .body(body);
        let id =
            parse_soniox_id(&fetch(create, TRANSCRIPT_TIMEOUT).await?).ok_or_else(|| SttError(UNEXPECTED.into()))?;
        cleanup.job = Some(self.url(&["v1", "transcriptions", &id]));
        let mut polls = 0;
        let transcribed = tokio::time::timeout(budget, async {
            let mut wait = FIRST_POLL;
            loop {
                sleep(wait).await;
                wait = (wait * 2).min(MAX_POLL);
                polls += 1;
                let poll = self.request(&authorization, reqwest::Method::GET, &["v1", "transcriptions", &id], budget);
                let job = parse_soniox_job(&fetch(poll, TRANSCRIPT_TIMEOUT).await?)
                    .ok_or_else(|| SttError(UNEXPECTED.into()))?;
                if job.status == "completed" {
                    break;
                }
                if is_soniox_job_failed(&job.status) {
                    let message = job.error_message.as_deref().unwrap_or("transcription failed");
                    return Err(SttError(format!("Soniox error: {message}")));
                }
            }
            let transcript = self.request(
                &authorization,
                reqwest::Method::GET,
                &["v1", "transcriptions", &id, "transcript"],
                budget,
            );
            parse_soniox_transcript_text(&fetch(transcript, TRANSCRIPT_TIMEOUT).await?)
                .ok_or_else(|| SttError(UNEXPECTED.into()))
        })
        .await;
        let text = transcribed.unwrap_or_else(|_| Err(SttError(TRANSCRIPT_TIMEOUT.into())))?;
        debug!(
            "{TAG}: Timing: upload {} KB, stop->transcript {} ms, {polls} polls",
            wav_kb,
            stopped_at.elapsed().as_millis()
        );
        Ok(text.trim().to_string())
    }

    fn url(&self, segments: &[&str]) -> Url {
        let mut url = self.base_url.clone();
        url.path_segments_mut().expect("Soniox base URL").pop_if_empty().extend(segments);
        url
    }

    fn request(
        &self,
        authorization: &HeaderValue,
        method: reqwest::Method,
        segments: &[&str],
        timeout: Duration,
    ) -> reqwest::RequestBuilder {
        self.client.request(method, self.url(segments)).header(AUTHORIZATION, authorization.clone()).timeout(timeout)
    }
}

/// The 2xx response's body. A timeout fails with `timeout_message` (the upload's: sending the audio; the others':
/// the transcript).
async fn fetch(request: reqwest::RequestBuilder, timeout_message: &str) -> Result<String, SttError> {
    let error =
        |e: NetError| SttError(if e.timeout { timeout_message.to_string() } else { soniox_network_error_message(&e) });
    let response = request.send().await.map_err(|e| error(e.into()))?;
    let (code, body) = net::read_response(response).await.map_err(error)?;
    if !(200..300).contains(&code) {
        return Err(SttError(soniox_http_error_message(code, &body)));
    }
    Ok(body)
}

/// The created job and file, deleted when dropped.
struct Cleanup {
    client: reqwest::Client,
    authorization: HeaderValue,
    job: Option<Url>,
    file: Option<Url>,
}

impl Drop for Cleanup {
    /// Fire-and-forget `DELETE`s that outlive a cancelled session. A failure is only logged (a job still processing
    /// answers 409 and is left to Soniox's 30-day auto-delete).
    fn drop(&mut self) {
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        for url in [self.job.take(), self.file.take()].into_iter().flatten() {
            let request = self
                .client
                .delete(url.clone())
                .header(AUTHORIZATION, self.authorization.clone())
                .timeout(DELETE_TIMEOUT);
            runtime.spawn(async move {
                match request.send().await {
                    Ok(response) if !response.status().is_success() => {
                        debug!("{TAG}: Delete {} {}", url.path(), response.status().as_u16())
                    }
                    Ok(_) => {}
                    Err(e) => debug!("{TAG}: Delete {} failed: {}", url.path(), NetError::from(e).message),
                }
            });
        }
    }
}

impl SpeechToTextProvider for SonioxFileTranscription {
    fn sample_rate(&self, _model: &str) -> u32 {
        SONIOX_SAMPLE_RATE
    }

    fn streams(&self) -> bool {
        false
    }

    fn takes_custom_words(&self, _model: &str) -> bool {
        true
    }

    fn transcribe<'a>(
        &'a self,
        audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        _on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(self.session(audio, request))
    }

    /// `HEAD /v1/models` without a key: a quick 405 that leaves an idle connection in the shared pool for the upload.
    /// Skipped within `net::WARM_UP_WINDOW` of an earlier warm-up (the connection returns to the pool, so no session
    /// clears it).
    fn warm_up(&self) {
        self.warm_up.head(&self.client, &self.warm_up_url, TAG);
    }
}

#[cfg(test)]
mod tests;
