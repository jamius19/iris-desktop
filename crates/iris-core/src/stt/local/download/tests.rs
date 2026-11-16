/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Handy's `download/tests.rs`: each test drives the downloader against a scripted local socket server.

use super::*;
use std::path::PathBuf;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// ── SHA256 verification tests ─────────────────────────────────────────────

/// Writes `data` to a temp file; the TempDir must outlive the test.
fn write_temp_file(data: &[u8]) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("model.partial");
    fs::write(&path, data).unwrap();
    (dir, path)
}

#[test]
fn test_verify_sha256_passes_on_correct_hash() {
    let (_dir, path) = write_temp_file(b"hello world");
    let actual = compute_sha256(&path).unwrap();
    assert!(verify_sha256(&path, &actual).is_ok(), "should pass when hash matches");
    assert!(path.exists(), "file must be kept on successful verification");
}

#[test]
fn test_verify_sha256_fails_and_deletes_partial_on_mismatch() {
    let (_dir, path) = write_temp_file(b"this is not the real model");
    let wrong_hash = "0000000000000000000000000000000000000000000000000000000000000000";

    let result = verify_sha256(&path, wrong_hash);

    assert!(result.unwrap_err().to_string().contains("corrupt"), "error message should mention corruption");
    assert!(!path.exists(), "partial file must be deleted after hash mismatch");
}

#[test]
fn test_verify_sha256_fails_and_deletes_partial_when_file_missing() {
    let dir = TempDir::new().unwrap();
    let missing_path = dir.path().join("gone.partial");

    assert!(verify_sha256(&missing_path, "anyexpectedhash").is_err(), "missing file must return an error");
}

// ── Resumable HTTP downloader tests ───────────────────────────────────────

pub(crate) fn sha_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

pub(crate) fn http_response(status_line: &str, headers: &[String], body: &[u8]) -> Vec<u8> {
    let mut head = format!("HTTP/1.1 {status_line}\r\nConnection: close\r\n");
    for h in headers {
        head.push_str(h);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    let mut bytes = head.into_bytes();
    bytes.extend_from_slice(body);
    bytes
}

pub(crate) async fn read_request_head(sock: &mut TcpStream) -> String {
    let mut buf = Vec::new();
    let mut byte = [0u8; 1];
    while !buf.ends_with(b"\r\n\r\n") {
        if sock.read_exact(&mut byte).await.is_err() {
            break;
        }
        buf.push(byte[0]);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// Serves exactly one connection: captures the request head, writes `response` verbatim, closes. Returns the URL and
/// a handle yielding the request head (lowercased).
async fn serve_once(response: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let head = read_request_head(&mut sock).await;
        // The client may hang up early (error tests); that's not a server-side failure.
        let _ = sock.write_all(&response).await;
        let _ = sock.shutdown().await;
        head.to_lowercase()
    });
    (format!("http://{addr}/file"), handle)
}

async fn run_download(
    url: &str,
    partial: &Path,
    expected_size: u64,
    expected_sha256: &str,
    cancel: &AtomicBool,
) -> Result<HttpDownloadOutcome, DownloadError> {
    download_http_resumable(&reqwest::Client::new(), url, partial, expected_size, expected_sha256, cancel, &|_| {})
        .await
}

/// A cancel flag set after `delay`.
fn cancel_after(delay: Duration) -> Arc<AtomicBool> {
    let cancel = Arc::new(AtomicBool::new(false));
    let trigger = Arc::clone(&cancel);
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        trigger.store(true, Ordering::Relaxed);
    });
    cancel
}

#[tokio::test]
async fn http_fresh_download_completes_and_verifies() {
    let body = b"hello world";
    let (url, server) = serve_once(http_response("200 OK", &[format!("Content-Length: {}", body.len())], body)).await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");

    let out = run_download(&url, &partial, body.len() as u64, &sha_hex(body), &AtomicBool::new(false)).await.unwrap();

    assert_eq!(HttpDownloadOutcome::Completed, out);
    assert_eq!(fs::read(&partial).unwrap(), body);
    let head = server.await.unwrap();
    assert!(!head.contains("range:"), "fresh download must not send a Range header");
}

#[tokio::test]
async fn http_resume_appends_with_valid_content_range() {
    let full = b"helloworld";
    let (url, server) = serve_once(http_response(
        "206 Partial Content",
        &["Content-Range: bytes 5-9/10".to_string(), "Content-Length: 5".to_string()],
        &full[5..],
    ))
    .await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");
    fs::write(&partial, &full[..5]).unwrap();

    let out = run_download(&url, &partial, 10, &sha_hex(full), &AtomicBool::new(false)).await.unwrap();

    assert_eq!(HttpDownloadOutcome::Completed, out);
    assert_eq!(fs::read(&partial).unwrap(), full);
    let head = server.await.unwrap();
    assert!(head.contains("range: bytes=5-"), "got request: {head}");
}

#[tokio::test]
async fn http_wrong_content_range_discards_partial() {
    // Server answers 206 but restarts the range at 0: appending would corrupt, so the partial must be dropped and
    // the call must fail.
    let full = b"helloworld";
    let (url, _server) = serve_once(http_response(
        "206 Partial Content",
        &["Content-Range: bytes 0-9/10".to_string(), "Content-Length: 10".to_string()],
        full,
    ))
    .await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");
    fs::write(&partial, &full[..5]).unwrap();

    let err = run_download(&url, &partial, 10, &sha_hex(full), &AtomicBool::new(false)).await.unwrap_err();

    assert!(err.to_string().contains("Content-Range"), "{err}");
    assert!(!partial.exists(), "corrupt-prone partial must be deleted");
}

#[tokio::test]
async fn http_range_ignored_200_restarts_from_zero() {
    let body = b"helloworld!";
    let (url, _server) = serve_once(http_response("200 OK", &[format!("Content-Length: {}", body.len())], body)).await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");
    fs::write(&partial, b"XXXXX").unwrap(); // stale bytes that must not survive

    let out = run_download(&url, &partial, body.len() as u64, &sha_hex(body), &AtomicBool::new(false)).await.unwrap();

    assert_eq!(HttpDownloadOutcome::Completed, out);
    assert_eq!(fs::read(&partial).unwrap(), body);
}

#[tokio::test]
async fn http_416_short_of_expected_size_clears_partial() {
    // Catalogue size known, partial shorter, yet the server says our offset is past EOF: its object is smaller than
    // the catalogue expects. Never blessed, even with a hash on hand.
    let (url, _server) = serve_once(http_response("416 Range Not Satisfiable", &[], b"")).await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");
    fs::write(&partial, b"12345").unwrap();

    let err = run_download(&url, &partial, 10, &sha_hex(b"1234567890"), &AtomicBool::new(false)).await.unwrap_err();

    assert!(err.to_string().contains("416"), "{err}");
    assert!(!partial.exists());
}

#[tokio::test]
async fn http_full_size_partial_verified_without_network() {
    // Crash-between-completion-and-rename recovery: the partial already has every byte, so no request is issued at
    // all (the URL points at a dead port to prove it).
    let body = b"complete content";
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");
    fs::write(&partial, body).unwrap();

    let out = run_download(
        "http://127.0.0.1:9/unreachable",
        &partial,
        body.len() as u64,
        &sha_hex(body),
        &AtomicBool::new(false),
    )
    .await
    .unwrap();

    assert_eq!(HttpDownloadOutcome::Completed, out);
    assert_eq!(fs::read(&partial).unwrap(), body);
}

#[tokio::test]
async fn http_oversized_partial_restarts_clean() {
    let body = b"12345";
    let (url, server) = serve_once(http_response("200 OK", &[format!("Content-Length: {}", body.len())], body)).await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");
    fs::write(&partial, b"garbage-beyond-expected").unwrap();

    let out = run_download(&url, &partial, body.len() as u64, &sha_hex(body), &AtomicBool::new(false)).await.unwrap();

    assert_eq!(HttpDownloadOutcome::Completed, out);
    assert_eq!(fs::read(&partial).unwrap(), body);
    let head = server.await.unwrap();
    assert!(!head.contains("range:"), "oversized partial must restart, not resume: {head}");
}

#[tokio::test]
async fn http_conflicting_content_length_rejected_before_writing() {
    let (url, _server) = serve_once(http_response("200 OK", &["Content-Length: 10".to_string()], b"0123456789")).await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");

    let err = run_download(&url, &partial, 5, &sha_hex(b"01234"), &AtomicBool::new(false)).await.unwrap_err();

    assert!(err.to_string().contains("advertises"), "{err}");
    assert!(!partial.exists(), "nothing may be written on an up-front size conflict");
}

#[tokio::test]
async fn http_body_exceeding_expected_size_aborts_and_clears() {
    // Chunked response (no Content-Length to reject up-front) that keeps sending past the expected size: the
    // disk-fill guard must cut the transfer at the first excess byte and clear the tainted partial.
    let (url, _server) = serve_once(http_response(
        "200 OK",
        &["Transfer-Encoding: chunked".to_string()],
        b"3\r\nabc\r\n5\r\ndefgh\r\n0\r\n\r\n",
    ))
    .await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");

    let err = run_download(&url, &partial, 5, &sha_hex(b"abcde"), &AtomicBool::new(false)).await.unwrap_err();

    assert!(err.to_string().contains("more than"), "{err}");
    assert!(!partial.exists(), "tainted partial must be deleted");
}

#[tokio::test]
async fn http_wrong_hash_after_download_clears_partial() {
    let body = b"tampered bytes";
    let (url, _server) = serve_once(http_response("200 OK", &[format!("Content-Length: {}", body.len())], body)).await;
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");

    let err = run_download(&url, &partial, body.len() as u64, &sha_hex(b"the expected bytes"), &AtomicBool::new(false))
        .await
        .unwrap_err();

    assert!(err.to_string().contains("corrupt"), "{err}");
    assert!(!partial.exists());
}

#[tokio::test]
async fn http_cancel_while_awaiting_headers() {
    // Server accepts and goes silent. Cancellation must win at once, not after the stall timeout.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request_head(&mut sock).await;
        tokio::time::sleep(Duration::from_secs(600)).await;
    });
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");
    let cancel = cancel_after(Duration::from_millis(100));

    let started = Instant::now();
    let out = run_download(&format!("http://{addr}/file"), &partial, 10, "00", &cancel).await.unwrap();

    assert_eq!(HttpDownloadOutcome::Cancelled, out);
    assert!(started.elapsed() < Duration::from_secs(30), "cancel must not wait out the stall timeout");
}

#[tokio::test]
async fn http_cancel_during_stalled_body_keeps_partial() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let _ = read_request_head(&mut sock).await;
        let _ = sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\nabc").await;
        tokio::time::sleep(Duration::from_secs(600)).await;
    });
    let dir = TempDir::new().unwrap();
    let partial = dir.path().join("m.partial");
    let cancel = cancel_after(Duration::from_millis(300));

    let out = run_download(&format!("http://{addr}/file"), &partial, 100, "00", &cancel).await.unwrap();

    assert_eq!(HttpDownloadOutcome::Cancelled, out);
    assert_eq!(fs::read(&partial).unwrap(), b"abc", "bytes received before the cancel must be kept for resume");
}
