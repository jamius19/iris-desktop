/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The tray icon: Slint's `SystemTrayIcon`, idle / recording / working, menu Settings / Cancel / Quit. Its glyph
//! follows the panel's theme: on Windows the taskbar's (registry, as Handy `windows_taskbar_theme`), on Linux the
//! app's. Linux: Slint's tray never registers with a StatusNotifierWatcher that appears after the tray was created, so
//! the tray is recreated whenever the watcher gets an owner.

use iris_core::dictation::DictationState;
use slint::ComponentHandle;

use crate::dictation::Dictation;
use crate::{IrisTray, MainWindow, TrayStatus};

#[cfg(target_os = "linux")]
const WATCHER: &str = "org.kde.StatusNotifierWatcher";

pub struct Tray {
    icon: IrisTray,
}

impl Tray {
    /// The callbacks hold no `ui::with_state`: showing the window can deliver its focus event at once, and that
    /// handler takes the state.
    pub fn new(window: &MainWindow, dictation: &Dictation) -> Result<Self, slint::PlatformError> {
        let icon = IrisTray::new()?;
        icon.set_size_index(size_index());
        let weak = window.as_weak();
        icon.on_open_settings(move || {
            if let Some(window) = weak.upgrade() {
                crate::ui::show_window(&window);
            }
        });
        let dictation = dictation.clone();
        icon.on_cancel(move || dictation.cancel());
        let weak = window.as_weak();
        icon.on_quit(move || {
            if let Some(window) = weak.upgrade() {
                crate::ui::quit(&window);
            }
        });
        Ok(Self { icon })
    }

    pub fn set_state(&self, state: &DictationState) {
        self.icon.set_status(match state {
            DictationState::Listening { .. } => TrayStatus::Recording,
            DictationState::Finalizing { .. } => TrayStatus::Working,
            _ => TrayStatus::Idle,
        });
    }

    /// `app_dark`: the app's theme, used where the panel's isn't known.
    pub fn set_app_dark(&self, app_dark: bool) {
        self.icon.set_dark(panel_dark(app_dark));
    }

    /// A new tray in the same state (the StatusNotifierWatcher appeared).
    #[cfg(target_os = "linux")]
    pub fn recreate(&mut self, window: &MainWindow, dictation: &Dictation) {
        let (status, dark) = (self.icon.get_status(), self.icon.get_dark());
        // The old icon's SNI service ends when it is dropped below; the new one registers at the next loop iteration.
        match Self::new(window, dictation) {
            Ok(tray) => {
                tray.icon.set_status(status);
                tray.icon.set_dark(dark);
                *self = tray;
            }
            Err(e) => log::warn!("Recreating the tray failed: {e}"),
        }
    }
}

/// Into the icon arrays (16, 20, 24, 32, 40, 48 px): the notification area's 16 px per 96 DPI.
#[cfg(windows)]
fn size_index() -> i32 {
    const SIZES: [u32; 6] = [16, 20, 24, 32, 40, 48];
    // SAFETY: no preconditions.
    let px = 16 * unsafe { windows::Win32::UI::HiDpi::GetDpiForSystem() } / 96;
    SIZES.iter().position(|&size| size >= px).unwrap_or(SIZES.len() - 1) as i32
}

/// 32 px: the SNI host scales it.
#[cfg(not(windows))]
fn size_index() -> i32 {
    3
}

/// Handy `windows_taskbar_theme`: the taskbar follows the system theme, which can differ from the apps' one. The
/// app's theme when the value is missing.
#[cfg(windows)]
fn panel_dark(app_dark: bool) -> bool {
    windows_registry::CURRENT_USER
        .open(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize")
        .and_then(|key| key.get_u32("SystemUsesLightTheme"))
        .map_or(app_dark, |light| light == 0)
}

#[cfg(not(windows))]
fn panel_dark(app_dark: bool) -> bool {
    app_dark
}

/// Whether a tray host can show the icon: a StatusNotifierWatcher owns its bus name.
#[cfg(target_os = "linux")]
pub fn host_present() -> bool {
    watcher_present().unwrap_or_else(|e| {
        log::warn!("Checking for a tray host failed: {e}");
        false
    })
}

#[cfg(not(target_os = "linux"))]
pub fn host_present() -> bool {
    true
}

#[cfg(target_os = "linux")]
fn watcher_present() -> zbus::Result<bool> {
    let connection = zbus::blocking::Connection::session()?;
    let dbus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
    Ok(dbus.name_has_owner(zbus::names::BusName::try_from(WATCHER)?)?)
}

/// Calls `appeared` on a thread of its own whenever the StatusNotifierWatcher gets an owner, and once at the start
/// when it has one now but didn't at `was_present`'s check.
#[cfg(target_os = "linux")]
pub fn watch_host(was_present: bool, appeared: impl Fn() + Send + 'static) {
    let watch = move || -> zbus::Result<()> {
        let connection = zbus::blocking::Connection::session()?;
        let dbus = zbus::blocking::fdo::DBusProxy::new(&connection)?;
        let changes = dbus.receive_name_owner_changed_with_args(&[(0, WATCHER)])?;
        // Subscribed: an owner that came after the caller's check is caught here.
        if !was_present && dbus.name_has_owner(zbus::names::BusName::try_from(WATCHER)?)? {
            appeared();
        }
        for change in changes {
            if change.args()?.new_owner().is_some() {
                appeared();
            }
        }
        Ok(())
    };
    let spawned = std::thread::Builder::new().name("tray-host".into()).spawn(move || {
        if let Err(e) = watch() {
            log::warn!("Watching for a tray host failed: {e}");
        }
    });
    if let Err(e) = spawned {
        log::warn!("Watching for a tray host failed: {e}");
    }
}
