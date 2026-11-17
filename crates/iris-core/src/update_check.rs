/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The footer's update check (Android `UpdateCheck.kt`): the installed version against the latest GitHub release.

use std::time::Duration;

use reqwest::header::{ACCEPT, USER_AGENT};
use serde_json::Value;

use crate::net::{self, NetError};

/// The latest release (the newest that is neither a draft nor a pre-release).
pub const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/jamius19/iris-desktop/releases/latest";

/// The page the footer's "Update available" opens.
pub const LATEST_RELEASE_PAGE: &str = "https://github.com/jamius19/iris-desktop/releases/latest";

/// The whole call (OkHttp's default connect, read and write timeouts are 10 s each).
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateStatus {
    Checking,
    UpToDate,
    Available,
    Unknown,
}

/// Whether release tag [tag] ("v1.2.0") is a higher version than [current] ("1.1.0"), compared number by number (a
/// missing number is 0). False when either isn't dot-separated numbers.
pub fn is_newer_version(tag: &str, current: &str) -> bool {
    let (Some(latest), Some(installed)) =
        (version_numbers(tag.strip_prefix('v').unwrap_or(tag)), version_numbers(current))
    else {
        return false;
    };
    for i in 0..latest.len().max(installed.len()) {
        let difference = latest.get(i).unwrap_or(&0) - installed.get(i).unwrap_or(&0);
        if difference != 0 {
            return difference > 0;
        }
    }
    false
}

fn version_numbers(version: &str) -> Option<Vec<i64>> {
    version.split('.').map(|n| n.parse::<i32>().ok().map(i64::from)).collect()
}

/// The `tag_name` of a GitHub release response, or None if [body] isn't one.
pub fn parse_release_tag(body: &str) -> Option<String> {
    serde_json::from_str::<Value>(body).ok()?.as_object()?.get("tag_name")?.as_str().map(str::to_string)
}

/// Compares [current_version] with the repo's latest GitHub release at [url] ([`LATEST_RELEASE_URL`]; its tag is
/// `v<version>`). No release yet (404) is up to date; a network error or any other response is
/// [`UpdateStatus::Unknown`].
pub async fn check_for_update(client: &reqwest::Client, current_version: &str, url: &str) -> UpdateStatus {
    // GitHub's API rejects requests without a User-Agent.
    let request = client
        .get(url)
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, "iris-desktop")
        .timeout(TIMEOUT);
    let response = match request.send().await {
        Ok(response) => net::read_response(response).await,
        Err(e) => Err(NetError::from(e)),
    };
    let (code, body) = match response {
        Ok(response) => response,
        Err(e) => {
            log::debug!("Update check failed: {}", e.message);
            return UpdateStatus::Unknown;
        }
    };
    if code == 404 {
        return UpdateStatus::UpToDate;
    }
    let tag = if (200..300).contains(&code) { parse_release_tag(&body) } else { None };
    match tag {
        None => UpdateStatus::Unknown,
        Some(tag) if is_newer_version(&tag, current_version) => UpdateStatus::Available,
        Some(_) => UpdateStatus::UpToDate,
    }
}

#[cfg(test)]
mod tests;
