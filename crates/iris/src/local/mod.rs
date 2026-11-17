/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Local (on-device) transcription: Handy's engine worker ([`supervisor`]), the engine the on-device provider runs
//! on ([`engine`]), the Silero VAD scorer ([`vad`]) and glibc tuning ([`memory`]).

pub mod engine;
pub mod memory;
pub mod supervisor;
pub mod vad;

use std::path::Path;

/// A 16 kHz mono 16-bit WAV file's samples, -1..1 (the headless `--transcribe-file` input).
pub fn read_wav_samples(path: impl AsRef<Path>) -> Result<Vec<f32>, String> {
    let reader = hound::WavReader::open(path.as_ref()).map_err(|e| e.to_string())?;
    let spec = reader.spec();
    if spec.sample_rate != 16_000 || spec.channels != 1 || spec.bits_per_sample != 16 {
        return Err(format!(
            "expected 16 kHz mono 16-bit PCM, got {} Hz, {} channel(s), {} bits",
            spec.sample_rate, spec.channels, spec.bits_per_sample
        ));
    }
    reader.into_samples::<i16>().map(|s| s.map(|v| v as f32 / 32768.0).map_err(|e| e.to_string())).collect()
}
