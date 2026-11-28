//! The dumps reader: one task for each session that asks the daemon for the
//! `dump()` calls it caught, one call at a time, so an older answer never
//! lands after a newer one. It reads the whole list when the session starts,
//! and after that only what is new (`?after=<the newest it sent>`): when the
//! screen opens and at each `dumps_changed` event. It never polls, and an
//! answer with nothing new that leaves the daemon's counts as they were is
//! not sent, so a quiet daemon costs no frame (rules.md 10).

use marsa_core::client::DaemonClient;
use marsa_core::types::{DumpList, DumpsStatus};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;

use super::{Window, notice};
use crate::model::Update;

/// What the reader is asked to do.
#[derive(Debug)]
pub(super) enum Job {
    /// Read what is new since the last answer.
    Read,
    /// Read everything: the session started.
    Reload,
    /// Forget every dump.
    Clear,
    /// Switch the capture, then read.
    Capture(bool),
}

/// The session's end of the reader. Dropping it stops the task, so an
/// answer of a lost session never reaches the window.
pub(super) struct Dumps {
    jobs: UnboundedSender<Job>,
    task: JoinHandle<()>,
}

impl Dumps {
    /// Starts the reader, and has it read everything.
    pub(super) fn start(client: &DaemonClient, window: &Window) -> Self {
        let (jobs, received) = unbounded_channel();
        let task = tokio::spawn(work(client.clone(), window.clone(), received));
        let dumps = Self { jobs, task };
        dumps.send(Job::Reload);
        dumps
    }

    pub(super) fn send(&self, job: Job) {
        // The task ends only when the window is gone, and the session then
        // ends too: nothing is left to tell.
        let _ = self.jobs.send(job);
    }
}

impl Drop for Dumps {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What the reader last told the window.
#[derive(Debug, Default)]
struct Feed {
    /// The newest id sent. `None`: nothing sent, so the next read is whole.
    last_id: Option<u64>,
    /// The status and the count of the last answer sent.
    last: Option<(DumpsStatus, u64)>,
}

async fn work(client: DaemonClient, window: Window, mut jobs: UnboundedReceiver<Job>) {
    let mut feed = Feed::default();
    while let Some(job) = jobs.recv().await {
        if !feed.run(&client, &window, job).await {
            return;
        }
    }
}

impl Feed {
    /// Does `job`. `false` once the window is gone.
    async fn run(&mut self, client: &DaemonClient, window: &Window, job: Job) -> bool {
        match job {
            Job::Read => self.read(client, window).await,
            Job::Reload => {
                self.last_id = None;
                self.last = None;
                self.read(client, window).await
            }
            Job::Clear => match client.clear_dumps().await {
                Ok(list) => {
                    tracing::info!("every dump deleted");
                    self.answer(window, list, true)
                }
                Err(err) => window.send(notice("cannot delete the dumps", &err)),
            },
            Job::Capture(enabled) => match client.set_dumps_capture(enabled).await {
                Ok(_) => {
                    tracing::info!(enabled, "the dump capture changed");
                    self.read(client, window).await
                }
                Err(err) => window.send(notice("cannot switch the dump capture", &err)),
            },
        }
    }

    /// Reads what is new (everything, the first time), and sends it unless
    /// it changes nothing.
    async fn read(&mut self, client: &DaemonClient, window: &Window) -> bool {
        match client.dumps(self.last_id).await {
            Ok(list) => {
                let whole = self.last_id.is_none();
                self.answer(window, list, whole)
            }
            Err(err) => {
                tracing::warn!(code = err.code(), error = %err, "cannot read the dumps");
                window.send(Update::DumpsFailed(err.code().to_owned()))
            }
        }
    }

    /// Sends `list`, unless it brings no dump and leaves the status and the
    /// count of the daemon as the last answer had them. `false` once the
    /// window is gone.
    fn answer(&mut self, window: &Window, list: DumpList, replace: bool) -> bool {
        let state = (list.status, list.count);
        let nothing = list.dumps.is_empty() && !replace && self.last == Some(state);
        self.last_id = Some(
            self.last_id
                .map_or(list.latest_id, |id| id.max(list.latest_id)),
        );
        self.last = Some(state);
        if nothing {
            return true;
        }
        window.send(Update::Dumps { list, replace })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, OnceLock, mpsc};

    use marsa_core::types::Dump;

    use super::*;
    use crate::bridge::Stamped;

    fn dump(id: u64) -> Dump {
        Dump {
            id,
            received_at: 0,
            source_file: None,
            source_line: None,
            uri: None,
            method: None,
            label: None,
            text: "1".into(),
            truncated: false,
        }
    }

    fn list(ids: &[u64], count: u64, status: DumpsStatus) -> DumpList {
        DumpList {
            status,
            latest_id: ids.first().copied().unwrap_or(0),
            count,
            dumps: ids.iter().map(|&id| dump(id)).collect(),
        }
    }

    #[test]
    fn an_answer_with_nothing_new_sends_nothing_and_a_change_of_count_or_status_does() {
        let (updates, received) = mpsc::channel();
        let window = Window {
            updates,
            repaint: Arc::new(OnceLock::new()),
            session: 0,
        };
        let mut feed = Feed::default();
        let sent = |received: &mpsc::Receiver<Stamped<Update>>| received.try_iter().count();
        let listening = DumpsStatus::Listening;
        // The first read is whole.
        assert!(feed.answer(&window, list(&[2, 1], 2, listening), true));
        assert_eq!(sent(&received), 1);
        assert_eq!(feed.last_id, Some(2));
        // Nothing new: no update, so no frame.
        assert!(feed.answer(&window, list(&[], 2, listening), false));
        assert_eq!(sent(&received), 0);
        // Something new.
        assert!(feed.answer(&window, list(&[3], 3, listening), false));
        assert_eq!(sent(&received), 1);
        assert_eq!(feed.last_id, Some(3));
        // Cleared elsewhere: no dump, but the count changed.
        assert!(feed.answer(&window, list(&[], 0, listening), false));
        assert_eq!(sent(&received), 1);
        assert_eq!(feed.last_id, Some(3), "the ids never go back");
        // The port was taken: the status changed.
        assert!(feed.answer(&window, list(&[], 0, DumpsStatus::PortInUse), false));
        assert_eq!(sent(&received), 1);
        // A whole read is always sent.
        assert!(feed.answer(&window, list(&[], 0, DumpsStatus::PortInUse), true));
        assert_eq!(sent(&received), 1);
        // The window is gone.
        drop(received);
        assert!(!feed.answer(&window, list(&[9], 1, listening), false));
    }
}
