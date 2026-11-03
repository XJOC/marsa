//! The `--bench` walk for egui. It runs at the start of every frame (first
//! pass only), changes the view (tab, scroll offset), and prints the `PHASE`
//! lines.
//!
//! Frame interval = wall time between the starts of two consecutive frames.
//! While scrolling, the bench asks for a repaint on every frame, so eframe
//! draws one frame per display refresh (vsync) when it keeps up. Between
//! phases it asks for no repaint at all, so egui sleeps.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use eframe::egui;
use s09_common::bench::{self, FrameStats};
use s09_common::data;

use crate::view::{Tab, View};

/// What comes after a pause.
#[derive(Clone, Copy)]
enum Next {
    TableScroll,
    SqlOpen,
    LogsScroll,
    Idle,
    Done,
}

/// One scroll from top to bottom over `bench::SCROLL_DURATION`.
struct ScrollRun {
    tab: Tab,
    phase: &'static str,
    /// The first frame shows the tab at the top; timing starts on the next one.
    warmed_up: bool,
    started: Option<Instant>,
    last_frame: Option<Instant>,
    /// The bottom was reached; the next frame closes the measurement.
    finishing: bool,
    intervals_ms: Vec<f64>,
    cpu_ms: Vec<f64>,
}

impl ScrollRun {
    fn new(tab: Tab, phase: &'static str) -> Self {
        Self {
            tab,
            phase,
            warmed_up: false,
            started: None,
            last_frame: None,
            finishing: false,
            // 5 s at 120 Hz is 600 frames: no reallocation while measuring.
            intervals_ms: Vec::with_capacity(1024),
            cpu_ms: Vec::with_capacity(1024),
        }
    }
}

enum Step {
    FirstFrame,
    Pause {
        until: Instant,
        next: Next,
    },
    Scroll(Box<ScrollRun>),
    SqlOpen {
        started: Instant,
    },
    Idle {
        until: Instant,
        repaints: u32,
        /// Why egui woke up during idle (diagnostic, printed on stderr).
        causes: BTreeSet<String>,
    },
    Done,
    Finished,
}

pub struct BenchRun {
    step: Step,
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

impl BenchRun {
    pub fn new() -> Self {
        Self {
            step: Step::FirstFrame,
        }
    }

    /// Advances the walk by one frame. `cpu_usage` is eframe's CPU time of the
    /// previous frame in seconds (`None` on the first frame).
    pub fn step(&mut self, ctx: &egui::Context, cpu_usage: Option<f32>, view: &mut View) {
        let now = Instant::now();
        let next_step = match &mut self.step {
            Step::FirstFrame => {
                // `cumulative_frame_nr` counts finished frames: when it is 1,
                // the first frame has been painted and presented.
                if ctx.cumulative_frame_nr() == 0 {
                    ctx.request_repaint();
                    None
                } else {
                    bench::phase("first-frame", &[("ms", bench::ms(bench::ms_since_start()))]);
                    Some(pause(ctx, now, Next::TableScroll))
                }
            }
            Step::Pause { until, next } => {
                if now < *until {
                    ctx.request_repaint_after(*until - now);
                    None
                } else {
                    Some(begin(ctx, now, *next, view))
                }
            }
            Step::Scroll(run) => scroll_frame(ctx, now, cpu_usage, run, view),
            Step::SqlOpen { started } => {
                // The SQL tab was laid out and painted on the previous frame.
                bench::phase(
                    "sql-loaded",
                    &[
                        ("ms", bench::ms(millis(now - *started))),
                        ("lines", data::SQL_LINES.to_string()),
                    ],
                );
                Some(pause(ctx, now, Next::LogsScroll))
            }
            Step::Idle {
                until,
                repaints,
                causes,
            } => {
                if now < *until {
                    // Woken up before the timer: something asked for a repaint.
                    *repaints += 1;
                    record_causes(ctx, causes);
                    ctx.request_repaint_after(*until - now);
                    None
                } else {
                    bench::phase(
                        "idle",
                        &[
                            ("secs", format!("{:.1}", bench::IDLE_DURATION.as_secs_f64())),
                            ("repaints", repaints.to_string()),
                        ],
                    );
                    if !causes.is_empty() {
                        let list: Vec<&str> = causes.iter().map(String::as_str).collect();
                        eprintln!("egui-app: idle repaint causes: {}", list.join("; "));
                    }
                    Some(pause(ctx, now, Next::Done))
                }
            }
            Step::Done => {
                bench::phase("done", &[]);
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                Some(Step::Finished)
            }
            Step::Finished => None,
        };
        if let Some(step) = next_step {
            self.step = step;
        }
    }
}

/// Collects why this frame ran: the code that asked for a repaint, or the
/// kind of input event (pointer, focus, ...) that woke eframe up.
fn record_causes(ctx: &egui::Context, causes: &mut BTreeSet<String>) {
    for cause in ctx.repaint_causes() {
        causes.insert(cause.to_string());
    }
    ctx.input(|input| {
        for event in &input.raw.events {
            let text = format!("{event:?}");
            let kind = text.split(['(', ' ', '{']).next().unwrap_or("event");
            causes.insert(format!("input {kind}"));
        }
    });
}

fn pause(ctx: &egui::Context, now: Instant, next: Next) -> Step {
    ctx.request_repaint_after(bench::PHASE_PAUSE);
    Step::Pause {
        until: now + bench::PHASE_PAUSE,
        next,
    }
}

fn begin(ctx: &egui::Context, now: Instant, next: Next, view: &mut View) -> Step {
    match next {
        Next::TableScroll => {
            ctx.request_repaint();
            Step::Scroll(Box::new(ScrollRun::new(Tab::Table, "table-scrolled")))
        }
        Next::LogsScroll => {
            ctx.request_repaint();
            Step::Scroll(Box::new(ScrollRun::new(Tab::Logs, "logs-scrolled")))
        }
        Next::SqlOpen => {
            // The tab opens on this frame; `View` loads the 5,000 lines.
            view.tab = Tab::Sql;
            ctx.request_repaint();
            Step::SqlOpen { started: now }
        }
        Next::Idle => {
            bench::phase("idle-start", &[]);
            ctx.request_repaint_after(bench::IDLE_DURATION);
            Step::Idle {
                until: now + bench::IDLE_DURATION,
                repaints: 0,
                causes: BTreeSet::new(),
            }
        }
        Next::Done => {
            ctx.request_repaint();
            Step::Done
        }
    }
}

fn scroll_frame(
    ctx: &egui::Context,
    now: Instant,
    cpu_usage: Option<f32>,
    run: &mut ScrollRun,
    view: &mut View,
) -> Option<Step> {
    view.tab = run.tab;
    let scroll = view.scroll_mut(run.tab);
    ctx.request_repaint();

    let Some(started) = run.started else {
        if run.warmed_up {
            run.started = Some(now);
            run.last_frame = Some(now);
        } else {
            run.warmed_up = true;
        }
        scroll.force = Some(0.0);
        return None;
    };

    if let Some(last) = run.last_frame {
        run.intervals_ms.push(millis(now - last));
    }
    if let Some(cpu) = cpu_usage {
        run.cpu_ms.push(f64::from(cpu) * 1000.0);
    }
    run.last_frame = Some(now);

    if run.finishing {
        let mut fields: Vec<(&str, String)> = Vec::with_capacity(8);
        match FrameStats::from_intervals(&run.intervals_ms) {
            Some(stats) => fields.extend(stats.fields()),
            None => fields.push(("frames", "0".to_string())),
        }
        if let Some(cpu) = FrameStats::from_intervals(&run.cpu_ms) {
            fields.extend(cpu.cpu_fields());
        }
        bench::phase(run.phase, &fields);
        let next = match run.tab {
            Tab::Table | Tab::Sql => Next::SqlOpen,
            Tab::Logs => Next::Idle,
        };
        return Some(pause(ctx, now, next));
    }

    let progress = ((now - started).as_secs_f64() / bench::SCROLL_DURATION.as_secs_f64()).min(1.0);
    scroll.force = Some(progress as f32 * scroll.max);
    run.finishing = progress >= 1.0;
    None
}
