//! Downloads a file to a temp folder over HTTPS only, with a size limit, and
//! computes its sha256 while writing. The result is a [`VerifiedFile`], which
//! exists only when the sha256 matches the signed manifest.
//!
//! All the checks live in [`download_verified`] and [`fetch_bytes`], on top of
//! the [`Fetch`] trait. [`HttpFetch`] only turns a response into chunks, so the
//! checks are tested with [`testing::MemoryFetch`] and no network.

use std::error::Error as StdError;
use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use reqwest::{StatusCode, Url, redirect};
use sha2::{Digest, Sha256};

/// The most redirects one request follows. GitHub Releases uses one.
pub const MAX_REDIRECTS: usize = 5;

/// Receives a response body chunk by chunk.
pub trait ChunkSink: Send {
    /// Takes the next part of the body. On an error the fetcher stops reading
    /// at once and returns this error from [`Fetch::get`] unchanged.
    fn accept(&mut self, chunk: &[u8]) -> Result<(), FetchError>;
}

/// Gets the body of one URL. Knows nothing about sizes or sha256.
pub trait Fetch: Sync {
    /// Sends the body of `url` to `sink`, one chunk at a time.
    fn get(
        &self,
        url: &str,
        sink: &mut dyn ChunkSink,
    ) -> impl Future<Output = Result<(), FetchError>> + Send;
}

/// What the signed manifest says about one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub sha256: [u8; 32],
    pub size: u64,
}

/// A file on disk whose size and sha256 matched the signed manifest.
///
/// Only [`download_verified`] creates one: the fields are private and there is
/// no constructor, so the extractor cannot get an unchecked file.
///
/// ```compile_fail
/// use marsa_core::installer::fetch::VerifiedFile;
/// let file = VerifiedFile { path: std::path::PathBuf::new(), sha256: [0; 32] };
/// ```
#[derive(Debug)]
pub struct VerifiedFile {
    path: PathBuf,
    sha256: [u8; 32],
}

impl VerifiedFile {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sha256(&self) -> &[u8; 32] {
        &self.sha256
    }
}

/// Why a download failed. [`FetchError::code`] gives a stable name that the UI
/// maps to a message. URLs in messages have no query, fragment, or user info.
#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("refusing {url}: only https URLs are allowed")]
    InsecureUrl { url: String },
    #[error("{url} answered with HTTP status {status}")]
    HttpStatus { url: String, status: u16 },
    #[error("cannot download {url}")]
    Network {
        url: String,
        #[source]
        source: Box<dyn StdError + Send + Sync>,
    },
    #[error("download failed after trying {tried} URLs")]
    Failed {
        tried: usize,
        #[source]
        last: Option<Box<FetchError>>,
    },
    #[error("{url} sent {received} bytes or more, but the manifest says {expected}")]
    TooLong {
        url: String,
        expected: u64,
        received: u64,
    },
    #[error("{url} sent {received} bytes, but the manifest says {expected}")]
    TooShort {
        url: String,
        expected: u64,
        received: u64,
    },
    #[error("sha256 of {url} is {actual}, but the manifest says {expected}")]
    ChecksumMismatch {
        url: String,
        expected: String,
        actual: String,
    },
    #[error("{url} is larger than {max} bytes")]
    TooLarge { url: String, max: u64 },
    #[error("cannot write {}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl FetchError {
    /// Stable error name for the UI. Never changes once released.
    pub fn code(&self) -> &'static str {
        match self {
            FetchError::InsecureUrl { .. } => "download_insecure_url",
            FetchError::HttpStatus { .. } => "download_http_status",
            FetchError::Network { .. } | FetchError::Failed { .. } => "download_failed",
            FetchError::TooLong { .. } | FetchError::TooShort { .. } => "download_size_mismatch",
            FetchError::ChecksumMismatch { .. } => "checksum_mismatch",
            FetchError::TooLarge { .. } => "download_too_large",
            FetchError::Io { .. } => "download_io",
        }
    }

    /// Only an unavailable mirror is a reason to try the next one. A file that
    /// arrived but differs from the signed manifest stops the download: it
    /// means tampering or a bad manifest, and the user must see it.
    fn try_next_mirror(&self) -> bool {
        matches!(
            self,
            FetchError::HttpStatus { .. } | FetchError::Network { .. }
        )
    }
}

/// Downloads the first URL that works into `dest`, a file that must not exist
/// yet, and checks its size and sha256 against `expected`. `progress` gets
/// `(received, total)`. On any error `dest` is deleted.
pub async fn download_verified(
    fetch: &impl Fetch,
    urls: &[String],
    expected: &Expected,
    dest: &Path,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<VerifiedFile, FetchError> {
    download_with(fetch, urls, expected, dest, progress, create_new).await
}

fn create_new(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

/// [`download_verified`] with the file opener as a parameter, so a test can
/// make writes fail.
async fn download_with(
    fetch: &impl Fetch,
    urls: &[String],
    expected: &Expected,
    dest: &Path,
    progress: &mut (dyn FnMut(u64, u64) + Send),
    open: fn(&Path) -> io::Result<File>,
) -> Result<VerifiedFile, FetchError> {
    let mut last = None;
    for (tried, url) in urls.iter().enumerate() {
        let url = check_https(url)?;
        let shown = redact(&url);
        // `create_new`: never write over a file that is already there. This
        // error returns before anything is deleted.
        let file = open(dest).map_err(|source| io_error(dest, source))?;
        tracing::info!(url = %shown, "downloading");
        match download_one(fetch, &url, &shown, file, expected, dest, progress).await {
            Ok(sha256) => {
                return Ok(VerifiedFile {
                    path: dest.to_path_buf(),
                    sha256,
                });
            }
            Err(err) => {
                remove_partial(dest);
                if !err.try_next_mirror() {
                    return Err(err);
                }
                tracing::warn!(url = %shown, attempt = tried + 1, error = %err, "mirror failed");
                last = Some(Box::new(err));
            }
        }
    }
    Err(FetchError::Failed {
        tried: urls.len(),
        last,
    })
}

/// One attempt. Owns `file`, so the file is closed when this returns and the
/// caller can delete it (Windows cannot delete an open file).
async fn download_one(
    fetch: &impl Fetch,
    url: &Url,
    shown: &str,
    file: File,
    expected: &Expected,
    dest: &Path,
    progress: &mut (dyn FnMut(u64, u64) + Send),
) -> Result<[u8; 32], FetchError> {
    progress(0, expected.size);
    let mut sink = FileSink {
        file,
        path: dest,
        url: shown,
        hasher: Sha256::new(),
        received: 0,
        expected: expected.size,
        failed: false,
        progress,
    };
    fetch.get(url.as_str(), &mut sink).await?;
    if sink.failed {
        // The fetcher ignored a refused chunk: never trust what is on disk.
        return Err(sink.refused());
    }
    if sink.received != expected.size {
        return Err(FetchError::TooShort {
            url: shown.to_owned(),
            expected: expected.size,
            received: sink.received,
        });
    }
    let actual: [u8; 32] = sink.hasher.finalize().into();
    if actual != expected.sha256 {
        return Err(FetchError::ChecksumMismatch {
            url: shown.to_owned(),
            expected: hex(&expected.sha256),
            actual: hex(&actual),
        });
    }
    // `sync_all` can take a while: tokio runs it on its blocking threads.
    let file = tokio::fs::File::from_std(sink.file);
    file.sync_all()
        .await
        .map_err(|source| io_error(dest, source))?;
    Ok(actual)
}

/// Writes each chunk to the file and to the hasher. Refuses a chunk that would
/// go past the expected size before writing it.
struct FileSink<'a> {
    file: File,
    path: &'a Path,
    url: &'a str,
    hasher: Sha256,
    received: u64,
    expected: u64,
    failed: bool,
    progress: &'a mut (dyn FnMut(u64, u64) + Send),
}

impl FileSink<'_> {
    fn refused(&self) -> FetchError {
        io_error(
            self.path,
            io::Error::other("download stopped after an earlier error"),
        )
    }

    fn try_accept(&mut self, chunk: &[u8]) -> Result<(), FetchError> {
        let received = self.received.saturating_add(chunk.len() as u64);
        if received > self.expected {
            return Err(FetchError::TooLong {
                url: self.url.to_owned(),
                expected: self.expected,
                received,
            });
        }
        // A blocking write of one chunk into the page cache is short enough to
        // run on the async thread; the slow `sync_all` is not done here.
        self.file
            .write_all(chunk)
            .map_err(|source| io_error(self.path, source))?;
        self.hasher.update(chunk);
        self.received = received;
        (self.progress)(received, self.expected);
        Ok(())
    }
}

impl ChunkSink for FileSink<'_> {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), FetchError> {
        if self.failed {
            return Err(self.refused());
        }
        let result = self.try_accept(chunk);
        self.failed = result.is_err();
        result
    }
}

/// Downloads a small file (the manifest or its signature) into memory. Fails
/// as soon as the body goes past `max` bytes.
pub async fn fetch_bytes(fetch: &impl Fetch, url: &str, max: u64) -> Result<Vec<u8>, FetchError> {
    let url = check_https(url)?;
    let mut sink = BytesSink {
        bytes: Vec::new(),
        url: redact(&url),
        max,
        failed: false,
    };
    fetch.get(url.as_str(), &mut sink).await?;
    if sink.failed {
        return Err(sink.too_large());
    }
    Ok(sink.bytes)
}

struct BytesSink {
    bytes: Vec<u8>,
    url: String,
    max: u64,
    failed: bool,
}

impl BytesSink {
    fn too_large(&self) -> FetchError {
        FetchError::TooLarge {
            url: self.url.clone(),
            max: self.max,
        }
    }
}

impl ChunkSink for BytesSink {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), FetchError> {
        let total = (self.bytes.len() as u64).saturating_add(chunk.len() as u64);
        if self.failed || total > self.max {
            self.failed = true;
            return Err(self.too_large());
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }
}

/// The real fetcher: one `reqwest` client with rustls and the system's
/// certificate store.
#[derive(Debug, Clone)]
pub struct HttpFetch {
    client: reqwest::Client,
}

impl HttpFetch {
    pub fn new() -> Result<Self, FetchError> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("marsa/", env!("CARGO_PKG_VERSION")))
            // A second guard behind `redirect_action`: reqwest refuses any
            // non-https URL itself too.
            .https_only(true)
            .redirect(redirect::Policy::custom(|attempt| {
                match redirect_action(attempt.url(), attempt.previous().len()) {
                    RedirectAction::Follow => attempt.follow(),
                    RedirectAction::Insecure => {
                        let url = redact(attempt.url());
                        attempt.error(InsecureRedirect { url })
                    }
                    RedirectAction::TooMany => attempt.error(TooManyRedirects),
                }
            }))
            .connect_timeout(Duration::from_secs(15))
            // Between two reads, not for the whole body: a 36 MB file on a slow
            // line must still finish.
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|err| FetchError::Network {
                url: "(client setup)".to_owned(),
                source: Box::new(err),
            })?;
        Ok(Self { client })
    }
}

impl Fetch for HttpFetch {
    async fn get(&self, url: &str, sink: &mut dyn ChunkSink) -> Result<(), FetchError> {
        let url = check_https(url)?;
        let shown = redact(&url);
        let mut response = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|err| network_error(&shown, err))?;
        // The redirect policy already checked each hop; check the end too.
        if response.url().scheme() != "https" {
            return Err(FetchError::InsecureUrl {
                url: redact(response.url()),
            });
        }
        let status = response.status();
        if status != StatusCode::OK {
            return Err(FetchError::HttpStatus {
                url: shown,
                status: status.as_u16(),
            });
        }
        // `Content-Length` is never read: the sink counts the real bytes.
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|err| network_error(&shown, err))?
        {
            sink.accept(&chunk)?;
        }
        Ok(())
    }
}

/// What to do with one redirect, given how many URLs the chain has seen so
/// far (the first request counts as one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RedirectAction {
    Follow,
    Insecure,
    TooMany,
}

fn redirect_action(next: &Url, previous: usize) -> RedirectAction {
    if next.scheme() != "https" {
        RedirectAction::Insecure
    } else if previous > MAX_REDIRECTS {
        RedirectAction::TooMany
    } else {
        RedirectAction::Follow
    }
}

#[derive(Debug, thiserror::Error)]
#[error("redirect to a non-https URL: {url}")]
struct InsecureRedirect {
    url: String,
}

#[derive(Debug, thiserror::Error)]
#[error("more than {MAX_REDIRECTS} redirects")]
struct TooManyRedirects;

fn network_error(shown: &str, err: reqwest::Error) -> FetchError {
    if let Some(insecure) = insecure_redirect_in(&err) {
        return FetchError::InsecureUrl {
            url: insecure.url.clone(),
        };
    }
    FetchError::Network {
        url: shown.to_owned(),
        // `without_url`: reqwest's message would show the full URL.
        source: Box::new(err.without_url()),
    }
}

/// Finds the error our redirect policy returned, at any depth of the chain.
fn insecure_redirect_in<'a>(err: &'a (dyn StdError + 'static)) -> Option<&'a InsecureRedirect> {
    let mut cause = Some(err);
    while let Some(current) = cause {
        if let Some(insecure) = current.downcast_ref::<InsecureRedirect>() {
            return Some(insecure);
        }
        cause = current.source();
    }
    None
}

/// Parses `url` and refuses any scheme but `https`, before any connection.
fn check_https(url: &str) -> Result<Url, FetchError> {
    match Url::parse(url) {
        Ok(parsed) if parsed.scheme() == "https" && parsed.has_host() => Ok(parsed),
        Ok(parsed) => Err(FetchError::InsecureUrl {
            url: redact(&parsed),
        }),
        Err(_) => Err(FetchError::InsecureUrl {
            url: "(not a valid URL)".to_owned(),
        }),
    }
}

/// The URL for logs and messages: no user info, query, or fragment.
fn redact(url: &Url) -> String {
    let mut shown = url.clone();
    // These fail only for URLs that cannot have user info (`file:`, `data:`),
    // which then have none to remove.
    let _ = shown.set_username("");
    let _ = shown.set_password(None);
    shown.set_query(None);
    shown.set_fragment(None);
    shown.into()
}

fn remove_partial(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "cannot delete a failed download");
        }
    }
}

fn io_error(path: &Path, source: io::Error) -> FetchError {
    FetchError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn hex(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(64);
    for byte in bytes {
        // Writing to a `String` cannot fail.
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// An in-memory [`Fetch`] for tests here and in later tasks. It opens no
/// socket and no file.
#[doc(hidden)]
pub mod testing {
    use std::collections::BTreeMap;
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::{ChunkSink, Fetch, FetchError};

    /// What [`MemoryFetch`] answers for one URL.
    #[derive(Debug, Clone)]
    pub enum Reply {
        /// HTTP 200 with this body, sent in these chunks.
        Chunks(Vec<Vec<u8>>),
        /// Any status but 200.
        Status(u16),
        /// A connection error.
        NetworkError,
    }

    /// Answers from a map of URL to [`Reply`]. An unknown URL is a network
    /// error. Counts requests and chunks.
    #[derive(Debug, Default)]
    pub struct MemoryFetch {
        replies: BTreeMap<String, Reply>,
        requests: Mutex<Vec<String>>,
        chunks_sent: AtomicUsize,
    }

    impl MemoryFetch {
        pub fn new() -> Self {
            Self::default()
        }

        pub fn with(mut self, url: &str, reply: Reply) -> Self {
            self.replies.insert(url.to_owned(), reply);
            self
        }

        /// The URLs asked for, in order.
        pub fn requests(&self) -> Vec<String> {
            self.requests
                .lock()
                .map(|requests| requests.clone())
                .unwrap_or_default()
        }

        /// How many chunks were handed to a sink, over all requests.
        pub fn chunks_sent(&self) -> usize {
            self.chunks_sent.load(Ordering::SeqCst)
        }
    }

    impl Fetch for MemoryFetch {
        async fn get(&self, url: &str, sink: &mut dyn ChunkSink) -> Result<(), FetchError> {
            if let Ok(mut requests) = self.requests.lock() {
                requests.push(url.to_owned());
            }
            match self.replies.get(url) {
                Some(Reply::Chunks(chunks)) => {
                    for chunk in chunks {
                        self.chunks_sent.fetch_add(1, Ordering::SeqCst);
                        sink.accept(chunk)?;
                    }
                    Ok(())
                }
                Some(Reply::Status(status)) => Err(FetchError::HttpStatus {
                    url: url.to_owned(),
                    status: *status,
                }),
                Some(Reply::NetworkError) | None => Err(FetchError::Network {
                    url: url.to_owned(),
                    source: Box::new(io::Error::from(io::ErrorKind::ConnectionRefused)),
                }),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::{MemoryFetch, Reply};
    use super::*;
    use tempfile::TempDir;

    const A: &str = "https://a.example/php.tar.gz";
    const B: &str = "https://b.example/php.tar.gz";

    fn body() -> Vec<Vec<u8>> {
        vec![b"hello ".to_vec(), b"marsa ".to_vec(), b"world".to_vec()]
    }

    fn expected_for(chunks: &[Vec<u8>]) -> Expected {
        let all = chunks.concat();
        Expected {
            sha256: Sha256::digest(&all).into(),
            size: all.len() as u64,
        }
    }

    fn urls(list: &[&str]) -> Vec<String> {
        list.iter().map(|url| (*url).to_owned()).collect()
    }

    fn dest() -> (TempDir, PathBuf) {
        let dir = TempDir::new().expect("create temp dir");
        let path = dir.path().join("php.tar.gz");
        (dir, path)
    }

    async fn download(
        fetch: &impl Fetch,
        list: &[&str],
        dest: &Path,
    ) -> Result<VerifiedFile, FetchError> {
        let expected = expected_for(&body());
        download_verified(fetch, &urls(list), &expected, dest, &mut |_, _| {}).await
    }

    #[tokio::test]
    async fn a_good_file_becomes_a_verified_file() {
        let fetch = MemoryFetch::new().with(A, Reply::Chunks(body()));
        let expected = expected_for(&body());
        let (_dir, dest) = dest();
        let mut seen = Vec::new();
        let mut progress = |got, total| seen.push((got, total));
        let file = download_verified(&fetch, &urls(&[A]), &expected, &dest, &mut progress)
            .await
            .expect("download succeeds");

        assert_eq!(file.path(), dest);
        assert_eq!(file.sha256(), &expected.sha256);
        assert_eq!(fs::read(&dest).expect("read file"), body().concat());
        assert_eq!(seen.first(), Some(&(0, expected.size)));
        assert_eq!(seen.last(), Some(&(expected.size, expected.size)));
    }

    #[tokio::test]
    async fn one_changed_byte_is_a_checksum_mismatch() {
        let mut bad = body();
        bad[1][0] ^= 1;
        let fetch = MemoryFetch::new().with(A, Reply::Chunks(bad));
        let (_dir, dest) = dest();
        let err = download(&fetch, &[A], &dest).await.expect_err("bad sha256");
        assert_eq!(err.code(), "checksum_mismatch");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn a_longer_body_stops_at_the_first_extra_chunk() {
        // The fake sends no `Content-Length`: the limit comes from counted bytes.
        let mut long = body();
        long.extend([b"extra".to_vec(), b"never read".to_vec()]);
        let fetch = MemoryFetch::new().with(A, Reply::Chunks(long));
        let (_dir, dest) = dest();
        let err = download(&fetch, &[A], &dest).await.expect_err("too long");
        assert_eq!(err.code(), "download_size_mismatch");
        assert_eq!(fetch.chunks_sent(), body().len() + 1);
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn one_huge_chunk_is_refused_before_it_is_written() {
        let fetch = MemoryFetch::new().with(A, Reply::Chunks(vec![vec![0; 1 << 20]]));
        let (_dir, dest) = dest();
        let err = download(&fetch, &[A], &dest).await.expect_err("too long");
        assert!(matches!(err, FetchError::TooLong { received, .. } if received == 1 << 20));
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn a_truncated_body_is_a_size_mismatch() {
        let fetch = MemoryFetch::new().with(A, Reply::Chunks(body()[..2].to_vec()));
        let (_dir, dest) = dest();
        let err = download(&fetch, &[A], &dest).await.expect_err("truncated");
        assert_eq!(err.code(), "download_size_mismatch");
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn a_network_error_tries_the_next_mirror() {
        let fetch = MemoryFetch::new()
            .with(A, Reply::NetworkError)
            .with(B, Reply::Chunks(body()));
        let (_dir, dest) = dest();
        let file = download(&fetch, &[A, B], &dest)
            .await
            .expect("second mirror works");
        assert_eq!(fs::read(file.path()).expect("read file"), body().concat());
        assert_eq!(fetch.requests(), urls(&[A, B]));
    }

    #[tokio::test]
    async fn a_bad_status_tries_the_next_mirror() {
        let fetch = MemoryFetch::new()
            .with(A, Reply::Status(404))
            .with(B, Reply::Chunks(body()));
        let (_dir, dest) = dest();
        download(&fetch, &[A, B], &dest)
            .await
            .expect("second mirror works");
    }

    #[tokio::test]
    async fn a_checksum_mismatch_does_not_try_the_next_mirror() {
        let wrong = vec![b"hello marsa worle".to_vec()];
        let fetch = MemoryFetch::new()
            .with(A, Reply::Chunks(wrong))
            .with(B, Reply::Chunks(body()));
        let (_dir, dest) = dest();
        let err = download(&fetch, &[A, B], &dest)
            .await
            .expect_err("stops at the first mirror");
        assert_eq!(err.code(), "checksum_mismatch");
        assert_eq!(fetch.requests(), urls(&[A]));
        assert!(!dest.exists());
    }

    #[tokio::test]
    async fn all_mirrors_failing_is_download_failed() {
        let fetch = MemoryFetch::new()
            .with(A, Reply::NetworkError)
            .with(B, Reply::Status(503));
        let (_dir, dest) = dest();
        let err = download(&fetch, &[A, B], &dest)
            .await
            .expect_err("no mirror works");
        assert!(matches!(
            err,
            FetchError::Failed {
                tried: 2,
                last: Some(_)
            }
        ));
        assert_eq!(err.code(), "download_failed");
        assert!(!dest.exists());

        let err = download(&fetch, &[], &dest).await.expect_err("no mirrors");
        assert_eq!(err.code(), "download_failed");
    }

    #[tokio::test]
    async fn an_existing_dest_is_not_touched() {
        let fetch = MemoryFetch::new().with(A, Reply::Chunks(body()));
        let (_dir, dest) = dest();
        fs::write(&dest, b"old").expect("write old file");
        let err = download(&fetch, &[A], &dest)
            .await
            .expect_err("dest exists");
        assert_eq!(err.code(), "download_io");
        assert_eq!(fs::read(&dest).expect("read old file"), b"old");
        assert!(fetch.requests().is_empty());
    }

    #[tokio::test]
    async fn a_failed_write_is_download_io_and_stops() {
        // Create the file, then hand back a read-only handle: every write fails.
        fn read_only(path: &Path) -> io::Result<File> {
            create_new(path)?;
            File::open(path)
        }
        let fetch = MemoryFetch::new()
            .with(A, Reply::Chunks(body()))
            .with(B, Reply::Chunks(body()));
        let (_dir, dest) = dest();
        let expected = expected_for(&body());
        let list = urls(&[A, B]);
        let err = download_with(&fetch, &list, &expected, &dest, &mut |_, _| {}, read_only)
            .await
            .expect_err("write fails");
        assert_eq!(err.code(), "download_io");
        assert_eq!(fetch.chunks_sent(), 1);
        assert_eq!(fetch.requests(), urls(&[A]));
        assert!(!dest.exists());
    }

    /// A broken fetcher that ignores the sink's errors and keeps sending.
    struct IgnoresErrors(Vec<Vec<u8>>);

    impl Fetch for IgnoresErrors {
        async fn get(&self, _url: &str, sink: &mut dyn ChunkSink) -> Result<(), FetchError> {
            for chunk in &self.0 {
                let _ = sink.accept(chunk);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_fetcher_that_ignores_a_refusal_still_fails() {
        // The second chunk is refused; the others add up to the right size.
        let fetch = IgnoresErrors(vec![
            b"hello marsa".to_vec(),
            b"too long!!".to_vec(),
            b" world".to_vec(),
        ]);
        let (_dir, dest) = dest();
        let err = download(&fetch, &[A], &dest).await.expect_err("refused");
        assert_eq!(err.code(), "download_io");
        assert!(!dest.exists());

        let err = fetch_bytes(&fetch, A, 11).await.expect_err("refused");
        assert_eq!(err.code(), "download_too_large");
    }

    #[tokio::test]
    async fn non_https_urls_are_refused_before_any_request() {
        let plain = "http://a.example/php.tar.gz";
        let fetch = MemoryFetch::new().with(plain, Reply::Chunks(body()));
        let (_dir, dest) = dest();
        let err = download(&fetch, &[plain], &dest).await.expect_err("http");
        assert_eq!(err.code(), "download_insecure_url");
        let err = fetch_bytes(&fetch, plain, 100).await.expect_err("http");
        assert_eq!(err.code(), "download_insecure_url");
        assert!(fetch.requests().is_empty());
        assert!(!dest.exists());

        // `check_https` runs before the client sends anything: no network.
        let http = HttpFetch::new().expect("build client");
        let bad = [
            plain,
            "file:///etc/passwd",
            "ftp://a.example/x",
            "https:",
            "not a url",
        ];
        for url in bad {
            let mut sink = BytesSink {
                bytes: Vec::new(),
                url: String::new(),
                max: 10,
                failed: false,
            };
            let err = http.get(url, &mut sink).await.expect_err("refused");
            assert_eq!(err.code(), "download_insecure_url", "{url}");
        }
    }

    #[test]
    fn redirects_must_stay_on_https_and_be_few() {
        let https = Url::parse("https://release-assets.githubusercontent.com/x").expect("url");
        let http = Url::parse("http://release-assets.githubusercontent.com/x").expect("url");
        let file = Url::parse("file:///etc/passwd").expect("url");
        assert_eq!(redirect_action(&https, 1), RedirectAction::Follow);
        assert_eq!(
            redirect_action(&https, MAX_REDIRECTS),
            RedirectAction::Follow
        );
        assert_eq!(
            redirect_action(&https, MAX_REDIRECTS + 1),
            RedirectAction::TooMany
        );
        assert_eq!(redirect_action(&http, 1), RedirectAction::Insecure);
        assert_eq!(redirect_action(&file, 1), RedirectAction::Insecure);
    }

    #[test]
    fn an_insecure_redirect_is_found_inside_wrapped_errors() {
        /// Like `reqwest::Error`: `source()` returns the boxed inner error.
        #[derive(Debug, thiserror::Error)]
        #[error("error following redirect")]
        struct Wrapper(#[source] Box<dyn StdError + Send + Sync>);

        let marker = InsecureRedirect {
            url: "http://evil.example/x".to_owned(),
        };
        let wrapped = Wrapper(Box::new(Wrapper(Box::new(marker))));
        let found = insecure_redirect_in(&wrapped).expect("found");
        assert_eq!(found.url, "http://evil.example/x");
        let plain = Wrapper(Box::new(io::Error::other("timeout")));
        assert!(insecure_redirect_in(&plain).is_none());
    }

    #[tokio::test]
    async fn fetch_bytes_enforces_max() {
        let fetch = MemoryFetch::new().with(A, Reply::Chunks(body()));
        let size = body().concat().len() as u64;
        let bytes = fetch_bytes(&fetch, A, size).await.expect("fits exactly");
        assert_eq!(bytes, body().concat());
        let err = fetch_bytes(&fetch, A, size - 1).await.expect_err("too big");
        assert_eq!(err.code(), "download_too_large");
    }

    #[test]
    fn redact_drops_secrets_from_urls() {
        let url = Url::parse("https://user:pw@a.example/f.tar.gz?token=x#frag").expect("url");
        assert_eq!(redact(&url), "https://a.example/f.tar.gz");
    }

    #[test]
    fn the_download_future_is_send() {
        fn assert_send<T: Send>(_: &T) {}
        let http = HttpFetch::new().expect("build client");
        let expected = expected_for(&body());
        let dest = PathBuf::from("unused");
        let list = urls(&[A]);
        let mut progress = |_, _| {};
        let future = download_verified(&http, &list, &expected, &dest, &mut progress);
        assert_send(&future);
    }
}
