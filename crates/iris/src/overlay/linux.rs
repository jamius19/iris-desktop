/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The overlay through the `iris-overlay` helper next to the executable (PLAN §2.5): spawned on first use, framed
//! JSON over its stdin/stdout. A writer thread sends; a reader thread posts helper events to the main thread. A
//! helper that died is respawned once, at the next session. Linux never draws the overlay in-process.

use std::cell::{Cell, RefCell};
use std::io;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::thread;

use iris_overlay::ipc::{FromHelper, ToHelper, read_frame, write_frame};
use iris_overlay::{BARS, OverlayConfig, OverlayState};

thread_local! {
    /// Handles helper events on the main thread (the reader thread can't hold the `Rc` callbacks).
    static HANDLER: RefCell<Option<Rc<dyn Fn(FromHelper)>>> = const { RefCell::new(None) };
}

struct Helper {
    messages: Sender<ToHelper>,
    alive: Arc<AtomicBool>,
}

pub struct Overlay {
    helper: RefCell<Option<Helper>>,
    /// The config the running helper has.
    config: RefCell<Option<OverlayConfig>>,
    /// The session the helper was last spawned for: one attempt per session.
    spawned_for: Cell<Option<u32>>,
}

impl Overlay {
    pub fn new(on_cancel: impl Fn() + 'static, on_notice: impl Fn() + 'static) -> Self {
        let handler = move |event| match event {
            FromHelper::Ready { mode } => log::info!("Overlay helper ready ({mode:?})"),
            FromHelper::CancelClicked => on_cancel(),
            FromHelper::NoticeClicked => on_notice(),
            FromHelper::Failed(e) => log::error!("Overlay helper failed: {e}"),
        };
        HANDLER.set(Some(Rc::new(handler)));
        Self { helper: RefCell::new(None), config: RefCell::new(None), spawned_for: Cell::new(None) }
    }

    pub fn show(&self, state: &OverlayState, config: &OverlayConfig) {
        if !self.is_alive() && self.spawned_for.get() != Some(state.session) {
            self.spawned_for.set(Some(state.session));
            match spawn() {
                Ok(helper) => {
                    *self.helper.borrow_mut() = Some(helper);
                    *self.config.borrow_mut() = None;
                }
                Err(e) => log::error!("Couldn't start the overlay helper: {e}"),
            }
        }
        if self.config.borrow().as_ref() != Some(config) {
            *self.config.borrow_mut() = Some(config.clone());
            self.send(ToHelper::Config(config.clone()));
        }
        self.send(ToHelper::State(state.clone()));
    }

    pub fn hide(&self) {
        self.send(ToHelper::Hide);
    }

    pub fn set_levels(&self, levels: &[f32; BARS]) {
        self.send(ToHelper::Levels(*levels));
    }

    fn is_alive(&self) -> bool {
        self.helper.borrow().as_ref().is_some_and(|helper| helper.alive.load(Ordering::SeqCst))
    }

    fn send(&self, message: ToHelper) {
        if let Some(helper) = self.helper.borrow().as_ref().filter(|helper| helper.alive.load(Ordering::SeqCst)) {
            let _ = helper.messages.send(message);
        }
    }
}

fn spawn() -> io::Result<Helper> {
    let exe = std::env::current_exe()?.with_file_name("iris-overlay");
    let mut child = Command::new(&exe).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
    let (mut stdin, mut stdout) = (child.stdin.take().expect("piped"), child.stdout.take().expect("piped"));
    let (messages, inbox) = mpsc::channel::<ToHelper>();
    let alive = Arc::new(AtomicBool::new(true));

    let writer_alive = alive.clone();
    thread::Builder::new().name("overlay-writer".into()).spawn(move || {
        // Ends when the Overlay drops its sender; dropping stdin then makes the helper exit.
        for message in inbox {
            if let Err(e) = write_frame(&mut stdin, &message) {
                log::warn!("Couldn't write to the overlay helper: {e}");
                writer_alive.store(false, Ordering::SeqCst);
                break;
            }
        }
    })?;

    let reader_alive = alive.clone();
    thread::Builder::new().name("overlay-reader".into()).spawn(move || {
        loop {
            match read_frame::<FromHelper>(&mut stdout) {
                Ok(Some(event)) => {
                    let posted = slint::invoke_from_event_loop(move || {
                        if let Some(handler) = HANDLER.with_borrow(Clone::clone) {
                            handler(event);
                        }
                    });
                    if posted.is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    log::warn!("Couldn't read from the overlay helper: {e}");
                    break;
                }
            }
        }
        reader_alive.store(false, Ordering::SeqCst);
        match child.wait() {
            Ok(status) if status.success() => log::info!("The overlay helper exited"),
            Ok(status) => log::warn!("The overlay helper exited ({status})"),
            Err(e) => log::warn!("The overlay helper is gone: {e}"),
        }
    })?;
    Ok(Helper { messages, alive })
}
