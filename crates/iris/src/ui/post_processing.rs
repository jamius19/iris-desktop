/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Post Processing and Custom Words (Android `PostProcessingPage.kt`, `CustomWordsPage.kt`): providers (with the
//! custom-provider form and favicons), prompts and custom words.

use iris_core::cleanup::compatible::{key_usable, provider_draft_errors};
use iris_core::copy::{
    self, CLEANUP_PROVIDERS, GATEWAY_OPTIONS_HINT, OPENROUTER_OPTIONS_HINT, SAVE_ERROR, reasoning_effort_label,
};
use iris_core::settings::secrets::custom_cleanup_key;
use iris_core::settings::{
    CleanupPrompt, REASONING_EFFORTS, Settings, UNNAMED_PROVIDER, cleanup_provider_label, default_prompt,
};
use iris_core::stt::stt_takes_custom_words;
use slint::ComponentHandle;

use super::{AppState, key_event, model, off_thread, option, refresh, saved, with_state};
use crate::{CustomProvider, Icons, MainWindow, Prefs, Prompt, ProviderFormErrors, SelectOption};

pub fn fill(state: &mut AppState, settings: &Settings) {
    let window = &state.window;
    let prefs = window.global::<Prefs>();
    let icons = window.global::<Icons>();

    prefs.set_remove_filler_words(settings.remove_filler_words);
    prefs.set_custom_words_enabled(settings.custom_words_enabled);
    prefs.set_correct_similar_words(settings.correct_similar_words);
    prefs.set_custom_words(model(settings.custom_words.iter().map(|w| w.into()).collect()));
    let sent = stt_takes_custom_words(settings);
    let (enabled, correct, cleanup) =
        (settings.custom_words_enabled, settings.correct_similar_words, settings.cleanup_enabled);
    let note = copy::custom_words_unsent_note(enabled, sent, correct, cleanup);
    prefs.set_custom_words_note(note.unwrap_or_default().into());
    prefs.set_custom_words_note_is_warning(enabled && !sent && !correct && !cleanup);

    prefs.set_cleanup_enabled(settings.cleanup_enabled);
    prefs.set_cleanup_provider_id(settings.cleanup_provider_id.clone().into());
    prefs.set_cleanup_open_router(settings.cleanup_open_router());
    prefs.set_cleanup_model(settings.cleanup_model.clone().into());
    prefs.set_cleanup_reasoning_effort(settings.cleanup_reasoning_effort.clone().unwrap_or_default().into());
    prefs.set_cleanup_provider_options(settings.cleanup_provider_options.clone().into());
    prefs.set_open_router_cleanup_model(settings.open_router_cleanup_model.clone().into());
    prefs.set_open_router_cleanup_reasoning_effort(
        settings.open_router_cleanup_reasoning_effort.clone().unwrap_or_default().into(),
    );
    prefs.set_open_router_cleanup_provider_options(settings.open_router_cleanup_provider_options.clone().into());
    prefs.set_cleanup_timeout_seconds(settings.cleanup_timeout_seconds);
    prefs.set_gateway_options_hint(GATEWAY_OPTIONS_HINT.into());
    prefs.set_openrouter_options_hint(OPENROUTER_OPTIONS_HINT.into());
    prefs.set_gateway_key_set(settings.gateway_key_set);
    prefs.set_openrouter_key_set(settings.open_router_key_set);

    let selected = settings.active_custom_cleanup().map(|p| CustomProvider {
        id: p.id.clone().into(),
        name: p.name.clone().into(),
        base_url: p.base_url.clone().into(),
        model: p.model.clone().into(),
        reasoning_effort: p.reasoning_effort.clone().unwrap_or_default().into(),
        send_json_schema: p.send_json_schema,
        key_set: settings.custom_cleanup_key_ids.contains(&p.id),
    });
    prefs.set_custom_selected(selected.is_some());
    prefs.set_custom(selected.unwrap_or_default());

    let mut providers: Vec<SelectOption> = CLEANUP_PROVIDERS
        .into_iter()
        .map(|id| {
            let gateway = id == CLEANUP_PROVIDERS[0];
            SelectOption {
                icon: if gateway { icons.get_vercel() } else { icons.get_openrouter() },
                has_icon: true,
                tint_icon: gateway,
                ..option(id, &cleanup_provider_label(id, &settings.custom_cleanup_providers))
            }
        })
        .collect();
    for provider in &settings.custom_cleanup_providers {
        let favicon = state.favicons.get(&provider.base_url, &state.runtime);
        providers.push(SelectOption {
            has_icon: true,
            tint_icon: favicon.is_none(),
            icon: favicon.unwrap_or_else(|| icons.get_cloud()),
            ..option(&provider.id, &cleanup_provider_label(&provider.id, &settings.custom_cleanup_providers))
        });
    }
    if let Some(draft) = &state.draft_id {
        providers.push(SelectOption {
            icon: icons.get_cloud(),
            has_icon: true,
            tint_icon: true,
            ..option(draft, UNNAMED_PROVIDER)
        });
    }
    prefs.set_cleanup_providers(model(providers));
    prefs.set_draft_id(state.draft_id.clone().unwrap_or_default().into());

    // "Model default" plus every level the APIs accept (a model's own list needs the M3 catalogue).
    let mut efforts = vec![option("", &reasoning_effort_label(None))];
    efforts.extend(REASONING_EFFORTS.iter().map(|e| option(e, &reasoning_effort_label(Some(e)))));
    prefs.set_reasoning_options(model(efforts));

    prefs.set_prompts(model(settings.prompts.iter().map(prompt).collect()));
    prefs.set_prompt_options(model(settings.prompts.iter().map(|p| option(&p.id, &p.name)).collect()));
    prefs.set_active_prompt(settings.active_prompt.as_ref().map(prompt).unwrap_or_default());
}

fn prompt(p: &CleanupPrompt) -> Prompt {
    Prompt { id: p.id.clone().into(), name: p.name.clone().into(), system_prompt: p.system_prompt.clone().into() }
}

pub fn wire(window: &MainWindow) {
    let prefs = window.global::<Prefs>();

    prefs.on_add_custom_word(|word| {
        with_state(|state| {
            let settings = state.store.settings(&state.stored_secrets);
            if let Some(error) = copy::custom_word_error(&settings.custom_words, &word) {
                return error.into();
            }
            if word.trim().is_empty() {
                return Default::default();
            }
            let result = state.store.add_custom_word(&word);
            saved(state, result)
        })
    });
    prefs.on_remove_custom_word(|word| {
        with_state(|state| {
            let result = state.store.remove_custom_word(&word);
            saved(state, result)
        })
    });

    prefs.on_new_draft(|| {
        with_state(|state| {
            state.draft_id = Some(uuid::Uuid::new_v4().to_string());
            refresh(state);
        })
    });
    prefs.on_discard_draft(|| {
        with_state(|state| {
            if state.draft_id.take().is_some() {
                refresh(state);
            }
        })
    });
    prefs.on_check_custom_provider(|id, name, base_url, key_input, key_shown| {
        with_state(|state| {
            let settings = state.store.settings(&state.stored_secrets);
            let stored_base_url =
                settings.custom_cleanup_providers.iter().find(|p| id == p.id).map(|p| p.base_url.clone());
            let key_ok = key_usable(key_shown, &base_url, stored_base_url.as_deref());
            let errors = provider_draft_errors(&name, &base_url, &key_input, key_ok);
            ProviderFormErrors {
                name: errors.name.unwrap_or_default().into(),
                base_url: errors.base_url.unwrap_or_default().into(),
                api_key: errors.api_key.unwrap_or_default().into(),
            }
        })
    });
    // Android `saveCustomCleanupProvider`: a new key is stored first, then the provider is saved and selected.
    prefs.on_save_custom_provider(|id, name, base_url, key_input| {
        let key = key_input.trim().to_string();
        let key_name = custom_cleanup_key(&id);
        with_state(|state| {
            off_thread(
                state,
                move |secrets| {
                    if key.is_empty() { Ok(false) } else { secrets.put(&key_name, &key).map(|_| true) }
                },
                move |state, result| {
                    let target = format!("provider:{id}");
                    let saved =
                        result.map_err(|e| log::warn!("Saving a provider key failed: {e}")).and_then(|stored_key| {
                            if stored_key {
                                state.stored_secrets.insert(custom_cleanup_key(&id));
                            }
                            state
                                .store
                                .save_custom_cleanup_provider(&id, name.trim(), base_url.trim())
                                .map_err(|e| log::warn!("Saving a provider failed: {e}"))
                        });
                    if saved.is_ok() && state.draft_id.as_deref() == Some(id.as_str()) {
                        state.draft_id = None;
                    }
                    refresh(state);
                    key_event(state, &target, if saved.is_ok() { "" } else { SAVE_ERROR });
                },
            )
        });
    });
    // The key goes first, so a failure leaves a keyless provider (retryable), never an orphaned key.
    prefs.on_delete_custom_provider(|id| {
        with_state(|state| {
            let key = custom_cleanup_key(&id);
            off_thread(
                state,
                move |secrets| secrets.clear(&key),
                move |state, result| {
                    let target = format!("provider:{id}");
                    let deleted =
                        result.map_err(|e| log::warn!("Deleting a provider key failed: {e}")).and_then(|()| {
                            state.stored_secrets.remove(&custom_cleanup_key(&id));
                            state
                                .store
                                .delete_custom_cleanup_provider(&id)
                                .map_err(|e| log::warn!("Deleting a provider failed: {e}"))
                        });
                    refresh(state);
                    key_event(state, &target, if deleted.is_ok() { "" } else { SAVE_ERROR });
                },
            )
        });
    });

    prefs.on_new_prompt(|| {
        with_state(|state| {
            let settings = state.store.settings(&state.stored_secrets);
            let names: Vec<String> = settings.prompts.iter().map(|p| p.name.clone()).collect();
            let prompt = CleanupPrompt {
                id: uuid::Uuid::new_v4().to_string(),
                name: copy::new_prompt_name(&names),
                system_prompt: default_prompt().system_prompt,
            };
            let id = prompt.id.clone();
            let result = state.store.save_prompt(prompt).and_then(|()| state.store.set_active_prompt(&id));
            saved(state, result)
        })
    });
    prefs.on_delete_prompt(|id| {
        with_state(|state| {
            let result = state.store.delete_prompt(&id);
            saved(state, result)
        })
    });
}
