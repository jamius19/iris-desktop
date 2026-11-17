/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! The resumable direct-HTTP model download (Handy's `managers/model/download.rs` without Tauri, hf-hub, the mirror,
//! events and the rate meter; a cancel flag replaces its `CancellationToken`). It leaves verified bytes in the partial
//! file; renaming it is the caller's job.

use log::{info, warn};
use reqwest::StatusCode;
use reqwest::header::{CONTENT_RANGE, RANGE};
use sha2::{Digest, Sha256};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// No headers or body bytes for this long means the transfer is wedged, not slow: it fails and keeps the partial.
pub const DOWNLOAD_STALL_TIMEOUT: Duration = Duration::from_secs(60);

/// Throttle for progress reports while bytes are flowing.
const PROGRESS_THROTTLE: Duration = Duration::from_millis(100);

/// How often the cancel flag is checked while waiting for the server.
const CANCEL_POLL: Duration = Duration::from_millis(50);

/// How a download ended; cancellation is an outcome (partial kept, no error), not a failure.
#[derive(Debug, PartialEq)]
pub enum HttpDownloadOutcome {
    Completed,
    Cancelled,
}

#[derive(Debug)]
pub enum DownloadError {
    /// No response, a stall or a broken body; the partial is kept for a resume.
    Network(String),
    /// An unusable answer with this HTTP status. After a 206 at another offset or a 416 the partial was deleted.
    Http(u16, String),
    /// The server's size or the SHA-256 is wrong; the partial was deleted.
    Corrupt(String),
    /// Reading or writing the partial failed.
    Storage(io::Error),
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Network(message) | Self::Http(_, message) | Self::Corrupt(message) => f.write_str(message),
            Self::Storage(e) => write!(f, "storage error: {e}"),
        }
    }
}

impl From<io::Error> for DownloadError {
    fn from(e: io::Error) -> Self {
        Self::Storage(e)
    }
}

/// Start offset of a `Content-Range: bytes <start>-<end>/<total>` header.
fn content_range_start(value: &str) -> Option<u64> {
    let range = value.trim().strip_prefix("bytes")?.trim_start();
    range.split('-').next()?.trim().parse().ok()
}

/// Resolves once `cancel` is set.
async fn cancelled(cancel: &AtomicBool) {
    while !cancel.load(Ordering::Relaxed) {
        tokio::time::sleep(CANCEL_POLL).await;
    }
}

/// Verifies the SHA-256 of `path`; on a mismatch or read error the file is deleted, so the next attempt starts clean.
fn verify_sha256(path: &Path, expected_sha256: &str) -> Result<(), DownloadError> {
    let result = match compute_sha256(path) {
        Ok(actual) if actual == expected_sha256 => {
            info!("SHA256 verified for {}", path.display());
            return Ok(());
        }
        Ok(actual) => {
            warn!("SHA256 mismatch for {}: expected {expected_sha256}, got {actual}", path.display());
            Err(DownloadError::Corrupt(format!(
                "Download verification failed for {}: file is corrupt. Please retry.",
                path.display()
            )))
        }
        Err(e) => Err(DownloadError::Storage(e)),
    };
    let _ = fs::remove_file(path);
    result
}

/// The SHA-256 hex digest of a file, read in 64 KB chunks.
fn compute_sha256(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

/// Lowercase hex (sha2 0.11's digest has no `LowerHex`).
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// [`verify_sha256`] on the blocking pool.
async fn verify_file(path: &Path, expected_sha256: &str) -> Result<(), DownloadError> {
    let (path, expected) = (path.to_path_buf(), expected_sha256.to_string());
    tokio::task::spawn_blocking(move || verify_sha256(&path, &expected))
        .await
        .map_err(|e| DownloadError::Storage(io::Error::other(e)))?
}

/// Fetches `url` into `partial_path`, resuming what is already there, and leaves verified bytes in it. `progress`
/// gets the bytes in the partial (throttled).
///
/// Robustness, in the order the failure modes appear (Handy's):
/// - a partial already at the expected size (crash between completion and rename) is verified without a request; an
///   oversized one is deleted; a 416 means the server's object is shorter, so the partial is deleted
/// - the response and every body chunk are bounded by [`DOWNLOAD_STALL_TIMEOUT`] and race the cancel flag
/// - a 200 to a Range request (server ignored it) restarts from zero; a 206 must start exactly at our offset or the
///   partial is deleted
/// - a server advertising or sending more than the expected size is cut off at once
/// - the final bytes are checked against `expected_size` and `expected_sha256`
pub async fn download_http_resumable(
    client: &reqwest::Client,
    url: &str,
    partial_path: &Path,
    expected_size: u64,
    expected_sha256: &str,
    cancel: &AtomicBool,
    progress: &(dyn Fn(u64) + Send + Sync),
) -> Result<HttpDownloadOutcome, DownloadError> {
    let mut resume_from = partial_path.metadata().map(|m| m.len()).unwrap_or(0);
    if resume_from > expected_size {
        let _ = fs::remove_file(partial_path);
        resume_from = 0;
    } else if resume_from == expected_size && expected_size > 0 {
        info!("Partial download {} is already full-size; verifying", partial_path.display());
        verify_file(partial_path, expected_sha256).await?;
        return Ok(HttpDownloadOutcome::Completed);
    }

    if resume_from > 0 {
        info!("Resuming download of {url} from byte {resume_from}");
    } else {
        info!("Starting fresh download of {url}");
    }

    let mut request = client.get(url);
    if resume_from > 0 {
        request = request.header(RANGE, format!("bytes={resume_from}-"));
    }
    let mut response = tokio::select! {
        r = tokio::time::timeout(DOWNLOAD_STALL_TIMEOUT, request.send()) => r
            .map_err(|_| DownloadError::Network(format!(
                "no response within {}s from {url}",
                DOWNLOAD_STALL_TIMEOUT.as_secs()
            )))?
            .map_err(|e| DownloadError::Network(e.to_string()))?,
        _ = cancelled(cancel) => return Ok(HttpDownloadOutcome::Cancelled),
    };
    let status = response.status();

    // 416 to our Range request: its start is at or past the object's end. A full-size partial never issues a
    // request (above), so the server's object is smaller than expected: restart clean.
    if resume_from > 0 && status == StatusCode::RANGE_NOT_SATISFIABLE {
        let _ = fs::remove_file(partial_path);
        return Err(DownloadError::Http(416, "server object ends before the expected size (HTTP 416)".into()));
    }
    // A 200 to a Range request: the server ignored it and sends the whole file; appending would corrupt the model.
    if resume_from > 0 && status == StatusCode::OK {
        let _ = fs::remove_file(partial_path);
        resume_from = 0;
    }
    if !status.is_success() {
        return Err(DownloadError::Http(status.as_u16(), format!("server returned HTTP {status}")));
    }
    // On a 206, a reply starting anywhere but exactly at the partial's end would corrupt the file on append.
    if resume_from > 0 && status == StatusCode::PARTIAL_CONTENT {
        let starts_at =
            response.headers().get(CONTENT_RANGE).and_then(|v| v.to_str().ok()).and_then(content_range_start);
        if starts_at != Some(resume_from) {
            let _ = fs::remove_file(partial_path);
            return Err(DownloadError::Http(
                206,
                format!("server returned Content-Range starting at {starts_at:?}, expected {resume_from}"),
            ));
        }
    }
    // A server advertising a different total is already misbehaving: reject before writing anything.
    if let Some(len) = response.content_length()
        && resume_from + len != expected_size
    {
        let _ = fs::remove_file(partial_path);
        return Err(DownloadError::Corrupt(format!(
            "server advertises {} bytes, expected {expected_size}",
            resume_from + len
        )));
    }

    let mut downloaded = resume_from;
    let mut file =
        if resume_from > 0 { OpenOptions::new().append(true).open(partial_path)? } else { File::create(partial_path)? };
    progress(downloaded);
    let mut last_progress = Instant::now();
    loop {
        let chunk = tokio::select! {
            c = tokio::time::timeout(DOWNLOAD_STALL_TIMEOUT, response.chunk()) => match c {
                // Stalled mid-body: keep the partial for resume.
                Err(_) => return Err(DownloadError::Network(format!(
                    "transfer stalled: no data for {}s",
                    DOWNLOAD_STALL_TIMEOUT.as_secs()
                ))),
                Ok(Err(e)) => return Err(DownloadError::Network(e.to_string())),
                Ok(Ok(None)) => break,
                Ok(Ok(Some(chunk))) => chunk,
            },
            // Keep the partial for resume; the caller decides.
            _ = cancelled(cancel) => return Ok(HttpDownloadOutcome::Cancelled),
        };
        // An untrusted server must not be able to fill the disk: cut it off at the first byte past the size and
        // clear everything it wrote.
        if downloaded + chunk.len() as u64 > expected_size {
            drop(file);
            let _ = fs::remove_file(partial_path);
            return Err(DownloadError::Corrupt(format!("server sent more than the expected {expected_size} bytes")));
        }
        file.write_all(&chunk)?;
        downloaded += chunk.len() as u64;
        if last_progress.elapsed() >= PROGRESS_THROTTLE {
            progress(downloaded);
            last_progress = Instant::now();
        }
    }
    file.flush()?;
    drop(file);
    progress(downloaded);

    let actual = partial_path.metadata()?.len();
    if actual != expected_size {
        let _ = fs::remove_file(partial_path);
        return Err(DownloadError::Corrupt(format!(
            "download incomplete: expected {expected_size} bytes, got {actual}"
        )));
    }
    verify_file(partial_path, expected_sha256).await?;
    Ok(HttpDownloadOutcome::Completed)
}

#[cfg(test)]
pub(crate) mod tests;
