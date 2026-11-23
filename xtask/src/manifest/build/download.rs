//! Downloading a file and hashing it on the way, with the cache that keeps
//! the files a later check reads.

use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use marsa_core::installer::fetch::{ChunkSink, Fetch, FetchError};
use marsa_core::installer::manifest::MAX_ARTIFACT_SIZE;
use sha2::{Digest, Sha256, Sha512};

use super::super::upstream::{hex, parse_hex};

/// A downloaded file, and the URL that served it.
pub(super) struct Downloaded {
    /// Where it is kept, or `None` when it was only hashed.
    pub(super) path: Option<PathBuf>,
    pub(super) url: String,
    pub(super) sha256: [u8; 32],
    pub(super) size: u64,
    /// Never read: deletes the file when this goes out of scope.
    _scratch: Option<RemoveOnDrop>,
}

impl Downloaded {
    /// A file that was checked elsewhere and is not on disk any more.
    pub(super) fn gone(url: String, sha256: [u8; 32], size: u64) -> Self {
        Self {
            path: None,
            url,
            sha256,
            size,
            _scratch: None,
        }
    }

    /// The file on disk, for a check that reads it.
    pub(super) fn kept(&self) -> Result<&Path> {
        self.path
            .as_deref()
            .with_context(|| format!("{} was hashed on the way and not kept", self.url))
    }
}

/// Saves the first URL that answers to `dir/<name>`, with `dir/<name>.sha256`
/// (`<hex> <url>`) next to it. A cached file is reused only if its sha256
/// still matches the saved one.
pub(super) async fn cached_download(
    fetch: &impl Fetch,
    urls: &[String],
    dir: &Path,
    name: &str,
) -> Result<Downloaded> {
    ensure!(
        !name.is_empty()
            && !name.starts_with('.')
            && name
                .bytes()
                .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-')),
        "artifact name {name:?} is not [a-z0-9._-]+"
    );
    fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let path = dir.join(name);
    let record = dir.join(format!("{name}.sha256"));

    if let (Ok(saved), true) = (fs::read_to_string(&record), path.is_file()) {
        let (saved_hex, url) = saved.trim_end().split_once(' ').unwrap_or_default();
        let (sha256, size) = hash_file(&path)?;
        if parse_hex(saved_hex) == Some(sha256) && urls.iter().any(|u| u == url) {
            return Ok(Downloaded {
                path: Some(path),
                url: url.to_owned(),
                sha256,
                size,
                _scratch: None,
            });
        }
    }

    let part = dir.join(format!("{name}.part"));
    let result = first_answer(fetch, urls, |url| {
        let _ = fs::remove_file(&part);
        let file =
            File::create(&part).with_context(|| format!("cannot create {}", part.display()))?;
        Ok(HashSink::new(url, Some((file, part.clone()))))
    })
    .await;
    let (url, sink) = match result {
        Ok(found) => found,
        Err(err) => {
            let _ = fs::remove_file(&part);
            return Err(err);
        }
    };
    let HashSink {
        file, hasher, size, ..
    } = sink;
    if let Some((mut file, _)) = file {
        file.flush()
            .with_context(|| format!("cannot write {}", part.display()))?;
    }
    let sha256: [u8; 32] = hasher.finalize().into();
    fs::rename(&part, &path).with_context(|| format!("cannot move {}", part.display()))?;
    fs::write(&record, format!("{} {url}\n", hex(&sha256)))?;
    Ok(Downloaded {
        path: Some(path),
        url,
        sha256,
        size,
        _scratch: None,
    })
}

/// Hashes the body of the first URL that answers, and keeps nothing: for
/// large files that only their sha256 proves.
pub(super) async fn hashed_download(fetch: &impl Fetch, urls: &[String]) -> Result<Downloaded> {
    let (url, sink) = first_answer(fetch, urls, |url| Ok(HashSink::new(url, None))).await?;
    Ok(Downloaded {
        path: None,
        url,
        sha256: sink.hasher.finalize().into(),
        size: sink.size,
        _scratch: None,
    })
}

/// Downloads the first URL that answers to `path`, hashing on the way, for
/// checks that read the file before the caller deletes it. A URL that fails
/// leaves a partial file, which the next one overwrites.
pub(super) async fn temp_download(
    fetch: &impl Fetch,
    urls: &[String],
    path: &Path,
) -> Result<Downloaded> {
    let (url, sink) = first_answer(fetch, urls, |url| {
        let file =
            File::create(path).with_context(|| format!("cannot create {}", path.display()))?;
        Ok(HashSink::new(url, Some((file, path.to_path_buf()))))
    })
    .await?;
    let HashSink {
        file, hasher, size, ..
    } = sink;
    if let Some((mut file, _)) = file {
        file.flush()
            .with_context(|| format!("cannot write {}", path.display()))?;
    }
    Ok(Downloaded {
        path: Some(path.to_path_buf()),
        url,
        sha256: hasher.finalize().into(),
        size,
        _scratch: None,
    })
}

/// Deletes a file when it goes out of scope: the large archives that are only
/// checked, never kept.
struct RemoveOnDrop(PathBuf);

impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        if let Err(err) = fs::remove_file(&self.0)
            && err.kind() != io::ErrorKind::NotFound
        {
            println!("    warning: cannot delete {}: {err}", self.0.display());
        }
    }
}

/// Downloads the first URL that answers to `work/archive`, hashing on the
/// way, for a check that reads the file (`gh` reads one). The file is deleted
/// when the answer goes out of scope, or at once on an error.
pub(super) async fn scratch_download(
    fetch: &impl Fetch,
    urls: &[String],
    work: &Path,
) -> Result<Downloaded> {
    fs::create_dir_all(work).with_context(|| format!("cannot create {}", work.display()))?;
    let path = work.join("archive");
    // A run that was killed may have left one.
    let _ = fs::remove_file(&path);
    let scratch = RemoveOnDrop(path.clone());
    temp_download(fetch, urls, &path)
        .await
        .map(|file| Downloaded {
            _scratch: Some(scratch),
            ..file
        })
}

/// Tries `urls` in order, each into a new sink from `sink_for`, and returns
/// the first that answers in full, with its sink. An HTTP error status or a
/// network error tries the next URL; any other error stops.
async fn first_answer(
    fetch: &impl Fetch,
    urls: &[String],
    mut sink_for: impl FnMut(&str) -> Result<HashSink>,
) -> Result<(String, HashSink)> {
    let mut last = None;
    for url in urls {
        let mut sink = sink_for(url)?;
        println!("  downloading {url}");
        match fetch.get(url, &mut sink).await {
            Ok(()) => {
                ensure!(sink.size > 0, "{url} is empty");
                return Ok((url.clone(), sink));
            }
            Err(err @ (FetchError::HttpStatus { .. } | FetchError::Network { .. })) => {
                println!("    {err}; trying the next URL");
                last = Some(err);
            }
            Err(err) => return Err(err.into()),
        }
    }
    match last {
        Some(err) => Err(anyhow::Error::from(err).context("no URL worked")),
        None => bail!("no URL"),
    }
}

/// Hashes the body on the way, and writes it to a file when it has one.
struct HashSink {
    /// The file, and its path for errors.
    file: Option<(File, PathBuf)>,
    url: String,
    hasher: Sha256,
    size: u64,
}

impl HashSink {
    fn new(url: &str, file: Option<(File, PathBuf)>) -> Self {
        Self {
            file,
            url: url.to_owned(),
            hasher: Sha256::new(),
            size: 0,
        }
    }
}

impl ChunkSink for HashSink {
    fn accept(&mut self, chunk: &[u8]) -> Result<(), FetchError> {
        self.size = self.size.saturating_add(chunk.len() as u64);
        if self.size > MAX_ARTIFACT_SIZE {
            return Err(FetchError::TooLarge {
                url: self.url.clone(),
                max: MAX_ARTIFACT_SIZE,
            });
        }
        self.hasher.update(chunk);
        match &mut self.file {
            Some((file, path)) => file.write_all(chunk).map_err(|source| FetchError::Io {
                path: path.clone(),
                source,
            }),
            None => Ok(()),
        }
    }
}

/// The sha256 and size of a file.
pub(super) fn hash_file(path: &Path) -> Result<([u8; 32], u64)> {
    let mut hasher = Sha256::new();
    let size = read_chunks(path, |chunk| hasher.update(chunk))?;
    Ok((hasher.finalize().into(), size))
}

/// The sha512 of a file, for sources that publish only sha512 (Caddy). The
/// same pass checks that the file still has the `sha256` the manifest will
/// record, so both hashes are of the same bytes.
pub(super) fn sha512_file(path: &Path, sha256: &[u8; 32]) -> Result<[u8; 64]> {
    let mut sha512 = Sha512::new();
    let mut check = Sha256::new();
    read_chunks(path, |chunk| {
        sha512.update(chunk);
        check.update(chunk);
    })?;
    let now: [u8; 32] = check.finalize().into();
    ensure!(
        &now == sha256,
        "{} changed on disk while the manifest was built",
        path.display()
    );
    Ok(sha512.finalize().into())
}

/// Gives each chunk of the file to `each`, and returns the size.
fn read_chunks(path: &Path, mut each: impl FnMut(&[u8])) -> Result<u64> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut buffer = vec![0u8; 1 << 16];
    let mut size = 0u64;
    loop {
        let read = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        };
        each(&buffer[..read]);
        size += read as u64;
    }
    Ok(size)
}
