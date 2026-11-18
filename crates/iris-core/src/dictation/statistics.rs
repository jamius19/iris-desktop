/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Dictation statistics for the Statistics page (Android `Statistics.kt`): on-device models' speed and load times,
//! cloud providers' wait and billed audio, cleanup times, all-time totals. Stored as one JSON file,
//! `{"models":{…},"cloud":{…},"cleanup":{…},"totals":{…}}` with Android's field names. No transcript text ever
//! reaches statistics: only word counts.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio::runtime::Handle;
use tokio::sync::watch;

use crate::settings::{STT_PROVIDER_OPENAI, Settings, cleanup_provider_label, stt_provider_label};
use crate::stt::local::stats::{ModelStatsMap, StatsSample, last_samples, speech_ms, with_load, with_sample};

/// One cloud dictation: stop -> final text, the whole recording, and the audio the service bills.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSample {
    pub wait_ms: i64,
    pub recorded_ms: i64,
    pub billed_ms: i64,
}

/// A cloud provider + model's last [`MAX_SAMPLES`](crate::stt::local::stats::MAX_SAMPLES) dictations, oldest first;
/// `title` as of the last one.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudStats {
    pub title: String,
    #[serde(default)]
    pub samples: Vec<CloudSample>,
}

/// A cleanup provider + model's last [`MAX_SAMPLES`](crate::stt::local::stats::MAX_SAMPLES) successful cleanup times,
/// oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupStats {
    pub title: String,
    #[serde(default)]
    pub samples_ms: Vec<i64>,
}

/// Sums since statistics began or the last reset: `recorded_ms` of dictations with text, and `saved_ms` = the time
/// typing their words would have taken minus the dictations' time ([`typing_saved_ms`]; net, may be negative).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Totals {
    #[serde(default)]
    pub recorded_ms: i64,
    #[serde(default)]
    pub saved_ms: i64,
}

/// Everything the Statistics page shows; maps are keyed by [`cloud_key`] / [`cleanup_key`] / model id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub on_device: ModelStatsMap,
    pub cloud: BTreeMap<String, CloudStats>,
    pub cleanup: BTreeMap<String, CleanupStats>,
    pub totals: Totals,
}

/// A dictation with text, as the controller measured it: `recorded_ms` the whole recording, `billed_ms` the audio the
/// service bills, `wait_ms` stop -> final transcript, `done_ms` stop -> pasted (or copied), `words` the pasted text's
/// [`word_count`] (no text, so none can leak into the stats layer).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DictationRecord {
    pub recorded_ms: i64,
    pub billed_ms: i64,
    pub wait_ms: i64,
    pub done_ms: i64,
    pub words: i32,
}

/// Typing speed on a desktop keyboard that dictating is compared with (Android: 38, a phone keyboard).
pub const TYPING_WPM: i64 = 40;

/// Whitespace-separated words (CJK text without spaces counts as few words).
pub fn word_count(text: &str) -> i32 {
    text.split_whitespace().count() as i32
}

/// Typing `words` at [`TYPING_WPM`] minus the dictation's time (recording + stop -> done); negative if slower.
pub fn typing_saved_ms(words: i32, recorded_ms: i64, done_ms: i64) -> i64 {
    words as i64 * 60_000 / TYPING_WPM - (recorded_ms + done_ms)
}

/// The cloud provider (an unknown id is OpenAI) and model; Deepgram's upload mode is its own entry.
pub fn cloud_key(s: &Settings) -> String {
    let provider =
        if s.stt_gateway() || s.stt_deepgram() || s.stt_soniox() { &s.stt_provider_id } else { STT_PROVIDER_OPENAI };
    let upload = if s.stt_deepgram() && s.deepgram_upload { "|upload" } else { "" };
    format!("{provider}|{}{upload}", s.active_stt_model())
}

/// "OpenAI · gpt-live-transcribe", "Deepgram · nova-3 (Non Real-time)".
pub fn cloud_title(s: &Settings) -> String {
    let upload = if s.stt_deepgram() && s.deepgram_upload { " (Non Real-time)" } else { "" };
    format!("{} · {}{upload}", stt_provider_label(&s.stt_provider_id), s.active_stt_model())
}

/// The cleanup provider (a custom one by its id) and model.
pub fn cleanup_key(s: &Settings) -> String {
    format!("{}|{}", s.cleanup_provider_id, s.active_cleanup_model())
}

/// "Vercel AI Gateway · openai/gpt-6-luna", "<custom name> · <model>".
pub fn cleanup_title(s: &Settings) -> String {
    format!(
        "{} · {}",
        cleanup_provider_label(&s.cleanup_provider_id, &s.custom_cleanup_providers),
        s.active_cleanup_model()
    )
}

impl StatsSnapshot {
    /// Adds `d` to the totals and, for a cloud provider, to its window (last `MAX_SAMPLES`).
    pub fn with_dictation(mut self, s: &Settings, d: DictationRecord) -> Self {
        self.totals.recorded_ms += d.recorded_ms;
        self.totals.saved_ms += typing_saved_ms(d.words, d.recorded_ms, d.done_ms);
        if s.stt_on_device() {
            return self;
        }
        let sample = CloudSample { wait_ms: d.wait_ms, recorded_ms: d.recorded_ms, billed_ms: d.billed_ms };
        let mut samples = self.cloud.remove(&cloud_key(s)).map(|c| c.samples).unwrap_or_default();
        samples.push(sample);
        self.cloud.insert(cloud_key(s), CloudStats { title: cloud_title(s), samples: last_samples(samples) });
        self
    }

    /// Adds a successful cleanup's `ms` to the selected provider + model's window (last `MAX_SAMPLES`).
    pub fn with_cleanup(mut self, s: &Settings, ms: i64) -> Self {
        let mut samples = self.cleanup.remove(&cleanup_key(s)).map(|c| c.samples_ms).unwrap_or_default();
        samples.push(ms);
        self.cleanup
            .insert(cleanup_key(s), CleanupStats { title: cleanup_title(s), samples_ms: last_samples(samples) });
        self
    }
}

/// The `p`th percentile by nearest rank (rank = ceil(p * n / 100)). None without values.
pub fn percentile_ms(values: &[i64], p: usize) -> Option<i64> {
    if values.is_empty() {
        return None;
    }
    let rank = (p * values.len()).div_ceil(100);
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    Some(sorted[rank - 1])
}

/// The statistics file's contents.
pub fn encode(snapshot: &StatsSnapshot) -> String {
    serde_json::json!({
        "models": snapshot.on_device,
        "cloud": snapshot.cloud,
        "cleanup": snapshot.cleanup,
        "totals": snapshot.totals,
    })
    .to_string()
}

/// The snapshot in a statistics file; None if it isn't a JSON object. A section that doesn't decode is empty (Android
/// decodes each key on its own); unknown fields are ignored.
pub fn decode(encoded: &str) -> Option<StatsSnapshot> {
    let file: Map<String, Value> = serde_json::from_str(encoded).ok()?;
    Some(StatsSnapshot {
        on_device: section(&file, "models"),
        cloud: section(&file, "cloud"),
        cleanup: section(&file, "cleanup"),
        totals: section(&file, "totals"),
    })
}

/// Every section is a JSON object (serde would also read a struct from an array).
fn section<T: DeserializeOwned + Default>(file: &Map<String, Value>, key: &str) -> T {
    file.get(key).filter(|value| value.is_object()).and_then(|value| T::deserialize(value).ok()).unwrap_or_default()
}

/// The statistics in the file at `path`: empty if it is missing, or (logged) unreadable or corrupt; the next write
/// starts over (Android's `ReplaceFileCorruptionHandler`).
pub fn load(path: &Path) -> StatsSnapshot {
    match fs::read_to_string(path) {
        Ok(text) => decode(&text).unwrap_or_else(|| {
            log::warn!("Statistics file {} is corrupt, starting over", path.display());
            StatsSnapshot::default()
        }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => StatsSnapshot::default(),
        Err(e) => {
            log::warn!("Couldn't read the statistics file {}, starting over: {e}", path.display());
            StatsSnapshot::default()
        }
    }
}

/// Reads the stored Speed (percent) when an on-device dictation is recorded.
pub type SpeedPercent = Box<dyn Fn() -> i32 + Send + Sync>;

/// The statistics, kept in memory and written to their file after each change. Recording never waits for the file:
/// writes run on the blocking pool, one at a time, each writing the latest snapshot.
#[derive(Clone)]
pub struct Statistics {
    inner: Arc<Inner>,
}

struct Inner {
    path: PathBuf,
    runtime: Handle,
    speed_percent: SpeedPercent,
    snapshot: watch::Sender<StatsSnapshot>,
    writing: Mutex<()>,
}

impl Statistics {
    /// Loads `path` ([`load`]); `runtime` runs the writes.
    pub fn new(path: PathBuf, runtime: Handle, speed_percent: SpeedPercent) -> Self {
        let snapshot = watch::channel(load(&path)).0;
        Self { inner: Arc::new(Inner { path, runtime, speed_percent, snapshot, writing: Mutex::new(()) }) }
    }

    /// The current statistics, updated after every change.
    pub fn stats(&self) -> watch::Receiver<StatsSnapshot> {
        self.inner.snapshot.subscribe()
    }

    /// An on-device dictation with text: `audio_ms` of model input (after Speed), `decode_ms` busy, `wait_ms` stop ->
    /// final.
    pub fn record_on_device(&self, model_id: &str, audio_ms: i64, decode_ms: i64, wait_ms: i64) {
        let sample = StatsSample {
            speech_ms: speech_ms(audio_ms, (self.inner.speed_percent)()),
            decode_ms: decode_ms.max(1),
            wait_ms,
        };
        self.update(|s| s.on_device = with_sample(std::mem::take(&mut s.on_device), model_id, sample));
    }

    pub fn record_load(&self, model_id: &str, ms: i64) {
        self.update(|s| s.on_device = with_load(std::mem::take(&mut s.on_device), model_id, ms));
    }

    /// A dictation with text from any provider, with the session's settings `s`.
    pub fn record_dictation(&self, s: &Settings, d: DictationRecord) {
        self.update(|snapshot| *snapshot = std::mem::take(snapshot).with_dictation(s, d));
    }

    /// A cleanup that returned text after `ms`, with the session's settings `s`.
    pub fn record_cleanup(&self, s: &Settings, ms: i64) {
        self.update(|snapshot| *snapshot = std::mem::take(snapshot).with_cleanup(s, ms));
    }

    /// Clears everything, totals included.
    pub fn reset(&self) {
        self.update(|snapshot| *snapshot = StatsSnapshot::default());
    }

    fn update(&self, change: impl FnOnce(&mut StatsSnapshot)) {
        self.inner.snapshot.send_modify(change);
        let inner = self.inner.clone();
        self.inner.runtime.spawn_blocking(move || {
            let _writing = inner.writing.lock().unwrap_or_else(|e| e.into_inner());
            // The latest snapshot, so writes finishing out of order still leave the newest in the file.
            let encoded = encode(&inner.snapshot.borrow());
            if let Err(e) = write(&inner.path, &encoded) {
                log::warn!("Couldn't write the statistics file {}: {e}", inner.path.display());
            }
        });
    }
}

/// Writes a temp file next to `path`, then renames it over it.
pub(super) fn write(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    let mut file = fs::File::create(&tmp)?;
    file.write_all(contents.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests;
