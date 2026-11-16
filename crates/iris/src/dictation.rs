/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Wires dictation together (Android `AppContainer.kt`, Handy's `lib.rs` setup): inputs (global shortcut, other
//! instances, SIGUSR2, the overlay's ✕) go to the hold-or-toggle coordinator, which starts and stops the dictation
//! controller; the controller's state drives the overlay, the cancel shortcut and the idle unload; a final transcript
//! is pasted. Settings reach the worker threads through a `watch` updated on every change.

use std::sync::{Arc, OnceLock};

use iris_core::dictation::DictationState;
use iris_core::dictation::controller::{Deps, DictationController};
use iris_core::dictation::coordinator::{Activation, Coordinator};
use iris_core::dictation::language::detect_language;
use iris_core::settings::Settings;
use iris_core::stt::local::store::ModelStore;
use iris_core::stt::{AudioSource, BoxFuture, OnPartial, SpeechToTextProvider, SttError, SttRequest};
use tokio::runtime::Runtime;
use tokio::sync::watch;

use crate::hotkey::{HotkeyEvent, Hotkeys};
use crate::local::engine::LocalEngine;
use crate::paste::{self, PasteOptions};
use crate::{capture, local};

#[derive(Clone)]
pub struct Dictation {
    pub controller: DictationController,
    pub coordinator: Coordinator,
    pub hotkeys: Hotkeys,
    pub store: ModelStore,
    pub engine: LocalEngine,
    pub settings: Arc<watch::Sender<Settings>>,
}

impl Dictation {
    pub fn new(runtime: &Runtime, settings: Settings) -> Self {
        let settings = Arc::new(watch::channel(settings).0);
        // The store waits for the engine to release a model before deleting it; the engine reads models from it.
        let engine_cell: Arc<OnceLock<LocalEngine>> = Arc::new(OnceLock::new());
        let release = engine_cell.clone();
        let store =
            ModelStore::new(crate::download_client(), crate::models_dir(), runtime.handle().clone(), move |id| {
                if let Some(engine) = release.get() {
                    engine.unload_model(id);
                }
            });
        let engine_settings = settings.clone();
        let engine = LocalEngine::new(
            store.clone(),
            Arc::new(move || {
                let s = engine_settings.borrow();
                (s.local_accelerator.clone(), s.on_device_unload_minutes)
            }),
        );
        let _ = engine_cell.set(engine.clone());

        let selection = settings.clone();
        let on_device: Arc<dyn SpeechToTextProvider> = Arc::new(engine.provider(move || {
            let s = selection.borrow();
            (s.on_device_stt_model.clone(), s.stt_language.clone())
        }));
        let data_dir = crate::project_dirs().data_local_dir().to_path_buf();
        let (audio_settings, session_settings, paste_settings) = (settings.clone(), settings.clone(), settings.clone());
        let controller = DictationController::new(
            runtime.handle().clone(),
            Deps {
                audio: Box::new(move |rate| capture::record(&audio_settings.borrow().microphone_device, rate)),
                stt: Box::new(move |s: &Settings| {
                    // Cloud providers come in M3.
                    if s.stt_on_device() { on_device.clone() } else { Arc::new(NotYetAvailable) }
                }),
                settings: Box::new(move || session_settings.borrow().clone()),
                on_final_transcript: Box::new(move |text| {
                    let options = PasteOptions::from_settings(&paste_settings.borrow());
                    Box::pin(async move {
                        let pasted = text.clone();
                        let notice = match tokio::task::spawn_blocking(move || paste::paste(&pasted, &options)).await {
                            Ok(Ok(())) => None,
                            Ok(Err(notice)) => Some(notice),
                            // The paste panicked: the text may not be on the clipboard yet.
                            Err(_) => {
                                let copied = text.clone();
                                Some(match tokio::task::spawn_blocking(move || paste::copy(&copied)).await {
                                    Ok(Ok(())) => paste::PASTE_FAILED.to_owned(),
                                    _ => paste::not_copied(&text),
                                })
                            }
                        };
                        DictationState::Done { text, notice }
                    })
                }),
                vad_model: Box::new(move || local::vad::scorer(&data_dir)),
                detect_language: Box::new(|text| {
                    Box::pin(
                        async move { tokio::task::spawn_blocking(move || detect_language(&text)).await.ok().flatten() },
                    )
                }),
            },
        );

        let (starts, stops) = (controller.clone(), controller.clone());
        let coordinator = Coordinator::spawn(move || starts.start(), move || stops.stop());
        let (keys_controller, keys_coordinator, keys_settings) =
            (controller.clone(), coordinator.clone(), settings.clone());
        let hotkeys = Hotkeys::start(move |event| match event {
            HotkeyEvent::Transcribe { pressed } => {
                let activation = Activation::from_setting(&keys_settings.borrow().shortcut_activation);
                keys_coordinator.key(pressed, activation);
            }
            HotkeyEvent::Cancel => cancel_listening(&keys_controller, &keys_coordinator),
        });
        let dictation = Self { controller, coordinator, hotkeys, store, engine, settings };
        runtime.spawn(dictation.clone().follow_state());
        dictation
    }

    /// The overlay ✕, `iris --cancel`, the cancel shortcut: discards a session that is still listening. A finalizing
    /// one can't be cancelled.
    pub fn cancel(&self) {
        cancel_listening(&self.controller, &self.coordinator);
    }

    /// Reacts to every state change: the cancel shortcut while listening, the coordinator's busy window, the idle
    /// unload, and the overlay (on the main thread).
    async fn follow_state(self) {
        let mut states = self.controller.state();
        let mut cancel_registered = false;
        loop {
            let state = states.borrow_and_update().clone();
            // The variant only: never log transcripts.
            log::debug!("Dictation: {}", format!("{state:?}").split([' ', '{']).next().unwrap_or_default());
            let listening = matches!(state, DictationState::Listening { .. });
            if listening != cancel_registered {
                let binding = listening.then(|| self.settings.borrow().shortcut_cancel.clone());
                self.hotkeys.set_cancel(binding.as_deref());
                cancel_registered = listening;
            }
            if matches!(state, DictationState::Done { .. } | DictationState::Error { .. }) {
                // A press remembered while finalizing may start the next session now.
                self.coordinator.notify_processing_finished();
            }
            self.engine
                .set_idle(!matches!(state, DictationState::Listening { .. } | DictationState::Finalizing { .. }));
            let _ = slint::invoke_from_event_loop(move || crate::ui::with_state(|s| s.overlay.update(&state)));
            if states.changed().await.is_err() {
                break;
            }
        }
    }
}

/// Checked and cancelled under the controller's lock, so a stop racing it can't leave the coordinator waiting for a
/// result that never comes.
fn cancel_listening(controller: &DictationController, coordinator: &Coordinator) {
    if controller.cancel_if_listening() {
        coordinator.notify_cancel(true);
    }
}

/// The cloud providers until M3: every session ends with this error before the microphone opens.
struct NotYetAvailable;

impl SpeechToTextProvider for NotYetAvailable {
    fn transcribe<'a>(
        &'a self,
        _audio: Box<dyn AudioSource + 'a>,
        _request: SttRequest,
        _on_partial: OnPartial,
    ) -> BoxFuture<'a, Result<String, SttError>> {
        Box::pin(async { Err(SttError("Cloud providers aren't available yet. Pick On device.".into())) })
    }
}
