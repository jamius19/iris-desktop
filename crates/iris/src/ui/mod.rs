/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Glue between the Slint globals and `iris_core`: fills `Prefs` from the settings after every change and saves what
//! the pages send. Everything here runs on the main (Slint) thread; keychain and network work goes to the runtime and
//! comes back with `invoke_from_event_loop`.

mod about;
mod backup;
mod catalogs;
mod keys;
mod post_processing;
pub mod recovery;
mod shortcuts;
mod statistics;
mod transcription;
mod update;

use std::cell::RefCell;
use std::collections::HashSet;
use std::io;
use std::rc::Rc;

use iris_core::copy::{self, SAVE_ERROR};
use iris_core::settings::{SecretStore, Settings, Store, secrets};
use iris_core::stt::local::models;
use iris_core::stt::local::store::StoreState;
use iris_core::stt::vercel::protocol::gateway_stt_live;
use iris_core::ui_math::{self, Commit};
use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio::runtime::Runtime;

use crate::autostart;
use crate::dictation::Dictation;
use crate::favicons::Favicons;
use crate::overlay::OverlayHost;
use crate::services::{self, ServiceStatus};
use crate::tray::{self, Tray};
use crate::{App, CommitResult, Copy, MainWindow, Nav, Placement, Prefs, SelectOption, TextStyle, Type, UiMath};

/// The main-thread state behind the window.
pub struct AppState {
    pub window: MainWindow,
    pub runtime: Rc<Runtime>,
    pub store: Store,
    /// Keychain entries known to exist (keychains can't be listed: probed at start, then kept up to date).
    pub stored_secrets: HashSet<String>,
    /// Keychain entries the start probe couldn't read (their fields say so instead of "Not set").
    pub unreadable_secrets: HashSet<String>,
    /// The unsaved custom cleanup provider, if any.
    pub draft_id: Option<String>,
    pub favicons: Favicons,
    pub services: ServiceStatus,
    pub key_op_seq: i32,
    pub dictation: Dictation,
    /// The recording overlay (Windows: a window of this process; Linux: the helper).
    pub overlay: OverlayHost,
    /// The settings' model menus.
    pub catalogs: catalogs::Catalogs,
    /// The on-device (model, language) last warmed up, None while another provider is selected or the model isn't
    /// downloaded.
    pub warmed: Option<(String, String)>,
    /// None with `--no-tray` (or if it couldn't be created).
    pub tray: Option<Tray>,
    pub backup: backup::BackupState,
}

thread_local! {
    static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
}

/// Runs `f` with the state. Never call it re-entrantly (callbacks run one at a time on the main thread).
pub fn with_state<R>(f: impl FnOnce(&mut AppState) -> R) -> R {
    STATE.with(|state| f(state.borrow_mut().as_mut().expect("state not set")))
}

/// Runs the app until Quit (without a tray: until the window closes); Err if the event loop couldn't start or failed
/// (e.g. no GL for femtovg).
pub fn run(start_hidden: bool, no_tray: bool) -> Result<(), slint::PlatformError> {
    let runtime = Rc::new(
        tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime"),
    );
    let window = MainWindow::new()?;
    // Before the first show: GNOME matches the window to iris.desktop by this id, else it docks a generic "iris".
    slint::set_xdg_app_id("iris")?;
    let store = Store::open(Store::default_path());
    // Re-registers a login item whose app moved or whose settings changed elsewhere, removes a stale one.
    if let Err(e) = autostart::apply(&store.settings(&HashSet::new())) {
        log::warn!("Updating the login item failed: {e}");
    }
    let dictation = Dictation::new(&runtime, store.settings(&HashSet::new()));
    let cancel = dictation.clone();
    let overlay = OverlayHost::new(move || cancel.cancel());
    let tray = if no_tray {
        None
    } else {
        Tray::new(&window, &dictation).inspect_err(|e| log::warn!("Creating the tray failed: {e}")).ok()
    };
    let has_tray = tray.is_some();
    STATE.with(|state| {
        *state.borrow_mut() = Some(AppState {
            window: window.clone_strong(),
            runtime: runtime.clone(),
            store,
            stored_secrets: HashSet::new(),
            unreadable_secrets: HashSet::new(),
            draft_id: None,
            favicons: Favicons::new(),
            services: ServiceStatus::default(),
            key_op_seq: 0,
            dictation: dictation.clone(),
            overlay,
            catalogs: catalogs::Catalogs::default(),
            warmed: None,
            tray,
            backup: backup::BackupState::default(),
        })
    });

    wire_helpers(&window);
    wire_setters(&window);
    keys::wire(&window);
    post_processing::wire(&window);
    catalogs::wire(&window);
    shortcuts::wire(&window);
    recovery::wire(&window);
    about::fill(&window);
    services::wire(&window);
    statistics::wire(&window, &runtime, &dictation);
    backup::wire(&window);
    update::wire(&window);
    wire_dictation(&window, &runtime, &dictation);
    with_state(shortcuts::register);

    // The mouse back button goes back, like Esc and Alt+Left. Home's checks rerun when the window gets focus.
    let weak = window.as_weak();
    window.window().on_winit_window_event(move |_, event| {
        match event {
            winit::event::WindowEvent::MouseInput {
                state: winit::event::ElementState::Pressed,
                button: winit::event::MouseButton::Back,
                ..
            } => {
                if let Some(window) = weak.upgrade() {
                    window.invoke_go_back();
                }
            }
            winit::event::WindowEvent::Focused(focused) => {
                log::debug!("Window focused: {focused}");
                if *focused {
                    services::check();
                }
            }
            _ => {}
        }
        EventResult::Propagate
    });

    // Closing hides the window to the tray (quits without one); a focused text field commits first.
    let weak = window.as_weak();
    window.window().on_close_requested(move || {
        if let Some(window) = weak.upgrade() {
            if has_tray && tray::host_present() {
                window.invoke_focus_root();
            } else {
                quit(&window);
            }
        }
        slint::CloseRequestResponse::HideWindow
    });

    with_state(refresh);
    keys::probe_stored();
    services::check();
    let host = has_tray && tray::host_present();
    #[cfg(target_os = "linux")]
    if has_tray {
        tray::watch_host(host, || {
            let _ = slint::invoke_from_event_loop(|| {
                with_state(|s| {
                    if let Some(tray) = &mut s.tray {
                        tray.recreate(&s.window, &s.dictation);
                    }
                })
            });
        });
    }
    if !start_hidden {
        window.show()?;
    } else if !has_tray {
        log::info!("--start-hidden without a tray: showing the window");
        window.show()?;
    } else if !host {
        log::info!("--start-hidden without a tray host (StatusNotifierWatcher): showing the window");
        window.show()?;
    }
    with_state(update::check);
    let result = slint::run_event_loop_until_quit();
    // Quit while listening: put other audio back before the process ends.
    with_state(|s| s.dictation.sounds.restore_blocking());
    // Dropped while its threads still run. Left in the thread-local, Windows drops it during process exit, after the
    // other threads were ended, and the runtime's shutdown waits for them forever (`taskkill /F` fails).
    drop(STATE.with(|state| state.borrow_mut().take()));
    result
}

/// Quits after committing a focused text field: its commit is a change handler, which the loop runs at the start of
/// the iteration that handles the quit.
pub fn quit(window: &MainWindow) {
    window.invoke_focus_root();
    if let Err(e) = slint::quit_event_loop() {
        log::warn!("Quitting failed: {e}");
    }
}

/// (Re)registers the transcribe shortcut.
pub fn register_shortcut(state: &mut AppState) {
    shortcuts::register(state);
}

/// A backup restore replaced the settings and API keys: refreshes everything they drive (the pages, the providers'
/// settings, the stored keys, the shortcut, the model lists, the login item).
pub fn after_restore(state: &mut AppState) {
    refresh(state);
    if let Err(e) = autostart::apply(&state.store.settings(&state.stored_secrets)) {
        log::warn!("Updating the login item failed: {e}");
    }
    catalogs::restored(state);
    register_shortcut(state);
    // Its result refreshes again, which reloads the lists of the selected providers.
    keys::probe(state);
}

/// Fills every global from the current settings.
pub fn refresh(state: &mut AppState) {
    let settings = state.store.settings(&state.stored_secrets);
    let downloads = state.dictation.store.state().borrow().clone();
    transcription::fill(&state.window, &settings, &state.services, &downloads);
    post_processing::fill(state, &settings);
    catalogs::fill(state, &settings);
    shortcuts::fill(state, &settings);
    state.overlay.set_config(
        &settings.overlay_position,
        settings.panel_lines,
        settings.overlay_width_percent,
        state.window.get_dark(),
    );
    if let Some(tray) = &state.tray {
        tray.set_app_dark(state.window.get_dark());
    }
    state.dictation.settings.send_replace(settings.clone());
    state.dictation.engine.settings_changed();
    warm_on_device(state, &settings, &downloads);
    let prefs = state.window.global::<Prefs>();
    prefs.set_overlay_position(settings.overlay_position.clone().into());
    prefs.set_panel_lines(settings.panel_lines);
    prefs.set_overlay_width_percent(settings.overlay_width_percent);
    prefs.set_paste_method(settings.paste_method.clone().into());
    prefs.set_append_trailing_space(settings.append_trailing_space);
    prefs.set_restore_clipboard(settings.restore_clipboard);
    prefs.set_paste_delay_ms(settings.paste_delay_ms);
    prefs.set_paste_delay_after_ms(settings.paste_delay_after_ms);
    prefs.set_feedback_sounds(settings.feedback_sounds);
    prefs.set_feedback_volume_percent(settings.feedback_volume_percent);
    prefs.set_mute_while_recording(settings.mute_while_recording);
    prefs.set_duck_while_recording(settings.duck_while_recording);
    prefs.set_duck_amount(settings.duck_amount);
    prefs.set_launch_at_startup(settings.launch_at_startup);
    prefs.set_start_hidden(settings.start_hidden);
    prefs.set_wayland(services::wayland());
    let unreadable = |name| state.unreadable_secrets.contains(name);
    prefs.set_openai_key_unreadable(unreadable(secrets::OPENAI_API_KEY));
    prefs.set_gateway_key_unreadable(unreadable(secrets::GATEWAY_API_KEY));
    prefs.set_deepgram_key_unreadable(unreadable(secrets::DEEPGRAM_API_KEY));
    prefs.set_soniox_key_unreadable(unreadable(secrets::SONIOX_API_KEY));
    prefs.set_openrouter_key_unreadable(unreadable(secrets::OPENROUTER_API_KEY));
}

/// Android warms the model while the bubble shows; here at start, whenever the model or language changes, and when the
/// selected model's download finishes (a model that isn't downloaded isn't warmed). Not with Unload Model
/// "Immediately": it would be unloaded again at once.
fn warm_on_device(state: &mut AppState, settings: &Settings, downloads: &StoreState) {
    let downloaded = downloads.downloaded.contains(models::by_id(&settings.on_device_stt_model).id);
    let selection = (settings.stt_on_device() && downloaded)
        .then(|| (settings.on_device_stt_model.clone(), settings.stt_language.clone()));
    if selection != state.warmed {
        state.warmed = selection;
        if state.warmed.is_some() && settings.on_device_unload_minutes != 0 {
            state.dictation.providers.warm_up_on_device();
        }
    }
}

/// Maps a store write to the control's error text ("" = saved) and refreshes on success.
pub fn saved(state: &mut AppState, result: io::Result<()>) -> SharedString {
    match result {
        Ok(()) => {
            refresh(state);
            SharedString::new()
        }
        Err(e) => {
            log::warn!("Saving a setting failed: {e}");
            SAVE_ERROR.into()
        }
    }
}

pub fn open_url(url: &str) {
    if let Err(e) = open::that_detached(url) {
        log::warn!("Opening a link failed: {e}");
    }
}

pub fn option(id: &str, label: &str) -> SelectOption {
    SelectOption { id: id.into(), label: label.into(), ..Default::default() }
}

pub fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn wire_helpers(window: &MainWindow) {
    let math = window.global::<UiMath>();
    math.on_dropdown_placement(|anchor_top, anchor_bottom, window_height, content_height| {
        // Android's 8 dp margins to the window edges and 4 dp from the anchor, x 0.9 (see `theme.slint`).
        let placement = ui_math::dropdown_placement(
            anchor_top.round() as i32,
            anchor_bottom.round() as i32,
            7,
            window_height.round() as i32 - 7,
            content_height.round() as i32,
            4,
        );
        Placement { y: placement.y as f32, max_height: placement.max_height as f32, above: placement.above }
    });
    math.on_slider_value(|fraction, min, max, step| ui_math::slider_value(fraction, min..=max, step));
    math.on_slider_fraction(|value, min, max| ui_math::slider_fraction(value, min..=max));
    math.on_filter_options(|options, query| {
        let ids: Vec<String> = options.iter().map(|o| o.id.to_string()).collect();
        let kept = ui_math::filter_models(Some(&ids), &query);
        model(options.iter().filter(|o| kept.contains(&o.id.to_string())).collect())
    });
    math.on_index_of(|options, id| options.iter().position(|o| o.id == id).map_or(-1, |i| i as i32));

    window.global::<Copy>().on_api_key_helper(|is_set, shared| {
        copy::api_key_helper(is_set, Some(shared.as_str()).filter(|s| !s.is_empty())).into()
    });

    let mono = window.global::<Type>().get_mono();
    let family = if cfg!(windows) { "Consolas" } else { "monospace" };
    window.global::<Type>().set_mono(TextStyle { family: family.into(), ..mono });

    let app = window.global::<App>();
    app.set_version(env!("CARGO_PKG_VERSION").into());
    app.on_open_url(|url| open_url(&url));

    let weak = window.as_weak();
    window.global::<Nav>().on_focus_root(move || {
        if let Some(window) = weak.upgrade() {
            window.invoke_focus_root();
        }
    });
}

fn wire_setters(window: &MainWindow) {
    let prefs = window.global::<Prefs>();
    prefs.on_set_bool(|key, value| with_state(|state| set_bool(state, &key, value)));
    prefs.on_set_int(|key, value| with_state(|state| set_int(state, &key, value)));
    prefs.on_set_string(|key, value| with_state(|state| set_string(state, &key, &value)));
    prefs.on_commit_text(|key, draft| with_state(|state| commit_text(state, &key, &draft)));
}

fn set_bool(state: &mut AppState, key: &str, value: bool) -> SharedString {
    let store = &mut state.store;
    let result = match key {
        "deepgram_upload" => store.set_deepgram_upload(value),
        "vad_enabled" => store.set_vad_enabled(value),
        "remove_filler_words" => store.set_remove_filler_words(value),
        "custom_words_enabled" => store.set_custom_words_enabled(value),
        "correct_similar_words" => store.set_correct_similar_words(value),
        "cleanup_enabled" => store.set_cleanup_enabled(value),
        "append_trailing_space" => store.set_append_trailing_space(value),
        "restore_clipboard" => store.set_restore_clipboard(value),
        "feedback_sounds" => store.set_feedback_sounds(value),
        "mute_while_recording" => store.set_mute_while_recording(value),
        "duck_while_recording" => store.set_duck_while_recording(value),
        "launch_at_startup" => store.set_launch_at_startup(value),
        "start_hidden" => store.set_start_hidden(value),
        _ => match key.strip_prefix("custom_cleanup_send_json_schema:") {
            Some(id) => store.update_custom_cleanup_provider(id, |p| p.send_json_schema = value),
            None => unreachable!("unknown bool setting {key}"),
        },
    };
    let error = saved(state, result);
    if matches!(key, "launch_at_startup" | "start_hidden")
        && error.is_empty()
        && let Err(e) = autostart::apply(&state.store.settings(&state.stored_secrets))
    {
        log::warn!("Updating the login item failed: {e}");
        return autostart::ERROR.into();
    }
    error
}

fn set_int(state: &mut AppState, key: &str, value: i32) -> SharedString {
    let store = &mut state.store;
    let result = match key {
        "stt_timeout_seconds" => store.set_stt_timeout_seconds(value),
        "stt_speed_percent" => store.set_stt_speed_percent(value),
        "on_device_unload_minutes" => store.set_on_device_unload_minutes(value),
        "cleanup_timeout_seconds" => store.set_cleanup_timeout_seconds(value),
        "panel_lines" => store.set_panel_lines(value),
        "overlay_width_percent" => store.set_overlay_width_percent(value),
        "paste_delay_ms" => store.set_paste_delay_ms(value),
        "paste_delay_after_ms" => store.set_paste_delay_after_ms(value),
        "feedback_volume_percent" => store.set_feedback_volume_percent(value),
        "duck_amount" => store.set_duck_amount(value),
        _ => unreachable!("unknown int setting {key}"),
    };
    saved(state, result)
}

fn set_string(state: &mut AppState, key: &str, value: &str) -> SharedString {
    // Reasoning efforts: "" is the model default (None).
    let effort = Some(value).filter(|v| !v.is_empty());
    let store = &mut state.store;
    let result = match key {
        "stt_provider_id" => store.set_stt_provider(value),
        "soniox_region" => store.set_soniox_region(value),
        "on_device_stt_model" => store.set_on_device_stt_model(value),
        "local_accelerator" => store.set_local_accelerator(value),
        "microphone_device" => store.set_microphone_device(value),
        "cleanup_provider_id" => {
            // Picking any provider discards an unsaved draft.
            state.draft_id = None;
            store.set_cleanup_provider(value)
        }
        "cleanup_reasoning_effort" => store.set_cleanup_reasoning_effort(effort),
        "openrouter_cleanup_reasoning_effort" => store.set_open_router_cleanup_reasoning_effort(effort),
        "cleanup_active_prompt_id" => store.set_active_prompt(value),
        "overlay_position" => store.set_overlay_position(value),
        "shortcut_activation" => store.set_shortcut_activation(value),
        "paste_method" => store.set_paste_method(value),
        _ => match key.strip_prefix("custom_cleanup_reasoning_effort:") {
            Some(id) => store.update_custom_cleanup_provider(id, |p| p.reasoning_effort = effort.map(str::to_string)),
            None => unreachable!("unknown string setting {key}"),
        },
    };
    let error = saved(state, result);
    // Soniox's catalogue is per region (and key): reloaded once the new region reached the providers.
    if key == "soniox_region" && error.is_empty() {
        state.dictation.providers.soniox_catalog.reload();
    }
    error
}

const BLANK_MODEL: &str = "Enter a model id.";

/// Commits an autosaving text field's draft: Android's `commitDecision` with the field's validator, then the save.
fn commit_text(state: &mut AppState, key: &str, draft: &str) -> CommitResult {
    let settings = state.store.settings(&state.stored_secrets);
    // No prompt left to edit (the list was emptied): nothing to save into.
    if matches!(key, "prompt_name" | "prompt_text") && settings.active_prompt.is_none() {
        return CommitResult { invalid: false, value: SharedString::new(), error: SAVE_ERROR.into() };
    }
    let custom_id = key.strip_prefix("custom_cleanup_model:");
    let stored = match key {
        "stt_model" => settings.stt_model.clone(),
        "gateway_stt_model" => settings.gateway_stt_model.clone(),
        "deepgram_stt_model" => settings.deepgram_stt_model.clone(),
        "soniox_stt_model" => settings.soniox_stt_model.clone(),
        "soniox_languages" => settings.soniox_languages.clone(),
        "stt_language" => settings.stt_language.clone(),
        "cleanup_model" => settings.cleanup_model.clone(),
        "openrouter_cleanup_model" => settings.open_router_cleanup_model.clone(),
        "cleanup_provider_options" => settings.cleanup_provider_options.clone(),
        "openrouter_cleanup_provider_options" => settings.open_router_cleanup_provider_options.clone(),
        "prompt_name" => active_prompt(&settings).name,
        "prompt_text" => active_prompt(&settings).system_prompt,
        _ => custom_model(&settings, custom_id.expect("unknown text setting")),
    };
    let validate = |value: &str| -> Option<String> {
        let error = match key {
            "soniox_languages" | "stt_language" => None,
            "cleanup_provider_options" | "openrouter_cleanup_provider_options" => copy::provider_options_error(value),
            "prompt_name" => value.is_empty().then_some("Name can't be empty."),
            "prompt_text" => value.is_empty().then_some("Prompt can't be empty."),
            _ => value.is_empty().then_some(BLANK_MODEL),
        };
        error.map(str::to_string)
    };
    match ui_math::commit_decision(draft, &stored, validate) {
        Commit::None => CommitResult { invalid: false, value: stored.into(), error: SharedString::new() },
        Commit::Invalid(error) => CommitResult { invalid: true, value: draft.into(), error: error.into() },
        Commit::Save(value) => {
            let store = &mut state.store;
            let result = match key {
                "stt_model" => store.set_stt_model(&value),
                // Live per the catalogue's tag; a model it doesn't list (or no catalogue) counts as live.
                "gateway_stt_model" => {
                    let live = gateway_stt_live(&value, state.catalogs.gateway.models.as_deref());
                    store.set_gateway_stt_model(&value, live)
                }
                "deepgram_stt_model" => store.set_deepgram_stt_model(&value),
                "soniox_stt_model" => store.set_soniox_stt_model(&value),
                "soniox_languages" => store.set_soniox_languages(&value),
                "stt_language" => store.set_stt_language(&value),
                "cleanup_model" => store.set_cleanup_model(&value),
                "openrouter_cleanup_model" => store.set_open_router_cleanup_model(&value),
                "cleanup_provider_options" => store.set_cleanup_provider_options(&value),
                "openrouter_cleanup_provider_options" => store.set_open_router_cleanup_provider_options(&value),
                "prompt_name" => store.save_prompt(iris_core::settings::CleanupPrompt {
                    name: value.clone(),
                    ..active_prompt(&settings)
                }),
                "prompt_text" => store.save_prompt(iris_core::settings::CleanupPrompt {
                    system_prompt: value.clone(),
                    ..active_prompt(&settings)
                }),
                _ => store.update_custom_cleanup_provider(custom_id.unwrap(), |p| p.model = value.clone()),
            };
            CommitResult { invalid: false, error: saved(state, result), value: value.into() }
        }
    }
}

fn active_prompt(settings: &Settings) -> iris_core::settings::CleanupPrompt {
    settings.active_prompt.clone().expect("there is always a prompt")
}

fn custom_model(settings: &Settings, id: &str) -> String {
    settings.custom_cleanup_providers.iter().find(|p| p.id == id).map(|p| p.model.clone()).unwrap_or_default()
}

/// Dictation's links to the UI: the download buttons, the overlay's levels and model-load notice, other instances'
/// commands, SIGUSR2 and the colour scheme.
fn wire_dictation(window: &MainWindow, runtime: &Runtime, dictation: &Dictation) {
    let prefs = window.global::<Prefs>();
    prefs.on_download_model(|| with_state(|s| s.dictation.store.download(&selected_model(s))));
    prefs.on_cancel_download(|| with_state(|s| s.dictation.store.cancel()));
    prefs.on_delete_model(|| with_state(|s| s.dictation.store.delete(&selected_model(s))));
    window.global::<App>().on_dark_changed(|_| with_state(refresh));

    let mut downloads = dictation.store.state();
    runtime.spawn(async move {
        while downloads.changed().await.is_ok() {
            let downloads = downloads.borrow_and_update().clone();
            let _ = slint::invoke_from_event_loop(move || {
                with_state(|s| {
                    let settings = s.store.settings(&s.stored_secrets);
                    transcription::fill(&s.window, &settings, &s.services, &downloads);
                    warm_on_device(s, &settings, &downloads);
                })
            });
        }
    });
    let mut levels = dictation.controller.levels();
    runtime.spawn(async move {
        while levels.changed().await.is_ok() {
            let levels = levels.borrow_and_update().clone();
            let _ = slint::invoke_from_event_loop(move || with_state(|s| s.overlay.set_levels(&levels)));
        }
    });
    let mut soniox = dictation.providers.soniox_catalog.state();
    runtime.spawn(async move {
        while soniox.changed().await.is_ok() {
            let _ = slint::invoke_from_event_loop(|| with_state(refresh));
        }
    });
    // Android loads it at app start (without a key: the "set your key" error, no request).
    dictation.providers.soniox_catalog.reload();
    let mut loading = dictation.engine.loading();
    runtime.spawn(async move {
        while loading.changed().await.is_ok() {
            let loading = *loading.borrow_and_update();
            let _ = slint::invoke_from_event_loop(move || with_state(|s| s.overlay.set_model_loading(loading)));
        }
    });

    let commands = dictation.clone();
    crate::single_instance::serve(move |command| match command {
        "toggle" => commands.coordinator.external(),
        "cancel" => commands.cancel(),
        _ => {
            let _ = slint::invoke_from_event_loop(|| with_state(|s| show_window(&s.window)));
        }
    });
    #[cfg(target_os = "linux")]
    {
        let coordinator = dictation.coordinator.clone();
        crate::signals::listen(move || coordinator.external());
    }
}

/// Shows the window and brings it to the front (another instance was started, tray Settings).
pub fn show_window(window: &MainWindow) {
    if let Err(e) = window.show() {
        log::warn!("Showing the window failed: {e}");
    }
    window.window().set_minimized(false);
    window.window().with_winit_window(|w| w.focus_window());
}

fn selected_model(state: &AppState) -> String {
    let settings = state.store.settings(&state.stored_secrets);
    iris_core::stt::local::models::by_id(&settings.on_device_stt_model).id.to_string()
}

/// Runs blocking keychain work on the runtime, then `done` on the main thread with the state.
pub fn off_thread<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce(&SecretStore) -> T + Send + 'static,
    done: impl FnOnce(&mut AppState, T) + Send + 'static,
) {
    state.runtime.spawn_blocking(move || {
        let result = work(&SecretStore::new());
        let _ = slint::invoke_from_event_loop(move || with_state(|state| done(state, result)));
    });
}

/// Tells the field named `target` how its keychain operation went ("" = done).
pub fn key_event(state: &mut AppState, target: &str, error: &str) {
    state.key_op_seq += 1;
    let prefs = state.window.global::<Prefs>();
    prefs.set_key_op_target(target.into());
    prefs.set_key_op_error(error.into());
    prefs.set_key_op_seq(state.key_op_seq);
}
