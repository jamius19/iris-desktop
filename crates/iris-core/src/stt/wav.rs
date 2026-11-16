/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! PCM helpers from Android's `Wav.kt` and `AudioLevel.kt`: the WAV file and read timeout for the providers that
//! upload after stop, and splitting long audio at pauses (upload size limits; the on-device provider caps a segment
//! at 20 s).

use std::time::Duration;

const MIN_DB: f32 = -55.0;
const MAX_DB: f32 = -12.0;

/// Size of the RIFF/WAVE header [`wav_bytes`] writes.
pub const WAV_HEADER_BYTES: usize = 44;

/// A PCM16 mono WAV file of `chunks` at `sample_rate`. Audio under 1 s is padded with silence to 1.25 s, as Handy
/// does (some services reject very short files).
pub fn wav_bytes(chunks: &[Vec<u8>], sample_rate: u32) -> Vec<u8> {
    let bytes_per_second = sample_rate * 2;
    let audio_bytes: usize = chunks.iter().map(Vec::len).sum();
    let data_bytes =
        if audio_bytes < bytes_per_second as usize { bytes_per_second as usize * 5 / 4 } else { audio_bytes };
    let mut wav = Vec::with_capacity(WAV_HEADER_BYTES + data_bytes);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_bytes as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // Mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&bytes_per_second.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes()); // Block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // Bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_bytes as u32).to_le_bytes());
    for chunk in chunks {
        wav.extend_from_slice(chunk);
    }
    wav.resize(WAV_HEADER_BYTES + data_bytes, 0);
    wav
}

/// How long an upload may wait for the response once it is sent: the STT timeout setting plus a quarter of the
/// audio's duration, as transcribing takes longer the more audio there is.
pub fn upload_read_timeout(timeout: Duration, audio: Duration) -> Duration {
    timeout + audio / 4
}

/// How long an upload of `audio` may take to send before the read timeout applies: OkHttp's 10 s write timeout plus
/// the audio's duration (an uplink of at least real-time pace, 256 kbit/s at 16 kHz). Kotlin's read timeout only
/// starts once the body is sent; a single request timeout covers both here.
pub fn upload_send_allowance(audio: Duration) -> Duration {
    Duration::from_secs(10) + audio
}

/// Duration of `bytes` of PCM16 mono audio at `sample_rate`.
pub fn pcm16_duration(bytes: usize, sample_rate: u32) -> Duration {
    Duration::from_millis(bytes as u64 * 1000 / (sample_rate as u64 * 2))
}

/// Loudness of little-endian PCM16 `bytes` (a trailing odd byte is ignored), 0..1: RMS in dBFS mapped from
/// -55..-12 dB, then eased (`pow 0.7`) so quiet speech still shows.
pub fn pcm16_level(bytes: &[u8]) -> f32 {
    let samples = bytes.len() / 2;
    if samples == 0 {
        return 0.0;
    }
    let sum: f64 = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let sample = i16::from_le_bytes(*pair) as f64;
            sample * sample
        })
        .sum();
    let rms = (sum / samples as f64).sqrt();
    if rms == 0.0 {
        return 0.0;
    }
    let db = 20.0 * (rms / 32768.0).log10() as f32;
    ((db - MIN_DB) / (MAX_DB - MIN_DB)).clamp(0.0, 1.0).powf(0.7)
}

/// Splits `chunks` into parts of at most `max_bytes` each (a single larger chunk is a part of its own). Each cut is
/// after the quietest chunk within the last `search_bytes` of the part (the latest of equally quiet ones), so it lands
/// in a pause rather than mid-word.
pub fn split_at_pauses(chunks: &[Vec<u8>], max_bytes: usize, search_bytes: usize) -> Vec<Vec<Vec<u8>>> {
    let mut parts = Vec::new();
    let mut start = 0;
    while start < chunks.len() {
        // The longest part from `start` that fits.
        let mut end = start;
        let mut size = 0;
        while end < chunks.len() && (end == start || size + chunks[end].len() <= max_bytes) {
            size += chunks[end].len();
            end += 1;
        }
        if end == chunks.len() {
            parts.push(chunks[start..end].to_vec());
            break;
        }
        // Back up within the search window to the quietest chunk; the part ends after it.
        let mut cut = end;
        let mut quietest = f32::MAX;
        let mut candidate = end;
        let mut tail = 0;
        while candidate > start && tail < search_bytes {
            let level = pcm16_level(&chunks[candidate - 1]);
            if level < quietest {
                quietest = level;
                cut = candidate;
            }
            tail += chunks[candidate - 1].len();
            candidate -= 1;
        }
        parts.push(chunks[start..cut].to_vec());
        start = cut;
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHUNK: usize = 1_600;

    // Android WavTest.

    #[test]
    fn header_describes_pcm16_mono_and_chunks_follow_in_order() {
        let (a, b) = (vec![1u8; 16_000], vec![2u8; 16_002]);

        let wav = wav_bytes(&[a.clone(), b.clone()], 16_000);

        let data = 32_002u32;
        let int = |at: usize| u32::from_le_bytes(wav[at..at + 4].try_into().unwrap());
        let short = |at: usize| u16::from_le_bytes(wav[at..at + 2].try_into().unwrap());
        assert_eq!(WAV_HEADER_BYTES + data as usize, wav.len());
        assert_eq!(b"RIFF", &wav[0..4]);
        assert_eq!(36 + data, int(4));
        assert_eq!(b"WAVE", &wav[8..12]);
        assert_eq!(b"fmt ", &wav[12..16]);
        assert_eq!(16, int(16));
        assert_eq!(1, short(20)); // PCM
        assert_eq!(1, short(22)); // Mono
        assert_eq!(16_000, int(24));
        assert_eq!(32_000, int(28));
        assert_eq!(2, short(32));
        assert_eq!(16, short(34));
        assert_eq!(b"data", &wav[36..40]);
        assert_eq!(data, int(40));
        assert_eq!([a, b].concat(), wav[WAV_HEADER_BYTES..]);
    }

    #[test]
    fn audio_under_one_second_is_padded_to_one_and_a_quarter_seconds() {
        let half = vec![3u8; 16_000]; // 0.5 s at 16 kHz.

        let wav = wav_bytes(std::slice::from_ref(&half), 16_000);

        assert_eq!(WAV_HEADER_BYTES + 40_000, wav.len());
        assert_eq!(40_000, u32::from_le_bytes(wav[40..44].try_into().unwrap()));
        assert_eq!(half, wav[WAV_HEADER_BYTES..WAV_HEADER_BYTES + 16_000]);
        assert!(wav[WAV_HEADER_BYTES + 16_000..].iter().all(|b| *b == 0));
    }

    #[test]
    fn one_second_or_more_is_not_padded() {
        assert_eq!(WAV_HEADER_BYTES + 32_000, wav_bytes(&[vec![0; 32_000]], 16_000).len());
        assert_eq!(WAV_HEADER_BYTES + 96_000, wav_bytes(&[vec![0; 96_000]], 24_000).len());
    }

    #[test]
    fn upload_timeout_adds_a_quarter_of_the_audio() {
        let s = Duration::from_secs;
        assert_eq!(s(10), upload_read_timeout(s(10), s(0)));
        assert_eq!(s(25), upload_read_timeout(s(10), s(60)));
    }

    /// A loud 50 ms chunk at 16 kHz: every sample 0x4000.
    fn loud() -> Vec<u8> {
        (0..CHUNK).map(|i| if i % 2 == 1 { 0x40 } else { 0 }).collect()
    }

    fn quiet() -> Vec<u8> {
        vec![0; CHUNK]
    }

    fn sizes(parts: &[Vec<Vec<u8>>]) -> Vec<usize> {
        parts.iter().map(Vec::len).collect()
    }

    #[test]
    fn audio_within_the_limit_is_one_part() {
        let chunks: Vec<_> = (0..10).map(|_| loud()).collect();

        let parts = split_at_pauses(&chunks, 10 * CHUNK, 4 * CHUNK);

        assert_eq!(vec![chunks], parts);
    }

    #[test]
    fn cuts_after_the_quietest_chunk_in_the_search_window() {
        let chunks: Vec<_> = (0..25).map(|i| if i == 7 { quiet() } else { loud() }).collect();

        let parts = split_at_pauses(&chunks, 10 * CHUNK, 4 * CHUNK);

        // Part 1 ends after the quiet chunk 7; the rest has no pause in its windows, so it is cut at the limit.
        assert_eq!(vec![8, 10, 7], sizes(&parts));
        assert_eq!(&quiet(), parts[0].last().unwrap());
        assert_eq!(chunks, parts.concat());
    }

    #[test]
    fn quiet_chunk_outside_the_window_is_not_used() {
        let chunks: Vec<_> = (0..15).map(|i| if i == 2 { quiet() } else { loud() }).collect();

        let parts = split_at_pauses(&chunks, 10 * CHUNK, 4 * CHUNK);

        assert_eq!(vec![10, 5], sizes(&parts));
    }

    #[test]
    fn equally_quiet_chunks_cut_at_the_latest() {
        let chunks: Vec<_> = (0..15).map(|_| quiet()).collect();

        let parts = split_at_pauses(&chunks, 10 * CHUNK, 4 * CHUNK);

        assert_eq!(vec![10, 5], sizes(&parts));
        assert!(parts.iter().all(|part| part.iter().map(Vec::len).sum::<usize>() <= 10 * CHUNK));
    }

    #[test]
    fn level_is_zero_for_silence_and_full_for_loud_audio() {
        assert_eq!(0.0, pcm16_level(&quiet()));
        assert_eq!(0.0, pcm16_level(&[]));
        assert_eq!(1.0, pcm16_level(&loud()));
    }
}
