/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The settings' model menus (Android `ModelCatalogs.kt` and the loads `SettingsApp.kt` starts): Vercel AI Gateway's
//! catalogue (Gateway STT model, cleanup model and reasoning level), OpenRouter's, OpenAI's transcription models
//! (with the key) and one custom cleanup provider's (its key; one slot, for the selected provider). In memory only,
//! one load at a time each. Soniox's is app-wide (`Providers::soniox_catalog`); Deepgram's is a static list.

use std::future::Future;

use iris_core::cleanup::GatewayModel;
use iris_core::cleanup::protocol::is_chat_model;
use iris_core::copy::reasoning_effort_label;
use iris_core::settings::{CustomCleanupProvider, REASONING_EFFORTS, Settings};
use iris_core::stt::deepgram::protocol::DEEPGRAM_MODELS;
use iris_core::stt::openai::protocol::is_realtime_transcription_model;
use iris_core::stt::soniox::soniox_streams;
use iris_core::stt::vercel::protocol::{is_streaming_transcription_model, is_transcription_model};
use slint::ComponentHandle;

use super::{AppState, model, option, refresh, with_state};
use crate::{ModelCatalog, Prefs, SelectOption};

/// One catalogue: its models once loaded, whether a load runs and how the last one failed.
pub struct Slot<T> {
    pub models: Option<T>,
    loading: bool,
    error: Option<String>,
    /// Bumped by every load and clear: an older load's result is dropped.
    generation: u64,
}

impl<T> Default for Slot<T> {
    fn default() -> Self {
        Self { models: None, loading: false, error: None, generation: 0 }
    }
}

impl<T> Slot<T> {
    fn clear(&mut self) {
        *self = Self { generation: self.generation + 1, ..Self::default() };
    }

    /// Not loaded, not loading and not failed: the automatic loads below start it.
    fn idle(&self) -> bool {
        self.models.is_none() && !self.loading && self.error.is_none()
    }
}

#[derive(Default)]
pub struct Catalogs {
    pub gateway: Slot<Vec<GatewayModel>>,
    openrouter: Slot<Vec<GatewayModel>>,
    openai: Slot<Vec<String>>,
    custom: Slot<Vec<GatewayModel>>,
    /// The provider the custom slot belongs to.
    custom_id: Option<String>,
}

/// Loads into the slot `slot` picks unless a load runs; the result lands on the main thread (dropped if the slot was
/// cleared or reloaded meanwhile) and refreshes the pages.
fn load<T: Send + 'static>(
    state: &mut AppState,
    slot: fn(&mut Catalogs) -> &mut Slot<T>,
    work: impl Future<Output = Result<T, String>> + Send + 'static,
) {
    let current = slot(&mut state.catalogs);
    if current.loading {
        return;
    }
    current.loading = true;
    current.error = None;
    current.generation += 1;
    let generation = current.generation;
    state.runtime.spawn(async move {
        let result = work.await;
        let _ = slint::invoke_from_event_loop(move || {
            with_state(|state| {
                let current = slot(&mut state.catalogs);
                if current.generation != generation {
                    return;
                }
                current.loading = false;
                match result {
                    Ok(models) => current.models = Some(models),
                    Err(error) => current.error = Some(error),
                }
                refresh(state);
            })
        });
    });
}

fn load_gateway(state: &mut AppState) {
    let providers = state.dictation.providers.clone();
    load(state, |c| &mut c.gateway, async move { providers.gateway_models().await.map_err(|e| e.0) });
}

fn load_openrouter(state: &mut AppState) {
    let providers = state.dictation.providers.clone();
    load(state, |c| &mut c.openrouter, async move { providers.openrouter_models().await.map_err(|e| e.0) });
}

fn load_openai(state: &mut AppState) {
    let providers = state.dictation.providers.clone();
    load(state, |c| &mut c.openai, async move { providers.openai_models().await.map_err(|e| e.0) });
}

/// Loads `p`'s models into the custom slot; another provider's list (or load) is dropped first.
fn load_custom(state: &mut AppState, p: &CustomCleanupProvider) {
    if state.catalogs.custom_id.as_deref() != Some(p.id.as_str()) {
        state.catalogs.custom.clear();
        state.catalogs.custom_id = Some(p.id.clone());
    }
    let (providers, p) = (state.dictation.providers.clone(), p.clone());
    load(state, |c| &mut c.custom, async move { providers.custom_cleanup_models(&p).await.map_err(|e| e.0) });
}

/// The OpenAI list depends on the key: a key change drops it (a running load's result is never written); the next
/// refresh reloads it while OpenAI is selected.
pub fn openai_key_changed(state: &mut AppState) {
    state.catalogs.openai.clear();
}

/// A custom provider's URL or key may have changed: drops its list (reloaded while it is selected).
pub fn custom_provider_saved(state: &mut AppState, id: &str) {
    if state.catalogs.custom_id.as_deref() == Some(id) {
        state.catalogs.custom.clear();
    }
}

/// A backup was restored (Android `catalogGeneration++`): every list is dropped (reloaded while its provider is
/// selected), Soniox's reloaded.
pub fn restored(state: &mut AppState) {
    let catalogs = &mut state.catalogs;
    catalogs.gateway.clear();
    catalogs.openrouter.clear();
    catalogs.openai.clear();
    catalogs.custom.clear();
    catalogs.custom_id = None;
    state.dictation.providers.soniox_catalog.reload();
}

/// The automatic loads (Android `SettingsApp`'s effects): the Gateway catalogue while Gateway cleanup or
/// transcription is on, OpenRouter's while OpenRouter cleanup is, the selected custom provider's while it is selected
/// with cleanup on, OpenAI's while OpenAI transcription is, Soniox's while Soniox is. A failed load waits for the
/// refresh button.
fn auto_load(state: &mut AppState, settings: &Settings) {
    let custom = settings.active_custom_cleanup().filter(|_| settings.cleanup_enabled);
    let cleanup_gateway = settings.cleanup_enabled && !settings.cleanup_open_router() && custom.is_none();
    if (cleanup_gateway || settings.stt_gateway()) && state.catalogs.gateway.idle() {
        load_gateway(state);
    }
    if settings.cleanup_enabled && settings.cleanup_open_router() && state.catalogs.openrouter.idle() {
        load_openrouter(state);
    }
    if let Some(custom) = custom {
        let own = state.catalogs.custom_id.as_deref() == Some(custom.id.as_str());
        if !own || state.catalogs.custom.idle() {
            load_custom(state, custom);
        }
    }
    if stt_openai(settings) && state.catalogs.openai.idle() {
        load_openai(state);
    }
    let soniox = state.dictation.providers.soniox_catalog.state().borrow().clone();
    if settings.stt_soniox() && soniox.models.is_none() && !soniox.loading && soniox.error.is_none() {
        state.dictation.providers.soniox_catalog.refresh();
    }
}

fn stt_openai(settings: &Settings) -> bool {
    !settings.stt_on_device() && !settings.stt_gateway() && !settings.stt_deepgram() && !settings.stt_soniox()
}

fn catalog<T>(slot: &Slot<T>, ids: impl Iterator<Item = SelectOption>) -> ModelCatalog {
    ModelCatalog {
        models: model(ids.collect()),
        refreshable: true,
        loaded: slot.models.is_some(),
        loading: slot.loading,
        error: slot.error.clone().unwrap_or_default().into(),
    }
}

fn streaming(id: &str, streams: bool) -> SelectOption {
    SelectOption { streams, ..option(id, id) }
}

/// Fills the model menus and the reasoning levels for the current settings, then starts any automatic load.
pub fn fill(state: &mut AppState, settings: &Settings) {
    auto_load(state, settings);
    let prefs = state.window.global::<Prefs>();
    let catalogs = &state.catalogs;
    let soniox = state.dictation.providers.soniox_catalog.state().borrow().clone();

    let stt = if settings.stt_gateway() {
        let models = catalogs.gateway.models.iter().flatten().filter(|m| is_transcription_model(m));
        catalog(&catalogs.gateway, models.map(|m| streaming(&m.id, is_streaming_transcription_model(m))))
    } else if settings.stt_deepgram() {
        let models = DEEPGRAM_MODELS.iter().map(|id| streaming(id, !settings.deepgram_upload));
        ModelCatalog { models: model(models.collect()), refreshable: false, loaded: true, ..Default::default() }
    } else if settings.stt_soniox() {
        let models = soniox.models.as_deref();
        ModelCatalog {
            models: model(
                models.into_iter().flatten().map(|m| streaming(&m.id, soniox_streams(&m.id, models))).collect(),
            ),
            refreshable: true,
            loaded: models.is_some(),
            loading: soniox.loading,
            error: soniox.error.clone().unwrap_or_default().into(),
        }
    } else {
        let models = catalogs.openai.models.iter().flatten();
        catalog(&catalogs.openai, models.map(|id| streaming(id, is_realtime_transcription_model(id))))
    };
    prefs.set_stt_catalog(stt);
    prefs
        .set_soniox_live(settings.stt_soniox() && soniox_streams(&settings.soniox_stt_model, soniox.models.as_deref()));

    let custom = settings.active_custom_cleanup();
    let (slot, chat_only) = match custom {
        Some(p) if catalogs.custom_id.as_deref() == Some(p.id.as_str()) => (Some(&catalogs.custom), false),
        Some(_) => (None, false),
        None if settings.cleanup_open_router() => (Some(&catalogs.openrouter), false),
        None => (Some(&catalogs.gateway), true),
    };
    let cleanup_models: Option<&Vec<GatewayModel>> = slot.and_then(|s| s.models.as_ref());
    let cleanup = match slot {
        Some(slot) => {
            let models = cleanup_models.into_iter().flatten().filter(|m| !chat_only || is_chat_model(m));
            catalog(slot, models.map(|m| option(&m.id, &m.id)))
        }
        None => ModelCatalog { refreshable: true, ..Default::default() },
    };
    prefs.set_cleanup_catalog(cleanup);

    // "Model default" plus the saved model's efforts, or every level the APIs accept when that's unknown.
    let effort = settings.active_cleanup_reasoning_effort();
    let efforts = cleanup_models
        .and_then(|models| models.iter().find(|m| m.id == settings.active_cleanup_model()))
        .and_then(|m| m.reasoning_efforts.clone());
    let mut options = vec![option("", &reasoning_effort_label(None))];
    let listed = efforts.clone().unwrap_or_else(|| REASONING_EFFORTS.iter().map(|e| e.to_string()).collect());
    options.extend(listed.iter().map(|e| option(e, &reasoning_effort_label(Some(e)))));
    prefs.set_reasoning_options(model(options));
    let unlisted = effort.is_some_and(|e| efforts.is_some_and(|efforts| !efforts.iter().any(|x| x == e)));
    prefs.set_reasoning_error(if unlisted { "Not listed for this model" } else { "" }.into());
}

pub fn wire(window: &crate::MainWindow) {
    let prefs = window.global::<Prefs>();
    prefs.on_load_stt_models(|| {
        with_state(|state| {
            let settings = state.store.settings(&state.stored_secrets);
            if settings.stt_gateway() {
                load_gateway(state);
            } else if settings.stt_soniox() {
                state.dictation.providers.soniox_catalog.refresh();
            } else if stt_openai(&settings) {
                load_openai(state);
            }
            refresh(state);
        })
    });
    prefs.on_load_cleanup_models(|| {
        with_state(|state| {
            let settings = state.store.settings(&state.stored_secrets);
            match settings.active_custom_cleanup() {
                Some(p) => load_custom(state, &p.clone()),
                None if settings.cleanup_open_router() => load_openrouter(state),
                None => load_gateway(state),
            }
            refresh(state);
        })
    });
}
