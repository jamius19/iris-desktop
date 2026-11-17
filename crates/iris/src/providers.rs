/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The speech-to-text and cleanup providers (Android `AppContainer.kt`): one instance of each, sharing one HTTP
//! client, with API keys read from the keychain per call. The provider for a session's settings is a plain match.

use std::sync::Arc;

use iris_core::cleanup::compatible::OpenAiCompatibleCleanup;
use iris_core::cleanup::openrouter::OpenRouterCleanup;
use iris_core::cleanup::vercel::VercelGatewayCleanup;
use iris_core::cleanup::{CleanupError, CleanupProvider, GatewayModel};
use iris_core::net::{self, KeySource};
use iris_core::settings::secrets::{self, custom_cleanup_key};
use iris_core::settings::{CustomCleanupProvider, SecretStore, Settings};
use iris_core::stt::deepgram::{DeepgramFileTranscription, DeepgramSpeechToText};
use iris_core::stt::openai::protocol::is_realtime_transcription_model;
use iris_core::stt::openai::{OpenAiFileTranscription, OpenAiRealtimeSpeechToText, list_openai_transcription_models};
use iris_core::stt::soniox::{
    ModelInfo, SonioxCatalog, SonioxFileTranscription, SonioxRegion, SonioxSpeechToText, list_soniox_models,
    soniox_region, soniox_streams,
};
use iris_core::stt::vercel::{VercelGatewayFileTranscription, VercelGatewaySpeechToText};
use iris_core::stt::{SpeechToTextProvider, SttError};
use tokio::runtime::Handle;
use tokio::sync::watch;

type Stt = Arc<dyn SpeechToTextProvider>;

#[derive(Clone)]
pub struct Providers {
    client: reqwest::Client,
    on_device: Stt,
    openai: Stt,
    openai_file: Stt,
    gateway: Stt,
    gateway_file: Stt,
    deepgram: Stt,
    deepgram_file: Stt,
    /// One pair per region (a provider's URLs are fixed): real-time, async.
    soniox: Arc<Vec<(SonioxRegion, Stt, Stt)>>,
    /// Soniox's model catalogue for the stored key and region: the settings' menu and the sessions (mode and
    /// strict-hints check). Reload it when the key or region changes.
    pub soniox_catalog: SonioxCatalog,
    gateway_cleanup: Arc<VercelGatewayCleanup>,
    openrouter_cleanup: Arc<OpenRouterCleanup>,
}

/// The keychain entry `name`, read on each call.
fn key(name: impl Into<String>) -> KeySource {
    let name = name.into();
    Arc::new(move || SecretStore::new().get(&name))
}

impl Providers {
    pub fn new(runtime: &Handle, settings: Arc<watch::Sender<Settings>>, on_device: Stt) -> Self {
        let client = net::http_client();
        let region = move || soniox_region(&settings.borrow().soniox_region);
        let list_client = client.clone();
        let soniox_catalog = SonioxCatalog::new(
            runtime.clone(),
            Arc::new(move || {
                let (client, base_url) = (list_client.clone(), region().rest_url());
                Box::pin(async move {
                    let api_key = net::api_key(&key(secrets::SONIOX_API_KEY)).await.map_err(|e| SttError(e.into()))?;
                    list_soniox_models(&client, api_key, base_url).await
                })
            }),
        );
        let catalog = soniox_catalog.clone();
        let model_info: ModelInfo = Arc::new(move |model| {
            let catalog = catalog.clone();
            Box::pin(async move { catalog.entry(&model).await })
        });
        let soniox = SonioxRegion::ALL
            .into_iter()
            .map(|region| {
                let api_key = key(secrets::SONIOX_API_KEY);
                let stream: Stt = Arc::new(SonioxSpeechToText::new(api_key.clone(), model_info.clone(), region));
                let file: Stt =
                    Arc::new(SonioxFileTranscription::new(client.clone(), api_key, model_info.clone(), region));
                (region, stream, file)
            })
            .collect();
        let (openai_key, gateway_key, deepgram_key) =
            (key(secrets::OPENAI_API_KEY), key(secrets::GATEWAY_API_KEY), key(secrets::DEEPGRAM_API_KEY));
        Self {
            on_device,
            openai: Arc::new(OpenAiRealtimeSpeechToText::new(openai_key.clone())),
            openai_file: Arc::new(OpenAiFileTranscription::new(client.clone(), openai_key)),
            // Both share the API key with cleanup.
            gateway: Arc::new(VercelGatewaySpeechToText::new(gateway_key.clone())),
            gateway_file: Arc::new(VercelGatewayFileTranscription::new(client.clone(), gateway_key.clone())),
            deepgram: Arc::new(DeepgramSpeechToText::new(deepgram_key.clone())),
            deepgram_file: Arc::new(DeepgramFileTranscription::new(client.clone(), deepgram_key)),
            soniox: Arc::new(soniox),
            soniox_catalog,
            gateway_cleanup: Arc::new(VercelGatewayCleanup::new(client.clone(), gateway_key)),
            openrouter_cleanup: Arc::new(OpenRouterCleanup::new(client.clone(), key(secrets::OPENROUTER_API_KEY))),
            client,
        }
    }

    /// The speech-to-text provider `settings` select: On device, or a cloud one whose model picks streaming or upload
    /// after stop (OpenAI: the Realtime models stream; Gateway: the catalogue's tag, stored at save). Deepgram: every
    /// model streams, so its own setting picks the cheaper upload instead. Soniox: the catalogue's
    /// `transcription_mode` (id rule `stt-rt-*` until it loads), on the region's domains.
    pub fn speech_to_text(&self, settings: &Settings) -> Stt {
        if settings.stt_on_device() {
            self.on_device.clone()
        } else if settings.stt_soniox() {
            let region = soniox_region(&settings.soniox_region);
            let (_, stream, file) = self.soniox.iter().find(|(r, _, _)| *r == region).expect("every region");
            let models = self.soniox_catalog.state().borrow().models.clone();
            if soniox_streams(&settings.soniox_stt_model, models.as_deref()) { stream.clone() } else { file.clone() }
        } else if settings.stt_deepgram() {
            if settings.deepgram_upload { self.deepgram_file.clone() } else { self.deepgram.clone() }
        } else if settings.stt_gateway() {
            if settings.gateway_stt_live { self.gateway.clone() } else { self.gateway_file.clone() }
        } else if is_realtime_transcription_model(&settings.stt_model) {
            self.openai.clone()
        } else {
            self.openai_file.clone()
        }
    }

    /// Loads the stored on-device model ahead of the next dictation (if downloaded); returns at once.
    pub fn warm_up_on_device(&self) {
        self.on_device.warm_up();
    }

    /// The cleanup provider `settings` select: a custom one, OpenRouter, else the Gateway.
    pub fn cleanup(&self, settings: &Settings) -> Arc<dyn CleanupProvider> {
        match settings.active_custom_cleanup() {
            Some(custom) => self.custom_cleanup(custom),
            None if settings.cleanup_open_router() => self.openrouter_cleanup.clone(),
            None => self.gateway_cleanup.clone(),
        }
    }

    /// A new instance per call is cheap; it shares the pool, so the warm-up connection is reused.
    fn custom_cleanup(&self, p: &CustomCleanupProvider) -> Arc<dyn CleanupProvider> {
        Arc::new(OpenAiCompatibleCleanup::new(
            self.client.clone(),
            &p.name,
            &p.base_url,
            p.send_json_schema,
            key(custom_cleanup_key(&p.id)),
        ))
    }

    /// Vercel AI Gateway's catalogue (cleanup and transcription models), for the settings' model menus.
    pub async fn gateway_models(&self) -> Result<Vec<GatewayModel>, CleanupError> {
        self.gateway_cleanup.list_models().await
    }

    /// OpenRouter's structured-output chat models, for the settings' cleanup model menu.
    pub async fn openrouter_models(&self) -> Result<Vec<GatewayModel>, CleanupError> {
        self.openrouter_cleanup.list_models().await
    }

    /// A custom provider's models (listed with its key), for the settings' cleanup model menu.
    pub async fn custom_cleanup_models(&self, p: &CustomCleanupProvider) -> Result<Vec<GatewayModel>, CleanupError> {
        self.custom_cleanup(p).list_models().await
    }

    /// The OpenAI transcription models the stored key can use, for the settings' model menu.
    pub async fn openai_models(&self) -> Result<Vec<String>, SttError> {
        let api_key = net::api_key(&key(secrets::OPENAI_API_KEY)).await.map_err(|e| SttError(e.into()))?;
        list_openai_transcription_models(&self.client, api_key).await
    }
}
