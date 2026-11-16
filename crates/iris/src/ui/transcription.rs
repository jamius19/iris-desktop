/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Transcription page values (Android `TranscriptionPage.kt`, `OnDeviceSettings.kt`) and the model download group.

use iris_core::copy::{self, STT_PROVIDERS, UNLOAD_HELPER, UNLOAD_RAM_GUIDE};
use iris_core::settings::{ON_DEVICE_UNLOAD_MINUTES, STT_PROVIDER_OPENAI, Settings, stt_provider_label};
use iris_core::stt::local::models::{self, ON_DEVICE_MODELS};
use iris_core::stt::local::store::StoreState;
use iris_core::stt::soniox::{SonioxRegion, soniox_region};
use iris_core::stt::stt_takes_custom_words;
use slint::ComponentHandle;

use super::{model, option};
use crate::services::ServiceStatus;
use crate::{GuideLine, Icons, MainWindow, Prefs, SelectOption};

pub fn fill(window: &MainWindow, settings: &Settings, services: &ServiceStatus, downloads: &StoreState) {
    let prefs = window.global::<Prefs>();
    let icons = window.global::<Icons>();
    let on_device = models::by_id(&settings.on_device_stt_model);
    // An unknown provider id shows (and works) as OpenAI.
    let provider = STT_PROVIDERS.into_iter().find(|p| *p == settings.stt_provider_id).unwrap_or(STT_PROVIDER_OPENAI);
    prefs.set_stt_provider_id(provider.into());
    prefs.set_stt_on_device(settings.stt_on_device());
    prefs.set_stt_gateway(settings.stt_gateway());
    prefs.set_stt_deepgram(settings.stt_deepgram());
    prefs.set_stt_soniox(settings.stt_soniox());
    prefs.set_stt_providers(model(
        STT_PROVIDERS
            .into_iter()
            .map(|id| {
                let (icon, tint) = match id {
                    "on-device" => (icons.get_monitor(), true),
                    "vercel-gateway" => (icons.get_vercel(), true),
                    "deepgram" => (icons.get_deepgram(), false),
                    "soniox" => (icons.get_soniox(), false),
                    _ => (icons.get_openai(), true),
                };
                SelectOption { icon, has_icon: true, tint_icon: tint, ..option(id, stt_provider_label(id)) }
            })
            .collect(),
    ));
    let sent = stt_takes_custom_words(settings);
    prefs.set_stt_provider_note(copy::custom_words_stt_note(settings, sent).unwrap_or_default().into());

    prefs.set_openai_key_set(settings.open_ai_key_set);
    prefs.set_gateway_key_set(settings.gateway_key_set);
    prefs.set_deepgram_key_set(settings.deepgram_key_set);
    prefs.set_soniox_key_set(settings.soniox_key_set);

    prefs.set_soniox_region(soniox_region(&settings.soniox_region).id().into());
    prefs.set_soniox_regions(model(SonioxRegion::ALL.into_iter().map(|r| option(r.id(), r.label())).collect()));

    prefs.set_stt_model(settings.stt_model.clone().into());
    prefs.set_gateway_stt_model(settings.gateway_stt_model.clone().into());
    prefs.set_deepgram_stt_model(settings.deepgram_stt_model.clone().into());
    prefs.set_deepgram_upload(settings.deepgram_upload);
    prefs.set_soniox_stt_model(settings.soniox_stt_model.clone().into());
    prefs.set_soniox_languages(settings.soniox_languages.clone().into());

    prefs.set_on_device_stt_model(on_device.id.into());
    prefs.set_on_device_models(model(
        ON_DEVICE_MODELS
            .iter()
            .map(|m| SelectOption {
                icon: if m.maker == "nvidia" { icons.get_nvidia() } else { icons.get_openai() },
                has_icon: true,
                tint_icon: m.maker == "openai",
                badge: downloads.downloaded.contains(m.id),
                streams: m.streams,
                ..option(m.id, m.label)
            })
            .collect(),
    ));
    prefs.set_on_device_model_helper(copy::on_device_model_helper(on_device).into());
    fill_download(window, settings, downloads);
    prefs.set_on_device_licence(on_device.licence.into());
    prefs.set_on_device_licence_url(on_device.licence_url.into());
    prefs.set_accelerators(model(vec![option("auto", "Auto"), option("cpu", "CPU only")]));
    prefs.set_local_accelerator(settings.local_accelerator.clone().into());
    // On device, only Whisper takes a language.
    prefs.set_takes_language(!settings.stt_on_device() || on_device.id == "whisper-small");
    prefs.set_stt_language(settings.stt_language.clone().into());
    prefs.set_stt_timeout_seconds(settings.stt_timeout_seconds);
    prefs.set_on_device_unload_minutes(settings.on_device_unload_minutes);
    prefs.set_unload_options(model(
        ON_DEVICE_UNLOAD_MINUTES
            .into_iter()
            .map(|m| option(&m.to_string(), &copy::on_device_unload_label(m)))
            .collect(),
    ));
    prefs.set_unload_helper(UNLOAD_HELPER.into());
    prefs.set_unload_guide(model(
        UNLOAD_RAM_GUIDE
            .into_iter()
            .map(|(label, advice)| GuideLine { label: label.into(), advice: advice.into() })
            .collect(),
    ));

    prefs.set_microphone_device(settings.microphone_device.clone().into());
    let mut microphones = vec![option("", "System default")];
    microphones.extend(services.microphones.iter().map(|(id, name)| option(id, name)));
    // A saved device that isn't connected still shows by its id.
    if !settings.microphone_device.is_empty()
        && !services.microphones.iter().any(|(id, _)| *id == settings.microphone_device)
    {
        microphones.push(option(&settings.microphone_device, &settings.microphone_device));
    }
    prefs.set_microphones(model(microphones));
    prefs.set_vad_enabled(settings.vad_enabled);
    let skip_silence_model = settings.stt_on_device().then_some(on_device);
    prefs.set_skip_silence_subtitle(copy::skip_silence_subtitle(skip_silence_model).into());
    prefs.set_stt_speed_percent(settings.stt_speed_percent);
}

/// The selected model's download group (Android `OnDeviceModelSettings`).
pub fn fill_download(window: &MainWindow, settings: &Settings, downloads: &StoreState) {
    let prefs = window.global::<Prefs>();
    let model = models::by_id(&settings.on_device_stt_model);
    let downloading = downloads.downloading.as_deref() == Some(model.id);
    prefs.set_model_downloaded(downloads.downloaded.contains(model.id));
    prefs.set_model_downloading(downloading);
    prefs.set_model_deleting(downloads.deleting.as_deref() == Some(model.id));
    prefs.set_other_downloading(downloads.downloading.as_ref().is_some_and(|id| id != model.id));
    let fraction = if downloading && downloads.total_bytes > 0 {
        downloads.done_bytes as f32 / downloads.total_bytes as f32
    } else {
        0.0
    };
    prefs.set_download_fraction(fraction);
    prefs.set_download_progress(
        copy::download_progress_text(downloads.done_bytes as i64, downloads.total_bytes as i64).into(),
    );
    prefs.set_model_size(copy::megabytes_text(model.size_bytes as i64).into());
    let error = downloads.error.as_ref().filter(|e| e.model_id == model.id).map(|e| e.message.clone());
    prefs.set_download_error(error.unwrap_or_default().into());
}
