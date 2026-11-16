/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The overlay window in-process (S2 recipe, `docs/architecture.md`, Overlay): winit attributes from a hook that only
//! acts while the overlay is created, then the HWND fix-up `WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW` (no
//! `WS_EX_APPWINDOW`), kept by a subclass that answers `WM_MOUSEACTIVATE` with `MA_NOACTIVATE` and forces the bits on
//! `WM_STYLECHANGING` (winit rewrites `GWL_EXSTYLE` on every show/hide). Placed in the cursor monitor's physical pixels
//! as Handy's `place_windows_overlay`. A window region keeps clicks beside the card for the windows below.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iris_overlay::geometry::{max_card_width, overlay_size, place_on_monitor};
use iris_overlay::{BARS, HIDE_DELAY, OverlayConfig, OverlayState, OverlayWindow, apply, set_levels};
use slint::winit_030::winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::winit_030::winit::window::WindowLevel;
use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{ComponentHandle, PhysicalPosition, PhysicalSize, Timer};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateRectRgn, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    SetWindowRgn,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::Shell::{DefSubclassProc, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    GWL_EXSTYLE, GetCursorPos, GetWindowLongPtrW, HWND_TOPMOST, MA_NOACTIVATE, STYLESTRUCT, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_SHOWWINDOW, SetWindowLongPtrW, SetWindowPos, WM_MOUSEACTIVATE, WM_STYLECHANGING,
    WS_EX_APPWINDOW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

thread_local! {
    /// Set while `Overlay::new` creates the overlay window: the attributes hook is process-wide.
    static CREATING: Cell<bool> = const { Cell::new(false) };
}

/// Adds the overlay's winit attributes hook. Must be on the selector that selects the backend.
pub fn with_window_hook(selector: slint::BackendSelector) -> slint::BackendSelector {
    selector.with_winit_window_attributes_hook(|attributes| {
        if !CREATING.get() {
            return attributes;
        }
        use winit::platform::windows::WindowAttributesExtWindows;
        attributes
            .with_decorations(false)
            .with_transparent(true)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_active(false)
            .with_skip_taskbar(true)
    })
}

pub struct Overlay {
    window: OverlayWindow,
    /// Bumped per show, so a late hide never hides a newer show.
    shows: Rc<Cell<u64>>,
    /// The config the window was last placed with.
    placed: RefCell<Option<OverlayConfig>>,
}

impl Overlay {
    pub fn new(on_cancel: impl Fn() + 'static, on_notice: impl Fn() + 'static) -> Self {
        CREATING.set(true);
        let window = OverlayWindow::new();
        CREATING.set(false);
        let window = window.expect("overlay window");
        window.on_cancel_clicked(on_cancel);
        window.on_notice_clicked(on_notice);
        let weak = window.as_weak();
        window.on_card_changed(move || {
            if let Some(window) = weak.upgrade() {
                update_region(&window);
            }
        });
        Self { window, shows: Rc::new(Cell::new(0)), placed: RefCell::new(None) }
    }

    pub fn show(&self, state: &OverlayState, config: &OverlayConfig) {
        self.shows.set(self.shows.get() + 1);
        apply(&self.window, state, config);
        let visible = self.window.window().is_visible();
        if visible && self.placed.borrow().as_ref() == Some(config) {
            return;
        }
        *self.placed.borrow_mut() = Some(config.clone());
        let bounds = self.place(config);
        if !visible && let Err(e) = self.window.show() {
            log::error!("Couldn't show the overlay: {e}");
            return;
        }
        let weak = self.window.as_weak();
        let fix_up = async move {
            let Some(window) = weak.upgrade() else { return };
            match window.window().winit_window().await {
                Ok(winit_window) => match hwnd(&winit_window) {
                    Some(hwnd) => fix_up(hwnd, bounds),
                    None => log::error!("The overlay has no HWND"),
                },
                Err(e) => log::error!("The overlay has no winit window: {e}"),
            }
            update_region(&window);
        };
        if let Err(e) = slint::spawn_local(fix_up) {
            log::error!("Couldn't fix up the overlay window: {e}");
        }
    }

    /// Fades out, then hides the window unless a show came first.
    pub fn hide(&self) {
        self.window.set_shown(false);
        let (shows, at_hide) = (self.shows.clone(), self.shows.get());
        let weak = self.window.as_weak();
        Timer::single_shot(HIDE_DELAY, move || {
            if shows.get() == at_hide
                && let Some(window) = weak.upgrade()
                && let Err(e) = window.hide()
            {
                log::warn!("Couldn't hide the overlay: {e}");
            }
        });
    }

    pub fn set_levels(&self, levels: &[f32; BARS]) {
        set_levels(&self.window, levels);
    }

    /// Sizes the window for the monitor under the cursor (else the primary) and returns its physical bounds.
    fn place(&self, config: &OverlayConfig) -> (i32, i32, i32, i32) {
        let (origin, size, scale) = cursor_monitor();
        let monitor_width = size.0 as f64 / scale;
        self.window.set_max_card_width(max_card_width(config, monitor_width) as f32);
        // Slint doesn't apply Windows' text size setting, so the text scale is 1.
        let bounds = place_on_monitor(origin, size, scale, 1.0, overlay_size(config, monitor_width), config.position);
        let window = self.window.window();
        window.set_position(PhysicalPosition::new(bounds.0, bounds.1));
        window.set_size(PhysicalSize::new(bounds.2 as u32, bounds.3 as u32));
        bounds
    }
}

/// Monitor under the cursor: physical origin, size and DPI scale. `GetCursorPos` fails on a locked desktop: primary.
fn cursor_monitor() -> ((i32, i32), (u32, u32), f64) {
    unsafe {
        let mut point = POINT::default();
        let monitor = if GetCursorPos(&mut point).is_ok() {
            MonitorFromPoint(point, MONITOR_DEFAULTTONEAREST)
        } else {
            MonitorFromPoint(POINT::default(), MONITOR_DEFAULTTOPRIMARY)
        };
        let mut info = MONITORINFO { cbSize: size_of::<MONITORINFO>() as u32, ..Default::default() };
        let _ = GetMonitorInfoW(monitor, &mut info);
        let (mut dpi_x, mut dpi_y) = (96, 96);
        let _ = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
        let RECT { left, top, right, bottom } = info.rcMonitor;
        ((left, top), ((right - left) as u32, (bottom - top) as u32), dpi_x as f64 / 96.0)
    }
}

fn hwnd(window: &winit::window::Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(HWND(handle.hwnd.get() as *mut _)),
        _ => None,
    }
}

fn fix_up(hwnd: HWND, (x, y, width, height): (i32, i32, i32, i32)) {
    unsafe {
        // Idempotent for the same id.
        if !SetWindowSubclass(hwnd, Some(subclass), 1, 0).as_bool() {
            log::error!("Couldn't subclass the overlay window");
        }
        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, no_activate(style) as isize);
        // Also re-asserts the bounds after show: a move across DPIs is reflowed by WM_DPICHANGED (Handy).
        if let Err(e) = SetWindowPos(
            hwnd,
            Some(HWND_TOPMOST),
            x,
            y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_FRAMECHANGED,
        ) {
            log::error!("Couldn't place the overlay: {e}");
        }
    }
}

fn no_activate(style: u32) -> u32 {
    (style | WS_EX_NOACTIVATE.0 | WS_EX_TOOLWINDOW.0) & !WS_EX_APPWINDOW.0
}

unsafe extern "system" fn subclass(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    unsafe {
        if message == WM_MOUSEACTIVATE {
            return LRESULT(MA_NOACTIVATE as isize);
        }
        if message == WM_STYLECHANGING && wparam.0 as i32 == GWL_EXSTYLE.0 {
            let style = &mut *(lparam.0 as *mut STYLESTRUCT);
            style.styleNew = no_activate(style.styleNew);
        }
        DefSubclassProc(hwnd, message, wparam, lparam)
    }
}

/// Limits the window (and its clicks) to the card.
fn update_region(window: &OverlayWindow) {
    let Some(hwnd) = window.window().with_winit_window(hwnd).flatten() else { return };
    let scale = window.window().scale_factor();
    let left = (window.get_card_x() * scale).floor() as i32;
    let top = (window.get_card_y() * scale).floor() as i32;
    let right = ((window.get_card_x() + window.get_card_width()) * scale).ceil() as i32;
    let bottom = ((window.get_card_y() + window.get_card_height()) * scale).ceil() as i32;
    unsafe {
        // The window owns the region from here on.
        let region = CreateRectRgn(left, top, right, bottom);
        SetWindowRgn(hwnd, Some(region), true);
    }
}
