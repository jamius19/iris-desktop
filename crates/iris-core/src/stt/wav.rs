/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! PCM helpers from Android's `Wav.kt` and `AudioLevel.kt`: splitting long audio at pauses (the on-device provider
//! caps a segment at 20 s). The WAV encoding for upload providers comes with them in M3.

const MIN_DB: f32 = -55.0;
const MAX_DB: f32 = -12.0;

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
