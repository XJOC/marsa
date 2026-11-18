//! Measurement mode (task M5-D10): fills the window with made-up data and
//! counts its frames, so the memory and the idle cost of a long list can be
//! measured on a release build with no daemon.
//!
//! `MARSA_GUI_MEASURE=sites=500,idle=120` opens the Sites screen with 500
//! made-up sites, keeps the window idle for 120 seconds after a short settle,
//! then logs one line (the time of the first frame, and how many frames ran
//! while the window was idle) and closes the window. `sites` is 0 and `idle`
//! is 60 when left out; with no site the window opens on Home.
//!
//! It can never touch a real data folder: it starts no bridge and no daemon,
//! and it refuses to start unless the data folder is missing or empty, which
//! a folder Marsa has used never is. Without the variable nothing here runs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    Health, RuntimeList, RuntimeVersions, ServiceState, ServiceStatus, Site, SiteKind, SiteList,
    Source,
};

use crate::model::{Model, Route, Update, WEB_SERVER};

/// The variable that asks for the mode.
const VAR: &str = "MARSA_GUI_MEASURE";
/// The window shows before the idle frames are counted: the first frames
/// after the window opens are not idle.
const SETTLE: Duration = Duration::from_secs(5);
/// The idle time when `idle` is left out.
const IDLE: Duration = Duration::from_secs(60);

/// What to measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// How many made-up sites the window lists.
    pub sites: usize,
    /// How long the window is idle once it has settled.
    pub idle: Duration,
}

/// Why the mode cannot start.
#[derive(Debug, thiserror::Error)]
pub enum MeasureError {
    #[error("MARSA_GUI_MEASURE needs a new, empty data folder, and {} is not", .0.display())]
    NotEmpty(PathBuf),
    #[error("MARSA_GUI_MEASURE is not understood: {0:?} (for example sites=500,idle=120)")]
    Unreadable(String),
}

/// The plan `MARSA_GUI_MEASURE` asks for, if it is set.
///
/// # Errors
///
/// The text is not `sites=<n>,idle=<seconds>`, or `data_dir` holds anything.
pub fn requested(data_dir: &Path) -> Result<Option<Plan>, MeasureError> {
    let text = std::env::var_os(VAR);
    check(
        text.as_deref()
            .map(|text| text.to_string_lossy())
            .as_deref(),
        data_dir,
    )
}

/// [`requested`] for the text of the variable, if it is set.
fn check(text: Option<&str>, data_dir: &Path) -> Result<Option<Plan>, MeasureError> {
    let Some(text) = text else {
        return Ok(None);
    };
    let plan = parse(text).ok_or_else(|| MeasureError::Unreadable(text.to_owned()))?;
    if !is_empty(data_dir) {
        return Err(MeasureError::NotEmpty(data_dir.to_path_buf()));
    }
    Ok(Some(plan))
}

/// `sites=500,idle=120` as a plan.
fn parse(text: &str) -> Option<Plan> {
    let mut plan = Plan {
        sites: 0,
        idle: IDLE,
    };
    for part in text.split(',').filter(|part| !part.trim().is_empty()) {
        let (key, value) = part.split_once('=')?;
        let value: u64 = value.trim().parse().ok()?;
        match key.trim() {
            "sites" => plan.sites = usize::try_from(value).ok()?,
            "idle" => plan.idle = Duration::from_secs(value),
            _ => return None,
        }
    }
    Some(plan)
}

/// Whether `dir` is missing, or a folder with nothing in it.
fn is_empty(dir: &Path) -> bool {
    match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(err) => err.kind() == std::io::ErrorKind::NotFound,
    }
}

/// Puts a connected window's data in `model`: Caddy and a PHP pool running,
/// PHP and Caddy installed, and `plan.sites` sites. The window opens on Sites
/// when there are sites.
pub fn fill(model: &mut Model, plan: Plan) {
    model.update(Update::Connected(Health {
        app_version: "0.0.0".into(),
        api_version: 1,
        pid: 1,
    }));
    model.home = Some(PathBuf::from("/demo"));
    let row = |runtime, installed: &[&str], default: Option<&str>| RuntimeVersions {
        runtime,
        installed: installed.iter().map(|text| (*text).to_owned()).collect(),
        default: default.map(str::to_owned),
    };
    model.update(Update::Runtimes(RuntimeList {
        runtimes: vec![
            row(Runtime::Php, &["8.3.27", "8.4.27"], Some("8.4")),
            row(Runtime::Caddy, &["2.11.7"], None),
        ],
    }));
    let running = |id: &str| ServiceStatus {
        id: id.into(),
        state: ServiceState::Running,
        pid: Some(1),
        restarts: 0,
        since_ms: None,
    };
    model.update(Update::Services(vec![
        running(WEB_SERVER),
        running("php-fpm-8.4"),
    ]));
    model.update(Update::Sites(sites(plan.sites)));
    if plan.sites > 0 {
        model.route = Route::Sites;
    }
}

/// `count` sites named `site-0001`, on PHP 8.4 and 8.3 by turns.
fn sites(count: usize) -> SiteList {
    let sites = (1..=count)
        .map(|n| {
            let root = PathBuf::from("/demo/Code").join(format!("site-{n:04}"));
            let php = if n % 3 == 0 { "8.3.27" } else { "8.4.27" };
            Site {
                name: format!("site-{n:04}"),
                docroot: root.join("public"),
                root,
                kind: SiteKind::Laravel,
                php: php.parse().ok(),
                php_source: Some(Source::GlobalDefault),
                secure: n % 7 != 0,
            }
        })
        .collect();
    SiteList {
        sites,
        ..SiteList::default()
    }
}

/// A measurement under way. [`Run::frame`] is called by every frame.
pub struct Run {
    plan: Plan,
    /// The time of the first frame, in milliseconds since the program began.
    first_frame_ms: u128,
    /// When the first frame ran.
    first: Option<Instant>,
    /// The frames that ran while the window was idle.
    idle_frames: u64,
    /// The result is logged and the window is closing: one more frame may
    /// run before it does.
    done: bool,
}

impl Run {
    pub const fn new(plan: Plan) -> Self {
        Self {
            plan,
            first_frame_ms: 0,
            first: None,
            idle_frames: 0,
            done: false,
        }
    }

    /// The time the first frame took to appear, from the start of the program.
    pub fn first_frame(&mut self, ms: u128) {
        self.first_frame_ms = ms;
    }

    /// Counts a frame. After the settle and the idle time it logs the result
    /// and closes the window. It asks for one frame at the end, and for no
    /// other: an idle window draws nothing in between.
    pub fn frame(&mut self, ctx: &egui::Context) {
        let now = Instant::now();
        let first = *self.first.get_or_insert(now);
        let end = SETTLE + self.plan.idle;
        let since = now.saturating_duration_since(first);
        if self.done {
            return;
        }
        if since >= end {
            self.done = true;
            tracing::info!(
                sites = self.plan.sites,
                first_frame_ms = self.first_frame_ms,
                idle_secs = self.plan.idle.as_secs(),
                idle_frames = self.idle_frames,
                "measure"
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if since >= SETTLE {
            self.idle_frames += 1;
        }
        ctx.request_repaint_after(end - since);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::sites::{List, view};

    #[test]
    fn a_plan_has_sites_and_an_idle_time_and_anything_else_is_refused() {
        assert_eq!(
            parse("sites=500,idle=120"),
            Some(Plan {
                sites: 500,
                idle: Duration::from_secs(120)
            })
        );
        assert_eq!(
            parse("sites=3"),
            Some(Plan {
                sites: 3,
                idle: IDLE
            })
        );
        assert_eq!(
            parse(""),
            Some(Plan {
                sites: 0,
                idle: IDLE
            })
        );
        for bad in ["sites", "sites=x", "sites=-1", "frames=3", "sites=5;idle=1"] {
            assert_eq!(parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn only_a_missing_or_empty_folder_may_be_measured() {
        let dir = tempfile::tempdir().expect("temp dir");
        assert!(is_empty(dir.path()));
        assert!(is_empty(&dir.path().join("not-there-yet")));
        std::fs::create_dir(dir.path().join("run")).expect("a folder");
        assert!(
            !is_empty(dir.path()),
            "a folder Marsa used has things in it"
        );
        let file = dir.path().join("config.toml");
        std::fs::write(&file, "x").expect("a file");
        // A file is not a folder: nothing to measure in it.
        assert!(!is_empty(&file));
    }

    #[test]
    fn the_mode_starts_only_when_asked_and_only_in_a_new_folder() {
        let dir = tempfile::tempdir().expect("temp dir");
        // Not asked: nothing changes.
        assert_eq!(check(None, dir.path()).expect("no plan"), None);
        let plan = check(Some("sites=500"), dir.path())
            .expect("a plan")
            .expect("asked");
        assert_eq!(plan.sites, 500);
        // A text it cannot read is an error, not a guess.
        assert!(matches!(
            check(Some("lots"), dir.path()),
            Err(MeasureError::Unreadable(_))
        ));
        // A folder with anything in it is refused, so a real one never is used.
        std::fs::create_dir(dir.path().join("config")).expect("a folder");
        let refused = check(Some("sites=500"), dir.path()).expect_err("a refusal");
        assert!(matches!(refused, MeasureError::NotEmpty(_)));
        assert!(
            refused.to_string().contains("empty data folder"),
            "{refused}"
        );
    }

    #[test]
    fn the_filled_window_is_connected_lists_the_sites_and_opens_on_sites() {
        let plan = Plan {
            sites: 500,
            idle: IDLE,
        };
        let mut model = Model::default();
        fill(&mut model, plan);
        assert!(model.is_connected());
        assert_eq!(model.route, Route::Sites);
        assert_eq!(model.services.len(), 2);
        let shown = view(&model, false);
        let List::Rows(rows) = &shown.list else {
            panic!("rows");
        };
        assert_eq!(rows.len(), 500);
        // With no site the window opens on Home, as it does for a new user.
        let mut empty = Model::default();
        fill(&mut empty, Plan { sites: 0, ..plan });
        assert_eq!(empty.route, Route::Home);
    }

    #[test]
    fn frames_count_only_after_the_settle_and_the_window_closes_after_the_idle_time() {
        let ctx = egui::Context::default();
        let mut run = Run::new(Plan {
            sites: 0,
            idle: Duration::from_secs(60),
        });
        // The first frame is the start of the clock, and is not idle.
        run.frame(&ctx);
        assert_eq!(run.idle_frames, 0);
        run.first = run.first.map(|first| first - SETTLE);
        run.frame(&ctx);
        run.frame(&ctx);
        assert_eq!(run.idle_frames, 2);
        // Past the end: the result is logged, and the frame is not counted.
        run.first = run.first.map(|first| first - Duration::from_secs(61));
        run.frame(&ctx);
        assert_eq!(run.idle_frames, 2);
        assert!(run.done, "the result is logged once");
        // The frame that may still run before the window closes counts for nothing.
        run.frame(&ctx);
        assert_eq!(run.idle_frames, 2);
    }
}
