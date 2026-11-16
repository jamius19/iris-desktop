/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The Statistics page (Android `StatisticsPage.kt`): fills `Stats` from the statistics, again after every change.

use iris_core::copy::{
    audio_saved_text, duration_text, minutes_text, saved_percent, speed_factor_text, stats_basis_text, tenths_text,
    verdict_label,
};
use iris_core::dictation::statistics::{StatsSnapshot, percentile_ms};
use iris_core::stt::local::stats::{SpeedVerdict, median, models_with_stats, speed_verdict};
use slint::ComponentHandle;
use tokio::runtime::Runtime;

use super::{model, with_state};
use crate::dictation::Dictation;
use crate::{MainWindow, StatRow, StatSection, Stats, Tone};

pub fn wire(window: &MainWindow, runtime: &Runtime, dictation: &Dictation) {
    window.global::<Stats>().on_reset(|| with_state(|s| s.dictation.statistics.reset()));
    let mut stats = dictation.statistics.stats();
    fill(window, &stats.borrow_and_update());
    runtime.spawn(async move {
        while stats.changed().await.is_ok() {
            let snapshot = stats.borrow_and_update().clone();
            let _ = slint::invoke_from_event_loop(move || with_state(|s| fill(&s.window, &snapshot)));
        }
    });
}

fn row(title: &str, value: &str, tone: Tone, subtitle: &str) -> StatRow {
    StatRow { title: title.into(), value: value.into(), tone, subtitle: subtitle.into() }
}

/// The median stop -> final text.
fn transcription_time(median_ms: Option<i64>) -> StatRow {
    let value = tenths_text(median_ms.unwrap_or_default());
    row("Transcription Time", &value, Tone::Text, "Typical time the transcription took.")
}

fn section(title: &str, rows: Vec<StatRow>, basis: String) -> StatSection {
    StatSection { title: title.into(), rows: model(rows), basis: basis.into() }
}

fn fill(window: &MainWindow, stats: &StatsSnapshot) {
    let models = models_with_stats(&stats.on_device);
    let mut cloud: Vec<_> = stats.cloud.values().filter(|c| !c.samples.is_empty()).collect();
    cloud.sort_by(|a, b| a.title.cmp(&b.title));
    let mut cleanup: Vec<_> = stats.cleanup.values().filter(|c| !c.samples_ms.is_empty()).collect();
    cleanup.sort_by(|a, b| a.title.cmp(&b.title));
    let has_totals = stats.totals.recorded_ms > 0;

    let mut sections = Vec::new();
    for (m, s) in &models {
        let factor = s.speed_factor().expect("a model with samples");
        let verdict = speed_verdict(factor);
        let tone = match verdict {
            SpeedVerdict::KeepsUp => Tone::Enabled,
            SpeedVerdict::Borderline => Tone::Warning,
            SpeedVerdict::FallsBehind => Tone::Failure,
        };
        let mut rows = vec![
            row("Processing Speed", verdict_label(verdict), tone, &speed_factor_text(factor)),
            transcription_time(s.median_wait_ms()),
        ];
        if let Some(ms) = s.last_load_ms {
            rows.push(row("Model Load", &tenths_text(ms), Tone::Text, "Last time the model was loaded into memory."));
        }
        sections.push(section(m.label, rows, stats_basis_text(s.samples.len() as i32, "dictation")));
    }
    for c in &cloud {
        let recorded_ms = c.samples.iter().map(|s| s.recorded_ms).sum();
        let billed_ms = c.samples.iter().map(|s| s.billed_ms).sum();
        let waits: Vec<i64> = c.samples.iter().map(|s| s.wait_ms).collect();
        let saved = audio_saved_text(saved_percent(recorded_ms, billed_ms));
        let rows =
            vec![transcription_time(median(&waits)), row("Audio Sent", &minutes_text(billed_ms), Tone::Text, &saved)];
        sections.push(section(&c.title, rows, stats_basis_text(c.samples.len() as i32, "dictation")));
    }
    for c in &cleanup {
        let p = |n| tenths_text(percentile_ms(&c.samples_ms, n).unwrap_or_default());
        let rows = vec![
            row("Post Processing Time (p75)", &p(75), Tone::Text, "3 in 4 requests finished within this time."),
            row("Post Processing Time (p90)", &p(90), Tone::Text, "9 in 10 requests finished within this time."),
        ];
        let basis = stats_basis_text(c.samples_ms.len() as i32, "post processing request");
        sections.push(section(&c.title, rows, basis));
    }

    let page = window.global::<Stats>();
    page.set_empty(models.is_empty() && cloud.is_empty() && cleanup.is_empty() && !has_totals);
    page.set_has_totals(has_totals);
    page.set_time_transcribed(duration_text(stats.totals.recorded_ms).into());
    page.set_time_saved(duration_text(stats.totals.saved_ms).into());
    page.set_sections(model(sections));
    page.set_has_models(!models.is_empty());
    page.set_has_cloud(!cloud.is_empty());
    page.set_has_cleanup(!cleanup.is_empty());
}
