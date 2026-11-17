/*
* Author: Jamius Siam
* Since: 16/11/2026
*/

//! Android's `OnDeviceModelStoreTest.kt` on one file per model, against a scripted local socket server.

use super::*;
use crate::stt::local::download::tests::{http_response, read_request_head, sha_hex};
use std::sync::LazyLock;
use std::sync::mpsc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio::time::timeout;

const FILE: &str = "model.gguf";
const TIMEOUT: Duration = Duration::from_secs(10);

static BYTES: LazyLock<Vec<u8>> =
    LazyLock::new(|| (0..3_000u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8).collect());
static SHA256: LazyLock<String> = LazyLock::new(|| sha_hex(&BYTES));
static MODELS: LazyLock<Vec<OnDeviceModel>> = LazyLock::new(|| vec![model("m"), model("n")]);

fn model(id: &'static str) -> OnDeviceModel {
    OnDeviceModel {
        id,
        label: id,
        maker: "nvidia",
        languages: "English",
        output_language: None,
        streams: false,
        licence: "MIT",
        licence_url: "https://example.com",
        repo: "owner/repo",
        revision: "0000000000000000000000000000000000000000",
        file: FILE,
        size_bytes: BYTES.len() as u64,
        sha256: SHA256.as_str(),
    }
}

/// A temp `root` and a server answering each connection with the next scripted response (then closing it); the
/// request heads are recorded, lowercased.
struct Fixture {
    _temp: TempDir,
    root: PathBuf,
    base: String,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    async fn new(responses: Vec<Vec<u8>>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::<Mutex<Vec<String>>>::default();
        let recorded = Arc::clone(&requests);
        tokio::spawn(async move {
            for response in responses {
                let (mut sock, _) = listener.accept().await.unwrap();
                let head = read_request_head(&mut sock).await;
                recorded.lock().unwrap().push(head.to_lowercase());
                let _ = sock.write_all(&response).await;
                let _ = sock.shutdown().await;
            }
        });
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("models");
        Self { _temp: temp, root, base, requests }
    }

    fn dir(&self) -> PathBuf {
        self.root.join("m")
    }

    /// A store whose startup scan has ended: a late scan could otherwise land after the test's download or delete
    /// (e.g. re-add a model the test just deleted).
    async fn store_with(&self, free_bytes: u64, before_delete: impl Fn(&str) + Send + Sync + 'static) -> ModelStore {
        let base = self.base.clone();
        let url = move |m: &OnDeviceModel| format!("{base}/{}/{}", m.id, m.file);
        let (store, scan) = ModelStore::with(
            reqwest::Client::new(),
            self.root.clone(),
            Handle::current(),
            before_delete,
            move |_: &Path| free_bytes,
            MODELS.as_slice(),
            url,
        );
        scan.await.unwrap();
        store
    }

    async fn store(&self) -> ModelStore {
        self.store_with(u64::MAX, |_| {}).await
    }

    fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }

    fn assert_complete(&self) {
        assert_eq!(*BYTES, fs::read(self.dir().join(FILE)).unwrap());
        assert!(!self.tmp().exists());
    }

    fn tmp(&self) -> PathBuf {
        self.dir().join("model.gguf.tmp")
    }

    fn write_tmp(&self, bytes: &[u8]) {
        fs::create_dir_all(self.dir()).unwrap();
        fs::write(self.tmp(), bytes).unwrap();
    }
}

fn ok(bytes: &[u8]) -> Vec<u8> {
    http_response("200 OK", &[format!("Content-Length: {}", bytes.len())], bytes)
}

fn partial(start: usize) -> Vec<u8> {
    let headers =
        [format!("Content-Range: bytes {start}-2999/3000"), format!("Content-Length: {}", BYTES.len() - start)];
    http_response("206 Partial Content", &headers, &BYTES[start..])
}

/// The Range header of a recorded request head.
fn range(head: &str) -> Option<&str> {
    head.lines().find_map(|line| line.strip_prefix("range: "))
}

/// Waits until `store`'s state matches `done`.
async fn wait(store: &ModelStore, done: impl FnMut(&StoreState) -> bool) -> StoreState {
    timeout(TIMEOUT, store.state().wait_for(done)).await.unwrap().unwrap().clone()
}

/// Downloads `m` and waits until it ends.
async fn download_and_wait(store: &ModelStore) -> StoreState {
    store.download("m");
    wait(store, |s| s.downloading.is_none()).await
}

fn error(message: &str) -> Option<StoreError> {
    Some(StoreError { model_id: "m".into(), message: message.into() })
}

#[tokio::test]
async fn fresh_download_verifies_and_renames_every_file() {
    let fx = Fixture::new(vec![ok(&BYTES)]).await;
    let store = fx.store().await;

    let state = download_and_wait(&store).await;

    assert_eq!(None, state.error);
    assert!(store.is_downloaded("m"));
    assert!(!store.is_downloaded("n"));
    fx.assert_complete();
    assert_eq!(fx.dir().join(FILE), store.path("m"));
    let requests = fx.requests();
    assert!(requests[0].starts_with("get /m/model.gguf "), "{}", requests[0]);
    assert_eq!(None, range(&requests[0]));
}

#[tokio::test]
async fn resume_requests_the_rest_and_appends_a_206() {
    let fx = Fixture::new(vec![partial(1_000)]).await;
    fx.write_tmp(&BYTES[..1_000]);

    let state = download_and_wait(&fx.store().await).await;

    assert_eq!(None, state.error);
    assert_eq!(Some("bytes=1000-"), range(&fx.requests()[0]));
    fx.assert_complete();
}

#[tokio::test]
async fn partial_content_at_another_start_restarts_the_file_from_zero() {
    let fx = Fixture::new(vec![partial(500), ok(&BYTES)]).await;
    fx.write_tmp(&BYTES[..1_000]);

    let state = download_and_wait(&fx.store().await).await;

    assert_eq!(None, state.error);
    let requests = fx.requests();
    assert_eq!(Some("bytes=1000-"), range(&requests[0]));
    assert_eq!(None, range(&requests[1]));
    fx.assert_complete();
}

#[tokio::test]
async fn starting_a_download_deletes_only_other_models_partial_files() {
    let fx = Fixture::new(vec![partial(1_000)]).await;
    fx.write_tmp(&BYTES[..1_000]);
    let other = fx.root.join("n");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("model.gguf.tmp"), [0; 500]).unwrap();
    fs::write(other.join(FILE), &BYTES[..100]).unwrap();

    let state = download_and_wait(&fx.store().await).await;

    assert_eq!(None, state.error);
    assert_eq!(Some("bytes=1000-"), range(&fx.requests()[0])); // Its own partial was resumed.
    fx.assert_complete();
    assert!(!other.join("model.gguf.tmp").exists());
    assert!(other.join(FILE).exists());
}

#[tokio::test]
async fn ignored_range_restarts_the_file() {
    let fx = Fixture::new(vec![ok(&BYTES)]).await;
    fx.write_tmp(&[7; 1_000]);

    let state = download_and_wait(&fx.store().await).await;

    assert_eq!(None, state.error);
    assert_eq!(Some("bytes=1000-"), range(&fx.requests()[0]));
    fx.assert_complete();
}

/// Desktop: Handy's transport verifies a full-size partial without a request (Android asks and gets a 416).
#[tokio::test]
async fn range_not_satisfiable_with_a_complete_tmp_is_verified() {
    let fx = Fixture::new(vec![]).await;
    fx.write_tmp(&BYTES);

    let state = download_and_wait(&fx.store().await).await;

    assert_eq!(None, state.error);
    assert!(fx.requests().is_empty());
    fx.assert_complete();
}

#[tokio::test]
async fn hash_mismatch_fails_and_keeps_nothing() {
    let fx = Fixture::new(vec![ok(&[0; 3_000])]).await;
    let store = fx.store().await;

    let state = download_and_wait(&store).await;

    assert_eq!(error("Download was corrupted. Try again."), state.error);
    assert!(!store.is_downloaded("m"));
    assert!(!fx.dir().join(FILE).exists());
    assert!(!fx.tmp().exists());
}

#[tokio::test]
async fn not_enough_storage_fails_without_a_request() {
    let fx = Fixture::new(vec![]).await;

    let state = download_and_wait(&fx.store_with(1_000_000, |_| {}).await).await;

    assert_eq!(error("Not enough storage: needs 51 MB free."), state.error);
    assert!(fx.requests().is_empty());
}

#[tokio::test]
async fn http_error_is_shown() {
    let fx = Fixture::new(vec![http_response("404 Not Found", &["Content-Length: 0".into()], b"")]).await;

    let state = download_and_wait(&fx.store().await).await;

    assert_eq!(error("Download failed (HTTP 404)."), state.error);
}

#[tokio::test]
async fn body_cut_off_is_a_network_error_and_keeps_the_tmp() {
    let mut cut_off = ok(&BYTES);
    cut_off.truncate(cut_off.len() - 2_000);
    let fx = Fixture::new(vec![cut_off]).await;

    let state = download_and_wait(&fx.store().await).await;

    assert_eq!(error("Download failed (network error). Click Download to resume."), state.error);
    assert!(fx.tmp().exists());
}

#[tokio::test]
async fn cancel_deletes_the_tmp_without_an_error() {
    let fx = Fixture::new(vec![]).await;
    // Sends the first 1000 bytes, then the rest only once released, so the download stays mid-file until the cancel.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        read_request_head(&mut sock).await;
        let response = ok(&BYTES);
        let gate = response.len() - 2_000;
        sock.write_all(&response[..gate]).await.unwrap();
        let _ = released.await;
        let _ = sock.write_all(&response[gate..]).await;
    });
    let fx = Fixture { base, ..fx };
    let store = fx.store().await;
    store.download("m");
    timeout(TIMEOUT, async {
        while file_len(&fx.tmp()) < 1_000 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    store.cancel();
    let state = wait(&store, |s| s.downloading.is_none()).await;
    let _ = release.send(());

    assert_eq!(None, state.error);
    assert!(!store.is_downloaded("m"));
    assert!(!fx.tmp().exists());
}

#[tokio::test]
async fn scan_marks_only_complete_models() {
    let fx = Fixture::new(vec![]).await;
    fs::create_dir_all(fx.dir()).unwrap();
    fs::write(fx.dir().join(FILE), &*BYTES).unwrap();
    fs::create_dir_all(fx.root.join("n")).unwrap();
    fs::write(fx.root.join("n").join(FILE), &BYTES[..1_000]).unwrap();

    let store = fx.store().await;
    let state = wait(&store, |s| !s.downloaded.is_empty()).await;

    assert_eq!(BTreeSet::from(["m".to_string()]), state.downloaded);
}

#[tokio::test]
async fn delete_releases_the_model_then_removes_its_files() {
    let fx = Fixture::new(vec![ok(&BYTES)]).await;
    let deleted = Arc::<Mutex<Vec<String>>>::default();
    let recorded = Arc::clone(&deleted);
    let store = fx.store_with(u64::MAX, move |id| recorded.lock().unwrap().push(id.to_string())).await;
    download_and_wait(&store).await;

    store.delete("m"); // Sets deleting at once, so the wait below can't see a stale None.
    wait(&store, |s| s.deleting.is_none()).await;

    assert_eq!(vec!["m".to_string()], *deleted.lock().unwrap());
    assert!(!store.is_downloaded("m"));
    assert!(!fx.dir().join(FILE).exists());
    assert!(!fx.tmp().exists());
}

#[tokio::test]
async fn download_is_ignored_while_the_model_is_deleting() {
    let fx = Fixture::new(vec![ok(&BYTES)]).await;
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let store = fx
        .store_with(u64::MAX, move |_| {
            let _ = released.lock().unwrap().recv();
        })
        .await;
    download_and_wait(&store).await;

    store.delete("m");
    // The delete waits for the engine; the file is still there.
    wait(&store, |s| !s.downloaded.contains("m")).await;
    assert_eq!(Some("m".to_string()), store.state().borrow().deleting);
    store.download("m");
    assert_eq!(None, store.state().borrow().downloading);

    release.send(()).unwrap();
    let state = wait(&store, |s| s.deleting.is_none()).await;

    assert!(!state.downloaded.contains("m"));
    assert!(!fx.dir().join(FILE).exists());
    assert!(!fx.tmp().exists());
    assert_eq!(1, fx.requests().len());
}
