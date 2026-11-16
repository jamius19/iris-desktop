/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The recording overlay shared by the app (Windows, in-process) and the `iris-overlay` helper (Linux): the Slint
//! `OverlayWindow`, what it shows (`OverlayState`, `OverlayConfig`), its geometry and the helper protocol.

pub mod geometry;
pub mod ipc;

use std::time::Duration;

use serde::{Deserialize, Serialize};
use slint::{Model, VecModel};

slint::include_modules!();

/// Waveform bars. The visualizer's 16 buckets are smoothed by the host, which sends the first 9, as Handy.
pub const BARS: usize = 9;

/// Fade-out before the window is unmapped (Handy's `hide_recording_overlay` delay).
pub const HIDE_DELAY: Duration = Duration::from_millis(300);

/// What the card shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Phase {
    /// Dot, waveform, timer and ✕; the panel opens with live text.
    Listening,
    /// Spinner; the panel stays open while there is text.
    Working,
    /// Tick.
    Done,
    /// Badge and message (Done with a notice, or Error); click dismisses.
    Notice,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlayState {
    pub phase: Phase,
    /// Live text (Listening, Working) or the message (Notice).
    pub text: String,
    /// Changes per listening session; restarts the timer.
    pub session: u32,
    /// "Loading model..." in the panel while there is no text yet.
    pub loading_model: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Position {
    Top,
    Bottom,
}

/// The Appearance settings that shape the overlay.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OverlayConfig {
    pub position: Position,
    /// Live-text lines (notices show at least 4).
    pub lines: i32,
    /// Share of the monitor width the card can use.
    pub max_width_pct: i32,
    pub dark: bool,
}

impl Default for OverlayConfig {
    fn default() -> Self {
        Self { position: Position::Bottom, lines: 3, max_width_pct: 50, dark: false }
    }
}

/// Shows `state` with `config` (fades in if hidden). The host sizes and places the window.
pub fn apply(window: &OverlayWindow, state: &OverlayState, config: &OverlayConfig) {
    window.set_top(config.position == Position::Top);
    window.set_lines(config.lines.clamp(1, 6));
    window.set_dark(config.dark);
    window.set_mode(match state.phase {
        Phase::Listening => OverlayMode::Listening,
        Phase::Working => OverlayMode::Working,
        Phase::Done => OverlayMode::Done,
        Phase::Notice => OverlayMode::Notice,
    });
    window.set_text(state.text.as_str().into());
    window.set_session(state.session as i32);
    window.set_loading_model(state.loading_model);
    if window.get_levels().row_count() != BARS {
        set_levels(window, &[0.0; BARS]);
    }
    window.set_shown(true);
}

/// Waveform levels, 0..1. Updates the rows in place so the bars animate.
pub fn set_levels(window: &OverlayWindow, levels: &[f32]) {
    let model = window.get_levels();
    if model.row_count() != levels.len() {
        window.set_levels(VecModel::from_slice(levels));
        return;
    }
    for (i, level) in levels.iter().enumerate() {
        model.set_row_data(i, *level);
    }
}
