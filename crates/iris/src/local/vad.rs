/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Silero VAD v6.2 in the app process on the CPU (Handy `audio_toolkit/vad/silero.rs` and `cpu_engine.rs`): the
//! scorer the speech gate injects. The model ships inside the binary and is written to `<data>/vad/` on first use.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use iris_core::audio::speech_gate::{VAD_FRAME_SAMPLES, VAD_RATE, VadScorer};
use transcribe_cpp::{Backend, BackendMask, Model, ModelOptions, VadSessionOptions};

const MODEL: &[u8] = include_bytes!("../../../../assets/models/silero-vad-v6.2-F32.gguf");
const MODEL_FILE: &str = "silero-vad-v6.2-F32.gguf";

/// A new Silero stream for one dictation. A model that can't load gives a scorer that always fails, so the gate
/// fails open (sends everything).
pub fn scorer(data_dir: &Path) -> VadScorer {
    match model(data_dir) {
        Ok(model) => match model.vad_session_with(&VadSessionOptions { n_threads: 1 }) {
            Ok(mut session) => Box::new(move |frame: &[f32]| {
                let result = session.stream_feed(frame).map_err(|e| format!("Silero VAD error: {e}"))?;
                result.probs.first().copied().ok_or_else(|| "Silero VAD returned no probability".to_string())
            }),
            Err(e) => failing(format!("Failed to create the VAD session: {e}")),
        },
        Err(e) => failing(e),
    }
}

fn failing(error: String) -> VadScorer {
    log::warn!("{error}");
    Box::new(move |_| Err(error.clone()))
}

/// The loaded VAD model, shared by every session.
fn model(data_dir: &Path) -> Result<Model, String> {
    static MODEL_CELL: OnceLock<Result<Model, String>> = OnceLock::new();
    MODEL_CELL.get_or_init(|| load(&model_path(data_dir)?)).clone()
}

fn load(path: &Path) -> Result<Model, String> {
    init_cpu_backends()?;
    let options = ModelOptions { backend: Backend::Cpu, device: None, ..Default::default() };
    let model = Model::load_with(path, &options).map_err(|e| format!("Failed to load the VAD model: {e}"))?;
    let info = model.vad_info().map_err(|e| format!("Model does not support VAD: {e}"))?;
    if info.sample_rate != VAD_RATE as i32 || info.frame_samples != VAD_FRAME_SAMPLES as i32 {
        return Err(format!(
            "Unsupported VAD model: {} Hz, {} samples per frame",
            info.sample_rate, info.frame_samples
        ));
    }
    Ok(model)
}

/// `<data>/vad/<file>`, (re)written when missing or different from the bundled bytes.
fn model_path(data_dir: &Path) -> Result<PathBuf, String> {
    let path = data_dir.join("vad").join(MODEL_FILE);
    if std::fs::read(&path).is_ok_and(|bytes| bytes == MODEL) {
        return Ok(path);
    }
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(path.parent().expect("has a parent"))?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, MODEL)?;
        std::fs::rename(&tmp, &path)
    };
    write().map_err(|e| format!("Couldn't write the VAD model: {e}"))?;
    Ok(path)
}

/// Registers transcribe.cpp's CPU backends once per process; the app process never registers a GPU (that runs in the
/// engine worker).
pub fn init_cpu_backends() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        transcribe_cpp::init_logging();
        transcribe_cpp::init_backends_with(None::<&Path>, BackendMask::CPU).map_err(|e| e.to_string())
    })
    .clone()
    .map_err(|e| format!("Failed to initialize the transcribe.cpp CPU backend: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Handy's `scores_the_same_in_pieces_and_frame_by_frame`, reduced to the scorer the gate uses: frame by frame
    /// gives a probability per frame, deterministically. Loads native code, so run with `--ignored`.
    #[test]
    #[ignore]
    fn scores_frame_by_frame_deterministically() {
        let dir = tempfile::tempdir().unwrap();
        let pcm: Vec<f32> =
            (0..VAD_FRAME_SAMPLES * 60).map(|i| (i as f32 * 0.013).sin() * (i as f32 * 0.0007).cos() * 0.2).collect();
        let run = |mut scorer: VadScorer| -> Vec<f32> {
            pcm.chunks(VAD_FRAME_SAMPLES).map(|frame| scorer(frame).unwrap()).collect()
        };
        let first = run(scorer(dir.path()));
        assert_eq!(60, first.len());
        assert_eq!(first, run(scorer(dir.path())));
        assert!(first.iter().all(|p| (0.0..=1.0).contains(p)));
    }
}
