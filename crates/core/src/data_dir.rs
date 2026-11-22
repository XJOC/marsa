//! The folders inside the app data directory. Every program that needs one
//! asks here, so a folder's name is written once.

use std::path::{Path, PathBuf};

/// The app data directory and the names of its top-level folders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataDir(PathBuf);

impl DataDir {
    /// The data directory at `root`.
    pub fn new(root: impl AsRef<Path>) -> Self {
        Self(root.as_ref().to_path_buf())
    }

    /// The data directory itself.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.0
    }

    /// `run/`: the daemon's lock and file, pid files, and sockets.
    #[must_use]
    pub fn run(&self) -> PathBuf {
        self.0.join("run")
    }

    /// `logs/`.
    #[must_use]
    pub fn logs(&self) -> PathBuf {
        self.0.join("logs")
    }

    /// The daemon's own log.
    #[must_use]
    pub fn marsad_log(&self) -> PathBuf {
        self.logs().join("marsad.log")
    }

    /// `config/`: `global.toml` and `sites.toml`.
    #[must_use]
    pub fn config(&self) -> PathBuf {
        self.0.join("config")
    }

    /// `runtimes/`: installed PHP, Node, Caddy, and the database servers.
    #[must_use]
    pub fn runtimes(&self) -> PathBuf {
        self.0.join("runtimes")
    }

    /// `shims/`: the folder put in `PATH`.
    #[must_use]
    pub fn shims(&self) -> PathBuf {
        self.0.join("shims")
    }

    /// `bin/`: the programs Marsa installs for itself.
    #[must_use]
    pub fn bin(&self) -> PathBuf {
        self.0.join("bin")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_sit_directly_under_the_root() {
        let data = DataDir::new("/data");
        assert_eq!(data.root(), Path::new("/data"));
        assert_eq!(data.run(), Path::new("/data").join("run"));
        assert_eq!(data.logs(), Path::new("/data").join("logs"));
        assert_eq!(
            data.marsad_log(),
            Path::new("/data").join("logs").join("marsad.log")
        );
        assert_eq!(data.config(), Path::new("/data").join("config"));
        assert_eq!(data.runtimes(), Path::new("/data").join("runtimes"));
        assert_eq!(data.shims(), Path::new("/data").join("shims"));
        assert_eq!(data.bin(), Path::new("/data").join("bin"));
    }
}
