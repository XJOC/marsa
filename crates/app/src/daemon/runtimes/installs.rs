//! The install side of the daemon: how an install runs, and whether one runs
//! now. The routes that start it are in `runtimes_api`.

use std::fmt;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use marsa_core::installer::{self, InstallEnv, InstallError, InstalledVersion};
use marsa_core::types::InstallEvent;

/// What [`RunInstall::run`] returns.
pub type InstallFuture<'a> =
    Pin<Box<dyn Future<Output = Result<InstalledVersion, InstallError>> + Send + 'a>>;

/// Runs one install. The daemon uses [`ProductionInstall`]. Tests put a fake
/// in `Daemon::installs.runner`, as `tests/services.rs` puts a fake Caddy in
/// `Daemon::caddy.spec`: the real installer needs the network and the
/// manifest key, and neither can be swapped.
pub trait RunInstall: Send + Sync {
    fn run<'a>(
        &'a self,
        data_dir: &'a Path,
        request: installer::InstallRequest<'a>,
        events: &'a mut (dyn FnMut(InstallEvent) + Send),
    ) -> InstallFuture<'a>;
}

/// `marsa_core::installer::install` with [`InstallEnv::production`]: the
/// published manifest, the key built into the binary, and every check.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProductionInstall;

impl RunInstall for ProductionInstall {
    fn run<'a>(
        &'a self,
        data_dir: &'a Path,
        request: installer::InstallRequest<'a>,
        events: &'a mut (dyn FnMut(InstallEvent) + Send),
    ) -> InstallFuture<'a> {
        Box::pin(async move {
            let env = InstallEnv::production(data_dir)?;
            installer::install(&request, &env, events).await
        })
    }
}

/// The install side of the daemon: who runs it, and whether one runs now.
pub struct Installs {
    /// Runs each install: [`ProductionInstall`], unless a test put a fake
    /// here before the daemon went into its `Arc`.
    pub runner: Arc<dyn RunInstall>,
    pub(crate) running: AtomicBool,
    pub(crate) last_id: AtomicU64,
}

impl Installs {
    /// With [`ProductionInstall`].
    pub fn new() -> Self {
        Self {
            runner: Arc::new(ProductionInstall),
            running: AtomicBool::new(false),
            last_id: AtomicU64::new(0),
        }
    }

    /// Whether an install, or an uninstall, runs now.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }
}

impl Default for Installs {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for Installs {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Installs")
            .field("running", &self.is_running())
            .field("last_id", &self.last_id.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}
