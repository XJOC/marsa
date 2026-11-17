//! Windows: the names of the sites in the hosts file, after each sites
//! pass (ADR 0018, task M4-19g2).
//!
//! The hosts file has no wildcards, so each site's `<name>.test` needs its
//! own line. The helper service writes Marsa's block of the file; the daemon
//! sends it the names through the pipe, with no prompt, after each pass of
//! [`sites_sync`](super::sites_sync). Nothing is sent while the names stay
//! the same as the last ones the service wrote.
//!
//! Before the first `marsa setup` there is no service: that is quiet, and
//! the next pass tries again. Any other failure is a warning in the log,
//! and the next pass tries again too. macOS and Linux send nothing: their
//! DNS covers every `.test` name.

use marsa_core::dns::{DnsStrategy, HostsFile};
use marsa_core::helper::{HelperOp, HelperRequest, HelperResponse, OpStatus};
use marsa_core::sites::Site;

/// The names the service wrote last, if it did.
#[derive(Debug, Default)]
pub struct HostsSync {
    written: Option<Vec<String>>,
}

impl HostsSync {
    pub fn new() -> Self {
        Self::default()
    }

    /// The names of `sites`, and the request that writes them, unless the
    /// service wrote these names already.
    pub fn request(&self, sites: &[Site]) -> Option<(Vec<String>, String)> {
        let Some(HelperOp::HostsSync { names }) = HostsFile.on_sites_changed(sites) else {
            return None;
        };
        if self.written.as_ref() == Some(&names) {
            return None;
        }
        let op = HelperOp::HostsSync {
            names: names.clone(),
        };
        let request = serde_json::to_string(&HelperRequest::new(vec![op])).ok()?;
        Some((names, request))
    }

    /// The service wrote `names`.
    pub fn written(&mut self, names: Vec<String>) {
        self.written = Some(names);
    }

    /// After each pass: on Windows, the names to the service. Nothing
    /// elsewhere.
    pub async fn after_pass(&mut self, sites: &[Site]) {
        // `#[cfg]` outside `platform`: the pipe is a Windows object, and only
        // Windows needs the names (its hosts file has no wildcards).
        #[cfg(windows)]
        self.send(sites).await;
        #[cfg(not(windows))]
        let _ = sites;
    }

    #[cfg(windows)]
    async fn send(&mut self, sites: &[Site]) {
        use marsa_core::platform::windows::helper_pipe::{self, PipeError};
        /// A `hosts_sync` takes a moment; a service busy with a slow
        /// `certutil` is tried again at the next pass.
        const LIMIT: std::time::Duration = std::time::Duration::from_secs(60);
        let Some((names, request)) = self.request(sites) else {
            return;
        };
        match helper_pipe::send(request.as_bytes(), LIMIT).await {
            Ok(answer) => match outcome(&answer) {
                Ok(()) => {
                    tracing::info!(names = names.len(), "the hosts file has the sites' names");
                    self.written(names);
                }
                Err(code) => {
                    tracing::warn!(%code, "the helper service did not write the hosts file");
                }
            },
            Err(PipeError::NotRunning) => {
                tracing::debug!("no helper service yet; \"marsa setup\" installs it");
            }
            Err(err) => {
                tracing::warn!(
                    code = err.code(),
                    error = %err,
                    "cannot send the sites' names to the helper service"
                );
            }
        }
    }
}

/// Whether the service's `answer` says the block was written (or already
/// was): `Ok`, or the code of what went wrong.
pub fn outcome(answer: &[u8]) -> Result<(), String> {
    let answer: HelperResponse = serde_json::from_slice(answer.trim_ascii())
        .map_err(|_| "helper_answer_unreadable".to_owned())?;
    if let Some(refused) = answer.error {
        return Err(refused.code);
    }
    match answer.results.as_slice() {
        [result] if result.op == "hosts_sync" => match result.status {
            OpStatus::Done | OpStatus::Unchanged => Ok(()),
            OpStatus::Failed => Err(result.code.clone().unwrap_or_default()),
        },
        _ => Err("helper_answer_unexpected".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use marsa_core::sites::SiteKind;
    use serde_json::{Value, json};

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
        }
    }

    #[test]
    fn the_request_holds_every_name_once_and_sorted() {
        let sync = HostsSync::new();
        let (names, request) = sync
            .request(&[site("shop"), site("blog"), site("shop")])
            .unwrap();
        assert_eq!(names, ["blog.test", "shop.test"]);
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(
            request,
            json!({ "version": 1, "ops": [{ "op": "hosts_sync", "names": ["blog.test", "shop.test"] }] })
        );
    }

    #[test]
    fn the_same_names_again_send_nothing() {
        let mut sync = HostsSync::new();
        let (names, _) = sync.request(&[site("shop")]).unwrap();
        // Not written yet: the next pass sends them again.
        assert!(sync.request(&[site("shop")]).is_some());
        sync.written(names);
        assert!(sync.request(&[site("shop")]).is_none());
        // A change, or no site at all, goes out.
        assert!(sync.request(&[site("shop"), site("blog")]).is_some());
        let (none, _) = sync.request(&[]).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn the_answer_says_whether_the_block_was_written() {
        let answer = |value: Value| outcome(value.to_string().as_bytes());
        let one = |status: &str| json!({ "version": 1, "results": [{ "op": "hosts_sync", "status": status }] });
        assert_eq!(answer(one("done")), Ok(()));
        assert_eq!(answer(one("unchanged")), Ok(()));
        let failed = json!({ "version": 1, "results": [{
            "op": "hosts_sync", "status": "failed", "code": "hosts_write", "message": "read-only"
        }] });
        assert_eq!(answer(failed), Err("hosts_write".to_owned()));
        let refused =
            json!({ "version": 1, "error": { "code": "client_not_allowed", "message": "no" } });
        assert_eq!(answer(refused), Err("client_not_allowed".to_owned()));
        assert_eq!(
            answer(json!({ "version": 1, "results": [] })),
            Err("helper_answer_unexpected".to_owned())
        );
        assert_eq!(
            outcome(b"not json"),
            Err("helper_answer_unreadable".to_owned())
        );
    }
}
