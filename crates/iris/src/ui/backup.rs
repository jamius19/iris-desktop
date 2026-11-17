/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The Backup and Restore page (Android `BackupPage.kt`): native file dialogs (rfd) that don't block the window, the
//! export and the restore on the runtime. Messages and logs never hold the password, a key or the payload.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use iris_core::settings::Values;
use iris_core::settings::backup::{
    BACKUP_FAILED, BACKUP_SAVED, BackupError, MAX_BACKUP_BYTES, NOT_A_BACKUP, PASSWORD_REQUIRED, RESTORE_FAILED,
    RESTORED, export_backup, restore_backup,
};
use rfd::AsyncFileDialog;
use slint::winit_030::WinitWindowAccessor;
use slint::{ComponentHandle, SharedString};

use super::{AppState, after_restore, off_thread, with_state};
use crate::{Backup, MainWindow};

const BACKUP: &str = "backup";
const RESTORE: &str = "restore";

/// What the page keeps outside Slint: whether a file dialog is open, and the picked backup file.
#[derive(Default)]
pub struct BackupState {
    dialog_open: bool,
    picked: Option<PathBuf>,
}

pub fn wire(window: &MainWindow) {
    let page = window.global::<Backup>();
    page.on_create_backup(|| with_state(create_backup));
    page.on_choose_file(|| with_state(choose_file));
    page.on_restore(|| with_state(restore));
    // Nothing on the page outlives it (a running operation still finishes).
    page.on_left(|| {
        with_state(|state| {
            let page = state.window.global::<Backup>();
            page.set_backup_password(SharedString::new());
            page.set_backup_password_error(SharedString::new());
            page.set_restore_password(SharedString::new());
            page.set_picked_name(SharedString::new());
            set_outcome(state, "", "", false);
            state.backup.picked = None;
        })
    });
}

fn set_outcome(state: &AppState, action: &str, text: &str, error: bool) {
    let page = state.window.global::<Backup>();
    page.set_outcome_action(action.into());
    page.set_outcome_text(text.into());
    page.set_outcome_error(error);
}

/// A second click while a dialog is open or an operation runs does nothing.
fn busy(state: &AppState) -> bool {
    state.backup.dialog_open || !state.window.global::<Backup>().get_running().is_empty()
}

/// Shows the save (`save`) or open dialog over the window without blocking it; `done` gets the chosen file on the main
/// thread.
fn show_dialog(state: &mut AppState, dialog: AsyncFileDialog, save: bool, done: fn(&mut AppState, PathBuf)) {
    state.backup.dialog_open = true;
    let dialog = dialog.add_filter("Iris backup", &["irisbackup"]);
    let dialog = state.window.window().with_winit_window(|w| dialog.clone().set_parent(w)).unwrap_or(dialog);
    state.runtime.spawn(async move {
        let file = if save { dialog.save_file().await } else { dialog.pick_file().await };
        let path = file.map(|file| file.path().to_path_buf());
        let _ = slint::invoke_from_event_loop(move || {
            with_state(|state| {
                state.backup.dialog_open = false;
                if let Some(path) = path {
                    done(state, path);
                }
            })
        });
    });
}

fn create_backup(state: &mut AppState) {
    if busy(state) {
        return;
    }
    set_outcome(state, "", "", false);
    let date = chrono::Local::now().format("%Y%m%d");
    let dialog = AsyncFileDialog::new().set_file_name(format!("iris-backup-{date}.irisbackup"));
    show_dialog(state, dialog, true, write_backup);
}

fn write_backup(state: &mut AppState, path: PathBuf) {
    let page = state.window.global::<Backup>();
    let password = page.get_backup_password().to_string();
    if password.is_empty() {
        page.set_backup_password_error(PASSWORD_REQUIRED.into());
        return;
    }
    page.set_running(BACKUP.into());
    let values = state.store.values().clone();
    off_thread(
        state,
        move |secrets| {
            let bytes = export_backup(&values, secrets, &password)?;
            // Written next to it, then renamed over it: a failure never touches an existing file.
            let mut tmp_name = path.file_name().unwrap_or_default().to_os_string();
            tmp_name.push(".tmp");
            let tmp = path.with_file_name(tmp_name);
            let written = File::create(&tmp)
                .and_then(|mut file| file.write_all(&bytes).and_then(|()| file.sync_all()))
                .and_then(|()| fs::rename(&tmp, &path));
            written.map_err(|e| {
                let _ = fs::remove_file(&tmp);
                BackupError::Other(e.to_string())
            })
        },
        |state, result| {
            let page = state.window.global::<Backup>();
            page.set_running(SharedString::new());
            match result {
                Ok(()) => {
                    page.set_backup_password(SharedString::new());
                    set_outcome(state, BACKUP, BACKUP_SAVED, false);
                }
                Err(e) => {
                    log::warn!("Backup failed: {e}");
                    set_outcome(state, BACKUP, e.user_message(BACKUP_FAILED), true);
                }
            }
        },
    );
}

fn choose_file(state: &mut AppState) {
    if busy(state) {
        return;
    }
    set_outcome(state, "", "", false);
    show_dialog(state, AsyncFileDialog::new(), false, picked);
}

fn picked(state: &mut AppState, path: PathBuf) {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).filter(|n| !n.trim().is_empty());
    state.window.global::<Backup>().set_picked_name(name.as_deref().unwrap_or("Backup file").into());
    state.backup.picked = Some(path);
}

fn restore(state: &mut AppState) {
    let page = state.window.global::<Backup>();
    let password = page.get_restore_password().to_string();
    let Some(path) = state.backup.picked.clone().filter(|_| !busy(state) && !password.is_empty()) else {
        return;
    };
    set_outcome(state, "", "", false);
    page.set_running(RESTORE.into());
    let current = state.store.values().clone();
    off_thread(
        state,
        move |secrets| {
            let file = read_at_most(&path, MAX_BACKUP_BYTES + 1).map_err(|e| BackupError::Other(e.to_string()))?;
            if file.len() > MAX_BACKUP_BYTES {
                return Err(BackupError::Message(NOT_A_BACKUP));
            }
            restore_backup(secrets, &current, &file, &password, write_settings)
        },
        |state, result| {
            let page = state.window.global::<Backup>();
            page.set_running(SharedString::new());
            let changed = !matches!(result, Err(BackupError::Message(_)));
            match result {
                Ok(()) => {
                    page.set_restore_password(SharedString::new());
                    page.set_picked_name(SharedString::new());
                    state.backup.picked = None;
                    set_outcome(state, RESTORE, RESTORED, false);
                }
                Err(e) => {
                    log::warn!("Restore failed: {e}");
                    set_outcome(state, RESTORE, e.user_message(RESTORE_FAILED), true);
                }
            }
            // A message error (size, password, damaged file) comes before any write; any other failure may have
            // cleared keys or written the settings.
            if changed {
                after_restore(state);
            }
        },
    );
}

/// At most `limit` bytes of the file (a picked file may be huge).
fn read_at_most(path: &Path, limit: usize) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    File::open(path)?.take(limit as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Replaces the settings on the main thread (the store lives there) and waits for it.
fn write_settings(values: Values) -> io::Result<()> {
    let (sender, receiver) = mpsc::channel();
    slint::invoke_from_event_loop(move || {
        let _ = sender.send(with_state(|state| state.store.replace_all(values)));
    })
    .map_err(io::Error::other)?;
    receiver.recv().map_err(|_| io::Error::other("the event loop stopped"))?
}
