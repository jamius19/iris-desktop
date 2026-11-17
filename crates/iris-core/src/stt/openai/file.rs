/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! OpenAI file transcription (`POST /v1/audio/transcriptions`, Android `OpenAiFileTranscription.kt`) for models
//! outside the Realtime list: the recording is uploaded as 16 kHz WAV after stop, with no live text. Recordings over
//! the 25 MB file limit are split at pauses into several uploads, sent one after another, and their texts joined.

use std::ops::{Deref, DerefMut};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use log::debug;
use reqwest::header::AUTHORIZATION;
use reqwest::multipart::{Form, Part};
use serde_json::Value;
use tokio::sync::watch;
use tokio::time::{Instant, sleep, timeout};

use super::protocol::{
    WARM_UP_URL, glossary_prompt, openai_http_error_message, takes_prompt, uses_keywords, uses_language_list,
};
use super::{INVALID_API_KEY, api_key, bearer};
use crate::net::{self, KeySource, NetError, WarmUp};
use crate::stt::wav::{WAV_HEADER_BYTES, pcm16_duration, split_at_pauses, upload_read_timeout, wav_bytes};
use crate::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest, joined};

/// OpenAI's file transcription endpoint.
pub const TRANSCRIPTIONS_URL: &str = "https://api.openai.com/v1/audio/transcriptions";

const TAG: &str = "OpenAiFileStt";
const SAMPLE_RATE: u32 = 16_000;
const TRANSCRIPT_TIMEOUT: &str = "Timed out waiting for the transcript";

/// Under OpenAI's 25 MB file limit with room to spare: ~10 min of 16 kHz audio.
const MAX_PART_BYTES: usize = 20_000_000;

/// Where a cut may go: the quietest chunk of a part's last 30 s.
const SPLIT_SEARCH_BYTES: usize = 30 * SAMPLE_RATE as usize * 2;

/// OkHttp's connect and write timeouts. The upload's progress can't be seen, so sending may take this plus the
/// audio's duration (an uplink of at least the audio's 256 kbit/s).
const SEND_TIMEOUT: Duration = Duration::from_secs(10);

pub struct OpenAiFileTranscription {
    client: reqwest::Client,
    api_key: KeySource,
    url: String,
    warm_up_url: String,
    max_part_bytes: usize,
    warm_up: Arc<WarmUp>,
}

impl OpenAiFileTranscription {
    pub fn new(client: reqwest::Client, api_key: KeySource) -> Self {
        OpenAiFileTranscription {
            client,
            api_key,
            url: TRANSCRIPTIONS_URL.into(),
            warm_up_url: WARM_UP_URL.into(),
            max_part_bytes: MAX_PART_BYTES,
            warm_up: Arc::default(),
        }
    }

    #[cfg(test)]
    fn with_urls(api_key: KeySource, url: String, warm_up_url: String, max_part_bytes: usize) -> Self {
        OpenAiFileTranscription { url, warm_up_url, max_part_bytes, ..Self::new(net::http_client(), api_key) }
    }

    async fn session(&self, mut audio: Box<dyn AudioSource + '_>, request: SttRequest) -> Result<String, SttError> {
        let key = api_key(&self.api_key).await?;
        let authorization = bearer(&key).ok_or_else(|| SttError(INVALID_API_KEY.into()))?;
        let mut chunks = Vec::new();
        while let Some(chunk) = audio.next().await? {
            chunks.push(chunk);
        }
        let stopped_at = Instant::now();
        let parts = split_at_pauses(&chunks, self.max_part_bytes, SPLIT_SEARCH_BYTES);
        let mut text = String::new();
        for part in &parts {
            text = joined(&text, &self.upload(&authorization, wav_bytes(part, SAMPLE_RATE), &request).await?);
        }
        let kb = chunks.iter().map(Vec::len).sum::<usize>() / 1024;
        debug!(
            "{TAG}: Timing: upload {kb} KB in {} part(s), stop->response {} ms",
            parts.len(),
            stopped_at.elapsed().as_millis()
        );
        Ok(text)
    }

    /// Uploads one WAV file and returns its trimmed text.
    async fn upload(&self, authorization: &str, wav: Vec<u8>, request: &SttRequest) -> Result<String, SttError> {
        let audio = pcm16_duration(wav.len() - WAV_HEADER_BYTES, SAMPLE_RATE);
        let read_timeout = upload_read_timeout(request.timeout, audio);
        let (sent, mut sent_rx) = watch::channel(false);
        let form = transcription_form(wav, &request.model, &request.language, &request.custom_words, sent);
        let response = self.client.post(&self.url).header(AUTHORIZATION, authorization).multipart(form).send();
        tokio::pin!(response);
        let is_sent = async {
            if sent_rx.wait_for(|sent| *sent).await.is_err() {
                std::future::pending::<()>().await;
            }
        };
        let early = tokio::select! {
            response = &mut response => Some(response),
            () = is_sent => None,
            () = sleep(SEND_TIMEOUT + audio) => return Err(SttError("Timed out sending audio to OpenAI".into())),
        };
        let response = match early {
            Some(response) => response,
            // Sent: only the server's time counts from here (OkHttp's read timeout).
            None => timeout(read_timeout, response).await.map_err(|_| SttError(TRANSCRIPT_TIMEOUT.into()))?,
        };
        let result = match response {
            Ok(response) => match timeout(read_timeout, net::read_response(response)).await {
                Ok(result) => result,
                Err(_) => Err(NetError::timeout()),
            },
            Err(e) => Err(NetError::from(e)),
        };
        let (code, body) = result.map_err(|e| {
            SttError(match (e.timeout, *sent_rx.borrow()) {
                (true, true) => TRANSCRIPT_TIMEOUT.into(),
                (true, false) => "Timed out sending audio to OpenAI".into(),
                (false, _) => format!("Network error: {}", e.message),
            })
        })?;
        if !(200..300).contains(&code) {
            return Err(SttError(openai_http_error_message(code, &body)));
        }
        parse_transcription_text(&body)
            .map(|text| text.trim().to_string())
            .ok_or_else(|| SttError("Unexpected response from OpenAI".into()))
    }
}

impl SpeechToTextProvider for OpenAiFileTranscription {
    fn sample_rate(&self, _model: &str) -> u32 {
        SAMPLE_RATE
    }

    fn streams(&self) -> bool {
        false
    }

    fn takes_custom_words(&self, model: &str) -> bool {
        uses_keywords(model) || takes_prompt(model)
    }

    fn transcribe<'a>(
        &'a self,
        audio: Box<dyn AudioSource + 'a>,
        request: SttRequest,
        _on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(self.session(audio, request))
    }

    /// `HEAD /v1/models` without a key: a quick 401 that leaves an idle (HTTP/2) connection in the shared pool for the
    /// upload. Skipped within the warm-up window of an earlier one (an HTTP/2 connection returns to the pool, so an
    /// upload doesn't clear it).
    fn warm_up(&self) {
        self.warm_up.head(&self.client, &self.warm_up_url, TAG);
    }
}

/// The multipart form for one file: `file` (`audio.wav`), `model`, `response_format=json`, and the language when set:
/// `languages[]` for models that take the list, else `language`. `words` (custom words) go in as one `keywords[]`
/// part each or as a `prompt`, as the model takes them ([`uses_keywords`], [`takes_prompt`]). `sent` turns true once
/// the request has taken all of the file (Android `SentBody`).
pub(crate) fn transcription_form(
    wav: Vec<u8>,
    model: &str,
    language: &str,
    words: &[String],
    sent: watch::Sender<bool>,
) -> Form {
    let file = Part::bytes(wav).file_name("audio.wav").mime_str("audio/wav").expect("valid MIME type");
    // The part after the file: its first read means the file has been handed to the connection.
    let model_body = reqwest::Body::wrap(Pin::new(Watched { body: reqwest::Body::from(model.to_string()), sent }));
    let mut form = Form::new()
        .part("file", file)
        .part("model", Part::stream_with_length(model_body, model.len() as u64))
        .text("response_format", "json");
    if !language.trim().is_empty() {
        let name = if uses_language_list(model) { "languages[]" } else { "language" };
        form = form.text(name, language.to_string());
    }
    if !words.is_empty() {
        if uses_keywords(model) {
            for word in words {
                form = form.text("keywords[]", word.clone());
            }
        } else if takes_prompt(model) {
            form = form.text("prompt", glossary_prompt(words));
        }
    }
    form
}

/// A body that marks `sent` when it is first read: every read goes through `deref_mut` (`Pin<P>`'s body impl).
struct Watched {
    body: reqwest::Body,
    sent: watch::Sender<bool>,
}

impl Deref for Watched {
    type Target = reqwest::Body;

    fn deref(&self) -> &reqwest::Body {
        &self.body
    }
}

impl DerefMut for Watched {
    fn deref_mut(&mut self) -> &mut reqwest::Body {
        self.sent.send_replace(true);
        &mut self.body
    }
}

/// `text` of a `response_format=json` response, or None if the body isn't that shape.
pub(crate) fn parse_transcription_text(body: &str) -> Option<String> {
    let body = serde_json::from_str::<Value>(body).ok()?;
    body.as_object()?.get("text")?.as_str().map(String::from)
}

#[cfg(test)]
mod tests;
