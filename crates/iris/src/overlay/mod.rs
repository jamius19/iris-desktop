/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The recording overlay host: maps the dictation state to `OverlayState` and shows, updates or hides the overlay.
//! Windows draws it in-process (`windows.rs`), Linux through the `iris-overlay` helper (`linux.rs`). Main thread
//! only. Timings: notices and errors 3 s (Android), success hides at once; "Loading model..." after a 2 s load, hide
//! after a 300 ms fade, smoothed levels and the show-generation counter (Handy).

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};
use std::time::Duration;

use iris_core::dictation::DictationState;
use iris_overlay::{BARS, OverlayConfig, OverlayState, Phase, Position};
#[cfg(target_os = "linux")]
use linux::Overlay;
use slint::Timer;
#[cfg(windows)]
use windows::Overlay;

const NOTICE_HOLD: Duration = Duration::from_secs(3);
const SLOW_MODEL_LOAD: Duration = Duration::from_secs(2);
/// The visualizer's buckets.
const BUCKETS: usize = 16;

/// `BackendSelector::new()` plus the Windows overlay's winit attributes hook; the app selects its backend with it.
pub fn backend_selector() -> slint::BackendSelector {
    #[cfg(windows)]
    return windows::with_window_hook(slint::BackendSelector::new());
    #[cfg(not(windows))]
    slint::BackendSelector::new()
}

pub struct OverlayHost {
    inner: Rc<Inner>,
}

struct Inner {
    overlay: Overlay,
    on_cancel: Box<dyn Fn()>,
    config: RefCell<OverlayConfig>,
    dictation: RefCell<DictationState>,
    /// What the overlay shows; `None` while hidden.
    shown: RefCell<Option<OverlayState>>,
    /// Bumped per show, so a delayed hide never hides a newer state.
    generation: Cell<u64>,
    session: Cell<u32>,
    model_loading: Cell<bool>,
    /// Bumped per load, so a stale slow-load timer does nothing.
    loads: Cell<u64>,
    loading_slow: Cell<bool>,
    levels: RefCell<[f32; BUCKETS]>,
}

impl OverlayHost {
    /// Main thread, after the backend is selected. `on_cancel` runs on the main thread when ✕ is clicked.
    pub fn new(on_cancel: impl Fn() + 'static) -> Self {
        let inner = Rc::new_cyclic(|weak: &Weak<Inner>| {
            let (cancel, notice) = (weak.clone(), weak.clone());
            Inner {
                overlay: Overlay::new(
                    move || {
                        if let Some(inner) = cancel.upgrade()
                            && matches!(*inner.dictation.borrow(), DictationState::Listening { .. })
                        {
                            (inner.on_cancel)();
                        }
                    },
                    move || {
                        if let Some(inner) = notice.upgrade() {
                            inner.dismiss_notice();
                        }
                    },
                ),
                on_cancel: Box::new(on_cancel),
                config: RefCell::new(OverlayConfig::default()),
                dictation: RefCell::new(DictationState::Idle),
                shown: RefCell::new(None),
                generation: Cell::new(0),
                session: Cell::new(0),
                model_loading: Cell::new(false),
                loads: Cell::new(0),
                loading_slow: Cell::new(false),
                levels: RefCell::new([0.0; BUCKETS]),
            }
        });
        Self { inner }
    }

    pub fn set_config(&self, position: &str, lines: i32, width_percent: i32, dark: bool) {
        let position = if position == "top" { Position::Top } else { Position::Bottom };
        let config = OverlayConfig { position, lines, width_pct: width_percent, dark };
        if *self.inner.config.borrow() == config {
            return;
        }
        *self.inner.config.borrow_mut() = config;
        let shown = self.inner.shown.borrow().clone();
        if let Some(state) = shown {
            self.inner.overlay.show(&state, &self.inner.config.borrow());
        }
    }

    /// Listening = dot, waveform, timer and ✕ (+ live text); Finalizing = spinner (+ text); Done with a notice
    /// (warning) and Error = the message in the row under the last live text, then hides; Done and Idle = hide (the
    /// card fades out as it is).
    pub fn update(&self, state: &DictationState) {
        let inner = &self.inner;
        if *inner.dictation.borrow() == *state {
            return;
        }
        let new_session = !matches!(*inner.dictation.borrow(), DictationState::Listening { .. });
        // The text shown so far: it stays above a message.
        let live = match &*inner.dictation.borrow() {
            DictationState::Listening { text } | DictationState::Finalizing { text } => text.clone(),
            _ => String::new(),
        };
        *inner.dictation.borrow_mut() = state.clone();
        let (phase, text, message, hold) = match state {
            DictationState::Idle | DictationState::Done { notice: None, .. } => {
                inner.hide();
                return;
            }
            DictationState::Listening { text } => {
                if new_session {
                    inner.session.set(inner.session.get().wrapping_add(1));
                    *inner.levels.borrow_mut() = [0.0; BUCKETS];
                }
                (Phase::Listening, text.as_str(), "", None)
            }
            DictationState::Finalizing { text } => (Phase::Working, text.as_str(), "", None),
            DictationState::Done { notice: Some(notice), .. } => {
                (Phase::Warning, live.as_str(), notice.as_str(), Some(NOTICE_HOLD))
            }
            DictationState::Error { message } => (Phase::Error, live.as_str(), message.as_str(), Some(NOTICE_HOLD)),
        };
        inner.show(phase, text, message);
        if new_session && phase == Phase::Listening {
            inner.overlay.set_levels(&[0.0; BARS]);
        }
        if let Some(hold) = hold {
            inner.hide_after(hold);
        }
    }

    /// "Loading model..." in the panel while a load has taken over 2 s during Listening (Handy).
    pub fn set_model_loading(&self, loading: bool) {
        let inner = &self.inner;
        if inner.model_loading.replace(loading) == loading {
            return;
        }
        inner.loads.set(inner.loads.get() + 1);
        inner.loading_slow.set(false);
        if loading {
            let (weak, load) = (Rc::downgrade(inner), inner.loads.get());
            Timer::single_shot(SLOW_MODEL_LOAD, move || {
                if let Some(inner) = weak.upgrade()
                    && inner.loads.get() == load
                {
                    inner.loading_slow.set(true);
                    inner.refresh_listening();
                }
            });
        } else {
            inner.refresh_listening();
        }
    }

    /// Waveform levels, 0..1 each, at most every 33 ms: smoothed (0.7 / 0.3), the first 9 drawn (Handy).
    pub fn set_levels(&self, levels: &[f32]) {
        let inner = &self.inner;
        if inner.shown.borrow().as_ref().is_none_or(|state| state.phase != Phase::Listening) {
            return;
        }
        let mut smoothed = inner.levels.borrow_mut();
        for (i, level) in smoothed.iter_mut().enumerate() {
            *level = *level * 0.7 + levels.get(i).copied().unwrap_or(0.0) * 0.3;
        }
        let bars: [f32; BARS] = smoothed[..BARS].try_into().expect("BARS <= BUCKETS");
        inner.overlay.set_levels(&bars);
    }
}

impl Inner {
    fn show(&self, phase: Phase, text: &str, message: &str) {
        let state = OverlayState {
            phase,
            text: text.to_owned(),
            message: message.to_owned(),
            session: self.session.get(),
            loading_model: phase == Phase::Listening && self.loading_slow.get(),
        };
        self.generation.set(self.generation.get() + 1);
        self.overlay.show(&state, &self.config.borrow());
        *self.shown.borrow_mut() = Some(state);
    }

    fn hide(&self) {
        if self.shown.take().is_some() {
            self.overlay.hide();
        }
    }

    /// Hides after `hold` unless something newer showed meanwhile.
    fn hide_after(self: &Rc<Self>, hold: Duration) {
        let (weak, generation) = (Rc::downgrade(self), self.generation.get());
        Timer::single_shot(hold, move || {
            if let Some(inner) = weak.upgrade()
                && inner.generation.get() == generation
            {
                inner.hide();
            }
        });
    }

    fn dismiss_notice(&self) {
        if self.shown.borrow().as_ref().is_some_and(|state| matches!(state.phase, Phase::Error | Phase::Warning)) {
            self.hide();
        }
    }

    fn refresh_listening(&self) {
        let text = match &*self.dictation.borrow() {
            DictationState::Listening { text } => text.clone(),
            _ => return,
        };
        self.show(Phase::Listening, &text, "");
    }
}
