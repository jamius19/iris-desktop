/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Voice activity gate for one recording (Android `SpeechGate.kt`, Handy's SmoothedVad): only audio around speech is
//! sent. Splits PCM16 LE mono chunks at the sample rate (16 or 24 kHz) into 32 ms frames, scores each with the
//! injected [`VadScorer`] ([`VAD_FRAME_SAMPLES`] samples at 16 kHz, -1..1; 24 kHz is downsampled 3:2 first) and keeps
//! frames once [`ONSET_FRAMES`] in a row score above [`THRESHOLD`], until the hangover after the last voiced one. A
//! chunk is sent whole if a frame completed in it is kept; the first kept chunk also sends the unsent chunks of the
//! last [`PRE_ROLL_MS`]. If scoring fails, fails open: [`SpeechGate::failure`] is set and every chunk is sent from then
//! on. With segment ends, an empty array follows the chunk in which a segment's hangover ran out (none in fail-open
//! mode, nor at the stop).

use std::collections::VecDeque;

use super::resample::{downsample_24k_to_16k, sample};

/// Scores one [`VAD_FRAME_SAMPLES`]-sample 16 kHz frame (-1..1): the speech probability, or why it couldn't.
pub type VadScorer = Box<dyn FnMut(&[f32]) -> Result<f32, String> + Send>;

// Handy's VAD parameters (src-tauri/src/audio_toolkit/vad/mod.rs, Silero with a 0.3 threshold).
pub const THRESHOLD: f32 = 0.3;
pub const ONSET_FRAMES: u32 = 2;
pub const PRE_ROLL_MS: u64 = 450;

/// Hangover for providers that stream while recording: the server's own segmentation needs more tail.
pub const STREAMING_HANGOVER_MS: u64 = 1650;

/// Hangover for providers that upload after stop.
pub const OFFLINE_HANGOVER_MS: u64 = 450;

pub const VAD_RATE: u32 = 16_000;
pub const VAD_FRAME_SAMPLES: usize = 512;

/// Whole VAD frames covering `ms`, rounded up (Handy's `frames_for_duration_ms`).
fn frames_for(ms: u64) -> u64 {
    (ms * VAD_RATE as u64).div_ceil(VAD_FRAME_SAMPLES as u64 * 1000)
}

pub struct SpeechGate {
    sample_rate: u32,
    vad: VadScorer,
    segment_ends: bool,
    failure: Option<String>,
    frame_bytes: usize,
    pre_roll_bytes: usize,
    hangover_frames: u64,
    samples: [f32; VAD_FRAME_SAMPLES],
    /// Bytes of an incomplete frame, carried to the next chunk.
    pending: Vec<u8>,
    pre_roll: VecDeque<Vec<u8>>,
    pre_roll_size: usize,
    in_speech: bool,
    onset: u32,
    hangover: u64,
    /// A segment's hangover ran out in the chunk being processed (only with segment ends).
    segment_ended: bool,
}

impl SpeechGate {
    /// A gate for `sample_rate` (16 or 24 kHz) audio; `segment_ends` marks where a segment's hangover ran out.
    pub fn new(sample_rate: u32, hangover_ms: u64, vad: VadScorer, segment_ends: bool) -> Self {
        assert!(sample_rate == 16_000 || sample_rate == 24_000, "Unsupported rate {sample_rate}");
        let rate = sample_rate as usize;
        Self {
            sample_rate,
            vad,
            segment_ends,
            failure: None,
            frame_bytes: rate * 2 * VAD_FRAME_SAMPLES / VAD_RATE as usize,
            pre_roll_bytes: rate * 2 * PRE_ROLL_MS as usize / 1000,
            hangover_frames: frames_for(hangover_ms),
            samples: [0.0; VAD_FRAME_SAMPLES],
            pending: Vec::new(),
            pre_roll: VecDeque::new(),
            pre_roll_size: 0,
            in_speech: false,
            onset: 0,
            hangover: 0,
            segment_ended: false,
        }
    }

    /// Set once the VAD failed (everything is sent from then on).
    pub fn failure(&self) -> Option<&str> {
        self.failure.as_deref()
    }

    /// The byte arrays to send for `chunk`, in order: none, `[chunk]`, or the pre-roll followed by `chunk`; then an
    /// empty Vec if a segment ended in `chunk` (only with segment ends).
    pub fn process(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        if self.failure.is_some() {
            return vec![chunk.to_vec()];
        }
        self.segment_ended = false;
        let kept = match self.keeps_chunk(chunk) {
            Ok(kept) => kept,
            Err(e) => {
                self.failure = Some(e);
                self.segment_ended = false;
                true
            }
        };
        let end = self.segment_ended;
        let mut out = Vec::new();
        if !kept {
            self.pre_roll.push_back(chunk.to_vec());
            self.pre_roll_size += chunk.len();
            // Drop the oldest chunks while the rest still cover the pre-roll.
            while self.pre_roll_size - self.pre_roll[0].len() >= self.pre_roll_bytes {
                self.pre_roll_size -= self.pre_roll.pop_front().unwrap().len();
            }
        } else {
            out.extend(self.pre_roll.drain(..));
            out.push(chunk.to_vec());
            self.pre_roll_size = 0;
        }
        if end {
            out.push(Vec::new());
        }
        out
    }

    /// Runs every frame `chunk` completes; true if one of them is kept (or none completed while in speech).
    fn keeps_chunk(&mut self, chunk: &[u8]) -> Result<bool, String> {
        self.pending.extend_from_slice(chunk);
        let mut offset = 0;
        let mut completed = false;
        let mut kept = false;
        while self.pending.len() - offset >= self.frame_bytes {
            completed = true;
            let frame = &self.pending[offset..offset + self.frame_bytes];
            if self.sample_rate == VAD_RATE {
                for (i, s) in self.samples.iter_mut().enumerate() {
                    *s = sample(frame, i) as f32 / 32768.0;
                }
            } else {
                downsample_24k_to_16k(frame, &mut self.samples);
            }
            let voiced = (self.vad)(&self.samples)? > THRESHOLD;
            if self.keeps_frame(voiced) {
                kept = true;
            }
            offset += self.frame_bytes;
        }
        self.pending.drain(..offset);
        Ok(kept || (!completed && self.in_speech))
    }

    /// Handy's `SmoothedVad.push_frame`: whether a frame is sent.
    fn keeps_frame(&mut self, voiced: bool) -> bool {
        match (self.in_speech, voiced) {
            (false, true) => {
                self.onset += 1;
                if self.onset >= ONSET_FRAMES {
                    self.in_speech = true;
                    self.hangover = self.hangover_frames;
                    self.onset = 0;
                    true
                } else {
                    false
                }
            }
            (true, true) => {
                self.hangover = self.hangover_frames;
                true
            }
            (true, false) => {
                if self.hangover > 0 {
                    self.hangover -= 1;
                    true
                } else {
                    self.in_speech = false;
                    if self.segment_ends {
                        self.segment_ended = true;
                    }
                    false
                }
            }
            (false, false) => {
                self.onset = 0;
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    /// Speech = any non-zero sample.
    fn model() -> VadScorer {
        Box::new(|frame| Ok(if frame.iter().any(|s| *s != 0.0) { 1.0 } else { 0.0 }))
    }

    fn gate(hangover_ms: u64) -> SpeechGate {
        SpeechGate::new(16_000, hangover_ms, model(), false)
    }

    /// One 32 ms frame at 16 kHz, tagged with `id` in its first sample (0 = silence).
    fn frame(id: u8) -> Vec<u8> {
        let mut frame = vec![0; 1024];
        frame[0] = id;
        frame
    }

    /// `count` little-endian PCM16 samples counting up from `start`.
    fn ramp(start: usize, count: usize) -> Vec<u8> {
        (start..start + count).flat_map(|s| (s as i16).to_le_bytes()).collect()
    }

    /// A model that records every frame it scores and returns 0.
    fn recording_model() -> (VadScorer, Arc<Mutex<Vec<Vec<f32>>>>) {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let seen = frames.clone();
        let model: VadScorer = Box::new(move |frame| {
            seen.lock().unwrap().push(frame.to_vec());
            Ok(0.0)
        });
        (model, frames)
    }

    #[test]
    fn silence_is_never_sent() {
        let mut gate = gate(STREAMING_HANGOVER_MS);
        for _ in 0..100 {
            assert!(gate.process(&frame(0)).is_empty());
        }
    }

    #[test]
    fn a_single_voiced_frame_is_not_speech_and_the_onset_resets_on_silence() {
        let mut gate = gate(STREAMING_HANGOVER_MS);
        for _ in 0..10 {
            assert!(gate.process(&frame(0)).is_empty());
            assert!(gate.process(&frame(1)).is_empty());
        }
    }

    #[test]
    fn onset_sends_the_pre_roll_oldest_first_then_each_speech_chunk_alone() {
        let mut gate = gate(STREAMING_HANGOVER_MS);
        let chunks: Vec<Vec<u8>> = (0..20).map(|_| vec![0; 1024]).collect();
        for chunk in &chunks {
            assert!(gate.process(chunk).is_empty());
        }
        let first = frame(1);
        assert!(gate.process(&first).is_empty());
        let onset = frame(2);

        let sent = gate.process(&onset);

        // 450 ms = 14.06 frames: the last 15 unsent chunks (the first voiced one included), then the onset chunk.
        assert_eq!(16, sent.len());
        assert_eq!(chunks[6..], sent[..14]);
        assert_eq!(first, sent[14]);
        assert_eq!(onset, sent[15]);
        let next = frame(3);
        assert_eq!(vec![next.clone()], gate.process(&next));
    }

    #[test]
    fn streaming_hangover_sends_fifty_two_frames_after_the_last_voiced_one() {
        assert_hangover(STREAMING_HANGOVER_MS, 52);
    }

    #[test]
    fn offline_hangover_sends_fifteen_frames_after_the_last_voiced_one() {
        assert_hangover(OFFLINE_HANGOVER_MS, 15);
    }

    fn assert_hangover(hangover_ms: u64, frames: usize) {
        let mut gate = gate(hangover_ms);
        gate.process(&frame(1));
        assert_eq!(2, gate.process(&frame(1)).len());
        for _ in 0..frames {
            assert_eq!(1, gate.process(&frame(0)).len());
        }
        for _ in 0..5 {
            assert!(gate.process(&frame(0)).is_empty());
        }
    }

    #[test]
    fn later_onset_sends_only_chunks_that_werent_sent_yet() {
        let mut gate = gate(OFFLINE_HANGOVER_MS);
        gate.process(&frame(1));
        gate.process(&frame(1));
        for _ in 0..15 {
            gate.process(&frame(0)); // Hangover: all sent.
        }
        let unsent: Vec<Vec<u8>> = (0..3).map(|_| vec![0; 1024]).collect();
        for chunk in &unsent {
            assert!(gate.process(chunk).is_empty());
        }
        let first = frame(1);
        gate.process(&first);
        let onset = frame(1);

        let mut expected = unsent.clone();
        expected.extend([first, onset.clone()]);
        assert_eq!(expected, gate.process(&onset));
    }

    #[test]
    fn speech_at_the_start_is_sent_from_the_onset_with_whatever_pre_roll_exists() {
        let mut gate = gate(STREAMING_HANGOVER_MS);
        let first = frame(1);
        assert!(gate.process(&first).is_empty());
        let second = frame(1);
        assert_eq!(vec![first, second.clone()], gate.process(&second));
    }

    #[test]
    fn threshold_is_strict() {
        let mut at = SpeechGate::new(16_000, STREAMING_HANGOVER_MS, Box::new(|_| Ok(0.3)), false);
        for _ in 0..10 {
            assert!(at.process(&frame(0)).is_empty());
        }
        let mut above = SpeechGate::new(16_000, STREAMING_HANGOVER_MS, Box::new(|_| Ok(0.31)), false);
        assert!(above.process(&frame(0)).is_empty());
        assert_eq!(2, above.process(&frame(0)).len());
    }

    #[test]
    fn frames_span_chunk_boundaries_and_are_scaled() {
        let (model, frames) = recording_model();
        let mut gate = SpeechGate::new(16_000, STREAMING_HANGOVER_MS, model, false);
        // 50 ms chunks (800 samples) of a sample ramp 0, 1, 2, ...
        for chunk in 0..4 {
            gate.process(&ramp(chunk * 800, 800));
        }

        let frames = frames.lock().unwrap();
        assert_eq!(6, frames.len()); // 3200 samples = 6 whole frames, 128 carried.
        for (f, frame) in frames.iter().enumerate() {
            let expected: Vec<f32> = (0..512).map(|i| (f * 512 + i) as f32 / 32768.0).collect();
            assert_eq!(&expected, frame);
        }
    }

    #[test]
    fn chunks_completing_no_frame_follow_the_speech_state() {
        let mut gate = gate(STREAMING_HANGOVER_MS);
        // 16 ms chunks: every other one completes a frame.
        let voiced = vec![1; 512];
        assert!(gate.process(&voiced).is_empty());
        assert!(gate.process(&voiced).is_empty()); // Frame 1: onset 1.
        assert!(gate.process(&voiced).is_empty());
        assert_eq!(4, gate.process(&voiced).len()); // Frame 2: onset, with the pre-roll.
        assert_eq!(1, gate.process(&voiced).len()); // No frame completed, in speech.
    }

    #[test]
    fn twenty_four_kilohertz_frames_are_downsampled_for_the_model() {
        let (model, frames) = recording_model();
        let mut gate = SpeechGate::new(24_000, STREAMING_HANGOVER_MS, model, false);
        // 50 ms chunks at 24 kHz (1200 samples).
        for chunk in 0..2 {
            gate.process(&ramp(chunk * 1200, 1200));
        }

        let frames = frames.lock().unwrap();
        assert_eq!(3, frames.len()); // 2400 samples = 3 frames of 768, 96 carried.
        for (f, frame) in frames.iter().enumerate() {
            let base = f * 768;
            for (i, value) in frame.iter().enumerate() {
                let s = base + 3 * (i / 2);
                let expected = if i % 2 == 0 { s as f32 / 32768.0 } else { (2 * s + 3) as f32 / 65536.0 };
                assert!((expected - value).abs() <= 1e-7, "frame {f} sample {i}");
            }
        }
    }

    #[test]
    fn model_failure_fails_open() {
        let calls = Arc::new(Mutex::new(0));
        let counted = calls.clone();
        let model: VadScorer = Box::new(move |_| {
            let mut calls = counted.lock().unwrap();
            *calls += 1;
            if *calls == 3 { Err("VAD model unavailable".to_string()) } else { Ok(0.0) }
        });
        let mut gate = SpeechGate::new(16_000, STREAMING_HANGOVER_MS, model, false);
        let silent: Vec<Vec<u8>> = (0..2).map(|_| vec![0; 1024]).collect();
        for chunk in &silent {
            assert!(gate.process(chunk).is_empty());
        }
        let failing = frame(0);

        let mut expected = silent.clone();
        expected.push(failing.clone());
        assert_eq!(expected, gate.process(&failing));
        assert_eq!(Some("VAD model unavailable"), gate.failure());
        let later = frame(0);
        assert_eq!(vec![later.clone()], gate.process(&later));
        assert_eq!(3, *calls.lock().unwrap());
    }

    #[test]
    fn segment_end_is_marked_alone_when_the_chunk_is_withheld() {
        let mut gate = SpeechGate::new(16_000, OFFLINE_HANGOVER_MS, model(), true);
        gate.process(&frame(1));
        assert_eq!(2, gate.process(&frame(1)).len());
        for _ in 0..15 {
            assert_eq!(1, gate.process(&frame(0)).len());
        }

        let end = gate.process(&frame(0));

        assert_eq!(1, end.len());
        assert!(end[0].is_empty());
        for _ in 0..5 {
            assert!(gate.process(&frame(0)).is_empty());
        }
    }

    #[test]
    fn segment_end_follows_the_last_kept_chunk() {
        let mut gate = SpeechGate::new(16_000, OFFLINE_HANGOVER_MS, model(), true);
        // Two frames per chunk: frame 16 after the speech (the end) shares a chunk with the last hangover frame.
        let mut speech = vec![0; 2048];
        speech[0] = 1;
        speech[1024] = 1;
        assert_eq!(vec![speech.clone()], gate.process(&speech));
        for _ in 0..7 {
            assert_eq!(1, gate.process(&[0; 2048]).len());
        }
        let last = vec![0; 2048];

        let sent = gate.process(&last);

        assert_eq!(2, sent.len());
        assert_eq!(last, sent[0]);
        assert!(sent[1].is_empty());
        assert!(gate.process(&[0; 2048]).is_empty());
    }

    #[test]
    fn no_segment_end_without_the_flag() {
        let mut gate = gate(OFFLINE_HANGOVER_MS);
        gate.process(&frame(1));
        gate.process(&frame(1));
        let sent: Vec<Vec<u8>> = (0..30).flat_map(|_| gate.process(&frame(0))).collect();
        assert_eq!(15, sent.len());
        assert!(sent.iter().all(|chunk| !chunk.is_empty()));
    }

    #[test]
    fn hangover_frames_round_up_like_handy() {
        assert_eq!(52, frames_for(STREAMING_HANGOVER_MS));
        assert_eq!(15, frames_for(OFFLINE_HANGOVER_MS));
        assert_eq!(15, frames_for(PRE_ROLL_MS));
    }
}
