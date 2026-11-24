//! Services: the id, state, status, and actions of every managed process.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize};

use crate::databases::ServerKind;
use crate::php::PhpMinor;
use crate::recipes::Os;

/// The state of one managed process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ServiceState {
    Stopped,
    Starting,
    Running,
    Stopping,
    /// Stopped by an error. `code` is a stable error name, for example
    /// `port_in_use`, that the UI maps to a message.
    Failed {
        code: String,
    },
}

/// One managed process, as `GET /v1/services` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceStatus {
    pub id: ServiceId,
    #[serde(flatten)]
    pub state: ServiceState,
    /// The OS process id while the process runs.
    pub pid: Option<u32>,
    /// Restarts after a failure since the last start asked by the user.
    pub restarts: u32,
    /// When the process entered its current state.
    pub since_ms: Option<u64>,
}

/// A service `marsad` runs. On the wire it is its id text: `caddy`, `dns`,
/// `php-fpm-8.4` (`php-cgi-8.4` on Windows, ADR 0016), `postgres`, `mysql`,
/// `valkey`, or a site's worker (`queue-shop`, `schedule-shop`). The id also
/// names the log file `logs/<id>.log` and the pid file `run/<id>.pid`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ServiceId {
    /// The web server that serves every site.
    Caddy,
    /// The DNS server for `.test` names, inside the daemon (macOS, Linux).
    Dns,
    /// The PHP pool of one PHP line, which starts and stops with the sites
    /// that need it.
    Pool(PoolKind, PhpMinor),
    Database(ServerKind),
    /// A Laravel site's `artisan queue:work` or `artisan schedule:work`,
    /// which runs while the site's setting asks for it.
    Worker(WorkerKind, SiteKey),
}

/// What a site's worker runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum WorkerKind {
    /// `php artisan queue:work`: the jobs of the site's queue.
    Queue,
    /// `php artisan schedule:work`: the site's scheduled tasks, each minute.
    Schedule,
}

impl WorkerKind {
    pub const ALL: [Self; 2] = [Self::Queue, Self::Schedule];

    /// The start of the worker's id: `queue-<site>`.
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Queue => "queue",
            Self::Schedule => "schedule",
        }
    }

    /// The `artisan` command.
    pub const fn command(self) -> &'static str {
        match self {
            Self::Queue => "queue:work",
            Self::Schedule => "schedule:work",
        }
    }
}

/// A site name inside a [`ServiceId`], which stays `Copy`. Made only from a
/// valid site name (1 to 63 of `a-z`, `0-9`, and `-`,
/// `sites::validate_name`), so an id never holds `/`, `..`, or a space, and
/// `logs/queue-<site>.log` is always one file in `logs/`.
#[derive(Clone, Copy)]
pub struct SiteKey {
    bytes: [u8; crate::sites::MAX_NAME_LEN],
    len: u8,
}

impl SiteKey {
    /// `None` for a name that is not a valid site name.
    pub fn new(name: &str) -> Option<Self> {
        crate::sites::validate_name(name).ok()?;
        let mut bytes = [0; crate::sites::MAX_NAME_LEN];
        bytes[..name.len()].copy_from_slice(name.as_bytes());
        let len = u8::try_from(name.len()).ok()?;
        Some(Self { bytes, len })
    }

    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.bytes[..usize::from(self.len)])
            .expect("a site key holds ASCII only, checked by `new`")
    }
}

impl PartialEq for SiteKey {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl Eq for SiteKey {}

impl PartialOrd for SiteKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for SiteKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.as_str().cmp(other.as_str())
    }
}

impl std::hash::Hash for SiteKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.as_str().hash(state);
    }
}

impl fmt::Debug for SiteKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(self.as_str(), f)
    }
}

impl fmt::Display for SiteKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The program a PHP pool runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PoolKind {
    /// php-fpm: macOS and Linux.
    Fpm,
    /// php-cgi: Windows (ADR 0016).
    Cgi,
}

impl PoolKind {
    /// The program name, and the start of the pool's id.
    pub const fn program(self) -> &'static str {
        match self {
            Self::Fpm => "php-fpm",
            Self::Cgi => "php-cgi",
        }
    }
}

impl ServiceId {
    /// The pool of `minor` on the system `os`: php-cgi on Windows, php-fpm
    /// on the others.
    pub fn pool(os: Os, minor: PhpMinor) -> Self {
        let kind = match os {
            Os::Windows => PoolKind::Cgi,
            Os::Mac | Os::Linux | Os::Any => PoolKind::Fpm,
        };
        Self::Pool(kind, minor)
    }

    /// Whether this is a PHP pool, which the sites start and stop.
    pub fn is_pool(self) -> bool {
        matches!(self, Self::Pool(..))
    }

    /// A site's worker: it runs while the site's setting asks for it.
    pub fn is_worker(self) -> bool {
        matches!(self, Self::Worker(..))
    }
}

impl fmt::Display for ServiceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Caddy => f.write_str("caddy"),
            Self::Dns => f.write_str("dns"),
            Self::Pool(kind, minor) => write!(f, "{}-{minor}", kind.program()),
            Self::Database(kind) => f.write_str(kind.id()),
            Self::Worker(kind, site) => write!(f, "{}-{site}", kind.prefix()),
        }
    }
}

/// A service id that names no service of this build.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("no service named {0:?}")]
pub struct UnknownService(pub String);

impl FromStr for ServiceId {
    type Err = UnknownService;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let unknown = || UnknownService(text.to_owned());
        match text {
            "caddy" => return Ok(Self::Caddy),
            "dns" => return Ok(Self::Dns),
            _ => {}
        }
        if let Some(kind) = ServerKind::from_id(text) {
            return Ok(Self::Database(kind));
        }
        for kind in WorkerKind::ALL {
            let site = text
                .strip_prefix(kind.prefix())
                .and_then(|rest| rest.strip_prefix('-'));
            if let Some(site) = site {
                return SiteKey::new(site)
                    .map(|site| Self::Worker(kind, site))
                    .ok_or_else(unknown);
            }
        }
        let (program, line) = text.rsplit_once('-').ok_or_else(unknown)?;
        let kind = [PoolKind::Fpm, PoolKind::Cgi]
            .into_iter()
            .find(|kind| kind.program() == program)
            .ok_or_else(unknown)?;
        let minor = PhpMinor::parse(line).ok_or_else(unknown)?;
        let id = Self::Pool(kind, minor);
        // One text per id: `php-fpm-08.4` is not `php-fpm-8.4`.
        if id.to_string() == text {
            Ok(id)
        } else {
            Err(unknown())
        }
    }
}

impl Serialize for ServiceId {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ServiceId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

/// The `{action}` in `POST /v1/services/{id}/{action}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAction {
    Start,
    Stop,
    Restart,
}

impl ServiceAction {
    /// The path segment: `start`, `stop`, or `restart`.
    pub fn as_str(self) -> &'static str {
        match self {
            ServiceAction::Start => "start",
            ServiceAction::Stop => "stop",
            ServiceAction::Restart => "restart",
        }
    }
}

impl fmt::Display for ServiceAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An `{action}` path segment that is not `start`, `stop`, or `restart`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown service action {0:?}")]
pub struct UnknownAction(pub String);

impl UnknownAction {
    /// Stable error name for the UI.
    pub fn code(&self) -> &'static str {
        crate::codes::UNKNOWN_ACTION
    }
}

impl FromStr for ServiceAction {
    type Err = UnknownAction;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "start" => Ok(ServiceAction::Start),
            "stop" => Ok(ServiceAction::Stop),
            "restart" => Ok(ServiceAction::Restart),
            other => Err(UnknownAction(other.to_owned())),
        }
    }
}
