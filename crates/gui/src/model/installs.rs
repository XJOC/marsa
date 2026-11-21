//! Installs: the jobs the window sends, the queue, and how each install is shown.

use super::*;

/// The most installs kept for the Runtimes section. The daemon runs one at a
/// time, so a few are plenty.
const MAX_INSTALLS: usize = 4;

/// The window's own code: the daemon stopped before an install ended, as
/// `marsa install` says it.
pub const INSTALL_INTERRUPTED: &str = "install_interrupted";

/// One install the window asks the daemon for:
/// `POST /v1/runtimes/{tool}/install`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallJob {
    pub runtime: Runtime,
    /// `8.4`, with the rules of a `.php-version` file. `None`: the newest
    /// stable version.
    pub version: Option<String>,
}

/// What the Runtimes section shows of one install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallView {
    /// From the window's job, or from the daemon's `selected` step for an
    /// install started elsewhere (`marsa install` in a terminal).
    pub runtime: Option<Runtime>,
    /// What was asked for (`8.4`), then the version the daemon picked
    /// (`8.4.23`).
    pub version: Option<String>,
    pub step: InstallStep,
}

/// How far an install is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallStep {
    /// Sent; the daemon has reported no step yet.
    Waiting,
    /// The signed version list was checked; a version is being picked.
    ListChecked,
    /// `done` of `total` bytes of the file `artifact`.
    Downloading {
        artifact: String,
        done: u64,
        total: u64,
    },
    /// The file passed its checksum and is being unpacked.
    Unpacking { artifact: String },
    /// The new program runs its own check (`php -v`).
    Checking,
    /// The version folder is in place; the daemon sets the default and
    /// the sites.
    Finishing,
    /// `already`: the version was there, and nothing was downloaded.
    Done { already: bool },
    /// The daemon's code and the facts its text may show.
    Failed(InstallFailure),
}

impl InstallStep {
    /// Whether the install has ended, well or not.
    pub fn is_over(&self) -> bool {
        matches!(self, InstallStep::Done { .. } | InstallStep::Failed(_))
    }

    /// The value of the progress bar, from 0 to 1: the share of the current
    /// file that has arrived, and a full bar once it is all there.
    pub fn fraction(&self) -> f32 {
        match self {
            InstallStep::Downloading { done, total, .. } if *total > 0 => {
                // Display only: `f64` holds any file size closely enough.
                (*done as f64 / *total as f64).clamp(0.0, 1.0) as f32
            }
            InstallStep::Unpacking { .. }
            | InstallStep::Checking
            | InstallStep::Finishing
            | InstallStep::Done { .. } => 1.0,
            InstallStep::Waiting
            | InstallStep::ListChecked
            | InstallStep::Downloading { .. }
            | InstallStep::Failed(_) => 0.0,
        }
    }
}

impl InstallView {
    fn waiting(job: &InstallJob) -> Self {
        Self {
            runtime: Some(job.runtime),
            version: job.version.clone(),
            step: InstallStep::Waiting,
        }
    }

    /// One `install` event of this install.
    fn apply(&mut self, update: InstallUpdate) {
        self.step = match update {
            InstallUpdate::Running(event) => match event {
                InstallEvent::ManifestVerified { .. } => InstallStep::ListChecked,
                InstallEvent::Selected { runtime, version } => {
                    self.runtime = Some(runtime);
                    self.version = Some(version);
                    return;
                }
                InstallEvent::Downloading {
                    artifact,
                    done,
                    total,
                } => InstallStep::Downloading {
                    artifact,
                    done,
                    total,
                },
                InstallEvent::Verified { artifact } | InstallEvent::Extracting { artifact } => {
                    InstallStep::Unpacking { artifact }
                }
                InstallEvent::Checking => InstallStep::Checking,
                InstallEvent::Installed { .. } => InstallStep::Finishing,
            },
            InstallUpdate::Done(done) => {
                self.runtime = Some(done.runtime);
                self.version = Some(done.version);
                InstallStep::Done {
                    already: done.already,
                }
            }
            InstallUpdate::Failed(failure) => InstallStep::Failed(failure),
        };
    }
}

/// One install the daemon reported, or one the window asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Install {
    /// The daemon's id. `None` until the daemon accepts the window's job, and
    /// once its session is gone: a new daemon counts from 1 again.
    pub(super) id: Option<u64>,
    /// Started by this window: its end starts the next job of the queue.
    pub(super) ours: bool,
    pub(super) view: InstallView,
    /// Its result was shown in a notice that has since closed.
    pub(super) noticed: bool,
}

impl Install {
    fn runs_for_us(&self) -> bool {
        self.ours && !self.view.step.is_over()
    }
}

/// The version to ask for from a version field: `None` (the newest stable
/// version) when it is empty, else the text without the spaces around it.
///
/// # Errors
///
/// `invalid_version` or `unsupported_alias`: the rules the daemon's
/// installer uses, so no request is sent that it would refuse.
pub fn install_version(text: &str) -> Result<Option<String>, &'static str> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    resolver::best_match(text, &[]).map_err(|err| err.code())?;
    Ok(Some(text.to_owned()))
}

/// A failure with only its code, for an install that never reached the
/// daemon or whose end was lost.
pub(super) fn failure(code: String) -> InstallFailure {
    InstallFailure {
        code,
        ..InstallFailure::default()
    }
}

impl Model {
    /// The install the Runtimes section shows: one this window started that
    /// has not ended, else the newest one, whoever started it.
    pub fn install_view(&self) -> Option<&InstallView> {
        self.install_slot().map(|index| &self.installs[index].view)
    }

    /// Where [`Model::install_view`] is in the list.
    pub(super) fn install_slot(&self) -> Option<usize> {
        self.installs
            .iter()
            .rposition(Install::runs_for_us)
            .or_else(|| self.installs.len().checked_sub(1))
    }

    /// How the install shown ended, until the notice about it is closed. The
    /// bar says it once; Home and the first-run panel keep their own words
    /// about a failure beside the button that fixes it.
    pub fn install_result(&self) -> Option<&InstallView> {
        let install = &self.installs[self.install_slot()?];
        (install.view.step.is_over() && !install.noticed).then_some(&install.view)
    }

    /// Whether a job of this window waits or runs. The install buttons wait
    /// for it: the daemon runs one install at a time.
    pub fn installing(&self) -> bool {
        !self.session.queue.is_empty() || self.installs.iter().any(Install::runs_for_us)
    }

    /// The user pressed Install for `runtime`: queues the version in its
    /// field, or says why the field is wrong. `false` if nothing was queued.
    pub fn request_install(&mut self, runtime: Runtime) -> bool {
        if !self.is_connected() || self.installing() {
            return false;
        }
        match install_version(self.install_text(runtime)) {
            Ok(version) => {
                let queued = self.queue_install(InstallJob { runtime, version });
                if queued {
                    // The text is used, and the dialog has done its work.
                    self.install_input.remove(runtime.dir_name());
                    self.session.install_dialog = None;
                }
                queued
            }
            Err(code) => {
                self.session
                    .notices
                    .set_code(NoticeSource::Runtimes, code.to_owned());
                false
            }
        }
    }

    /// What the version field of `runtime`'s install dialog holds. Empty: the
    /// newest stable version.
    pub fn install_text(&self, runtime: Runtime) -> &str {
        self.install_input
            .get(runtime.dir_name())
            .map_or("", String::as_str)
    }

    /// The user typed `text` in the version field of `runtime`.
    pub fn set_install_text(&mut self, runtime: Runtime, text: String) {
        if text.is_empty() {
            self.install_input.remove(runtime.dir_name());
        } else {
            self.install_input.insert(runtime.dir_name(), text);
        }
    }

    /// Whether an install can be asked for now: connected, and no install or
    /// uninstall of this window waits or runs (the daemon runs one at a time).
    pub fn can_install(&self) -> bool {
        self.is_connected() && !self.installing() && !self.uninstalling()
    }

    /// "Install a version…": opens the install dialog of `runtime`. `false`
    /// when an install cannot be asked for now.
    pub fn open_install(&mut self, runtime: Runtime) -> bool {
        if !self.can_install() {
            return false;
        }
        // The notice was about the last action.
        self.session.notices.clear(NoticeSource::Runtimes);
        self.session.install_dialog = Some(runtime);
        true
    }

    /// "Cancel" or Escape in the install dialog. The typed text stays for the
    /// next time.
    pub fn close_install(&mut self) {
        self.session.install_dialog = None;
        self.session.notices.clear(NoticeSource::Runtimes);
    }

    /// Queues `job`, as the Install button does once its field is right.
    /// `false` if the window has no daemon, or one of its jobs waits or runs.
    pub fn queue_install(&mut self, job: InstallJob) -> bool {
        if !self.is_connected() || self.installing() {
            return false;
        }
        self.session.notices.clear(NoticeSource::Runtimes);
        self.session.queue.push_back(job);
        true
    }

    /// The next job to send, once no job of this window runs. It shows as
    /// waiting until the daemon answers. The window calls this after each
    /// change, so one call sends at most one job.
    pub fn next_install(&mut self) -> Option<InstallJob> {
        // The daemon runs one install or uninstall at a time.
        if !self.is_connected()
            || self.uninstalling()
            || self.installs.iter().any(Install::runs_for_us)
        {
            return None;
        }
        let job = self.session.queue.pop_front()?;
        self.push_install(Install {
            id: None,
            ours: true,
            view: InstallView::waiting(&job),
            noticed: false,
        });
        Some(job)
    }

    pub(super) fn push_install(&mut self, install: Install) {
        if self.installs.len() >= MAX_INSTALLS {
            let oldest_over = self
                .installs
                .iter()
                .position(|install| install.view.step.is_over());
            self.installs.remove(oldest_over.unwrap_or(0));
        }
        self.installs.push(install);
    }

    /// The window's job that waits for the daemon's answer.
    pub(super) fn unanswered(&self, job: &InstallJob) -> Option<usize> {
        self.installs.iter().position(|install| {
            install.runs_for_us()
                && install.id.is_none()
                && install.view.runtime == Some(job.runtime)
        })
    }

    /// The daemon accepted `job` as install `id`.
    pub(super) fn accepted(&mut self, job: &InstallJob, id: u64) {
        let Some(index) = self.unanswered(job) else {
            // From a session that has ended.
            return;
        };
        let Some(early) = self.installs.iter().position(|i| i.id == Some(id)) else {
            self.installs[index].id = Some(id);
            return;
        };
        // Its events came before the answer, as an install of no known
        // owner: they are this job's.
        let mut install = self.installs.remove(early);
        let index = if early < index { index - 1 } else { index };
        install.ours = true;
        install.view.runtime.get_or_insert(job.runtime);
        if install.view.version.is_none() {
            install.view.version.clone_from(&job.version);
        }
        let ended = match &install.view.step {
            InstallStep::Done { .. } => Some(true),
            InstallStep::Failed(_) => Some(false),
            _ => None,
        };
        self.installs[index] = install;
        if let Some(success) = ended {
            self.our_install_ended(success);
        }
    }

    /// The daemon refused `job`, or could not be reached.
    pub(super) fn refused(&mut self, job: &InstallJob, code: String) {
        let Some(index) = self.unanswered(job) else {
            return;
        };
        self.installs[index].view.step = InstallStep::Failed(failure(code));
        self.our_install_ended(false);
    }

    /// One `install` event, of any install: the window shows those started
    /// in a terminal too.
    pub(super) fn install_event(&mut self, progress: InstallProgress) {
        let index = match self.installs.iter().position(|i| i.id == Some(progress.id)) {
            Some(index) => index,
            None => {
                self.push_install(Install {
                    id: Some(progress.id),
                    ours: false,
                    view: InstallView {
                        runtime: None,
                        version: None,
                        step: InstallStep::Waiting,
                    },
                    noticed: false,
                });
                self.installs.len() - 1
            }
        };
        let install = &mut self.installs[index];
        let ran = install.runs_for_us();
        install.view.apply(progress.update);
        if ran && install.view.step.is_over() {
            let success = matches!(install.view.step, InstallStep::Done { .. });
            self.our_install_ended(success);
        }
    }

    /// A job of this window ended. A failure drops the jobs after it: the
    /// user sees why, and starts again.
    pub(super) fn our_install_ended(&mut self, success: bool) {
        if !success {
            self.session.queue.clear();
        }
        if self.session.first_run == Some(FirstRun::Installing) && self.session.queue.is_empty() {
            self.session.first_run = if success {
                Some(FirstRun::Setup)
            } else {
                // The new runtimes list closes it if PHP or Caddy came.
                first_run_needed(&self.session.runtimes).then_some(FirstRun::Offer)
            };
        }
    }

    /// No event comes from a lost session: an install that runs shows as
    /// interrupted, and no id is kept, since a new daemon counts from 1
    /// again. Only the install shown stays.
    pub(super) fn forget_installs(&mut self) {
        self.session.queue.clear();
        let shown = self
            .installs
            .iter()
            .rposition(Install::runs_for_us)
            .or_else(|| self.installs.len().checked_sub(1));
        self.installs = shown
            .map(|index| self.installs.swap_remove(index))
            .into_iter()
            .collect();
        for install in &mut self.installs {
            if !install.view.step.is_over() {
                install.view.step = InstallStep::Failed(failure(INSTALL_INTERRUPTED.to_owned()));
            }
            install.id = None;
            install.ours = false;
        }
    }
}
