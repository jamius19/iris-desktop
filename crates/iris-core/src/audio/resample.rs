/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! PCM16 helpers for the VAD: Android `SpeechGate.frame`'s 24 → 16 kHz 3:2 downsampler.

/// Little-endian PCM16 sample `index` of `pcm`.
pub fn sample(pcm: &[u8], index: usize) -> i32 {
    i16::from_le_bytes([pcm[2 * index], pcm[2 * index + 1]]) as i32
}

/// Fills `out` (an even length) with 16 kHz samples, -1..1, from 24 kHz PCM16 LE `pcm` (at least `out.len() * 3 / 2`
/// samples): of every three samples `s0, s1, s2`, keeps `s0` and averages `s1, s2`.
pub fn downsample_24k_to_16k(pcm: &[u8], out: &mut [f32]) {
    for j in 0..out.len() / 2 {
        out[2 * j] = sample(pcm, 3 * j) as f32 / 32768.0;
        out[2 * j + 1] = (sample(pcm, 3 * j + 1) + sample(pcm, 3 * j + 2)) as f32 / 65536.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(samples: &[i16]) -> Vec<u8> {
        samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn samples_are_little_endian_and_signed() {
        let bytes = pcm(&[1, -1, i16::MAX, i16::MIN, 0x1234]);
        assert_eq!(vec![1, -1, 32767, -32768, 0x1234], (0..5).map(|i| sample(&bytes, i)).collect::<Vec<_>>());
    }

    #[test]
    fn three_samples_become_the_first_and_the_mean_of_the_others() {
        let bytes = pcm(&[300, 100, 200, -32768, 32767, -32768]);
        let mut out = [0.0; 4];

        downsample_24k_to_16k(&bytes, &mut out);

        assert_eq!([300.0 / 32768.0, 300.0 / 65536.0, -1.0, -1.0 / 65536.0], out);
    }

    #[test]
    fn a_frame_of_512_takes_768_samples() {
        let ramp: Vec<i16> = (0..768).collect();
        let mut out = [0.0; 512];

        downsample_24k_to_16k(&pcm(&ramp), &mut out);

        for (i, value) in out.iter().enumerate() {
            let s = 3 * (i / 2) as i32;
            let expected = if i % 2 == 0 { s as f32 / 32768.0 } else { (2 * s + 3) as f32 / 65536.0 };
            assert_eq!(expected, *value, "sample {i}");
        }
    }
}
