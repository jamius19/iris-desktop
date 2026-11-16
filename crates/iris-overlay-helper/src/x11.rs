/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! X11 mode: Slint's winit backend on X11 or XWayland (`WAYLAND_DISPLAY` removed first), an override-redirect
//! notification window that never takes focus, placed on the monitor under the pointer, else the primary. Its X input
//! region is the card, so clicks beside it reach the windows below. On XWayland the pointer is only known over X
//! windows (R3).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iris_overlay::geometry::{is_within_monitor, max_card_width, overlay_size, place_on_monitor};
use iris_overlay::ipc::{FromHelper, Mode, ToHelper};
use iris_overlay::{HIDE_DELAY, OverlayConfig, OverlayState, OverlayWindow, apply, set_levels};
use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::window::WindowLevel;
use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{ComponentHandle, PhysicalPosition, PhysicalSize, Timer};
use x11rb::connection::Connection;
use x11rb::protocol::shape::{self, SK, SO};
use x11rb::protocol::xproto::{self, ClipOrdering, Rectangle};
use x11rb::rust_connection::RustConnection;

use crate::{fail, read_stdin, send};

struct Helper {
    window: OverlayWindow,
    /// Our own X connection, for the pointer position and the input region.
    x11: Option<(RustConnection, xproto::Window)>,
    config: RefCell<OverlayConfig>,
    state: RefCell<Option<OverlayState>>,
    /// The config the visible window was placed with.
    placed: RefCell<OverlayConfig>,
    /// Bumped per show, so a late hide never hides a newer show.
    shows: Rc<Cell<u64>>,
}

thread_local! {
    static HELPER: RefCell<Option<Rc<Helper>>> = const { RefCell::new(None) };
}

pub fn run() {
    // Before Slint starts and before any thread: winit must pick X11.
    unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    let selected = slint::BackendSelector::new()
        .backend_name("winit".into())
        .renderer_name("software".into())
        .with_winit_window_attributes_hook(|attributes| {
            use winit::platform::x11::{WindowAttributesExtX11, WindowType};
            attributes
                .with_override_redirect(true)
                .with_x11_window_type(vec![WindowType::Notification])
                .with_decorations(false)
                .with_window_level(WindowLevel::AlwaysOnTop)
                .with_transparent(true)
                .with_active(false)
        })
        .select();
    if let Err(e) = selected {
        return fail(format!("no X11 display: {e}"));
    }
    let window = match OverlayWindow::new() {
        Ok(window) => window,
        Err(e) => return fail(format!("couldn't create the overlay: {e}")),
    };
    window.on_cancel_clicked(|| send(FromHelper::CancelClicked));
    window.on_notice_clicked(|| send(FromHelper::NoticeClicked));
    window.on_card_changed(|| with_helper(|helper| helper.update_input_region()));
    let x11 = RustConnection::connect(None).ok().map(|(connection, screen)| {
        let root = connection.setup().roots[screen].root;
        (connection, root)
    });
    let helper = Helper {
        window,
        x11,
        config: RefCell::new(OverlayConfig::default()),
        state: RefCell::new(None),
        placed: RefCell::new(OverlayConfig::default()),
        shows: Rc::new(Cell::new(0)),
    };
    HELPER.set(Some(Rc::new(helper)));

    read_stdin(|message| {
        let _ = slint::invoke_from_event_loop(move || match message {
            Some(message) => with_helper(|helper| helper.handle(message)),
            None => {
                let _ = slint::quit_event_loop();
            }
        });
    });
    send(FromHelper::Ready { mode: Mode::X11 });
    if let Err(e) = slint::run_event_loop_until_quit() {
        fail(format!("the event loop failed: {e}"));
    }
}

fn with_helper(f: impl FnOnce(&Rc<Helper>)) {
    if let Some(helper) = HELPER.with_borrow(Clone::clone) {
        f(&helper);
    }
}

impl Helper {
    fn handle(self: &Rc<Self>, message: ToHelper) {
        match message {
            ToHelper::Config(config) => {
                *self.config.borrow_mut() = config;
                let state = self.state.borrow().clone();
                if let Some(state) = state.filter(|_| self.window.get_shown()) {
                    self.show(state);
                }
            }
            ToHelper::State(state) => self.show(state),
            ToHelper::Levels(levels) => set_levels(&self.window, &levels),
            ToHelper::Hide => self.hide(),
        }
    }

    fn show(self: &Rc<Self>, state: OverlayState) {
        self.shows.set(self.shows.get() + 1);
        apply(&self.window, &state, &self.config.borrow());
        *self.state.borrow_mut() = Some(state);
        let visible = self.window.window().is_visible();
        if visible && *self.placed.borrow() == *self.config.borrow() {
            return;
        }
        *self.placed.borrow_mut() = self.config.borrow().clone();
        self.place();
        if visible {
            return;
        }
        if let Err(e) = self.window.show() {
            return fail(format!("couldn't show the overlay: {e}"));
        }
        // The first show creates the X window: place it once it exists.
        let weak = Rc::downgrade(self);
        let placed = slint::spawn_local(async move {
            let Some(helper) = weak.upgrade() else { return };
            if helper.window.window().winit_window().await.is_ok() {
                helper.place();
                helper.update_input_region();
            }
        });
        if let Err(e) = placed {
            eprintln!("iris-overlay: couldn't place the overlay: {e}");
        }
    }

    /// Fades out, then hides the window unless a show came first.
    fn hide(&self) {
        self.window.set_shown(false);
        let (shows, at_hide) = (self.shows.clone(), self.shows.get());
        let weak = self.window.as_weak();
        Timer::single_shot(HIDE_DELAY, move || {
            if shows.get() == at_hide
                && let Some(window) = weak.upgrade()
            {
                let _ = window.hide();
            }
        });
    }

    /// Sizes and moves the window to the monitor under the pointer (else the primary). No-op before the X window
    /// exists.
    fn place(&self) {
        let config = self.config.borrow().clone();
        let pointer = self.pointer();
        let placement = self.window.window().with_winit_window(|window| {
            let monitors: Vec<_> = window.available_monitors().collect();
            let under_pointer = pointer.and_then(|point| {
                monitors.iter().find(|monitor| {
                    let (position, size) = (monitor.position(), monitor.size());
                    is_within_monitor(point, (position.x, position.y), (size.width, size.height))
                })
            });
            let monitor = under_pointer.cloned().or_else(|| window.primary_monitor()).or(monitors.first().cloned())?;
            let (position, size, scale) = (monitor.position(), monitor.size(), monitor.scale_factor());
            let monitor_width = size.width as f64 / scale;
            let bounds = place_on_monitor(
                (position.x, position.y),
                (size.width, size.height),
                scale,
                1.0,
                overlay_size(&config, monitor_width),
                config.position,
            );
            Some((bounds, max_card_width(&config, monitor_width)))
        });
        let Some(((x, y, width, height), max_width)) = placement.flatten() else { return };
        self.window.set_max_card_width(max_width as f32);
        let window = self.window.window();
        window.set_position(PhysicalPosition::new(x, y));
        window.set_size(PhysicalSize::new(width as u32, height as u32));
    }

    fn pointer(&self) -> Option<(i32, i32)> {
        let (connection, root) = self.x11.as_ref()?;
        let reply = xproto::query_pointer(connection, *root).ok()?.reply().ok()?;
        Some((reply.root_x as i32, reply.root_y as i32))
    }

    /// Limits the window's input to the card.
    fn update_input_region(&self) {
        let Some((connection, _)) = self.x11.as_ref() else { return };
        let Some(xid) = self.window.window().with_winit_window(|window| xid(window)).flatten() else { return };
        let scale = self.window.window().scale_factor();
        let (x, y) = (self.window.get_card_x() * scale, self.window.get_card_y() * scale);
        let rectangle = Rectangle {
            x: x.floor() as i16,
            y: y.floor() as i16,
            width: (self.window.get_card_width() * scale).ceil() as u16 + 1,
            height: (self.window.get_card_height() * scale).ceil() as u16 + 1,
        };
        let set = shape::rectangles(connection, SO::SET, SK::INPUT, ClipOrdering::UNSORTED, xid, 0, 0, &[rectangle]);
        if let Err(e) = set.map(|_| ()).and_then(|_| connection.flush()) {
            eprintln!("iris-overlay: couldn't set the input region: {e}");
        }
    }
}

fn xid(window: &winit::window::Window) -> Option<u32> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Xlib(handle) => Some(handle.window as u32),
        RawWindowHandle::Xcb(handle) => Some(handle.window.get()),
        _ => None,
    }
}
