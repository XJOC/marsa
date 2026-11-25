//! The reader of a Laravel site's own log: while Logs shows it, one task
//! follows `GET /v1/sites/{name}/log?follow=true` and passes the lines on.
//! One reader at a time: following another site stops the one before, and
//! so does leaving the screen. It never tries again by itself (rules.md 10):
//! the window asks again when the screen opens again, or after a reconnect.
//! The lines are never logged: they may hold the user's data.

use std::future::Future;
use std::time::Duration;

use marsa_core::client::{ClientError, DaemonClient, EventStream, StreamItem};
use marsa_core::types::SiteLogLine;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::Window;
use crate::model::Update;

/// How long lines that come together wait for the ones after them, so the
/// first read (up to thousands of lines) and a busy log wake the window a
/// few times a second, not once a line.
const GATHER: Duration = Duration::from_millis(50);

/// The most lines in one update.
const BATCH: usize = 1000;

/// The session's end of the reader. Dropping it stops the reader, so a line
/// of a lost session never reaches the window.
#[derive(Debug, Default)]
pub(super) struct SiteLog {
    task: Option<JoinHandle<()>>,
}

impl SiteLog {
    /// Follows the log of `site`, in place of any log followed before.
    pub(super) fn follow(&mut self, client: &DaemonClient, window: &Window, site: String) {
        self.start(read(client.clone(), window.clone(), site));
    }

    /// Runs `reader` as the one reader, after stopping the one before.
    fn start(&mut self, reader: impl Future<Output = ()> + Send + 'static) {
        self.stop();
        self.task = Some(tokio::spawn(reader));
    }

    /// Stops the reader, if one runs. The daemon closes its end when the
    /// stream goes.
    pub(super) fn stop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

impl Drop for SiteLog {
    fn drop(&mut self) {
        self.stop();
    }
}

async fn read(client: DaemonClient, window: Window, site: String) {
    let mut stream = match client.follow_site_log(&site).await {
        Ok(stream) => stream,
        Err(err) => {
            failed(&window, site, &err);
            return;
        }
    };
    loop {
        let mut lines = Vec::new();
        let open = gather(&mut stream, &mut lines).await;
        if !lines.is_empty() {
            let update = Update::SiteLog {
                site: site.clone(),
                lines,
            };
            if !window.send(update) {
                return;
            }
        }
        if !open {
            break;
        }
    }
    // The daemon ends the stream when it stops (the event stream tells the
    // window that), or when the log is no longer safe to read. One plain
    // read says which.
    match client.site_log(&site).await {
        Err(err) => failed(&window, site, &err),
        Ok(_) => tracing::warn!(%site, "the site log stream ended"),
    }
}

/// Waits for the next line, then takes the ones that come within
/// [`GATHER`], at most [`BATCH`]. `false` once the stream has ended.
async fn gather(stream: &mut EventStream<SiteLogLine>, lines: &mut Vec<SiteLogLine>) -> bool {
    let mut until: Option<Instant> = None;
    while lines.len() < BATCH {
        let next = match until {
            None => stream.next().await,
            // `next` keeps what it read when it is cut short, so nothing is
            // lost at the deadline.
            Some(until) => match tokio::time::timeout_at(until, stream.next()).await {
                Ok(next) => next,
                Err(_) => return true,
            },
        };
        match next {
            Some(Ok(StreamItem::Item(line))) => {
                lines.push(line);
                until.get_or_insert_with(|| Instant::now() + GATHER);
            }
            // The daemon's stream waits for a slow reader instead.
            Some(Ok(StreamItem::Lagged(count))) => {
                tracing::warn!(count, "the daemon dropped lines of a site log");
            }
            Some(Err(err)) => {
                tracing::warn!(code = err.code(), error = %err, "a bad line of a site log");
            }
            None => return false,
        }
    }
    true
}

fn failed(window: &Window, site: String, err: &ClientError) {
    tracing::warn!(%site, code = err.code(), error = %err, "cannot follow the site log");
    let code = err.code().to_owned();
    window.send(Update::SiteLogFailed { site, code });
}

#[cfg(test)]
mod tests {
    use tokio::sync::oneshot;

    use super::*;

    /// A reader that runs until it is stopped, and tells when it is gone.
    fn reader() -> (
        impl Future<Output = ()> + Send + 'static,
        oneshot::Receiver<()>,
    ) {
        let (held, gone) = oneshot::channel::<()>();
        let reader = async move {
            // Dropped with the task: the receiver then hears that it ended.
            let _held = held;
            std::future::pending::<()>().await;
        };
        (reader, gone)
    }

    #[test]
    fn one_reader_runs_at_a_time_and_stop_and_drop_end_it() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("a runtime");
        runtime.block_on(async {
            let mut site_log = SiteLog::default();
            let (first, first_gone) = reader();
            let (second, mut second_gone) = reader();
            site_log.start(first);
            site_log.start(second);
            // Following another log stopped the first reader.
            assert!(first_gone.await.is_err());
            tokio::task::yield_now().await;
            assert_eq!(
                second_gone.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            );
            // Leaving the screen stops the second.
            site_log.stop();
            assert!(second_gone.await.is_err());
            // A stop with no reader does nothing.
            site_log.stop();
            // The session's end stops the reader too.
            let (third, third_gone) = reader();
            site_log.start(third);
            drop(site_log);
            assert!(third_gone.await.is_err());
        });
    }
}
