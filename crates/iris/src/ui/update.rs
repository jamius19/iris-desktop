/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The footer's update status (Android `MainActivity`): checked once at start, again when "Up to date" is clicked.

use iris_core::net;
use iris_core::update_check::{LATEST_RELEASE_PAGE, LATEST_RELEASE_URL, UpdateStatus, check_for_update};
use slint::ComponentHandle;

use super::{AppState, open_url, with_state};
use crate::{App, MainWindow, UpdateState};

pub fn wire(window: &MainWindow) {
    let app = window.global::<App>();
    app.on_recheck_update(|| with_state(check));
    app.on_open_release_page(|| open_url(LATEST_RELEASE_PAGE));
}

pub fn check(state: &mut AppState) {
    state.window.global::<App>().set_update(UpdateState::Checking);
    state.runtime.spawn(async {
        let status = check_for_update(&net::http_client(), env!("CARGO_PKG_VERSION"), LATEST_RELEASE_URL).await;
        let update = match status {
            UpdateStatus::Checking => UpdateState::Checking,
            UpdateStatus::UpToDate => UpdateState::UpToDate,
            UpdateStatus::Available => UpdateState::Available,
            UpdateStatus::Unknown => UpdateState::Unknown,
        };
        let _ = slint::invoke_from_event_loop(move || with_state(|s| s.window.global::<App>().set_update(update)));
    });
}
