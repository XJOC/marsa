//! Measurement mode: fills the window with made-up data and
//! counts its frames, so the memory, the idle cost, and the frame time of a
//! long list can be measured on a release build with no daemon. It is built
//! only with the cargo feature `measure`, which is off by default, so a
//! shipped binary carries none of it.
//!
//! `MARSA_GUI_MEASURE=sites=500,idle=120` opens the Sites screen with 500
//! made-up sites, keeps the window idle for 120 seconds after a short settle,
//! then logs one line (the time of the first frame, and how many frames ran
//! while the window was idle) and closes the window. `logs=5000` opens the
//! Logs screen with that many lines of Caddy's log instead, and
//! `scroll=240` then turns the wheel on it for 240 frames, up and back down,
//! and logs the mean and the worst frame time of those frames. `sites` and
//! `logs` are 0, `scroll` is 0, and `idle` is 60 when left out; with
//! nothing to list the window opens on Home.
//!
//! It can never touch a real data folder: it starts no bridge and no daemon,
//! and it refuses to start unless the data folder is missing or empty, which
//! a folder Marsa has used never is. Without the variable nothing here runs.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use marsa_core::runtimes::Runtime;
use marsa_core::types::{
    Health, LogLine, LogStream, RuntimeList, RuntimeVersions, ServiceState, ServiceStatus, Site,
    SiteKind, SiteList, Source,
};

use crate::model::{LogSource, Model, Route, Update, WEB_SERVER};

/// The variable that asks for the mode.
const VAR: &str = "MARSA_GUI_MEASURE";
/// The window shows before the idle frames are counted: the first frames
/// after the window opens are not idle.
const SETTLE: Duration = Duration::from_secs(5);
/// The idle time when `idle` is left out.
const IDLE: Duration = Duration::from_secs(60);

/// How far the wheel moves the content in one frame of the scroll, in
/// points: about eleven rows of the log.
const SCROLL_STEP: f32 = 300.0;
/// Where the pointer is while the wheel turns: over the list, at 1120 by 720.
const SCROLL_AT: (f32, f32) = (700.0, 400.0);

/// What to measure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Plan {
    /// How many made-up sites the window lists.
    pub sites: usize,
    /// How many lines Caddy's log has (the window keeps 5000 at most).
    pub logs: usize,
    /// How long the window is idle once it has settled.
    pub idle: Duration,
    /// How many frames the wheel turns on the list after that: up for half
    /// of them, then back down. None: no scroll.
    pub scroll: u32,
}

/// Why the mode cannot start.
#[derive(Debug, thiserror::Error)]
pub enum MeasureError {
    #[error("MARSA_GUI_MEASURE needs a new, empty data folder, and {} is not", .0.display())]
    NotEmpty(PathBuf),
    #[error(
        "MARSA_GUI_MEASURE is not understood: {0:?} (for example sites=500,idle=120 or logs=5000,idle=10,scroll=240)"
    )]
    Unreadable(String),
}

/// The plan `MARSA_GUI_MEASURE` asks for, if it is set.
///
/// # Errors
///
/// The text is not `key=<number>` pairs of `sites`, `logs`, `idle`, and
/// `scroll`, or `data_dir` holds anything.
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
        logs: 0,
        idle: IDLE,
        scroll: 0,
    };
    for part in text.split(',').filter(|part| !part.trim().is_empty()) {
        let (key, value) = part.split_once('=')?;
        let value: u64 = value.trim().parse().ok()?;
        match key.trim() {
            "sites" => plan.sites = usize::try_from(value).ok()?,
            "logs" => plan.logs = usize::try_from(value).ok()?,
            "idle" => plan.idle = Duration::from_secs(value),
            "scroll" => plan.scroll = u32::try_from(value).ok()?,
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
/// PHP and Caddy installed, `plan.sites` sites, and `plan.logs` lines of
/// Caddy's log. The window opens on Logs when there are lines, else on Sites
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
    model.update(Update::Log(caddy_log(plan.logs)));
    if plan.logs > 0 {
        model.route = Route::Logs(LogSource::Caddy);
    } else if plan.sites > 0 {
        model.route = Route::Sites;
    }
}

/// `count` lines of Caddy's log as it writes them (one JSON object a line):
/// mostly information, a warning now and then, an error more rarely, and a
/// line wider than the window every 500th.
fn caddy_log(count: usize) -> Vec<LogLine> {
    (0..count)
        .map(|n| {
            let level = match n {
                n if n % 97 == 96 => "error",
                n if n % 13 == 12 => "warn",
                _ => "info",
            };
            let mut msg = format!(
                "GET https://site-{:04}.test/products/{n} 200 {}ms",
                n % 500 + 1,
                n % 90 + 3
            );
            if n % 500 == 499 {
                msg.push_str(&format!(" {}", "slow upstream answer ".repeat(20)));
            }
            LogLine {
                service: WEB_SERVER.to_owned(),
                seq: n as u64,
                at_ms: 0,
                stream: LogStream::Stderr,
                text: format!(
                    r#"{{"level":"{level}","ts":1763476328.{n},"logger":"http.log.access","msg":"{msg}"}}"#
                ),
            }
        })
        .collect()
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

/// The count, the sum, and the worst of some times, in seconds.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Stat {
    count: u32,
    sum: f64,
    worst: f64,
}

impl Stat {
    fn add(&mut self, seconds: f64) {
        self.count += 1;
        self.sum += seconds;
        self.worst = self.worst.max(seconds);
    }

    /// The mean, in milliseconds.
    fn mean_ms(self) -> f64 {
        if self.count == 0 {
            0.0
        } else {
            self.sum / f64::from(self.count) * 1000.0
        }
    }

    /// The worst, in milliseconds.
    fn worst_ms(self) -> f64 {
        self.worst * 1000.0
    }
}

/// A measurement under way. [`Run::frame`] is called by every frame, and
/// [`Run::inject`] before it.
pub struct Run {
    plan: Plan,
    /// The time of the first frame, in milliseconds since the program began.
    first_frame_ms: u128,
    /// When the first frame ran.
    first: Option<Instant>,
    /// The frames that ran while the window was idle.
    idle_frames: u64,
    /// The frames of the scroll that have run.
    scrolled: u32,
    /// When the last frame of the scroll ran.
    last_frame: Option<Instant>,
    /// What building the window cost in each frame of the scroll (the
    /// screens, `shell::show`).
    ui: Stat,
    /// What egui reports of the CPU time of each frame of the scroll: the
    /// whole frame on the CPU, tessellation included, without the GPU. It is
    /// the previous frame's, as egui has it.
    cpu: Stat,
    /// The time between two frames of the scroll: what the screen shows.
    period: Stat,
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
            scrolled: 0,
            last_frame: None,
            ui: Stat {
                count: 0,
                sum: 0.0,
                worst: 0.0,
            },
            cpu: Stat {
                count: 0,
                sum: 0.0,
                worst: 0.0,
            },
            period: Stat {
                count: 0,
                sum: 0.0,
                worst: 0.0,
            },
            done: false,
        }
    }

    /// The time the first frame took to appear, from the start of the program.
    pub fn first_frame(&mut self, ms: u128) {
        self.first_frame_ms = ms;
    }

    /// Whether the idle time is over and the wheel turns now.
    fn scrolling(&self, since: Duration) -> bool {
        since >= SETTLE + self.plan.idle && self.scrolled < self.plan.scroll
    }

    /// Before a frame: while the wheel turns, the events of one turn. The
    /// first turn puts the pointer over the list. The wheel moves the content
    /// down (the list scrolls up) for half of the frames, then up.
    pub fn inject(&mut self, input: &mut egui::RawInput) {
        let Some(first) = self.first else {
            return;
        };
        if !self.scrolling(first.elapsed()) {
            return;
        }
        if self.scrolled == 0 {
            let (x, y) = SCROLL_AT;
            input
                .events
                .push(egui::Event::PointerMoved(egui::pos2(x, y)));
        }
        let down = self.scrolled < self.plan.scroll / 2;
        let step = if down { SCROLL_STEP } else { -SCROLL_STEP };
        input.events.push(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, step),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
            source: egui::MouseWheelSource::Wheel,
        });
    }

    /// Counts a frame. `ui_cost` is what building the window took, and
    /// `cpu_last` what egui says the last frame cost on the CPU. After the
    /// settle and the idle time, and the scroll if there is one, it logs the
    /// result and closes the window. While idle it asks for one frame at the
    /// end, and for no other: an idle window draws nothing in between. While
    /// the wheel turns it asks for every frame.
    pub fn frame(&mut self, ctx: &egui::Context, ui_cost: Duration, cpu_last: Option<f32>) {
        let now = Instant::now();
        let first = *self.first.get_or_insert(now);
        let idle_end = SETTLE + self.plan.idle;
        let since = now.saturating_duration_since(first);
        if self.done {
            return;
        }
        if self.scrolling(since) {
            // The first frame of the scroll follows an idle one: not counted.
            if self.scrolled > 0 {
                self.ui.add(ui_cost.as_secs_f64());
                self.cpu.add(f64::from(cpu_last.unwrap_or_default()));
                if let Some(last) = self.last_frame {
                    self.period
                        .add(now.saturating_duration_since(last).as_secs_f64());
                }
            }
            self.scrolled += 1;
            self.last_frame = Some(now);
            ctx.request_repaint();
            return;
        }
        if since >= idle_end {
            self.done = true;
            tracing::info!(
                sites = self.plan.sites,
                logs = self.plan.logs,
                first_frame_ms = self.first_frame_ms,
                idle_secs = self.plan.idle.as_secs(),
                idle_frames = self.idle_frames,
                scroll_frames = self.ui.count,
                ui_mean_ms = self.ui.mean_ms(),
                ui_worst_ms = self.ui.worst_ms(),
                cpu_mean_ms = self.cpu.mean_ms(),
                cpu_worst_ms = self.cpu.worst_ms(),
                period_mean_ms = self.period.mean_ms(),
                period_worst_ms = self.period.worst_ms(),
                "measure"
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        if since >= SETTLE {
            self.idle_frames += 1;
        }
        ctx.request_repaint_after(idle_end - since);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screens::sites::{List, view};

    fn plan(sites: usize) -> Plan {
        Plan {
            sites,
            logs: 0,
            idle: IDLE,
            scroll: 0,
        }
    }

    #[test]
    fn a_plan_has_sites_logs_an_idle_time_and_a_scroll_and_anything_else_is_refused() {
        assert_eq!(
            parse("sites=500,idle=120"),
            Some(Plan {
                idle: Duration::from_secs(120),
                ..plan(500)
            })
        );
        assert_eq!(
            parse("logs=5000,idle=10,scroll=240"),
            Some(Plan {
                logs: 5000,
                idle: Duration::from_secs(10),
                scroll: 240,
                ..plan(0)
            })
        );
        assert_eq!(parse("sites=3"), Some(plan(3)));
        assert_eq!(parse(""), Some(plan(0)));
        for bad in [
            "sites",
            "sites=x",
            "sites=-1",
            "frames=3",
            "sites=5;idle=1",
            "scroll=-2",
        ] {
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
        let plan = plan(500);
        let mut model = Model::default();
        fill(&mut model, plan);
        assert!(model.is_connected());
        assert_eq!(model.route, Route::Sites);
        assert_eq!(model.session.services.len(), 2);
        let shown = view(&model, false);
        let List::Rows(rows) = &shown.list else {
            panic!("rows");
        };
        assert_eq!(rows.len(), 500);
        // With nothing to list the window opens on Home, as it does for a new user.
        let mut empty = Model::default();
        fill(&mut empty, Plan { sites: 0, ..plan });
        assert_eq!(empty.route, Route::Home);
    }

    #[test]
    fn a_log_plan_opens_on_logs_with_its_lines_levels_and_some_lines_wider_than_the_window() {
        use crate::logline::LogLevel;
        let mut model = Model::default();
        fill(
            &mut model,
            Plan {
                logs: 5000,
                ..plan(0)
            },
        );
        assert_eq!(model.route, Route::Logs(LogSource::Caddy));
        let rows = model.log_rows(WEB_SERVER);
        assert_eq!(rows.len(), 5000);
        let count = |level| rows.iter().filter(|row| row.level == level).count();
        assert!(count(LogLevel::Error) > 20 && count(LogLevel::Warning) > 200);
        assert!(count(LogLevel::Info) > 4000);
        // Ten lines are wider than a 1120 window can show.
        let wide = rows.iter().filter(|row| row.columns > 300).count();
        assert_eq!(wide, 10);
        // More lines than the window keeps are the newest 5000.
        let mut more = Model::default();
        fill(
            &mut more,
            Plan {
                logs: 6000,
                ..plan(0)
            },
        );
        assert_eq!(more.log_rows(WEB_SERVER).len(), crate::model::MAX_LOG_LINES);
    }

    #[test]
    fn frames_count_only_after_the_settle_and_the_window_closes_after_the_idle_time() {
        let ctx = egui::Context::default();
        let mut run = Run::new(Plan {
            idle: Duration::from_secs(60),
            ..plan(0)
        });
        let frame = |run: &mut Run| run.frame(&ctx, Duration::ZERO, None);
        // The first frame is the start of the clock, and is not idle.
        frame(&mut run);
        assert_eq!(run.idle_frames, 0);
        run.first = run.first.map(|first| first - SETTLE);
        frame(&mut run);
        frame(&mut run);
        assert_eq!(run.idle_frames, 2);
        // Past the end: the result is logged, and the frame is not counted.
        run.first = run.first.map(|first| first - Duration::from_secs(61));
        frame(&mut run);
        assert_eq!(run.idle_frames, 2);
        assert!(run.done, "the result is logged once");
        // The frame that may still run before the window closes counts for nothing.
        frame(&mut run);
        assert_eq!(run.idle_frames, 2);
    }

    #[test]
    fn the_wheel_turns_after_the_idle_time_up_then_down_and_the_frames_are_timed() {
        let ctx = egui::Context::default();
        let mut run = Run::new(Plan {
            idle: Duration::from_secs(10),
            scroll: 4,
            ..plan(0)
        });
        let turn = |run: &mut Run| {
            let mut input = egui::RawInput::default();
            run.inject(&mut input);
            input.events
        };
        // Idle: no wheel.
        run.frame(&ctx, Duration::ZERO, None);
        assert!(turn(&mut run).is_empty());
        run.first = run
            .first
            .map(|first| first - SETTLE - Duration::from_secs(10));
        // The first turn puts the pointer over the list, then moves the content
        // down; the third and fourth move it back.
        let deltas: Vec<f32> = (0..4)
            .map(|frame| {
                let events = turn(&mut run);
                assert_eq!(
                    events.len(),
                    if frame == 0 { 2 } else { 1 },
                    "frame {frame}"
                );
                let Some(egui::Event::MouseWheel { delta, .. }) = events.last() else {
                    panic!("a wheel turn");
                };
                run.frame(&ctx, Duration::from_millis(2 + frame), Some(0.004));
                delta.y
            })
            .collect();
        assert_eq!(
            deltas,
            [SCROLL_STEP, SCROLL_STEP, -SCROLL_STEP, -SCROLL_STEP]
        );
        // The first scroll frame follows an idle one: three of four are timed.
        assert_eq!((run.ui.count, run.cpu.count), (3, 3));
        assert!(
            (run.ui.worst_ms() - 5.0).abs() < 0.01,
            "{}",
            run.ui.worst_ms()
        );
        assert!((run.cpu.mean_ms() - 4.0).abs() < 0.01);
        assert_eq!(run.period.count, 3);
        // The scroll is over: no more wheel, and the next frame logs and closes.
        assert!(turn(&mut run).is_empty());
        run.frame(&ctx, Duration::ZERO, None);
        assert!(run.done);
    }

    #[test]
    fn a_mean_and_a_worst_come_from_the_times_added() {
        let mut stat = Stat::default();
        assert_eq!((stat.mean_ms(), stat.worst_ms()), (0.0, 0.0));
        for seconds in [0.002, 0.004, 0.012] {
            stat.add(seconds);
        }
        assert!((stat.mean_ms() - 6.0).abs() < 1e-9);
        assert!((stat.worst_ms() - 12.0).abs() < 1e-9);
    }
}
