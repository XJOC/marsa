//! The mail reader: one task for each session that asks the daemon for
//! Mailpit's mail, one call at a time, so an older answer never lands after
//! a newer one. No event carries mail, so it reads when the window asks (the
//! Mail screen opened, "Delete all…"), when Mailpit starts or stops, and
//! every [`POLL`] while Mailpit runs. An answer the same as the last one it
//! sent is not sent: the window neither changes nor draws again (rules.md
//! 10).

use std::time::Duration;

use marsa_core::client::DaemonClient;
use marsa_core::codes::MAIL_NOT_RUNNING;
use marsa_core::types::{MailList, ServiceState, ServiceStatus};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use tokio::task::JoinHandle;
use tokio::time::{Interval, MissedTickBehavior};

use super::{Window, notice};
use crate::model::{MAILPIT, Removal, Update};

/// How often the list is read while Mailpit runs.
const POLL: Duration = Duration::from_secs(10);

/// What the reader is asked to do.
#[derive(Debug)]
pub(super) enum Job {
    /// Read the list.
    Read,
    /// Read the text of the mail with this id.
    Text(String),
    /// Delete every mail.
    Clear,
    /// Mailpit's service changed: whether it runs now.
    Running(bool),
}

/// The session's end of the reader. Dropping it stops the task, so an
/// answer of a lost session never reaches the window.
pub(super) struct Mail {
    jobs: UnboundedSender<Job>,
    task: JoinHandle<()>,
}

impl Mail {
    /// Starts the reader. It reads nothing until it is asked, or told that
    /// Mailpit runs.
    pub(super) fn start(client: &DaemonClient, window: &Window) -> Self {
        let (jobs, received) = unbounded_channel();
        let task = tokio::spawn(work(client.clone(), window.clone(), received));
        Self { jobs, task }
    }

    pub(super) fn send(&self, job: Job) {
        // The task ends only when the window is gone, and the session then
        // ends too: nothing is left to tell.
        let _ = self.jobs.send(job);
    }

    /// A line of the services table, or a change of one: whether Mailpit
    /// runs.
    pub(super) fn follow(&self, service: &ServiceStatus) {
        if service.id == MAILPIT {
            self.send(Job::Running(service.state == ServiceState::Running));
        }
    }
}

impl Drop for Mail {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// What the reader last told the window about the list.
#[derive(Debug, Default)]
struct Feed {
    /// The list last sent, or the code of the failure last sent.
    last: Option<Result<MailList, String>>,
    running: bool,
}

async fn work(client: DaemonClient, window: Window, mut jobs: UnboundedReceiver<Job>) {
    let mut feed = Feed::default();
    let mut poll = tokio::time::interval(POLL);
    // After a slow answer, the next read waits a whole period again.
    poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        let open = tokio::select! {
            job = jobs.recv() => match job {
                Some(job) => feed.run(&client, &window, job, &mut poll).await,
                None => false,
            },
            _ = poll.tick(), if feed.running => feed.read(&client, &window).await,
        };
        if !open {
            return;
        }
    }
}

impl Feed {
    /// Does `job`. `false` once the window is gone.
    async fn run(
        &mut self,
        client: &DaemonClient,
        window: &Window,
        job: Job,
        poll: &mut Interval,
    ) -> bool {
        match job {
            Job::Read => self.read(client, window).await,
            Job::Text(id) => match client.mail_text(&id).await {
                Ok(text) => window.send(Update::MailText(text)),
                Err(err) if err.code() == MAIL_NOT_RUNNING => {
                    self.answer(window, Err(err.code().to_owned()))
                }
                Err(err) => window.send(notice("cannot read a mail", &err)),
            },
            Job::Clear => match client.clear_mail().await {
                Ok(list) => {
                    tracing::info!("every mail deleted");
                    window.send(Update::Removed(Removal::Mail)) && self.answer(window, Ok(list))
                }
                Err(err) => {
                    tracing::warn!(code = err.code(), error = %err, "cannot delete the mail");
                    let code = err.code().to_owned();
                    // The window shows Mailpit stopped from this answer: a
                    // later read that says so is nothing new.
                    if code == MAIL_NOT_RUNNING {
                        self.last = Some(Err(code.clone()));
                    }
                    window.send(Update::NotRemoved {
                        removal: Removal::Mail,
                        code,
                        in_use: None,
                    })
                }
            },
            Job::Running(running) => {
                if running == self.running {
                    return true;
                }
                self.running = running;
                // Read now, and the next time a whole period later.
                poll.reset();
                self.read(client, window).await
            }
        }
    }

    /// Reads the list, and sends it if it changed.
    async fn read(&mut self, client: &DaemonClient, window: &Window) -> bool {
        let read = client.mail().await.map_err(|err| {
            // Mailpit stopped is a state; a fault is logged once, when it
            // starts.
            let known = matches!(&self.last, Some(Err(code)) if code == err.code());
            if !known && err.code() != MAIL_NOT_RUNNING {
                tracing::warn!(code = err.code(), error = %err, "cannot read the mail");
            }
            err.code().to_owned()
        });
        self.answer(window, read)
    }

    /// Sends `read` unless it is what the window has from the last one.
    /// `false` once the window is gone.
    fn answer(&mut self, window: &Window, read: Result<MailList, String>) -> bool {
        if self.last.as_ref() == Some(&read) {
            return true;
        }
        let update = match &read {
            Ok(list) => Update::Mail(list.clone()),
            Err(code) => Update::MailFailed(code.clone()),
        };
        self.last = Some(read);
        window.send(update)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, OnceLock, mpsc};

    use marsa_core::types::MailSummary;

    use super::*;

    fn list(ids: &[&str]) -> MailList {
        let mail = |id: &&str| MailSummary {
            id: (*id).to_owned(),
            from: "app@example.test".into(),
            to: Vec::new(),
            subject: "Hello".into(),
            received: 0,
            size: 0,
            read: false,
            snippet: String::new(),
            attachments: 0,
        };
        MailList {
            total: ids.len() as u64,
            unread: ids.len() as u64,
            messages: ids.iter().map(mail).collect(),
        }
    }

    #[test]
    fn an_answer_like_the_last_one_sends_nothing() {
        let (updates, received) = mpsc::channel();
        let window = Window {
            updates,
            repaint: Arc::new(OnceLock::new()),
        };
        let mut feed = Feed::default();
        let sent = |received: &mpsc::Receiver<Update>| received.try_iter().count();
        assert!(feed.answer(&window, Ok(list(&["a1"]))));
        assert_eq!(sent(&received), 1);
        // Every 10 s the same list: no update, so no frame.
        assert!(feed.answer(&window, Ok(list(&["a1"]))));
        assert!(feed.answer(&window, Ok(list(&["a1"]))));
        assert_eq!(sent(&received), 0);
        assert!(feed.answer(&window, Ok(list(&["b2", "a1"]))));
        assert_eq!(sent(&received), 1);
        // A failure is sent once too, and the list after it again.
        let stopped = || Err(MAIL_NOT_RUNNING.to_owned());
        assert!(feed.answer(&window, stopped()));
        assert!(feed.answer(&window, stopped()));
        assert_eq!(sent(&received), 1);
        assert!(feed.answer(&window, Ok(list(&["b2", "a1"]))));
        let update = received.try_recv().expect("the list again");
        assert!(matches!(update, Update::Mail(ref list) if list.messages.len() == 2));
        // The window is gone.
        drop(received);
        assert!(!feed.answer(&window, Ok(list(&[]))));
    }
}
