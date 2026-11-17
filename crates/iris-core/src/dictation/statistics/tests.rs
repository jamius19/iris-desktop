/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `StatisticsTest.kt`. Desktop: typing is at 40 WPM (Android 38), so the dictations have 40 words where
//! Android's have 38, and the typing numbers follow; the store is a file in a temp directory (Android: in memory).

use super::*;
use crate::settings::{
    CLEANUP_PROVIDER_OPENROUTER, CustomCleanupProvider, STT_PROVIDER_DEEPGRAM, STT_PROVIDER_ON_DEVICE,
    STT_PROVIDER_SONIOX, STT_PROVIDER_VERCEL_GATEWAY, Values, to_settings,
};
use crate::stt::local::stats::ModelStats;
use std::collections::HashSet;
use std::time::Duration;

const A: &str = "parakeet-tdt-v3";
const B: &str = "whisper-small";

fn open_ai() -> Settings {
    Settings { stt_provider_id: STT_PROVIDER_OPENAI.into(), ..to_settings(&Values::new(), &HashSet::new()).unwrap() }
}

fn provider(id: &str) -> Settings {
    Settings { stt_provider_id: id.into(), ..open_ai() }
}

fn groq(name: &str) -> CustomCleanupProvider {
    CustomCleanupProvider {
        model: "llama".into(),
        ..CustomCleanupProvider::new("u-1", name, "https://api.groq.com/openai/v1")
    }
}

fn custom() -> Settings {
    Settings { custom_cleanup_providers: vec![groq("Groq")], cleanup_provider_id: "u-1".into(), ..open_ai() }
}

fn record(recorded_ms: i64, billed_ms: i64, wait_ms: i64, done_ms: i64) -> DictationRecord {
    DictationRecord { recorded_ms, billed_ms, wait_ms, done_ms, words: 40 }
}

fn default_record() -> DictationRecord {
    record(10_000, 6_000, 800, 1_200)
}

fn totals(recorded_ms: i64, saved_ms: i64) -> Totals {
    Totals { recorded_ms, saved_ms }
}

#[test]
fn cloud_keys_and_titles() {
    assert_eq!("openai|gpt-live-transcribe", cloud_key(&open_ai()));
    assert_eq!("OpenAI · gpt-live-transcribe", cloud_title(&open_ai()));
    // An unknown provider id is OpenAI.
    let unknown = provider("nope");
    assert_eq!("openai|gpt-live-transcribe", cloud_key(&unknown));
    assert_eq!("OpenAI · gpt-live-transcribe", cloud_title(&unknown));
    let gateway = provider(STT_PROVIDER_VERCEL_GATEWAY);
    assert_eq!("vercel-gateway|openai/gpt-live-transcribe", cloud_key(&gateway));
    assert_eq!("Vercel AI Gateway · openai/gpt-live-transcribe", cloud_title(&gateway));
    let soniox = provider(STT_PROVIDER_SONIOX);
    assert_eq!("soniox|stt-rt-v6", cloud_key(&soniox));
    assert_eq!("Soniox · stt-rt-v6", cloud_title(&soniox));
    let deepgram = provider(STT_PROVIDER_DEEPGRAM);
    assert_eq!("deepgram|nova-3", cloud_key(&deepgram));
    assert_eq!("Deepgram · nova-3", cloud_title(&deepgram));
    let upload = Settings { deepgram_upload: true, ..deepgram };
    assert_eq!("deepgram|nova-3|upload", cloud_key(&upload));
    assert_eq!("Deepgram · nova-3 (Non Real-time)", cloud_title(&upload));
    // Deepgram's switch means nothing for another provider.
    assert_eq!("openai|gpt-live-transcribe", cloud_key(&Settings { deepgram_upload: true, ..open_ai() }));
}

#[test]
fn cleanup_keys_and_titles() {
    assert_eq!("vercel-gateway|openai/gpt-6-luna", cleanup_key(&open_ai()));
    assert_eq!("Vercel AI Gateway · openai/gpt-6-luna", cleanup_title(&open_ai()));
    let open_router = Settings { cleanup_provider_id: CLEANUP_PROVIDER_OPENROUTER.into(), ..open_ai() };
    assert_eq!("openrouter|openai/gpt-6-luna", cleanup_key(&open_router));
    assert_eq!("OpenRouter · openai/gpt-6-luna", cleanup_title(&open_router));
    // A custom provider is keyed by its id and titled by its name.
    assert_eq!("u-1|llama", cleanup_key(&custom()));
    assert_eq!("Groq · llama", cleanup_title(&custom()));
}

#[test]
fn word_counts() {
    assert_eq!(0, word_count(""));
    assert_eq!(0, word_count("  \n "));
    assert_eq!(1, word_count("Hello"));
    assert_eq!(4, word_count("  Hello,   world.\nNew\tline "));
}

#[test]
fn typing_saved() {
    // 40 words take a minute to type; the dictation took 10 s + 1.2 s.
    assert_eq!(48_800, typing_saved_ms(40, 10_000, 1_200));
    // 3 words: 4,500 ms of typing, slower to dictate.
    assert_eq!(-6_700, typing_saved_ms(3, 10_000, 1_200));
    assert_eq!(-500, typing_saved_ms(0, 0, 500));
}

#[test]
fn on_device_dictation_only_counts_in_the_totals() {
    let after = StatsSnapshot::default().with_dictation(&provider(STT_PROVIDER_ON_DEVICE), default_record());

    assert!(after.cloud.is_empty());
    assert_eq!(totals(10_000, 48_800), after.totals);
}

#[test]
fn cloud_dictation_adds_a_sample_and_the_totals() {
    let after = StatsSnapshot { totals: totals(1_000, -2_000), ..StatsSnapshot::default() }
        .with_dictation(&provider(STT_PROVIDER_DEEPGRAM), default_record())
        .with_dictation(&provider(STT_PROVIDER_VERCEL_GATEWAY), record(5_000, 5_000, 300, 300));

    assert_eq!(
        CloudStats {
            title: "Deepgram · nova-3".into(),
            samples: vec![CloudSample { wait_ms: 800, recorded_ms: 10_000, billed_ms: 6_000 }]
        },
        after.cloud["deepgram|nova-3"]
    );
    assert_eq!(
        vec![CloudSample { wait_ms: 300, recorded_ms: 5_000, billed_ms: 5_000 }],
        after.cloud["vercel-gateway|openai/gpt-live-transcribe"].samples
    );
    // 1,000 + 10,000 + 5,000 recorded; -2,000 + 48,800 + (60,000 - 5,300) saved.
    assert_eq!(totals(16_000, 101_500), after.totals);
}

#[test]
fn the_twenty_first_cloud_sample_drops_the_oldest() {
    let mut stats = StatsSnapshot::default();
    for i in 1..=21 {
        stats = stats.with_dictation(&open_ai(), record(10_000, 6_000, i, 1_200));
    }

    let waits: Vec<i64> = stats.cloud[&cloud_key(&open_ai())].samples.iter().map(|s| s.wait_ms).collect();
    assert_eq!((2..=21).collect::<Vec<_>>(), waits);
    assert_eq!(210_000, stats.totals.recorded_ms);
}

#[test]
fn cleanup_keeps_the_last_twenty_and_refreshes_the_title() {
    let mut stats = StatsSnapshot::default();
    for i in 1..=21 {
        stats = stats.with_cleanup(&custom(), i);
    }
    let renamed = Settings { custom_cleanup_providers: vec![groq("Groq Cloud")], ..custom() };
    stats = stats.with_cleanup(&renamed, 22);

    assert_eq!(
        CleanupStats { title: "Groq Cloud · llama".into(), samples_ms: (3..=22).collect() },
        stats.cleanup["u-1|llama"]
    );
    assert_eq!(vec!["u-1|llama"], stats.cleanup.keys().collect::<Vec<_>>());
}

#[test]
fn percentiles() {
    let one_to = |n: i64| (1..=n).collect::<Vec<_>>();
    assert_eq!(None, percentile_ms(&[], 75));
    assert_eq!(Some(7), percentile_ms(&[7], 75));
    assert_eq!(Some(7), percentile_ms(&[7], 90));
    assert_eq!(Some(2), percentile_ms(&[1, 2], 75));
    assert_eq!(Some(2), percentile_ms(&[1, 2], 90));
    assert_eq!(Some(4), percentile_ms(&one_to(5), 75));
    assert_eq!(Some(5), percentile_ms(&one_to(5), 90));
    assert_eq!(Some(15), percentile_ms(&one_to(20), 75));
    assert_eq!(Some(18), percentile_ms(&one_to(20), 90));
    assert_eq!(Some(4), percentile_ms(&[5, 1, 4, 2, 3], 75));
}

/// Desktop: one file for all four sections.
#[test]
fn encode_decode_round_trips() {
    let sample = |speech_ms, decode_ms, wait_ms| StatsSample { speech_ms, decode_ms, wait_ms };
    let snapshot = StatsSnapshot {
        on_device: [(A.to_string(), ModelStats { samples: vec![sample(1, 2, 3)], last_load_ms: Some(4_800) })].into(),
        cloud: [(
            "openai|x".to_string(),
            CloudStats {
                title: "OpenAI · x".into(),
                samples: vec![
                    CloudSample { wait_ms: 1, recorded_ms: 2, billed_ms: 3 },
                    CloudSample { wait_ms: 4, recorded_ms: 5, billed_ms: 6 },
                ],
            },
        )]
        .into(),
        cleanup: [(
            "openrouter|y".to_string(),
            CleanupStats { title: "OpenRouter · y".into(), samples_ms: vec![1, 2, 3] },
        )]
        .into(),
        totals: totals(123, -45),
    };

    assert_eq!(Some(snapshot.clone()), decode(&encode(&snapshot)));
}

/// Desktop: junk as a whole file reads as None (the store starts over); junk in one section empties only it.
#[test]
fn decode_of_nothing_or_junk_is_empty() {
    for junk in ["", "{not json", "[1, 2]", "null"] {
        assert_eq!(None, decode(junk));
        let section = format!(r#"{{"models":{junk:?},"cloud":{junk:?},"cleanup":{junk:?},"totals":{junk:?}}}"#);
        assert_eq!(Some(StatsSnapshot::default()), decode(&section));
    }
    for junk in ["[1, 2]", "null", "1"] {
        let sections = format!(r#"{{"models":{junk},"cloud":{junk},"cleanup":{junk},"totals":{junk}}}"#);
        assert_eq!(Some(StatsSnapshot::default()), decode(&sections));
    }
    let partly = r#"{"cloud":[1, 2],"totals":{"recordedMs":5,"extra":true},"new":1}"#;
    assert_eq!(Some(StatsSnapshot { totals: totals(5, 0), ..StatsSnapshot::default() }), decode(partly));
}

struct Store {
    _dir: tempfile::TempDir,
    path: PathBuf,
    stats: Statistics,
}

impl Store {
    fn snapshot(&self) -> StatsSnapshot {
        self.stats.stats().borrow().clone()
    }

    /// Waits until the file holds `expected`.
    async fn assert_stored(&self, expected: &StatsSnapshot) {
        for _ in 0..500 {
            if load(&self.path) == *expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(*expected, load(&self.path));
    }
}

fn statistics(speed_percent: i32, initial: Option<&str>) -> Store {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("statistics.json");
    if let Some(initial) = initial {
        fs::write(&path, initial).unwrap();
    }
    let stats = Statistics::new(path.clone(), Handle::current(), Box::new(move || speed_percent));
    Store { _dir: dir, path, stats }
}

/// Android's models key from before the other sections existed; on desktop, a file with only that section.
#[tokio::test]
async fn cp16_models_json_still_decodes() {
    let json = format!(
        r#"{{"models":{{"{A}":{{"samples":[{{"speechMs":1500,"decodeMs":1,"waitMs":300}}],"lastLoadMs":4800}}}}}}"#
    );
    let store = statistics(100, Some(&json));

    let samples = vec![StatsSample { speech_ms: 1_500, decode_ms: 1, wait_ms: 300 }];
    assert_eq!(
        StatsSnapshot {
            on_device: [(A.to_string(), ModelStats { samples, last_load_ms: Some(4_800) })].into(),
            ..StatsSnapshot::default()
        },
        store.snapshot()
    );
}

#[tokio::test]
async fn a_missing_or_corrupt_file_reads_as_empty_and_is_replaced() {
    assert_eq!(StatsSnapshot::default(), statistics(100, None).snapshot());
    let store = statistics(100, Some("{not json"));
    assert_eq!(StatsSnapshot::default(), store.snapshot());

    store.stats.record_load(A, 4_800);

    let expected = StatsSnapshot {
        on_device: [(A.to_string(), ModelStats { samples: vec![], last_load_ms: Some(4_800) })].into(),
        ..StatsSnapshot::default()
    };
    store.assert_stored(&expected).await;
}

#[tokio::test]
async fn record_on_device_scales_speech_by_speed_and_keeps_decode_at_least_one_ms() {
    let store = statistics(150, None);
    store.stats.record_on_device(A, 1_000, 0, 300);

    // The provider's own sample: no totals (the controller records those).
    let samples = vec![StatsSample { speech_ms: 1_500, decode_ms: 1, wait_ms: 300 }];
    let expected = StatsSnapshot {
        on_device: [(A.to_string(), ModelStats { samples, last_load_ms: None })].into(),
        ..StatsSnapshot::default()
    };
    assert_eq!(expected, store.snapshot());
    store.assert_stored(&expected).await;
}

#[tokio::test]
async fn record_load_stores_the_last_load_time() {
    let store = statistics(100, None);
    store.stats.record_load(A, 4_800);
    store.stats.record_load(A, 3_100);

    let expected: ModelStatsMap = [(A.to_string(), ModelStats { samples: vec![], last_load_ms: Some(3_100) })].into();
    assert_eq!(expected, store.snapshot().on_device);
}

#[tokio::test]
async fn record_dictation_counts_each_dictation_once_in_the_totals() {
    let store = statistics(100, None);
    store.stats.record_on_device(A, 1_000, 100, 200);
    store.stats.record_dictation(&provider(STT_PROVIDER_ON_DEVICE), record(2_000, 6_000, 800, 0));
    store.stats.record_dictation(&provider(STT_PROVIDER_SONIOX), record(3_000, 3_000, 800, 0));

    let snapshot = store.snapshot();
    assert_eq!(totals(5_000, 60_000 - 2_000 + 60_000 - 3_000), snapshot.totals);
    assert_eq!(vec!["soniox|stt-rt-v6"], snapshot.cloud.keys().collect::<Vec<_>>());
    assert_eq!(1, snapshot.on_device[A].samples.len());
    store.assert_stored(&snapshot).await;
}

#[tokio::test]
async fn record_cleanup_adds_to_the_selected_provider() {
    let store = statistics(100, None);
    store.stats.record_cleanup(&open_ai(), 900);
    store.stats.record_cleanup(&open_ai(), 1_100);

    let expected =
        CleanupStats { title: "Vercel AI Gateway · openai/gpt-6-luna".into(), samples_ms: vec![900, 1_100] };
    assert_eq!(BTreeMap::from([(cleanup_key(&open_ai()), expected)]), store.snapshot().cleanup);
}

#[tokio::test]
async fn reset_clears_everything() {
    let store = statistics(100, None);
    store.stats.record_on_device(A, 1_000, 100, 200);
    store.stats.record_load(B, 2_000);
    store.stats.record_dictation(&open_ai(), default_record());
    store.stats.record_cleanup(&open_ai(), 900);
    let before = store.snapshot();
    assert_eq!(vec![A, B], before.on_device.keys().collect::<Vec<_>>());
    assert_eq!(1, before.cloud.len());
    assert_eq!(1, before.cleanup.len());
    store.assert_stored(&before).await;

    store.stats.reset();

    assert_eq!(StatsSnapshot::default(), store.snapshot());
    store.assert_stored(&StatsSnapshot::default()).await;
    // A new handle on the same file (the next app start) reads what was written.
    let reopened = Statistics::new(store.path.clone(), Handle::current(), Box::new(|| 100));
    assert_eq!(StatsSnapshot::default(), *reopened.stats().borrow());
}

/// Desktop: what a handle records is there for the next one on the same file.
#[tokio::test]
async fn statistics_survive_a_restart() {
    let store = statistics(100, None);
    store.stats.record_on_device(A, 1_000, 100, 200);
    store.stats.record_dictation(&open_ai(), default_record());
    store.stats.record_cleanup(&custom(), 900);
    let before = store.snapshot();
    store.assert_stored(&before).await;

    let reopened = Statistics::new(store.path.clone(), Handle::current(), Box::new(|| 100));

    assert_eq!(before, *reopened.stats().borrow());
}
