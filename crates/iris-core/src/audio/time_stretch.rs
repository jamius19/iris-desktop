/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Speeds PCM16 LE mono audio up with Sonic (`vendor/sonic`), pitch kept: Android's `Flow<ByteArray>.timeStretched`
//! on Handy's Sonic FFI (`audio_toolkit/time_stretch.rs`). Sonic holds back up to ~30 ms; that tail comes from
//! [`TimeStretch::finish`] when the audio ended normally (drop it otherwise: no flush).

use std::os::raw::{c_float, c_int, c_short};
use std::ptr::NonNull;

use log::{error, info};

#[repr(C)]
struct SonicStream {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn sonicCreateStream(sample_rate: c_int, num_channels: c_int) -> *mut SonicStream;
    fn sonicDestroyStream(stream: *mut SonicStream);
    fn sonicSetSpeed(stream: *mut SonicStream, speed: c_float);
    fn sonicWriteShortToStream(stream: *mut SonicStream, samples: *mut c_short, num_samples: c_int) -> c_int;
    fn sonicReadShortFromStream(stream: *mut SonicStream, samples: *mut c_short, max_samples: c_int) -> c_int;
    fn sonicSamplesAvailable(stream: *mut SonicStream) -> c_int;
    fn sonicFlushStream(stream: *mut SonicStream) -> c_int;
}

/// Most samples written to Sonic at once (~4 s at 16 kHz): Sonic sizes its buffers with `int` math.
const MAX_WRITE: usize = 1 << 16;

/// One stream of mono audio at `sample_rate`, `speed` times faster.
pub struct TimeStretch {
    /// `None` once Sonic ran out of memory: the rest is sent as it is.
    stream: Option<NonNull<SonicStream>>,
    speed: f32,
    sample_rate: u32,
    samples_in: usize,
    samples_out: usize,
}

// SAFETY: the stream is owned, used only through `&mut self`, and Sonic keeps no thread-local state. Not Sync.
unsafe impl Send for TimeStretch {}

impl TimeStretch {
    pub fn new(sample_rate: u32, speed: f32) -> Self {
        // SAFETY: plain constructor; a NULL result (out of memory) is handled as a failed stream.
        let stream = NonNull::new(unsafe { sonicCreateStream(sample_rate as c_int, 1) });
        match stream {
            // SAFETY: `stream` is live. Pitch, rate and volume stay at 1.0, so the output rate is the input rate.
            Some(stream) => unsafe { sonicSetSpeed(stream.as_ptr(), speed) },
            None => error!("Sonic ran out of memory; sending the audio at 1.0x"),
        }
        Self { stream, speed, sample_rate, samples_in: 0, samples_out: 0 }
    }

    /// The byte arrays to send for `chunk`, never empty ones except for an empty `chunk` (a segment end): its tail
    /// is flushed first (padded with silence; the stream stays usable), then the empty Vec is passed on.
    pub fn process(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        if chunk.is_empty() {
            out.extend(self.flush());
            out.push(Vec::new());
            return out;
        }
        let mut samples: Vec<i16> = chunk.as_chunks().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        let len = samples.len();
        self.samples_in += len;
        let mut ready = Vec::new();
        for start in (0..len).step_by(MAX_WRITE) {
            let Some(stream) = self.stream else {
                ready.extend_from_slice(&samples[start..]);
                break;
            };
            let piece = &mut samples[start..(start + MAX_WRITE).min(len)];
            // SAFETY: `stream` is live and `piece` holds `piece.len()` samples.
            if unsafe { sonicWriteShortToStream(stream.as_ptr(), piece.as_mut_ptr(), piece.len() as c_int) } != 1 {
                self.fail();
                ready.extend_from_slice(&samples[start..]);
                break;
            }
            ready.extend(self.read());
        }
        out.extend(self.bytes(ready));
        out
    }

    /// Flushes the held-back tail (`None` if there is none) and logs `Speed {:.1}x: sent A ms as B ms`.
    pub fn finish(mut self) -> Option<Vec<u8>> {
        let tail = self.flush();
        let ms = |samples: usize| samples as u64 * 1000 / self.sample_rate as u64;
        info!("Speed {:.1}x: sent {} ms as {} ms", self.speed, ms(self.samples_in), ms(self.samples_out));
        tail
    }

    /// Everything Sonic still holds, padded with silence; `None` if nothing.
    fn flush(&mut self) -> Option<Vec<u8>> {
        let stream = self.stream?;
        // SAFETY: `stream` is live.
        if unsafe { sonicFlushStream(stream.as_ptr()) } != 1 {
            self.fail();
            return None;
        }
        let tail = self.read();
        self.bytes(tail)
    }

    /// Everything Sonic has ready.
    fn read(&mut self) -> Vec<i16> {
        let Some(stream) = self.stream else {
            return Vec::new();
        };
        // SAFETY: `stream` is live.
        let available = unsafe { sonicSamplesAvailable(stream.as_ptr()) };
        if available <= 0 {
            return Vec::new();
        }
        let mut out = vec![0; available as usize];
        // SAFETY: `out` has room for `available` samples.
        let read = unsafe { sonicReadShortFromStream(stream.as_ptr(), out.as_mut_ptr(), available) };
        out.truncate(read.max(0) as usize);
        out
    }

    /// `samples` as PCM16 LE bytes, counted as sent; `None` if there are none.
    fn bytes(&mut self, samples: Vec<i16>) -> Option<Vec<u8>> {
        self.samples_out += samples.len();
        (!samples.is_empty()).then(|| samples.iter().flat_map(|s| s.to_le_bytes()).collect())
    }

    /// Drops the stream after Sonic ran out of memory.
    fn fail(&mut self) {
        error!("Sonic ran out of memory; sending the rest at 1.0x");
        if let Some(stream) = self.stream.take() {
            // SAFETY: `stream` is live and not used again.
            unsafe { sonicDestroyStream(stream.as_ptr()) };
        }
    }
}

impl Drop for TimeStretch {
    fn drop(&mut self) {
        if let Some(stream) = self.stream.take() {
            // SAFETY: `stream` is live and not used again.
            unsafe { sonicDestroyStream(stream.as_ptr()) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::PI;

    use super::*;

    /// `seconds` of a voice-like 150 Hz harmonic signal at `rate`, as PCM16 LE in 50 ms chunks.
    fn voice(rate: u32, seconds: u32) -> Vec<Vec<u8>> {
        pcm(rate, seconds, |t| {
            (1..=5).map(|h| (2.0 * PI * 150.0 * h as f64 * t).sin() / h as f64).sum::<f64>() * 6_000.0
        })
    }

    /// `seconds` of a 200 Hz sine at `rate`, as PCM16 LE in 50 ms chunks.
    fn sine(rate: u32, seconds: u32) -> Vec<Vec<u8>> {
        pcm(rate, seconds, |t| (2.0 * PI * 200.0 * t).sin() * 10_000.0)
    }

    fn pcm(rate: u32, seconds: u32, signal: impl Fn(f64) -> f64) -> Vec<Vec<u8>> {
        let chunk_samples = rate / 20;
        (0..seconds * 20)
            .map(|c| {
                (0..chunk_samples)
                    .flat_map(|i| {
                        let t = (c * chunk_samples + i) as f64 / rate as f64;
                        (signal(t).round() as i16).to_le_bytes()
                    })
                    .collect()
            })
            .collect()
    }

    fn samples(bytes: &[u8]) -> Vec<i16> {
        bytes.as_chunks().0.iter().map(|b| i16::from_le_bytes(*b)).collect()
    }

    /// Everything the stream gives out for `chunks`, the tail from `finish` included.
    fn stretch(chunks: &[Vec<u8>], rate: u32, speed: f32) -> Vec<Vec<u8>> {
        let mut stretch = TimeStretch::new(rate, speed);
        let mut out: Vec<Vec<u8>> = chunks.iter().flat_map(|chunk| stretch.process(chunk)).collect();
        out.extend(stretch.finish());
        out
    }

    fn total(chunks: &[Vec<u8>]) -> usize {
        chunks.iter().map(Vec::len).sum()
    }

    /// C Sonic ends ~5 ms short (Handy's test), over 1% of a second's output: one-second inputs get 2%.
    fn within(actual: usize, expected: f64, tolerance: f64) -> bool {
        (actual as f64 - expected).abs() <= expected * tolerance
    }

    #[test]
    fn output_length_is_input_divided_by_speed() {
        for rate in [16_000, 24_000] {
            for speed in [1.5, 2.0] {
                let input = voice(rate, 5);
                let in_bytes = total(&input);
                let out_bytes = total(&stretch(&input, rate, speed));
                let expected = in_bytes as f64 / speed as f64;
                assert!(within(out_bytes, expected, 0.01), "{rate} Hz {speed}x: {out_bytes} of {in_bytes}");
            }
        }
    }

    #[test]
    fn the_tail_is_flushed_after_upstream_completes() {
        let input = voice(16_000, 1);
        let mut stretch = TimeStretch::new(16_000, 2.0);
        let mut all: Vec<Vec<u8>> = input.iter().flat_map(|chunk| stretch.process(chunk)).collect();

        let tail = stretch.finish().expect("a tail");

        assert!(!tail.is_empty());
        all.push(tail);
        let expected = total(&input) as f64 / 2.0;
        assert!(within(total(&all), expected, 0.02), "{} of {expected}", total(&all));
    }

    /// Kotlin `upstreamErrorPropagatesUnchangedWithoutFlush`: on an error the stream is dropped, never finished.
    #[test]
    fn dropping_flushes_nothing() {
        let input = voice(16_000, 1);
        let mut stretch = TimeStretch::new(16_000, 2.0);
        let out = total(&input.iter().flat_map(|chunk| stretch.process(chunk)).collect::<Vec<_>>());
        drop(stretch);

        let expected = total(&input) as f64 / 2.0;
        assert!((out as f64) < expected);
        assert!(out as f64 > 0.9 * expected);
    }

    #[test]
    fn finish_without_audio_is_none() {
        assert!(TimeStretch::new(16_000, 2.0).finish().is_none());
    }

    #[test]
    fn never_emits_empty_arrays() {
        let mut input = vec![vec![0; 2]];
        input.extend(voice(24_000, 1));

        let output = stretch(&input, 24_000, 1.1);

        assert!(!output.is_empty());
        assert!(output.iter().all(|chunk| !chunk.is_empty()));
    }

    #[test]
    fn an_empty_input_flushes_then_is_passed_on() {
        let first = voice(16_000, 1);
        let second = voice(16_000, 1);
        let mut input = first.clone();
        input.push(Vec::new());
        input.extend(second);

        let output = stretch(&input, 16_000, 2.0);

        let ends: Vec<usize> = (0..output.len()).filter(|&i| output[i].is_empty()).collect();
        assert_eq!(1, ends.len());
        // Everything of the first segment came out before the marker, the second's after it.
        let before = total(&output[..ends[0]]);
        let expected = total(&first) as f64 / 2.0;
        assert!(within(before, expected, 0.02), "{before} of {expected}");
        let after = total(&output[ends[0] + 1..]);
        assert!(within(after, expected, 0.02), "{after} of {expected}");
    }

    #[test]
    fn pitch_is_kept() {
        let input = samples(&sine(16_000, 2).concat());
        let output = samples(&stretch(&sine(16_000, 2), 16_000, 2.0).concat());

        // Zero crossings per sample: 400 per second at 200 Hz.
        let crossing_rate =
            |s: &[i16]| s.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count() as f64 / s.len() as f64;
        let expected = crossing_rate(&input);
        assert!((expected - 400.0 / 16_000.0).abs() <= 0.001);
        assert!((crossing_rate(&output) - expected).abs() <= expected * 0.05);
    }
}
