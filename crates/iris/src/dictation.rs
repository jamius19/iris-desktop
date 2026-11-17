/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Wires dictation together (Android `AppContainer.kt`, Handy's `lib.rs` setup): inputs (global shortcut, other
//! instances, SIGUSR2, the overlay's ✕) go to the hold-or-toggle coordinator, which starts and stops the dictation
//! controller; the controller's state drives the overlay, the tray, the sounds, muting or lowering other audio, the
//! cancel shortcut and the idle unload; a final transcript is cleaned up (if enabled) and pasted. The controller, the
//! on-device provider, the engine's loads and the cleanup feed [`Statistics`]. Settings reach the worker threads
//! through a `watch` updated on every change.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use iris_core::audio::ducking::Duck;
use iris_core::cleanup::options::parse_provider_options;
use iris_core::cleanup::{CleanupRequest, timed_cleanup, with_glossary};
use iris_core::dictation::DictationState;
use iris_core::dictation::controller::{Deps, DictationController};
use iris_core::dictation::coordinator::{Activation, Coordinator};
use iris_core::dictation::language::detect_language;
use iris_core::dictation::statistics::Statistics;
use iris_core::settings::{Settings, default_prompt};
use iris_core::stt::SpeechToTextProvider;
use iris_core::stt::local::store::ModelStore;
use tokio::runtime::Runtime;
use tokio::sync::watch;

use crate::hotkey::{HotkeyEvent, Hotkeys};
use crate::local::engine::LocalEngine;
use crate::paste::{self, PasteOptions};
use crate::providers::Providers;
use crate::sounds::Sounds;
use crate::{capture, local};

#[derive(Clone)]
pub struct Dictation {
    pub controller: DictationController,
    pub coordinator: Coordinator,
    pub hotkeys: Hotkeys,
    pub store: ModelStore,
    pub engine: LocalEngine,
    pub providers: Providers,
    pub settings: Arc<watch::Sender<Settings>>,
    pub sounds: Sounds,
    pub statistics: Statistics,
}

impl Dictation {
    pub fn new(runtime: &Runtime, settings: Settings) -> Self {
        let settings = Arc::new(watch::channel(settings).0);
        let speed_settings = settings.clone();
        let statistics = Statistics::new(
            crate::project_dirs().config_dir().join("statistics.json"),
            runtime.handle().clone(),
            Box::new(move || speed_settings.borrow().stt_speed_percent),
        );
        // The store waits for the engine to release a model before deleting it; the engine reads models from it.
        let engine_cell: Arc<OnceLock<LocalEngine>> = Arc::new(OnceLock::new());
        let release = engine_cell.clone();
        let store =
            ModelStore::new(crate::download_client(), crate::models_dir(), runtime.handle().clone(), move |id| {
                if let Some(engine) = release.get() {
                    engine.unload_model(id);
                }
            });
        let (engine_settings, loads) = (settings.clone(), statistics.clone());
        let engine = LocalEngine::new(
            store.clone(),
            Arc::new(move || {
                let s = engine_settings.borrow();
                (s.local_accelerator.clone(), s.on_device_unload_minutes)
            }),
            Box::new(move |id, ms| loads.record_load(id, ms)),
        );
        let _ = engine_cell.set(engine.clone());

        let (selection, on_device_stats) = (settings.clone(), statistics.clone());
        let on_device: Arc<dyn SpeechToTextProvider> = Arc::new(engine.provider(
            move || {
                let s = selection.borrow();
                (s.on_device_stt_model.clone(), s.stt_language.clone())
            },
            Box::new(move |id, audio_ms, decode_ms, wait_ms| {
                on_device_stats.record_on_device(id, audio_ms, decode_ms, wait_ms)
            }),
        ));
        let providers = Providers::new(runtime.handle(), settings.clone(), on_device);
        let data_dir = crate::project_dirs().data_local_dir().to_path_buf();
        let (audio_settings, session_settings, paste_settings) = (settings.clone(), settings.clone(), settings.clone());
        let (stt_providers, warm_providers, final_providers) =
            (providers.clone(), providers.clone(), providers.clone());
        let warm_runtime = runtime.handle().clone();
        let (dictation_stats, cleanup_stats) = (statistics.clone(), statistics.clone());
        let controller = DictationController::new(
            runtime.handle().clone(),
            Deps {
                audio: Box::new(move |rate| capture::record(&audio_settings.borrow().microphone_device, rate)),
                stt: Box::new(move |s: &Settings| stt_providers.speech_to_text(s)),
                settings: Box::new(move || session_settings.borrow().clone()),
                // The stop comes from the coordinator's thread: the warm-up's request needs the runtime.
                warm_up_cleanup: Box::new(move |s: &Settings| {
                    let _runtime = warm_runtime.enter();
                    warm_providers.cleanup(s).warm_up();
                }),
                on_final_transcript: Box::new(move |text| {
                    let settings = paste_settings.borrow().clone();
                    Box::pin(deliver(text, settings, final_providers.clone(), cleanup_stats.clone()))
                }),
                vad_model: Box::new(move || local::vad::scorer_off_thread(data_dir.clone())),
                detect_language: Box::new(|text| {
                    Box::pin(
                        async move { tokio::task::spawn_blocking(move || detect_language(&text)).await.ok().flatten() },
                    )
                }),
                record: Box::new(move |s, d| dictation_stats.record_dictation(s, d)),
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
        let sounds = Sounds::start();
        let dictation =
            Self { controller, coordinator, hotkeys, store, engine, providers, settings, sounds, statistics };
        runtime.spawn(dictation.clone().follow_state());
        dictation
    }

    /// The overlay ✕, `iris --cancel`, the cancel shortcut: discards a session that is still listening. A finalizing
    /// one can't be cancelled.
    pub fn cancel(&self) {
        cancel_listening(&self.controller, &self.coordinator);
    }

    /// Reacts to every state change: the cancel shortcut, the start/stop sounds and other audio, the coordinator's busy
    /// window, the idle unload, and the overlay and tray (on the main thread).
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
                let settings = self.settings.borrow();
                for step in feedback(listening, &settings) {
                    match step {
                        Feedback::Play(start) => self.sounds.play(start, settings.feedback_volume_percent),
                        Feedback::Duck(duck) => self.sounds.duck(duck),
                        Feedback::Restore => self.sounds.restore(),
                    }
                }
            }
            if matches!(state, DictationState::Done { .. } | DictationState::Error { .. }) {
                // A press remembered while finalizing may start the next session now.
                self.coordinator.notify_processing_finished();
            }
            self.engine
                .set_idle(!matches!(state, DictationState::Listening { .. } | DictationState::Finalizing { .. }));
            let _ = slint::invoke_from_event_loop(move || {
                crate::ui::with_state(|s| {
                    s.overlay.update(&state);
                    if let Some(tray) = &s.tray {
                        tray.set_state(&state);
                    }
                })
            });
            if states.changed().await.is_err() {
                break;
            }
        }
    }
}

#[derive(Debug, PartialEq)]
enum Feedback {
    /// The start sound (true) or the stop sound.
    Play(bool),
    Duck(Duck),
    Restore,
}

/// On entering Listening (`listening`) or leaving it. Handy's timing: the start sound on entering, the stop sound on
/// leaving. Other audio is muted or lowered after the start sound, so it's heard, and put back before the stop sound;
/// always put back, so turning the setting off mid-dictation still restores.
fn feedback(listening: bool, settings: &Settings) -> Vec<Feedback> {
    let sound = settings.feedback_sounds.then_some(Feedback::Play(listening));
    if listening {
        sound.into_iter().chain(Duck::from_settings(settings).map(Feedback::Duck)).collect()
    } else {
        std::iter::once(Feedback::Restore).chain(sound).collect()
    }
}

/// Checked and cancelled under the controller's lock, so a stop racing it can't leave the coordinator waiting for a
/// result that never comes.
fn cancel_listening(controller: &DictationController, coordinator: &Coordinator) {
    if controller.cancel_if_listening() {
        coordinator.notify_cancel(true);
    }
}

/// Android `IrisAccessibilityService.onFinalTranscript`: the optional LLM cleanup, then the paste. A failed or
/// timed-out cleanup pastes the original transcript, and a failed paste leaves the text on the clipboard, so the
/// dictation is never lost.
async fn deliver(
    transcript: String,
    settings: Settings,
    providers: Providers,
    statistics: Statistics,
) -> DictationState {
    let mut text = transcript.clone();
    let mut cleanup_error = None;
    let mut cleanup_timed_out = false;
    if settings.cleanup_enabled {
        let started = std::time::Instant::now();
        match cleanup(transcript, &settings, &providers, &statistics).await {
            Some(Ok(cleaned)) => text = cleaned,
            Some(Err(e)) => cleanup_error = Some(e),
            None => {
                cleanup_timed_out = true;
                cleanup_error = Some("Cleanup timeout.".to_string());
            }
        }
        let outcome = if cleanup_timed_out {
            " timed out"
        } else if cleanup_error.is_some() {
            " failed"
        } else {
            ""
        };
        log::debug!("Timing: cleanup {} ms{outcome}", started.elapsed().as_millis());
    }
    let options = PasteOptions::from_settings(&settings);
    let pasted = text.clone();
    let paste_notice = match tokio::task::spawn_blocking(move || paste::paste(&pasted, &options)).await {
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
    if paste_notice.as_deref().is_some_and(paste::is_not_copied) {
        crate::ui::recovery::keep(text.clone());
    }
    let notice = done_notice(cleanup_error, cleanup_timed_out, paste_notice);
    DictationState::Done { text, notice }
}

/// The cleaned text, a user-readable failure, or None on timeout (Android `timedCleanup`).
async fn cleanup(
    text: String,
    settings: &Settings,
    providers: &Providers,
    statistics: &Statistics,
) -> Option<Result<String, String>> {
    // Validated when saved; a stored value that no longer parses fails like any other unexpected error.
    let Ok(provider_options) = parse_provider_options(settings.active_cleanup_provider_options()) else {
        return Some(Err("Cleanup failed".into()));
    };
    let prompt = settings.active_prompt.clone().unwrap_or_else(default_prompt);
    let request = CleanupRequest {
        text,
        system_prompt: with_glossary(&prompt.system_prompt, settings.active_custom_words()),
        model: settings.active_cleanup_model().to_string(),
        reasoning_effort: settings.active_cleanup_reasoning_effort().map(str::to_string),
        provider_options,
    };
    let provider = providers.cleanup(settings);
    let timeout = Duration::from_secs(settings.cleanup_timeout_seconds.max(0) as u64);
    let record = |took: Duration| statistics.record_cleanup(settings, took.as_millis() as i64);
    let result = timed_cleanup(timeout, record, provider.cleanup(request)).await?;
    Some(result.map_err(|e| e.0))
}

/// Android `doneNotice` for the desktop paste: a cleanup failure, then what happened to the text. None when the
/// cleaned (or, with cleanup off, the original) text was pasted.
fn done_notice(cleanup_error: Option<String>, cleanup_timed_out: bool, paste_notice: Option<String>) -> Option<String> {
    if cleanup_timed_out && paste_notice.is_none() {
        return Some("Cleanup timeout. Pasted original text.".into());
    }
    let delivery = paste_notice.or_else(|| cleanup_error.is_some().then(|| "Inserted the original text".into()));
    let lines: Vec<String> = cleanup_error.into_iter().chain(delivery).collect();
    (!lines.is_empty()).then(|| lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use iris_core::audio::ducking::Duck;
    use iris_core::settings::{Values, to_settings};

    use super::{Feedback, done_notice, feedback};

    #[test]
    fn feedback_order() {
        let mut settings = to_settings(&Values::new(), &Default::default()).unwrap();
        assert_eq!(Vec::<Feedback>::new(), feedback(true, &settings));
        assert_eq!(vec![Feedback::Restore], feedback(false, &settings));

        settings.feedback_sounds = true;
        assert_eq!(vec![Feedback::Play(true)], feedback(true, &settings));
        assert_eq!(vec![Feedback::Restore, Feedback::Play(false)], feedback(false, &settings));

        settings.duck_while_recording = true;
        assert_eq!(vec![Feedback::Play(true), Feedback::Duck(Duck::Lower(50))], feedback(true, &settings));
        assert_eq!(vec![Feedback::Restore, Feedback::Play(false)], feedback(false, &settings));

        settings.feedback_sounds = false;
        settings.mute_while_recording = true;
        assert_eq!(vec![Feedback::Duck(Duck::Mute)], feedback(true, &settings));
        assert_eq!(vec![Feedback::Restore], feedback(false, &settings));
    }

    #[test]
    fn done_notices() {
        let error = || Some("AI Gateway rate limit or quota reached".to_string());
        let paste_failed = || Some(crate::paste::PASTE_FAILED.to_string());
        assert_eq!(None, done_notice(None, false, None));
        assert_eq!(paste_failed(), done_notice(None, false, paste_failed()));
        assert_eq!(
            Some("Cleanup timeout. Pasted original text.".into()),
            done_notice(Some("Cleanup timeout.".into()), true, None)
        );
        assert_eq!(
            Some("Cleanup timeout.\nCouldn't paste. The text is on the clipboard.".into()),
            done_notice(Some("Cleanup timeout.".into()), true, paste_failed())
        );
        assert_eq!(
            Some("AI Gateway rate limit or quota reached\nInserted the original text".into()),
            done_notice(error(), false, None)
        );
        assert_eq!(
            Some("AI Gateway rate limit or quota reached\nCouldn't paste. The text is on the clipboard.".into()),
            done_notice(error(), false, paste_failed())
        );
    }
}
