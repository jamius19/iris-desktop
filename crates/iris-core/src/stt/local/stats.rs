/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! On-device models' speed statistics (Android `OnDeviceStats.kt`): samples, load times and speed verdicts. Stored by
//! [`crate::dictation::statistics`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::models::{ON_DEVICE_MODELS, OnDeviceModel};

/// One dictation: recorded speech (before Speed), the engine's busy time, stop -> final text.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatsSample {
    pub speech_ms: i64,
    pub decode_ms: i64,
    pub wait_ms: i64,
}

/// A model's last [`MAX_SAMPLES`] dictations, oldest first, and its last load time.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelStats {
    #[serde(default)]
    pub samples: Vec<StatsSample>,
    #[serde(default)]
    pub last_load_ms: Option<i64>,
}

/// Each model's stats by model id.
pub type ModelStatsMap = BTreeMap<String, ModelStats>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedVerdict {
    KeepsUp,
    Borderline,
    FallsBehind,
}

pub const MAX_SAMPLES: usize = 20;

/// The last [`MAX_SAMPLES`] of `values`.
pub(crate) fn last_samples<T>(mut values: Vec<T>) -> Vec<T> {
    let excess = values.len().saturating_sub(MAX_SAMPLES);
    values.drain(..excess);
    values
}

/// Adds `sample` to `model_id`'s samples, keeping the last [`MAX_SAMPLES`].
pub fn with_sample(mut stats: ModelStatsMap, model_id: &str, sample: StatsSample) -> ModelStatsMap {
    let current = stats.entry(model_id.to_string()).or_default();
    current.samples.push(sample);
    current.samples = last_samples(std::mem::take(&mut current.samples));
    stats
}

pub fn with_load(mut stats: ModelStatsMap, model_id: &str, ms: i64) -> ModelStatsMap {
    stats.entry(model_id.to_string()).or_default().last_load_ms = Some(ms);
    stats
}

impl ModelStats {
    /// Speech per decode time over all samples (a ratio of sums: long dictations weigh more); None without samples.
    pub fn speed_factor(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let speech: i64 = self.samples.iter().map(|s| s.speech_ms).sum();
        let decode: i64 = self.samples.iter().map(|s| s.decode_ms).sum();
        Some(speech as f64 / decode as f64)
    }

    /// The median stop -> final text; an even count takes the mean of the middle two. None without samples.
    pub fn median_wait_ms(&self) -> Option<i64> {
        median(&self.samples.iter().map(|s| s.wait_ms).collect::<Vec<_>>())
    }
}

/// An even count takes the mean of the middle two. None without values.
pub fn median(values: &[i64]) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    Some(if sorted.len() % 2 == 1 { sorted[mid] } else { (sorted[mid - 1] + sorted[mid]) / 2 })
}

/// [x] rounded to one decimal, as the page shows it: the verdict and the speed text both use it, so they agree.
/// Half up, as Java's `Math.round`.
pub fn round_to_tenth(x: f64) -> f64 {
    (x * 10.0 + 0.5).floor() / 10.0
}

/// On the factor rounded like the text ([`round_to_tenth`]): 1.45 keeps up, 0.95 is borderline.
pub fn speed_verdict(factor: f64) -> SpeedVerdict {
    if round_to_tenth(factor) >= 1.5 {
        SpeedVerdict::KeepsUp
    } else if round_to_tenth(factor) >= 1.0 {
        SpeedVerdict::Borderline
    } else {
        SpeedVerdict::FallsBehind
    }
}

/// Recorded speech from the model input's `audio_ms`, which Speed shortened by `speed_percent`.
pub fn speech_ms(audio_ms: i64, speed_percent: i32) -> i64 {
    audio_ms * speed_percent as i64 / 100
}

/// The catalogue's models with at least one sample, in menu order (unknown ids dropped).
pub fn models_with_stats(stats: &ModelStatsMap) -> Vec<(&'static OnDeviceModel, &ModelStats)> {
    ON_DEVICE_MODELS
        .iter()
        .filter_map(|model| stats.get(model.id).filter(|s| !s.samples.is_empty()).map(|s| (model, s)))
        .collect()
}

#[cfg(test)]
mod tests {
    //! Android's `OnDeviceStatsTest.kt`; Moonshine (not on desktop) is Parakeet TDT v3, another per-phrase model.

    use super::*;

    const A: &str = "parakeet-tdt-v3";
    const B: &str = "whisper-small";

    fn sample(speech_ms: i64, decode_ms: i64, wait_ms: i64) -> StatsSample {
        StatsSample { speech_ms, decode_ms, wait_ms }
    }

    fn speech(speech_ms: i64) -> StatsSample {
        sample(speech_ms, 100, 200)
    }

    fn waits(ms: &[i64]) -> ModelStats {
        ModelStats { samples: ms.iter().map(|w| sample(1_000, 100, *w)).collect(), last_load_ms: None }
    }

    fn map(entries: Vec<(&str, ModelStats)>) -> ModelStatsMap {
        entries.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    fn speeches(stats: &ModelStats) -> Vec<i64> {
        stats.samples.iter().map(|s| s.speech_ms).collect()
    }

    #[test]
    fn with_sample_appends_and_leaves_the_rest_alone() {
        let other = ModelStats { samples: vec![speech(5)], last_load_ms: Some(7) };
        let before =
            map(vec![(A, ModelStats { samples: vec![speech(1)], last_load_ms: Some(4_800) }), (B, other.clone())]);

        let after = with_sample(before, A, speech(2));

        assert_eq!(vec![1, 2], speeches(&after[A]));
        assert_eq!(Some(4_800), after[A].last_load_ms);
        assert_eq!(other, after[B]);
        let created = with_sample(ModelStatsMap::new(), A, speech(3));
        assert_eq!(vec![3], speeches(&created[A]));
    }

    #[test]
    fn the_twenty_first_sample_drops_the_oldest() {
        let mut stats = ModelStatsMap::new();
        for i in 1..=21 {
            stats = with_sample(stats, A, speech(i));
        }

        assert_eq!((2..=21).collect::<Vec<_>>(), speeches(&stats[A]));
    }

    #[test]
    fn with_load_replaces_keeps_samples_and_creates() {
        let before = map(vec![(A, ModelStats { samples: vec![speech(1_000)], last_load_ms: Some(100) })]);

        let after = with_load(with_load(before, A, 200), B, 300);

        assert_eq!(ModelStats { samples: vec![speech(1_000)], last_load_ms: Some(200) }, after[A]);
        assert_eq!(ModelStats { samples: vec![], last_load_ms: Some(300) }, after[B]);
    }

    #[test]
    fn speed_factor_is_the_ratio_of_sums() {
        // A phone dictation: 22,850 ms of speech decoded in 3,219 ms.
        let phone = ModelStats { samples: vec![sample(22_850, 3_219, 200)], last_load_ms: None };
        assert!((7.098 - phone.speed_factor().unwrap()).abs() < 0.001);
        // (1,000 + 9,000) / (1,000 + 1,000), not the mean of 1 and 9.
        let two =
            ModelStats { samples: vec![sample(1_000, 1_000, 200), sample(9_000, 1_000, 200)], last_load_ms: None };
        assert_eq!(Some(5.0), two.speed_factor());
        assert_eq!(None, ModelStats::default().speed_factor());
    }

    #[test]
    fn medians() {
        assert_eq!(None, median(&[]));
        assert_eq!(Some(300), median(&[500, 100, 300]));
        assert_eq!(Some(250), median(&[400, 100, 300, 200]));
    }

    #[test]
    fn median_wait() {
        assert_eq!(Some(300), waits(&[500, 100, 300]).median_wait_ms());
        assert_eq!(Some(250), waits(&[400, 100, 300, 200]).median_wait_ms());
        assert_eq!(None, ModelStats::default().median_wait_ms());
    }

    #[test]
    fn verdicts() {
        // On the factor rounded to one decimal, like the text.
        assert_eq!(SpeedVerdict::KeepsUp, speed_verdict(1.5));
        assert_eq!(SpeedVerdict::KeepsUp, speed_verdict(1.45));
        assert_eq!(SpeedVerdict::Borderline, speed_verdict(1.44));
        assert_eq!(SpeedVerdict::Borderline, speed_verdict(1.0));
        assert_eq!(SpeedVerdict::Borderline, speed_verdict(0.95));
        assert_eq!(SpeedVerdict::FallsBehind, speed_verdict(0.94));
    }

    #[test]
    fn speech_is_scaled_back_by_speed() {
        assert_eq!(22_845, speech_ms(15_230, 150));
        assert_eq!(15_230, speech_ms(15_230, 100));
    }

    #[test]
    fn encode_decode_round_trip() {
        let stats = map(vec![
            (A, ModelStats { samples: vec![sample(1, 2, 3), sample(4, 5, 6)], last_load_ms: Some(4_800) }),
            (B, ModelStats { samples: vec![speech(1_000)], last_load_ms: None }),
        ]);

        let json = serde_json::to_string(&stats).unwrap();

        assert_eq!(stats, serde_json::from_str::<ModelStatsMap>(&json).unwrap());
    }

    #[test]
    fn decode_of_nothing_or_junk_fails() {
        // Desktop: the statistics file reads a section that doesn't decode as empty (see `dictation::statistics`).
        for junk in ["", "{not json", "[1, 2]"] {
            assert!(serde_json::from_str::<ModelStatsMap>(junk).is_err());
        }
    }

    #[test]
    fn decode_ignores_unknown_fields_and_defaults_the_load() {
        let json =
            format!(r#"{{"{A}":{{"samples":[{{"speechMs":1,"decodeMs":2,"waitMs":3,"extra":true}}],"new":1}}}}"#);

        assert_eq!(
            map(vec![(A, ModelStats { samples: vec![sample(1, 2, 3)], last_load_ms: None })]),
            serde_json::from_str::<ModelStatsMap>(&json).unwrap()
        );
    }

    #[test]
    fn models_with_stats_follow_the_catalogue() {
        let stats = map(vec![
            ("parakeet-tdt-v3", ModelStats { samples: vec![speech(1_000)], last_load_ms: None }),
            ("unknown-model", ModelStats { samples: vec![speech(1_000)], last_load_ms: None }),
            ("whisper-small", ModelStats { samples: vec![], last_load_ms: Some(100) }),
            ("nemotron-streaming-en", ModelStats { samples: vec![speech(1_000)], last_load_ms: None }),
        ]);

        assert_eq!(
            vec!["nemotron-streaming-en", "parakeet-tdt-v3"],
            models_with_stats(&stats).iter().map(|(model, _)| model.id).collect::<Vec<_>>()
        );
    }
}
