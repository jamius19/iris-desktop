/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! `iris -f file.wav [--model id]`: runs the on-device provider on a WAV file without the UI or the microphone and
//! prints the text (a headless check of the models and the engine worker).

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iris_core::settings::Store;
use iris_core::stt::local::models;
use iris_core::stt::local::provider::SAMPLE_RATE;
use iris_core::stt::local::store::ModelStore;
use iris_core::stt::{AudioSource, BoxFuture, SpeechToTextProvider, SttError, SttRequest};

use crate::local::engine::LocalEngine;
use crate::local::read_wav_samples;

/// Returns the process exit code.
pub fn transcribe_file(path: &Path, model_id: Option<&str>) -> i32 {
    let samples = match read_wav_samples(path) {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("Couldn't read {}: {e}", path.display());
            return 2;
        }
    };
    let settings = Store::open(Store::default_path()).settings(&HashSet::new());
    let id = model_id.unwrap_or(&settings.on_device_stt_model).to_string();
    let model = models::by_id(&id);
    if model.id != id {
        eprintln!("Unknown model {id}; using {}", model.id);
    }
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("runtime");
    let store = ModelStore::new(crate::download_client(), crate::models_dir(), runtime.handle().clone(), |_| {});
    // The store scans the models directory at start.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !store.is_downloaded(model.id) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let accelerator = settings.local_accelerator.clone();
    // No statistics for a file.
    let engine = LocalEngine::new(store, Arc::new(move || (accelerator.clone(), -1)), Box::new(|_, _| {}));
    let provider = engine.provider(|| (String::new(), String::new()), Box::new(|_, _, _, _| {}));
    let request = SttRequest {
        model: model.id.into(),
        language: settings.stt_language.clone(),
        timeout: Duration::from_secs(10),
        custom_words: Vec::new(),
    };
    let started = Instant::now();
    let audio = FileAudio { samples, at: 0 };
    let result = runtime.block_on(provider.transcribe(Box::new(audio), request, Box::new(|_| {})));
    match result {
        Ok(text) => {
            eprintln!("{} in {} ms", model.label, started.elapsed().as_millis());
            println!("{text}");
            0
        }
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

/// The file as 50 ms PCM16 chunks, as the microphone would send them (no speech gate: no segment ends).
struct FileAudio {
    samples: Vec<f32>,
    at: usize,
}

impl AudioSource for FileAudio {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, SttError>> {
        let end = (self.at + SAMPLE_RATE as usize / 20).min(self.samples.len());
        let chunk = &self.samples[self.at..end];
        self.at = end;
        let bytes = (!chunk.is_empty())
            .then(|| chunk.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect());
        Box::pin(async move { Ok(bytes) })
    }
}
