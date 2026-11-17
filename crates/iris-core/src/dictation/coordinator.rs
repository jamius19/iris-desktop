/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The hold-or-toggle input machine: Handy's `transcription_coordinator.rs` (pure [`CoordinatorState`] and its
//! driver thread) for Iris's one transcribe shortcut, without Tauri, events or binding ids.

use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, error, warn};

const DEBOUNCE: Duration = Duration::from_millis(30);
const RELEASE_GRACE: Duration = Duration::from_millis(50);

/// Hold-or-toggle: a press at least this long is a hold (its release stops); shorter is a tap (recording stays on).
pub const HOLD_THRESHOLD: Duration = Duration::from_millis(300);

/// How the shortcut ends a recording (setting `shortcut_activation`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    HoldOrToggle,
    Toggle,
    PushToTalk,
}

impl Activation {
    /// `hold_or_toggle`, `toggle` or `push_to_talk`; anything else is hold-or-toggle.
    pub fn from_setting(value: &str) -> Self {
        match value {
            "toggle" => Self::Toggle,
            "push_to_talk" => Self::PushToTalk,
            _ => Self::HoldOrToggle,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PttAction {
    Passthrough,
    DeferRelease,
    CancelRelease,
}

/// A key-up deferred by `RELEASE_GRACE` so a synthesized X11 auto-repeat
/// press can cancel it (#1539). When the grace elapses the hold is resolved
/// by [`CoordinatorState::finish_hold`] (recording) or
/// [`CoordinatorState::finish_pending_hold`] (press remembered while busy).
struct PendingRelease {
    deadline: Instant,
    /// When the key actually went up. The hold duration is measured to this
    /// instant, not to the grace expiry.
    released_at: Instant,
    /// Holds at least this long stop recording; shorter ones lock it on.
    /// Push-to-talk passes zero so every release stops.
    hold_threshold: Duration,
}

/// A press that arrived while the pipeline was still busy processing the
/// previous transcription. Toggle-style triggers (SIGUSR2, CLI flags, some
/// pedal setups) flip state on every edge, so dropping a busy press desyncs
/// the parity: the next edge starts a recording nobody will ever stop.
struct PendingPress {
    /// The real key-down time, so a hold that straddles the drain is still
    /// measured from when the user pressed, not from when recording began.
    pressed_at: Instant,
    /// The recording will start locked on when the pipeline drains: set from
    /// the start for toggle, and for hold-or-toggle once the key came back up
    /// within the threshold (a tap). An unlocked pending press is a key we
    /// believe is still held.
    locked: bool,
}

impl PendingPress {
    fn remembered(&self) -> Remembered {
        if self.locked { Remembered::Locked } else { Remembered::Held }
    }
}

/// What kind of press is already waiting for the pipeline to drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Remembered {
    /// The key is still down as far as we know.
    Held,
    /// A toggle press or a classified tap: it will start a locked session.
    Locked,
}

/// Bookkeeping for the key press that started the current recording.
struct Hold {
    pressed_at: Instant,
    /// Recording outlives the key: the next press stops it, releases are
    /// ignored. Always set for toggle; set for hold-or-toggle once a release
    /// has been classified as a tap.
    locked: bool,
}

/// What to do with an input that arrives while the pipeline is busy
/// (`Stage::Processing`). `remembered` is the press already waiting for the
/// pipeline to drain, if any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BusyAction {
    /// Ignore the input entirely.
    Ignore,
    /// Remember the press; start recording when the pipeline finishes.
    Remember,
    /// This press cancels a previously remembered press: two presses during
    /// one busy window net to no-op, exactly as a press stops a locked
    /// session once recording.
    Forget,
}

fn classify_busy_input(is_pressed: bool, mode: Activation, remembered: Option<Remembered>) -> BusyAction {
    use Activation::*;
    match (mode, is_pressed, remembered) {
        // Toggle: presses alternate remember/forget to preserve parity.
        (Toggle, true, Some(_)) => BusyAction::Forget,
        (Toggle, true, None) => BusyAction::Remember,
        // Toggle mode ignores releases.
        (Toggle, false, _) => BusyAction::Ignore,
        // Hold modes: a press while busy means the user is holding the key —
        // start as soon as the pipeline drains. A press on a queued tap stops
        // it (parity); a press while the key is already down is a repeat.
        (PushToTalk | HoldOrToggle, true, None) => BusyAction::Remember,
        (PushToTalk | HoldOrToggle, true, Some(Remembered::Locked)) => BusyAction::Forget,
        (PushToTalk | HoldOrToggle, true, Some(Remembered::Held)) => BusyAction::Ignore,
        // Releases of a held pending press are deferred by the grace window
        // before reaching here and resolved by `finish_pending_hold`; any
        // other release (no press remembered, or already locked) is noise.
        (PushToTalk | HoldOrToggle, false, _) => BusyAction::Ignore,
    }
}

/// Pipeline lifecycle.
#[derive(Debug, PartialEq, Eq)]
enum Stage {
    Idle,
    Recording,
    Processing,
}

/// A keyboard/signal edge for the transcribe shortcut.
struct InputEvent {
    is_pressed: bool,
    mode: Activation,
    /// External triggers (SIGUSR2, CLI flags, tray) rather than physical keys.
    /// They fire on every edge by design and must never be debounced —
    /// dropping one desyncs toggle parity and wedges recording on.
    external: bool,
    /// When the sender observed the edge, not when the coordinator thread
    /// dequeues it. The coordinator executes `Effect::Start` inline, and the
    /// first activation after startup spends hundreds of milliseconds
    /// initializing the microphone; a release queued behind that block must
    /// still be measured from the real key-up, or a short tap classifies as
    /// a hold and the just-started recording stops with zero samples
    /// (#2089). The loop passes this as the `now` of `on_input`.
    received_at: Instant,
}

impl InputEvent {
    /// The hold duration at or above which a release stops recording.
    fn effective_hold_threshold(&self) -> Duration {
        match self.mode {
            Activation::HoldOrToggle => HOLD_THRESHOLD,
            // Every release stops; toggle never defers releases at all.
            Activation::PushToTalk | Activation::Toggle => Duration::ZERO,
        }
    }
}

/// A side effect decided by [`CoordinatorState`]; the coordinator thread is
/// the only executor. Keeping decisions pure lets tests drive the exact
/// production transitions without real timers.
#[derive(Debug, PartialEq, Eq)]
enum Effect {
    Start,
    Stop,
}

/// Commands processed sequentially by the coordinator thread.
enum Command {
    Input(InputEvent),
    Cancel { recording_was_active: bool },
    ProcessingFinished,
}

/// Decide whether a key-up should be deferred (so auto-repeat can cancel it)
/// or a key-down cancels a deferred release. `hold_to_talk` is whether a
/// release currently ends the session: true for push-to-talk and for an
/// unlocked hold-or-toggle session, false for toggle and a locked session.
/// `held` is whether we believe the key is down — recording, or remembered
/// while the pipeline is busy.
fn classify_ptt_event(pending_release: bool, is_pressed: bool, hold_to_talk: bool, held: bool) -> PttAction {
    if !hold_to_talk {
        return PttAction::Passthrough;
    }

    if is_pressed {
        if pending_release { PttAction::CancelRelease } else { PttAction::Passthrough }
    } else if held && !pending_release {
        PttAction::DeferRelease
    } else {
        PttAction::Passthrough
    }
}

/// Pure lifecycle state machine: owns every transition decision (release
/// grace, hold-vs-tap classification, debounce, busy-pipeline
/// remember/forget, cancel, drain). Produces [`Effect`]s instead of touching
/// the app, so unit tests exercise the real production logic.
///
/// All three activation modes run through one machine. A recording starts on
/// key-down in every mode; what differs is how it ends:
///
/// * push-to-talk — every release stops (hold threshold of zero)
/// * toggle — releases are ignored, the next press stops (locked from the start)
/// * hold-or-toggle — a release after a long hold stops; a release after a
///   short tap locks the session, and the next press stops
struct CoordinatorState {
    stage: Stage,
    hold: Option<Hold>,
    last_press: Option<Instant>,
    pending_release: Option<PendingRelease>,
    pending_press: Option<PendingPress>,
}

impl CoordinatorState {
    fn new() -> Self {
        Self { stage: Stage::Idle, hold: None, last_press: None, pending_release: None, pending_press: None }
    }

    /// Deadline of the deferred release, if any — drives `recv_timeout`.
    fn grace_deadline(&self) -> Option<Instant> {
        self.pending_release.as_ref().map(|p| p.deadline)
    }

    /// Whether the current session (recording, or remembered for the drain)
    /// outlives the key, so releases are ignored and the next press ends it.
    fn is_locked(&self) -> bool {
        self.hold.as_ref().is_some_and(|h| h.locked) || self.pending_press.as_ref().is_some_and(|p| p.locked)
    }

    fn on_input(&mut self, input: InputEvent, now: Instant) -> Option<Effect> {
        let held = match self.stage {
            Stage::Recording => true,
            Stage::Processing => self.pending_press.is_some(),
            Stage::Idle => false,
        };
        let hold_to_talk = input.mode != Activation::Toggle && !self.is_locked();

        match classify_ptt_event(self.pending_release.is_some(), input.is_pressed, hold_to_talk, held) {
            PttAction::CancelRelease => {
                self.pending_release = None;
                return None;
            }
            PttAction::DeferRelease => {
                self.pending_release = Some(PendingRelease {
                    hold_threshold: input.effective_hold_threshold(),
                    deadline: now + RELEASE_GRACE,
                    released_at: now,
                });
                return None;
            }
            PttAction::Passthrough => {}
        }

        // Debounce rapid-fire press events (key repeat / double-tap).
        // Releases in the hold modes are deferred above to absorb X11 auto-repeat.
        // External triggers are exempt: each one is a deliberate edge from the
        // user's own integration, and dropping it desyncs toggle parity.
        if input.is_pressed && !input.external {
            if self.last_press.is_some_and(|t| now.duration_since(t) < DEBOUNCE) {
                debug!("Debounced press");
                return None;
            }
            self.last_press = Some(now);
        }

        // A busy pipeline can't accept lifecycle changes now: classify the
        // input against any already-remembered press instead of dropping it
        // silently.
        if let Stage::Processing = self.stage {
            let remembered = self.pending_press.as_ref().map(|p| p.remembered());
            match classify_busy_input(input.is_pressed, input.mode, remembered) {
                BusyAction::Remember => {
                    debug!("Remembering press: pipeline busy");
                    // Toggle never ends on a release: locked from the start.
                    self.pending_press =
                        Some(PendingPress { locked: input.mode == Activation::Toggle, pressed_at: now });
                }
                BusyAction::Forget => {
                    debug!("Forgetting remembered press");
                    self.pending_press = None;
                }
                BusyAction::Ignore => debug!("Ignoring input: pipeline busy"),
            }
            return None;
        }

        if input.is_pressed {
            match self.stage {
                Stage::Idle => {
                    // Toggle never ends on a release: locked from the start.
                    let locked = input.mode == Activation::Toggle;
                    return Some(self.begin_recording(now, locked));
                }
                _ => {
                    // A locked session ends on the next press. In toggle mode
                    // every press ends it, even if the recording began under a
                    // hold mode (the setting changed mid-recording) — otherwise
                    // nothing but Escape could stop it.
                    if self.is_locked() || input.mode == Activation::Toggle {
                        return Some(self.begin_processing());
                    }
                    // The key is still held (its release will end this
                    // recording), so a repeated press means nothing.
                    debug!("Ignoring press: key is held");
                }
            }
        } else if hold_to_talk && self.stage == Stage::Recording {
            // A release that was not deferred (one is already pending): resolve
            // it immediately rather than dropping it.
            let threshold = input.effective_hold_threshold();
            return self.finish_hold(now, threshold);
        }
        None
    }

    /// The `RELEASE_GRACE` window elapsed with no cancelling press arriving:
    /// resolve the deferred release against whatever the key was holding —
    /// the live recording, or a press remembered while busy.
    fn on_grace_expired(&mut self) -> Option<Effect> {
        let pending = self.pending_release.take()?;
        match self.stage {
            Stage::Recording => self.finish_hold(pending.released_at, pending.hold_threshold),
            Stage::Processing => {
                self.finish_pending_hold(&pending);
                None
            }
            Stage::Idle => None,
        }
    }

    /// A press remembered while the pipeline was busy has been released for
    /// real, still before the drain. A completed hold has nothing left to
    /// start; a tap queues a locked session so the drain starts it — the
    /// same hold-vs-tap rule as [`CoordinatorState::finish_hold`].
    fn finish_pending_hold(&mut self, release: &PendingRelease) {
        let Some(pending) = self.pending_press.as_mut() else {
            return;
        };
        let held = release.released_at.saturating_duration_since(pending.pressed_at);
        if held < release.hold_threshold {
            debug!("Tap ({held:?}) while busy: will start locked on when the pipeline drains");
            pending.locked = true;
        } else {
            debug!("Forgetting remembered press: released after a {held:?} hold while busy");
            self.pending_press = None;
        }
    }

    /// The key that started the current recording has been released for real.
    /// A hold at least `threshold` long stops recording; anything shorter was a
    /// tap, which locks the session on until the next press.
    fn finish_hold(&mut self, released_at: Instant, threshold: Duration) -> Option<Effect> {
        let held = self
            .hold
            .as_ref()
            .map(|h| released_at.saturating_duration_since(h.pressed_at))
            // No hold bookkeeping means we cannot tell a tap from a hold;
            // stopping is the safe reading (it is what push-to-talk always did).
            .unwrap_or(Duration::MAX);
        if held >= threshold {
            return Some(self.begin_processing());
        }
        if let Some(hold) = &mut self.hold {
            debug!("Tap ({held:?}): recording locked on until the next press");
            hold.locked = true;
        }
        None
    }

    fn on_cancel(&mut self, recording_was_active: bool) {
        self.pending_release = None;
        // An explicit cancel abandons any remembered start too — the user
        // asked for silence, not a deferred recording.
        self.pending_press = None;
        // Don't reset during processing — wait for the pipeline to finish.
        if self.stage != Stage::Processing && (recording_was_active || self.stage == Stage::Recording) {
            self.stage = Stage::Idle;
            self.hold = None;
        }
    }

    fn on_processing_finished(&mut self) -> Option<Effect> {
        self.stage = Stage::Idle;
        self.hold = None;
        let pending = self.pending_press.take()?;
        debug!("Pipeline drained; starting remembered press");
        Some(self.begin_recording(pending.pressed_at, pending.locked))
    }

    /// Reconcile the optimistic `Stage::Recording` after the executor reports
    /// whether recording actually began (microphone access can be denied).
    fn on_start_result(&mut self, started: bool) {
        if !started && self.stage == Stage::Recording {
            self.stage = Stage::Idle;
            self.hold = None;
        }
    }

    /// Reconcile the optimistic `Stage::Processing` after the executor reports
    /// whether the stop ended a recording. If it didn't, the controller had
    /// already left Listening (cancelled, or ended on its own), so no Done or
    /// Error will come for this Stop. Mirrors [`CoordinatorState::on_start_result`].
    fn on_stop_result(&mut self, stopped: bool) {
        if !stopped && self.stage == Stage::Processing {
            self.stage = Stage::Idle;
            self.hold = None;
        }
    }

    /// Optimistic transition to `Recording`; rolled back via
    /// [`CoordinatorState::on_start_result`] if the effect fails to start
    /// recording for real.
    fn begin_recording(&mut self, pressed_at: Instant, locked: bool) -> Effect {
        self.stage = Stage::Recording;
        self.hold = Some(Hold { pressed_at, locked });
        Effect::Start
    }

    fn begin_processing(&mut self) -> Effect {
        self.stage = Stage::Processing;
        self.hold = None;
        Effect::Stop
    }
}

/// Serialises all transcription lifecycle events through a single thread
/// to eliminate race conditions between keyboard shortcuts, signals, and
/// the async transcribe-paste pipeline. The thread is a thin shell: it
/// transports commands to the pure [`CoordinatorState`] and executes the
/// returned [`Effect`]s. Cheap to clone; the thread ends when the last clone is dropped.
#[derive(Clone)]
pub struct Coordinator {
    tx: Sender<Command>,
}

impl Coordinator {
    /// `start` runs on the coordinator thread and returns whether recording began; `stop` ends recording and returns
    /// whether there was one to end (if not, no Done or Error will follow, so the machine goes back to Idle).
    pub fn spawn(
        mut start: impl FnMut() -> bool + Send + 'static,
        mut stop: impl FnMut() -> bool + Send + 'static,
    ) -> Self {
        let (tx, rx) = mpsc::channel();

        thread::spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut state = CoordinatorState::new();
                let mut run_effect = |state: &mut CoordinatorState, effect: Effect| match effect {
                    Effect::Start => {
                        let started = start();
                        if !started {
                            debug!("Start did not begin recording; staying idle");
                        }
                        state.on_start_result(started);
                    }
                    Effect::Stop => {
                        let stopped = stop();
                        state.on_stop_result(stopped);
                    }
                };

                loop {
                    let cmd = if let Some(deadline) = state.grace_deadline() {
                        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                            Ok(cmd) => cmd,
                            Err(mpsc::RecvTimeoutError::Timeout) => {
                                if let Some(effect) = state.on_grace_expired() {
                                    run_effect(&mut state, effect);
                                }
                                continue;
                            }
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    } else {
                        match rx.recv() {
                            Ok(cmd) => cmd,
                            Err(_) => break,
                        }
                    };

                    match cmd {
                        Command::Input(input) => {
                            // Hold durations are measured edge-to-edge via the
                            // sender's stamp, so time spent blocked in a prior
                            // effect (first-activation mic init) cannot turn a
                            // tap into a hold (#2089).
                            let now = input.received_at;
                            if let Some(effect) = state.on_input(input, now) {
                                run_effect(&mut state, effect);
                            }
                        }
                        Command::Cancel { recording_was_active } => state.on_cancel(recording_was_active),
                        Command::ProcessingFinished => {
                            if let Some(effect) = state.on_processing_finished() {
                                run_effect(&mut state, effect);
                            }
                        }
                    }
                }
                debug!("Transcription coordinator exited");
            }));
            if let Err(e) = result {
                error!("Transcription coordinator panicked: {e:?}");
            }
        });

        Self { tx }
    }

    /// A global shortcut edge.
    pub fn key(&self, pressed: bool, activation: Activation) {
        self.send(pressed, activation, false);
    }

    /// An external trigger (CLI `--toggle-transcription`, SIGUSR2, tray): always a toggle press, never debounced —
    /// see [`InputEvent::external`].
    pub fn external(&self) {
        self.send(true, Activation::Toggle, true);
    }

    fn send(&self, is_pressed: bool, mode: Activation, external: bool) {
        let input = InputEvent { is_pressed, mode, external, received_at: Instant::now() };
        if self.tx.send(Command::Input(input)).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_cancel(&self, recording_was_active: bool) {
        if self.tx.send(Command::Cancel { recording_was_active }).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }

    pub fn notify_processing_finished(&self) {
        if self.tx.send(Command::ProcessingFinished).is_err() {
            warn!("Transcription coordinator channel closed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_to_talk_release_while_recording_defers_release() {
        assert_eq!(classify_ptt_event(false, false, true, true), PttAction::DeferRelease);
    }

    #[test]
    fn push_to_talk_press_matching_pending_release_cancels_release() {
        assert_eq!(classify_ptt_event(true, true, true, true), PttAction::CancelRelease);
    }

    #[test]
    fn toggle_mode_press_and_release_pass_through() {
        assert_eq!(classify_ptt_event(true, true, false, true), PttAction::Passthrough);
        assert_eq!(classify_ptt_event(false, false, false, true), PttAction::Passthrough);
    }

    #[test]
    fn press_matching_pending_release_cancels_without_recording_state() {
        assert_eq!(classify_ptt_event(true, true, true, false), PttAction::CancelRelease);
    }

    // ---------------------------------------------------------------------
    // Busy-pipeline input classification.
    //
    // Toggle-style triggers (SIGUSR2, CLI flags, pedals that signal on both
    // edges) flip state on every edge. Dropping a press that arrives while
    // the previous pipeline is still processing desyncs the parity: the next
    // edge then starts a recording no one will stop, leaving the overlay
    // waiting for input with the button long released.
    // ---------------------------------------------------------------------

    #[test]
    fn toggle_press_during_processing_remembers_start() {
        assert_eq!(classify_busy_input(true, Activation::Toggle, None), BusyAction::Remember);
    }

    #[test]
    fn second_toggle_press_during_processing_forgets_press() {
        assert_eq!(classify_busy_input(true, Activation::Toggle, Some(Remembered::Locked)), BusyAction::Forget);
    }

    #[test]
    fn toggle_release_during_processing_is_ignored() {
        assert_eq!(classify_busy_input(false, Activation::Toggle, None), BusyAction::Ignore);
        assert_eq!(classify_busy_input(false, Activation::Toggle, Some(Remembered::Locked)), BusyAction::Ignore);
    }

    #[test]
    fn hold_modes_classify_busy_inputs_by_pending_state() {
        let cases = [
            (true, None, BusyAction::Remember),
            (true, Some(Remembered::Held), BusyAction::Ignore),
            (true, Some(Remembered::Locked), BusyAction::Forget),
            (false, None, BusyAction::Ignore),
            (false, Some(Remembered::Held), BusyAction::Ignore),
            (false, Some(Remembered::Locked), BusyAction::Ignore),
        ];

        for mode in [Activation::PushToTalk, Activation::HoldOrToggle] {
            for (is_pressed, remembered, expected) in cases {
                assert_eq!(classify_busy_input(is_pressed, mode, remembered), expected);
            }
        }
    }

    /// Toggle parity across a busy window: an odd number of presses remembers
    /// one start, each further press flips the remembered press off/on again.
    #[test]
    fn toggle_presses_alternate_remember_and_forget_while_busy() {
        let mut remembered = None;
        for expected in [BusyAction::Remember, BusyAction::Forget, BusyAction::Remember] {
            let action = classify_busy_input(true, Activation::Toggle, remembered);
            assert_eq!(action, expected);
            remembered = (action == BusyAction::Remember).then_some(Remembered::Locked);
        }
        assert!(remembered.is_some());
    }

    // ---------------------------------------------------------------------
    // Sequence-level regression coverage for issue #1539.
    //
    // Under X11 key auto-repeat, holding a push-to-talk key does not emit one
    // long press. It emits the initial press followed by a stream of
    // synthesized release/press pairs, then a single genuine release on key-up.
    // Before the fix, every synthesized release passed straight through and
    // stopped recording, so holding the key "rapidly toggled" recording on and
    // off. The fix defers each release for a short grace window and cancels it
    // when the matching auto-repeat press arrives.
    //
    // The unit tests above assert the classifiers in isolation. The harness
    // below drives the real `CoordinatorState` through whole event sequences
    // — the same `on_input` / `on_grace_expired` handlers the coordinator
    // thread runs — so a burst can be exercised deterministically without
    // real timers, and the tests can never drift from the production
    // transitions.
    // ---------------------------------------------------------------------

    #[derive(Clone, Copy)]
    enum Ev {
        /// A key-down event (real initial press or a synthesized auto-repeat press).
        Press,
        /// A key-up event (synthesized auto-repeat release or the genuine key-up).
        Release,
        /// The `RELEASE_GRACE` window elapsed with no cancelling press arriving.
        Grace,
    }

    struct DriveResult {
        starts: u32,
        stops: u32,
        stage: Stage,
    }

    fn ptt_input(is_pressed: bool) -> InputEvent {
        InputEvent { is_pressed, mode: Activation::PushToTalk, external: false, received_at: Instant::now() }
    }

    /// Feeds an event sequence to a real [`CoordinatorState`] the way the
    /// coordinator thread would; effects are counted instead of executed.
    fn drive(events: &[Ev]) -> DriveResult {
        let mut state = CoordinatorState::new();
        let mut clock = Instant::now();
        let mut starts = 0u32;
        let mut stops = 0u32;

        for ev in events {
            // Auto-repeat events arrive a few ms apart, well inside DEBOUNCE.
            clock += Duration::from_millis(5);

            let effect = match ev {
                Ev::Grace => state.on_grace_expired(),
                Ev::Press | Ev::Release => state.on_input(ptt_input(matches!(ev, Ev::Press)), clock),
            };
            match effect {
                Some(Effect::Start) => starts += 1,
                Some(Effect::Stop) => stops += 1,
                None => {}
            }
        }

        DriveResult { starts, stops, stage: state.stage }
    }

    /// Initial press plus several synthesized release/press pairs, as X11 emits
    /// while a push-to-talk key is held down.
    fn autorepeat_burst() -> Vec<Ev> {
        let mut events = vec![Ev::Press];
        for _ in 0..6 {
            events.push(Ev::Release);
            events.push(Ev::Press);
        }
        events
    }

    /// Regression for #1539: a burst of X11 auto-repeat release/press pairs must
    /// not stop recording. Before the fix the first synthesized release stopped
    /// recording immediately (stops == 1, stage left Recording), which produced
    /// the rapid on/off toggling. With the fix the releases are coalesced and
    /// recording stays continuously active for the whole burst.
    #[test]
    fn x11_autorepeat_burst_does_not_toggle_recording() {
        let result = drive(&autorepeat_burst());
        assert_eq!(result.starts, 1, "recording should start exactly once");
        assert_eq!(result.stops, 0, "synthesized auto-repeat releases must not stop recording mid-burst");
        assert_eq!(result.stage, Stage::Recording, "recording must remain active across the entire auto-repeat burst");
    }

    /// Complements the burst test: once the key is genuinely released and the
    /// grace window elapses with no re-press, recording stops exactly once. This
    /// proves the debounce only coalesces synthesized releases and does not wedge
    /// the coordinator or swallow the real key-up.
    #[test]
    fn genuine_release_after_grace_stops_recording_once() {
        let mut events = autorepeat_burst();
        events.push(Ev::Release); // genuine key-up
        events.push(Ev::Grace); // grace window elapses, no cancelling press
        let result = drive(&events);
        assert_eq!(result.starts, 1, "recording should start exactly once");
        assert_eq!(result.stops, 1, "a genuine release should stop recording exactly once");
        assert_eq!(result.stage, Stage::Processing);
    }

    // ---------------------------------------------------------------------
    // Sequence-level coverage of the busy-pipeline and cancel paths, driven
    // through the real machine.
    // ---------------------------------------------------------------------

    /// PTT press while the pipeline is busy is remembered and starts recording
    /// once the pipeline drains.
    #[test]
    fn press_during_processing_starts_after_drain() {
        let mut state = CoordinatorState::new();
        let now = Instant::now();

        let effect = state.on_input(ptt_input(true), now);
        assert_eq!(effect, Some(Effect::Start));

        let effect = state.on_input(ptt_input(false), now + Duration::from_millis(100));
        assert!(effect.is_none(), "release should be deferred, not fired");

        let effect = state.on_grace_expired();
        assert_eq!(effect, Some(Effect::Stop));

        let effect = state.on_input(ptt_input(true), now + Duration::from_millis(200));
        assert!(effect.is_none(), "busy pipeline must remember, not start");

        let effect = state.on_processing_finished();
        assert_eq!(effect, Some(Effect::Start), "remembered press should start once the pipeline drains");
    }

    /// Two toggle presses inside one busy window net to no-op: nothing starts
    /// when the pipeline drains (toggle parity).
    #[test]
    fn toggle_presses_during_processing_net_noop_after_drain() {
        let mut state = CoordinatorState::new();
        let now = Instant::now();

        let effect = state.on_input(ptt_input(true), now);
        assert_eq!(effect, Some(Effect::Start));
        let effect = state.on_input(ptt_input(false), now + Duration::from_millis(100));
        assert!(effect.is_none());
        let effect = state.on_grace_expired();
        assert_eq!(effect, Some(Effect::Stop));

        let toggle = |state: &mut CoordinatorState, at: Instant| {
            state.on_input(
                InputEvent { is_pressed: true, mode: Activation::Toggle, external: true, received_at: at },
                at,
            )
        };

        let effect = toggle(&mut state, now + Duration::from_millis(200));
        assert!(effect.is_none());
        let effect = toggle(&mut state, now + Duration::from_millis(300));
        assert!(effect.is_none());

        let effect = state.on_processing_finished();
        assert!(effect.is_none(), "even number of busy toggle presses must not start recording");
        assert_eq!(state.stage, Stage::Idle);
    }

    /// Cancel while processing abandons a remembered press: the pipeline drains
    /// to idle and nothing starts.
    #[test]
    fn cancel_during_processing_drops_remembered_press() {
        let mut state = CoordinatorState::new();
        let now = Instant::now();

        let effect = state.on_input(ptt_input(true), now);
        assert_eq!(effect, Some(Effect::Start));
        let effect = state.on_input(ptt_input(false), now + Duration::from_millis(100));
        assert!(effect.is_none());
        let effect = state.on_grace_expired();
        assert_eq!(effect, Some(Effect::Stop));

        let effect = state.on_input(ptt_input(true), now + Duration::from_millis(200));
        assert!(effect.is_none());

        state.on_cancel(false);
        assert_eq!(
            state.stage,
            Stage::Processing,
            "cancel must not reset mid-processing — the pipeline still finishes"
        );

        let effect = state.on_processing_finished();
        assert!(effect.is_none(), "cancelled session must not spawn a deferred recording");
        assert_eq!(state.stage, Stage::Idle);
    }

    fn toggle_input(external: bool) -> InputEvent {
        InputEvent { is_pressed: true, mode: Activation::Toggle, external, received_at: Instant::now() }
    }

    /// External triggers fire on every edge by design (e.g. SIGUSR2 sent on
    /// both key press and release). Two edges inside the debounce window must
    /// both be honoured, or the parity desyncs and recording wedges on.
    #[test]
    fn external_edges_inside_debounce_window_are_not_dropped() {
        let mut state = CoordinatorState::new();
        let now = Instant::now();

        let effect = state.on_input(toggle_input(true), now);
        assert_eq!(effect, Some(Effect::Start));

        let effect = state.on_input(toggle_input(true), now + Duration::from_millis(5));
        assert_eq!(effect, Some(Effect::Stop), "second external edge inside DEBOUNCE must stop the recording");
        assert_eq!(state.stage, Stage::Processing);
    }

    /// Physical keyboard presses keep the debounce: a repeat inside the window
    /// is still dropped and recording stays active.
    #[test]
    fn keyboard_press_inside_debounce_window_is_still_dropped() {
        let mut state = CoordinatorState::new();
        let now = Instant::now();

        let effect = state.on_input(toggle_input(false), now);
        assert_eq!(effect, Some(Effect::Start));

        let effect = state.on_input(toggle_input(false), now + Duration::from_millis(5));
        assert!(effect.is_none(), "keyboard repeat inside DEBOUNCE must be debounced");
        assert_eq!(state.stage, Stage::Recording);
    }

    /// If the start effect fails to begin recording (e.g. microphone access
    /// denied), the optimistic transition rolls back to idle.
    #[test]
    fn failed_start_rolls_back_to_idle() {
        let mut state = CoordinatorState::new();

        let effect = state.on_input(ptt_input(true), Instant::now());
        assert_eq!(effect, Some(Effect::Start));

        state.on_start_result(false);
        assert_eq!(state.stage, Stage::Idle);
    }

    /// If the stop effect finds no recording (the controller was cancelled or
    /// ended on its own just before), no Done/Error will come: the machine
    /// returns to idle and the next press starts.
    #[test]
    fn stop_without_recording_returns_to_idle() {
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        assert_eq!(state.on_input(toggle_input(true), t0), Some(Effect::Start));
        state.on_start_result(true);
        assert_eq!(state.on_input(toggle_input(true), t0 + ms(1000)), Some(Effect::Stop));
        state.on_stop_result(false);
        assert_eq!(state.stage, Stage::Idle);
        assert_eq!(state.on_input(toggle_input(true), t0 + ms(2000)), Some(Effect::Start));
    }

    // ---------------------------------------------------------------------
    // Hold-or-toggle (the combined mode from #147) and the two legacy modes,
    // driven through the real machine on a synthetic clock. Recording starts
    // on key-down in every mode; the tests pin how each mode ends it.
    // ---------------------------------------------------------------------

    fn input(mode: Activation, is_pressed: bool) -> InputEvent {
        InputEvent { is_pressed, mode, external: false, received_at: Instant::now() }
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Hold-or-toggle: a key held past the threshold is push-to-talk — the
    /// (deferred) release stops recording.
    #[test]
    fn hold_or_toggle_long_hold_stops_on_release() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        assert_eq!(state.on_input(input(mode, true), t0), Some(Effect::Start));
        assert!(state.on_input(input(mode, false), t0 + ms(800)).is_none());
        assert_eq!(
            state.on_grace_expired(),
            Some(Effect::Stop),
            "an 800ms hold must stop when its release grace elapses"
        );
        assert_eq!(state.stage, Stage::Processing);
    }

    /// Hold-or-toggle: a tap keeps recording (locked on); the next press stops.
    #[test]
    fn hold_or_toggle_tap_locks_recording_until_next_press() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        assert_eq!(state.on_input(input(mode, true), t0), Some(Effect::Start));
        assert!(state.on_input(input(mode, false), t0 + ms(120)).is_none());
        assert!(state.on_grace_expired().is_none(), "a 120ms tap must not stop recording");
        assert_eq!(state.stage, Stage::Recording);
        assert!(state.is_locked());

        // Seconds later the user presses again to finish.
        assert_eq!(state.on_input(input(mode, true), t0 + ms(5000)), Some(Effect::Stop));
        assert_eq!(state.stage, Stage::Processing);
        // The release of that stopping press lands in the busy window and is
        // ignored, so nothing is remembered for the drain.
        assert!(state.on_input(input(mode, false), t0 + ms(5080)).is_none());
        assert!(state.on_processing_finished().is_none());
        assert_eq!(state.stage, Stage::Idle);
    }

    /// Regression for #2089: the first activation after startup blocks the
    /// coordinator thread in `Effect::Start` while the microphone initializes
    /// (~400ms in the report). A short tap whose release is queued behind
    /// that block must still classify as a tap. The loop therefore measures
    /// the hold from the sender's `received_at` stamps (edge-to-edge), never
    /// from its own dequeue time; this drives the machine with exactly those
    /// stamps. Before the fix the release was stamped at dequeue — after the
    /// init — so a 150ms tap measured past the 300ms threshold, the
    /// just-started recording stopped on the spot, and the transcription
    /// came back empty.
    #[test]
    fn auto_mode_tap_survives_first_activation_start_latency() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        let mut press = input(mode, true);
        press.received_at = t0;
        assert_eq!(state.on_input(press, t0), Some(Effect::Start));

        // The user taps: the key really goes up 150ms in, while the
        // coordinator is still blocked initializing the microphone. The
        // release carries the edge time, not the much later dequeue time.
        let mut release = input(mode, false);
        release.received_at = t0 + ms(150);
        assert!(state.on_input(release, t0 + ms(150)).is_none());
        assert!(state.on_grace_expired().is_none(), "a 150ms tap measured edge-to-edge must not stop the recording");
        assert_eq!(state.stage, Stage::Recording);
        assert!(state.is_locked(), "the tap must lock the session on");

        // The next press — long after the drain of that empty start — is what
        // ends the session, per tap semantics.
        let mut stop_press = input(mode, true);
        stop_press.received_at = t0 + ms(5000);
        assert_eq!(state.on_input(stop_press, t0 + ms(5000)), Some(Effect::Stop));
    }

    /// Hold-or-toggle: a locked session ignores stray releases — only a press
    /// ends it.
    #[test]
    fn hold_or_toggle_locked_session_ignores_release() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        state.on_input(input(mode, true), t0);
        state.on_input(input(mode, false), t0 + ms(100));
        assert!(state.on_grace_expired().is_none());
        assert!(state.is_locked());

        assert!(state.on_input(input(mode, false), t0 + ms(900)).is_none());
        assert!(state.grace_deadline().is_none(), "no release may be deferred once locked");
        assert_eq!(state.stage, Stage::Recording);
    }

    /// Hold-or-toggle: while the key is genuinely held, extra presses do not
    /// stop the recording (that is the release's job).
    #[test]
    fn hold_or_toggle_press_while_held_is_ignored() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        state.on_input(input(mode, true), t0);
        assert!(state.on_input(input(mode, true), t0 + ms(400)).is_none());
        assert_eq!(state.stage, Stage::Recording);
        assert!(!state.is_locked());
    }

    /// Hold-or-toggle under X11 auto-repeat: the synthesized release/press
    /// pairs must not be misread as taps. The hold is measured from the
    /// original key-down to the genuine key-up.
    #[test]
    fn hold_or_toggle_autorepeat_burst_is_one_long_hold() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();
        let mut clock = t0;

        assert_eq!(state.on_input(input(mode, true), clock), Some(Effect::Start));
        // ~600ms of auto-repeat pairs a few ms apart.
        for _ in 0..60 {
            clock += ms(5);
            assert!(state.on_input(input(mode, false), clock).is_none());
            clock += ms(5);
            assert!(state.on_input(input(mode, true), clock).is_none());
            assert!(state.grace_deadline().is_none(), "auto-repeat press must cancel the deferred release");
        }
        assert!(!state.is_locked(), "no tap may be classified mid-burst");

        clock += ms(5);
        assert!(state.on_input(input(mode, false), clock).is_none());
        assert_eq!(
            state.on_grace_expired(),
            Some(Effect::Stop),
            "the genuine release after a ~600ms hold must stop recording"
        );
    }

    /// Hold-or-toggle: a press remembered during the busy window is measured
    /// from the real key-down, so a hold that straddles the drain still counts
    /// as a hold when it is released shortly after recording actually starts.
    #[test]
    fn hold_or_toggle_remembered_press_measures_hold_from_real_key_down() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        // Previous session: hold, release, stop → Processing.
        state.on_input(input(mode, true), t0);
        state.on_input(input(mode, false), t0 + ms(800));
        assert_eq!(state.on_grace_expired(), Some(Effect::Stop));

        // Pressed again while busy; still held when the pipeline drains 700ms later.
        assert!(state.on_input(input(mode, true), t0 + ms(1000)).is_none());
        assert_eq!(state.on_processing_finished(), Some(Effect::Start));
        // Released 100ms after recording began — but 800ms after key-down.
        assert!(state.on_input(input(mode, false), t0 + ms(1800)).is_none());
        assert_eq!(state.on_grace_expired(), Some(Effect::Stop), "held 800ms overall: must stop, not lock");
    }

    /// Toggle: releases never stop, the next press does. (Toggle is the
    /// combined machine with the session locked from the start.)
    #[test]
    fn toggle_mode_ignores_release_and_stops_on_next_press() {
        let mode = Activation::Toggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        assert_eq!(state.on_input(input(mode, true), t0), Some(Effect::Start));
        assert!(state.is_locked());
        assert!(state.on_input(input(mode, false), t0 + ms(100)).is_none());
        assert!(state.grace_deadline().is_none(), "toggle never defers releases");
        assert!(state.on_input(input(mode, false), t0 + ms(3000)).is_none());
        assert_eq!(state.on_input(input(mode, true), t0 + ms(4000)), Some(Effect::Stop));
    }

    /// Push-to-talk: even a very short press stops on release — there is no
    /// tap-to-lock in this mode (hold threshold of zero).
    #[test]
    fn push_to_talk_short_press_still_stops_on_release() {
        let mode = Activation::PushToTalk;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        assert_eq!(state.on_input(input(mode, true), t0), Some(Effect::Start));
        assert!(state.on_input(input(mode, false), t0 + ms(40)).is_none());
        assert_eq!(state.on_grace_expired(), Some(Effect::Stop));
    }

    /// Cancel (Escape) during a locked hold-or-toggle session resets cleanly so
    /// the next press starts a fresh recording rather than stopping a dead one.
    #[test]
    fn hold_or_toggle_cancel_clears_locked_session() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        state.on_input(input(mode, true), t0);
        state.on_input(input(mode, false), t0 + ms(100));
        assert!(state.on_grace_expired().is_none());
        assert!(state.is_locked());

        state.on_cancel(true);
        assert_eq!(state.stage, Stage::Idle);
        assert!(!state.is_locked());
        assert_eq!(state.on_input(input(mode, true), t0 + ms(2000)), Some(Effect::Start));
    }

    /// Switching to toggle while an unlocked hold recording is running must not
    /// strand it: in toggle mode a press always stops.
    #[test]
    fn toggle_press_stops_recording_started_as_hold() {
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();

        state.on_input(input(Activation::HoldOrToggle, true), t0);
        assert!(!state.is_locked());
        assert_eq!(state.on_input(input(Activation::Toggle, true), t0 + ms(2000)), Some(Effect::Stop));
    }

    // Hold-vs-tap classification while the previous transcription is busy.
    fn hold_or_toggle_into_processing(state: &mut CoordinatorState, t0: Instant) {
        let mode = Activation::HoldOrToggle;
        assert_eq!(state.on_input(input(mode, true), t0), Some(Effect::Start));
        assert!(state.on_input(input(mode, false), t0 + ms(800)).is_none());
        assert_eq!(state.on_grace_expired(), Some(Effect::Stop));
        assert_eq!(state.stage, Stage::Processing);
    }

    #[test]
    fn hold_or_toggle_tap_during_processing_queues_locked_start() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();
        hold_or_toggle_into_processing(&mut state, t0);

        assert!(state.on_input(input(mode, true), t0 + ms(1000)).is_none());
        assert!(state.on_input(input(mode, false), t0 + ms(1100)).is_none());
        assert!(state.on_grace_expired().is_none());
        assert!(state.is_locked(), "a busy tap should queue a locked start");
        assert_eq!(state.on_processing_finished(), Some(Effect::Start));
        assert!(state.is_locked());
    }

    #[test]
    fn hold_or_toggle_completed_hold_during_processing_nets_noop() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();
        hold_or_toggle_into_processing(&mut state, t0);

        assert!(state.on_input(input(mode, true), t0 + ms(1000)).is_none());
        assert!(state.on_input(input(mode, false), t0 + ms(1600)).is_none());
        assert!(state.on_grace_expired().is_none());
        assert!(!state.is_locked());

        assert!(
            state.on_processing_finished().is_none(),
            "a 600ms hold that ended before the drain has nothing left to start"
        );
        assert_eq!(state.stage, Stage::Idle);
    }

    #[test]
    fn hold_or_toggle_two_taps_during_processing_net_noop() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();
        hold_or_toggle_into_processing(&mut state, t0);

        assert!(state.on_input(input(mode, true), t0 + ms(1000)).is_none());
        assert!(state.on_input(input(mode, false), t0 + ms(1100)).is_none());
        assert!(state.on_grace_expired().is_none());
        assert!(state.is_locked());

        assert!(state.on_input(input(mode, true), t0 + ms(1500)).is_none());
        assert!(!state.is_locked(), "the second tap's press forgets the queued tap");
        assert!(state.on_input(input(mode, false), t0 + ms(1600)).is_none());
        assert!(state.grace_deadline().is_none());

        assert!(state.on_processing_finished().is_none());
        assert_eq!(state.stage, Stage::Idle);
    }

    #[test]
    fn ptt_tap_inside_busy_window_nets_noop() {
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();
        hold_or_toggle_into_processing(&mut state, t0);

        assert!(state.on_input(ptt_input(true), t0 + ms(1000)).is_none());
        assert!(state.on_input(ptt_input(false), t0 + ms(1040)).is_none());
        assert!(state.on_grace_expired().is_none());
        assert!(state.on_processing_finished().is_none());
    }

    /// The pipeline drains inside the 50ms grace of a busy tap: recording
    /// starts first (unlocked, from the real key-down), and the grace then
    /// resolves against the live recording, locking it as the tap it was.
    #[test]
    fn hold_or_toggle_drain_inside_busy_release_grace_still_classifies_tap() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();
        hold_or_toggle_into_processing(&mut state, t0);

        assert!(state.on_input(input(mode, true), t0 + ms(1000)).is_none());
        assert!(state.on_input(input(mode, false), t0 + ms(1100)).is_none());
        assert_eq!(state.on_processing_finished(), Some(Effect::Start));
        assert!(!state.is_locked());

        assert!(state.on_grace_expired().is_none());
        assert!(state.is_locked(), "the deferred 100ms release is a tap");
    }

    /// X11 auto-repeat while busy, key still held at the drain: recording
    /// starts measured from the first press, not from the last synthesized
    /// press before the drain. Released 400ms after the real key-down but
    /// only ~100ms after the drain — a hold, so it must stop rather than lock.
    #[test]
    fn hold_or_toggle_autorepeat_burst_straddling_drain_measures_from_first_press() {
        let mode = Activation::HoldOrToggle;
        let mut state = CoordinatorState::new();
        let t0 = Instant::now();
        hold_or_toggle_into_processing(&mut state, t0);

        let mut clock = t0 + ms(1000);
        assert!(state.on_input(input(mode, true), clock).is_none());
        for _ in 0..30 {
            clock += ms(5);
            assert!(state.on_input(input(mode, false), clock).is_none());
            clock += ms(5);
            assert!(state.on_input(input(mode, true), clock).is_none());
            assert!(state.grace_deadline().is_none());
        }

        // Drain at ~t0 + 1300ms with the key still down.
        assert_eq!(state.on_processing_finished(), Some(Effect::Start));
        assert!(!state.is_locked());

        for _ in 0..10 {
            clock += ms(5);
            assert!(state.on_input(input(mode, false), clock).is_none());
            clock += ms(5);
            assert!(state.on_input(input(mode, true), clock).is_none());
        }
        assert_eq!(clock, t0 + ms(1400));
        assert!(state.on_input(input(mode, false), clock).is_none());
        assert_eq!(
            state.on_grace_expired(),
            Some(Effect::Stop),
            "held 400ms since the real key-down: must stop, not lock"
        );
        assert_eq!(state.stage, Stage::Processing);
    }
}
