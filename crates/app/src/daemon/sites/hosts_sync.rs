//! Windows: the names of the sites in the hosts file, after each sites
//! pass (ADR 0018).
//!
//! The hosts file has no wildcards, so each site's `<name>.test` needs its
//! own line. The helper service writes Marsa's block of the file; the daemon
//! sends it the names through the pipe, with no prompt, after each pass of
//! [`sync`](super::sync) and after the routes it answers.
//!
//! - Only for a data folder whose `marsa setup` ran on Windows: setup writes
//!   [`MARKER`] there. A daemon of the tests, or one with a scratch
//!   `--data-dir`, has none, so it never touches the machine's hosts file,
//!   even with the service installed for its user (AGENTS.md section 7).
//! - No cache: the service answers `unchanged` and writes nothing when the
//!   block is already right, and setup, a hand edit, or `uninstall_all` may
//!   have changed the file since the last send.
//! - A failure is tried again after [`RETRY`], with or without a new pass.
//!   A warning is logged when the reason changes, not at every try.
//!
//! Windows only: on Linux each write of `/etc/hosts` is a password prompt,
//! and no prompt may show that the user did not cause. Linux updates the
//! names with the user's next command or `marsa setup` instead. macOS needs no names: its DNS covers every `.test` name.

use marsa_core::codes;
use std::collections::BTreeSet;
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use marsa_core::dns;
use marsa_core::helper::{
    HELPER_PROTOCOL_VERSION, HelperOp, HelperRequest, HelperResponse, OpName, OpStatus,
};
use marsa_core::sites::Site;
use tokio::time::Instant;

use crate::daemon::state::Daemon;

/// The file `marsa setup` writes in the data folder once the helper service
/// took its request on Windows.
pub const MARKER: &str = "helper-service";
/// How long after a failure the names are sent again.
pub const RETRY: Duration = Duration::from_secs(30);
/// How long one send may take: a `hosts_sync` takes a moment, and a pass
/// waits for it.
// Off Windows nothing sends.
#[cfg_attr(not(windows), allow(dead_code))]
pub const LIMIT: Duration = Duration::from_secs(10);

/// The state between sends: when to try again, and the last reason.
#[derive(Debug, Default)]
pub struct HostsSync {
    retry_at: Option<Instant>,
    last_failure: Option<String>,
}

impl HostsSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// Waits until a retry is due; never ends when none is.
    pub async fn retry_due(&self) {
        match self.retry_at {
            Some(at) => tokio::time::sleep_until(at).await,
            None => std::future::pending().await,
        }
    }

    /// The names of the daemon's sites to the helper service, on Windows.
    /// Nothing elsewhere.
    pub async fn sync(&mut self, daemon: &Daemon) {
        // `#[cfg]` outside `platform`: the pipe is a Windows object, and only
        // Windows writes the names without a prompt (see the module doc).
        #[cfg(windows)]
        {
            use marsa_core::platform::windows::helper_pipe;
            let names = daemon.sites().map(|list| names_of(&list.sites));
            self.sync_with(daemon.data_dir(), names, |request| async move {
                helper_pipe::send(request.as_bytes(), LIMIT)
                    .await
                    .map_err(|err| err.code())
            })
            .await;
        }
        #[cfg(not(windows))]
        let _ = daemon;
    }

    /// The step itself, with `send` for the pipe: the request goes to
    /// `send`, which gives the service's answer or the code of the pipe's
    /// failure. `names`: `None` when the site list could not be built.
    pub async fn sync_with<F, Fut>(&mut self, data_dir: &Path, names: Option<Vec<String>>, send: F)
    where
        F: FnOnce(String) -> Fut,
        Fut: Future<Output = Result<Vec<u8>, &'static str>>,
    {
        self.retry_at = None;
        if !data_dir.join(MARKER).exists() {
            return;
        }
        let Some(names) = names else {
            // Not "no site": the block stays as it is until the list is back.
            tracing::debug!("no site list, so no names for the hosts file");
            return;
        };
        if !dns::hosts_names_fit(&names) {
            // More names than one request takes: a new try gives the same.
            self.failed(
                codes::HOSTS_TOO_MANY_NAMES,
                "more sites than the hosts file can hold",
                false,
            );
            return;
        }
        let op = HelperOp::HostsSync { names };
        let Ok(request) = serde_json::to_string(&HelperRequest::new(vec![op])) else {
            self.failed(
                "hosts_request_unwritable",
                "cannot write the request",
                false,
            );
            return;
        };
        match send(request).await {
            Ok(answer) => match outcome(&answer) {
                Ok(()) => self.succeeded(),
                Err((code, message)) => self.failed(&code, &message, true),
            },
            Err(code) => self.failed(code, "the helper service's pipe", true),
        }
    }

    fn succeeded(&mut self) {
        if self.last_failure.take().is_some() {
            tracing::info!("the hosts file has the sites' names again");
        }
    }

    /// Logs the failure (a warning when its reason is new), and sets the
    /// next try if `retry`.
    fn failed(&mut self, code: &str, message: &str, retry: bool) {
        if self.last_failure.as_deref() == Some(code) {
            tracing::debug!(code, message, "the hosts file still lacks the sites' names");
        } else {
            tracing::warn!(code, message, "the hosts file lacks the sites' names");
            self.last_failure = Some(code.to_owned());
        }
        if retry {
            self.retry_at = Some(Instant::now() + RETRY);
        }
    }
}

/// The host names of `sites`, sorted, each once, as the helper expects.
// Off Windows only the tests use it.
#[cfg_attr(not(windows), allow(dead_code))]
fn names_of(sites: &[Site]) -> Vec<String> {
    let names: BTreeSet<String> = sites.iter().map(Site::host).collect();
    names.into_iter().collect()
}

/// Whether the service's `answer` says the block was written (or already
/// was): `Ok`, or the code and message of what went wrong.
pub fn outcome(answer: &[u8]) -> Result<(), (String, String)> {
    let unexpected = |what: &str| ("helper_answer_unexpected".to_owned(), what.to_owned());
    let answer: HelperResponse = serde_json::from_slice(answer.trim_ascii())
        .map_err(|_| unexpected("the answer is not the helper's JSON"))?;
    if answer.version != HELPER_PROTOCOL_VERSION {
        return Err(unexpected("the answer is of another version"));
    }
    if let Some(refused) = answer.error {
        return Err((refused.code, refused.message));
    }
    match answer.results.as_slice() {
        [result] if result.op == OpName::HostsSync => match result.status {
            OpStatus::Done | OpStatus::Unchanged => Ok(()),
            OpStatus::Failed => Err((
                result
                    .code
                    .clone()
                    .unwrap_or_else(|| "helper_answer_unexpected".to_owned()),
                result.message.clone().unwrap_or_default(),
            )),
        },
        _ => Err(unexpected("the answer is not one hosts_sync result")),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::fs;
    use std::path::PathBuf;

    use marsa_core::sites::SiteKind;
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use super::*;

    fn site(name: &str) -> Site {
        Site {
            name: name.to_owned(),
            root: PathBuf::from("/code").join(name),
            docroot: PathBuf::from("/code").join(name),
            kind: SiteKind::Static,
            php: None,
            php_source: None,
            secure: true,
            queue: false,
            schedule: false,
        }
    }

    /// A data folder, with the marker of a setup or without.
    fn data(set_up: bool) -> TempDir {
        let dir = TempDir::new().unwrap();
        if set_up {
            fs::write(dir.path().join(MARKER), "").unwrap();
        }
        dir
    }

    fn answer(status: &str) -> Result<Vec<u8>, &'static str> {
        let answer = json!({ "version": 1, "results": [{ "op": "hosts_sync", "status": status }] });
        Ok(answer.to_string().into_bytes())
    }

    /// Runs one sync with a pipe that gives `reply`, and returns what it was
    /// sent, if anything.
    async fn sync_once(
        sync: &mut HostsSync,
        dir: &Path,
        names: Option<Vec<String>>,
        reply: Result<Vec<u8>, &'static str>,
    ) -> Option<Value> {
        let sent = RefCell::new(None);
        sync.sync_with(dir, names, |request| {
            *sent.borrow_mut() = Some(serde_json::from_str(&request).unwrap());
            async move { reply }
        })
        .await;
        sent.into_inner()
    }

    #[test]
    fn the_names_are_sorted_and_each_once() {
        let names = names_of(&[site("shop"), site("blog"), site("shop")]);
        assert_eq!(names, ["blog.test", "shop.test"]);
    }

    /// No setup in this data folder (the tests, a scratch `--data-dir`):
    /// nothing goes to the machine's service.
    #[tokio::test]
    async fn without_a_setup_here_nothing_is_sent() {
        let dir = data(false);
        let mut sync = HostsSync::new();
        let names = Some(vec!["shop.test".to_owned()]);
        assert_eq!(
            sync_once(&mut sync, dir.path(), names, answer("done")).await,
            None
        );
        assert!(sync.retry_at.is_none());
    }

    #[tokio::test]
    async fn the_names_go_out_and_a_success_needs_no_retry() {
        let dir = data(true);
        let mut sync = HostsSync::new();
        let names = Some(vec!["blog.test".to_owned(), "shop.test".to_owned()]);
        let sent = sync_once(&mut sync, dir.path(), names, answer("unchanged")).await;
        assert_eq!(
            sent,
            Some(
                json!({ "version": 1, "ops": [{ "op": "hosts_sync", "names": ["blog.test", "shop.test"] }] })
            )
        );
        assert!(sync.retry_at.is_none() && sync.last_failure.is_none());
    }

    /// Every failure is tried again later: the pipe, the service, the file.
    #[tokio::test]
    async fn a_failure_is_tried_again() {
        let dir = data(true);
        let failed = json!({ "version": 1, "results": [{
            "op": "hosts_sync", "status": "failed", "code": "hosts_write", "message": "read-only"
        }] });
        for reply in [
            Err("helper_not_running"),
            Err("helper_busy"),
            Ok(failed.to_string().into_bytes()),
        ] {
            let mut sync = HostsSync::new();
            let names = Some(Vec::new());
            assert!(
                sync_once(&mut sync, dir.path(), names, reply)
                    .await
                    .is_some()
            );
            assert!(sync.retry_at.is_some());
            assert!(sync.last_failure.is_some());
        }
    }

    /// The site list could not be built: nothing is sent, so the block is
    /// never emptied by mistake, and nothing to retry.
    #[tokio::test]
    async fn an_unknown_site_list_sends_nothing() {
        let dir = data(true);
        let mut sync = HostsSync::new();
        assert_eq!(
            sync_once(&mut sync, dir.path(), None, answer("done")).await,
            None
        );
        assert!(sync.retry_at.is_none());
    }

    /// More names than one request takes: a warning, no send, no retry.
    #[tokio::test]
    async fn too_many_names_are_not_sent() {
        let dir = data(true);
        let mut sync = HostsSync::new();
        let names: Vec<String> = (0..1001).map(|i| format!("s{i}.test")).collect();
        assert_eq!(
            sync_once(&mut sync, dir.path(), Some(names), answer("done")).await,
            None
        );
        assert_eq!(sync.last_failure.as_deref(), Some("hosts_too_many_names"));
        assert!(sync.retry_at.is_none());
    }

    /// A success after failures clears the retry and the last reason.
    #[tokio::test]
    async fn a_success_after_a_failure_clears_it() {
        let dir = data(true);
        let mut sync = HostsSync::new();
        sync_once(&mut sync, dir.path(), Some(Vec::new()), Err("helper_busy")).await;
        sync_once(&mut sync, dir.path(), Some(Vec::new()), answer("done")).await;
        assert!(sync.retry_at.is_none() && sync.last_failure.is_none());
    }

    #[test]
    fn the_answer_says_whether_the_block_was_written() {
        let check = |value: Value| outcome(value.to_string().as_bytes()).map_err(|(code, _)| code);
        let one = |status: &str| json!({ "version": 1, "results": [{ "op": "hosts_sync", "status": status }] });
        assert_eq!(check(one("done")), Ok(()));
        assert_eq!(check(one("unchanged")), Ok(()));
        let refused =
            json!({ "version": 1, "error": { "code": "client_not_allowed", "message": "no" } });
        assert_eq!(check(refused), Err("client_not_allowed".to_owned()));
        let no_code =
            json!({ "version": 1, "results": [{ "op": "hosts_sync", "status": "failed" }] });
        assert_eq!(check(no_code), Err("helper_answer_unexpected".to_owned()));
        let other_version = json!({ "version": 2, "results": [] });
        assert_eq!(
            check(other_version),
            Err("helper_answer_unexpected".to_owned())
        );
        assert_eq!(
            check(json!({ "version": 1, "results": [] })),
            Err("helper_answer_unexpected".to_owned())
        );
        assert!(outcome(b"not json").is_err());
    }
}
