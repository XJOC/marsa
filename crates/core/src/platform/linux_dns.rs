//! Linux and Windows: reading the hosts file and the marker that says a data
//! folder's setup took the hosts way. The daemon writes both through the
//! helper; the CLI only reads them.

use crate::dns;
use std::collections::BTreeSet;
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

/// The hosts file. Never from a request.
pub const HOSTS_FILE: &str = "/etc/hosts";
/// The most of the hosts file that is read: the helper's own limit. A
/// larger file counts as one with no block, as the helper leaves it alone.
pub const MAX_HOSTS_BYTES: u64 = 16 * 1024 * 1024;

/// The names in Marsa's block of the hosts file at `path`, or `None`: no
/// file, no block, markers that do not pair up, or a file that cannot be
/// read or is larger than the helper reads.
pub fn read_block(path: &Path) -> Option<BTreeSet<String>> {
    let read = || -> io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_HOSTS_BYTES + 1)
            .read_to_end(&mut bytes)?;
        Ok(bytes)
    };
    match read() {
        Ok(bytes) if bytes.len() as u64 > MAX_HOSTS_BYTES => {
            tracing::warn!(path = %path.display(), "the hosts file is larger than Marsa reads");
            None
        }
        Ok(bytes) => dns::hosts_block_names(&String::from_utf8_lossy(&bytes)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            tracing::warn!(path = %path.display(), error = %err, "cannot read the hosts file");
            None
        }
    }
}

/// The file in the data folder that says this folder's setup took the
/// hosts way: `<data>/hosts-block`. Setup writes it after
/// a `hosts_sync` that is done, and takes it out once the link is made.
/// Only its presence counts. A data folder whose setup never wrote the
/// block, such as a test's or a scratch one, never syncs the hosts file
/// from `marsa park` or `marsa link`, so it never asks for the password.
pub fn marker(data_dir: &Path) -> PathBuf {
    data_dir.join(HOSTS_MARKER)
}

const HOSTS_MARKER: &str = "hosts-block";
