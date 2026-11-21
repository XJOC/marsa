//! The one seam between the services and the OS processes: who runs under a
//! pid, who listens on a port, and signals. [`SystemProbe`] asks the OS; a
//! test answers from a fake.

use std::fmt;
use std::future::Future;
use std::io;
use std::path::PathBuf;
use std::pin::Pin;

use marsa_core::caddy::Dial;
use marsa_core::orphan::{ChildProcess, Liveness};
use marsa_core::platform::process::{self, Signal};
use marsa_core::ports::PortLookup;

/// A boxed future, so [`ProcessProbe`] works as `dyn ProcessProbe`.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What the services ask the OS about processes. Each service uses a few of
/// these methods. The defaults answer "cannot tell" (fail closed), so a test
/// fake overrides only what its test reaches; [`SystemProbe`] overrides all.
pub trait ProcessProbe: fmt::Debug + Send + Sync {
    /// The path a process was started with, and its start time
    /// ([`process::identity`]).
    fn identity(&self, _pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(std::future::ready(Liveness::Unknown))
    }

    /// The file the process really runs, and its start time
    /// ([`process::real_identity`]): for programs that rewrite `argv[0]`.
    fn real_identity(&self, _pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(std::future::ready(Liveness::Unknown))
    }

    /// The file the process really runs ([`process::executable`]).
    fn executable(&self, _pid: u32) -> BoxFuture<'_, Option<PathBuf>> {
        Box::pin(std::future::ready(None))
    }

    /// The children of `ppid` now, each with its start time: a list, never a
    /// proof. `None` when they cannot be listed: then none is signalled.
    fn children(&self, _ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>> {
        Box::pin(std::future::ready(None))
    }

    /// The processes that hold the listener at `listen` open now, an
    /// inherited one too: a list, never a proof alone. `None` when they
    /// cannot be listed: then no child of pid 1 is signalled.
    fn listener_holders<'a>(&'a self, _listen: &'a Dial) -> BoxFuture<'a, Option<Vec<u32>>> {
        Box::pin(std::future::ready(None))
    }

    /// Who may have adopted the children of an earlier daemon
    /// ([`process::adopters`]): a list to search, never a proof.
    fn adopters(&self) -> BoxFuture<'_, Vec<u32>> {
        Box::pin(std::future::ready(Vec::new()))
    }

    /// Who listens on the port, on any address ([`process::port_lookup`]). An
    /// owner, or a look that failed, means busy.
    fn port_lookup(&self, _port: u16) -> BoxFuture<'_, PortLookup> {
        Box::pin(std::future::ready(PortLookup::Failed))
    }

    /// Sends `signal` to `pid`. The caller checked the pid right before.
    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>>;
}

/// The real OS: `marsa_core::platform::process`.
#[derive(Debug)]
pub struct SystemProbe;

impl ProcessProbe for SystemProbe {
    fn identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::identity(pid))
    }

    fn real_identity(&self, pid: u32) -> BoxFuture<'_, Liveness> {
        Box::pin(process::real_identity(pid))
    }

    fn executable(&self, pid: u32) -> BoxFuture<'_, Option<PathBuf>> {
        Box::pin(process::executable(pid))
    }

    fn children(&self, ppid: u32) -> BoxFuture<'_, Option<Vec<ChildProcess>>> {
        Box::pin(process::children(ppid))
    }

    fn listener_holders<'a>(&'a self, listen: &'a Dial) -> BoxFuture<'a, Option<Vec<u32>>> {
        Box::pin(process::listener_holders(listen))
    }

    fn adopters(&self) -> BoxFuture<'_, Vec<u32>> {
        Box::pin(process::adopters())
    }

    fn port_lookup(&self, port: u16) -> BoxFuture<'_, PortLookup> {
        Box::pin(process::port_lookup(port))
    }

    fn signal(&self, pid: u32, signal: Signal) -> BoxFuture<'_, io::Result<()>> {
        Box::pin(process::send_signal(pid, signal))
    }
}
