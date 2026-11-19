//! The tray's side of the menu-bar panel (ADR 0022): on a left click it
//! starts `marsa-gui --panel` under the icon, a second click while it is open
//! closes it, and what the panel's exit code asks for ([`PanelExit`]) is done
//! by the tray, which owns the icon, the worker, and the window program.
//!
//! The tray never draws the panel and runs no egui: it passes the place of its
//! icon and the monitor that holds it as numbers, waits for the child, and
//! reads its exit code. Everything here is plain data, so the logic is tested
//! with a panel that is not a process.

use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use eframe::egui::{Rect, pos2, vec2};

use crate::panel::PanelExit;
use crate::panel::place::Anchor;

/// How often the tray looks whether the panel has exited, while one is open.
/// Nothing wakes the event loop when a child ends, and a thread that waits for
/// one could not also close it (a `Child` is waited on or killed, not both
/// from two threads).
pub const POLL: Duration = Duration::from_millis(60);

/// A click on the icon this soon after the panel closed itself because
/// another window took the focus is the click that did it: the user pressed
/// the icon to close the panel. It must not open the panel again. The press
/// that takes the focus and the click that follows are a few milliseconds
/// apart; a person cannot press twice in this time.
pub const REOPEN_GUARD: Duration = Duration::from_millis(300);

/// A rectangle in the physical pixels the system reports.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Px {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// A monitor: where it is and how big, in physical pixels, and its scale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Monitor {
    pub rect: Px,
    pub scale: f64,
}

/// The icon's height in points on the monitor that holds it: a menu bar or a
/// taskbar icon is 16 to 48 points, and the range keeps out the monitor that
/// the same pixels would belong to at another scale (macOS reports the
/// physical pixels of each screen with that screen's own scale, so a Retina
/// screen and a plain one next to it can both "contain" the icon).
const ICON_POINTS: std::ops::RangeInclusive<f64> = 14.0..=64.0;

impl Px {
    fn center(self) -> (f64, f64) {
        (self.x + self.width / 2.0, self.y + self.height / 2.0)
    }

    fn contains(self, (x, y): (f64, f64)) -> bool {
        x >= self.x && x < self.x + self.width && y >= self.y && y < self.y + self.height
    }

    fn rect(self) -> Rect {
        // Pixel counts are far below `f32`'s exact range.
        Rect::from_min_size(
            pos2(self.x as f32, self.y as f32),
            vec2(self.width as f32, self.height as f32),
        )
    }
}

/// The monitor that holds the icon, with the icon, as the panel's anchor.
/// The monitors are tried in the order given (the primary one first), and the
/// first that contains the icon's center and gives it a height an icon has
/// wins; failing that, the first that contains the center. `None` for an
/// icon with no size (the system did not say where it is) or one that no
/// monitor contains: the system then places the panel.
pub fn anchor(icon: Px, monitors: &[Monitor]) -> Option<Anchor> {
    if icon.width <= 0.0 || icon.height <= 0.0 {
        return None;
    }
    let holds: Vec<&Monitor> = monitors
        .iter()
        .filter(|monitor| monitor.scale > 0.0 && monitor.rect.contains(icon.center()))
        .collect();
    let plausible = holds
        .iter()
        .find(|monitor| ICON_POINTS.contains(&(icon.height / monitor.scale)));
    let monitor = plausible.or(holds.first())?;
    Some(Anchor {
        icon: icon.rect(),
        monitor: monitor.rect.rect(),
        scale: monitor.scale as f32,
    })
}

/// The arguments of `marsa-gui --panel`: the data folder of the tray, and the
/// place of the icon when it is known. The window program takes no other.
pub fn args(data_dir: &Path, anchor: Option<&Anchor>) -> Vec<OsString> {
    let mut args = vec![
        OsString::from("--panel"),
        OsString::from("--data-dir"),
        data_dir.as_os_str().to_owned(),
    ];
    if let Some(anchor) = anchor {
        args.extend(anchor.to_args().map(OsString::from));
    }
    args
}

/// Starts the panel: the program by its absolute path (never `PATH`), each
/// argument as one argument (never a shell string), no input.
///
/// # Errors
///
/// The program cannot be started.
pub fn spawn(program: &Path, args: &[OsString]) -> io::Result<Child> {
    Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .spawn()
}

/// A panel that runs. A [`Child`] is one; the tests use a stand-in.
pub trait Running {
    /// `None` while it runs; once it has ended, `Some` with its exit code
    /// (`None` inside when a signal ended it).
    fn poll(&mut self) -> Option<Option<i32>>;
    /// Ends it, and collects its exit status so it leaves no zombie.
    fn stop(&mut self);
}

impl Running for Child {
    fn poll(&mut self) -> Option<Option<i32>> {
        match self.try_wait() {
            Ok(Some(status)) => Some(status.code()),
            Ok(None) => None,
            // It cannot be asked about: count it as gone.
            Err(_) => Some(None),
        }
    }

    fn stop(&mut self) {
        // It may have ended already; the status is collected either way.
        let _ = self.kill();
        let _ = self.wait();
    }
}

/// What a click on the icon does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    /// No panel runs: start one (and give it to [`Slot::start`]).
    Open,
    /// A panel was open, and is closed now.
    Close,
    /// The click that closed the panel by taking its focus: nothing to do.
    Skip,
}

/// The one panel the tray may have open.
#[derive(Debug)]
pub struct Slot<P> {
    panel: Option<P>,
    /// When the last panel closed itself because it lost the focus.
    lost_focus: Option<Instant>,
}

impl<P: Running> Default for Slot<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Running> Slot<P> {
    pub const fn new() -> Self {
        Self {
            panel: None,
            lost_focus: None,
        }
    }

    pub const fn is_open(&self) -> bool {
        self.panel.is_some()
    }

    /// A panel was started.
    pub fn start(&mut self, panel: P) {
        self.panel = Some(panel);
    }

    /// Whether the panel has ended, and why. Call it before [`Slot::click`], so
    /// a click that follows an exit sees it, and act on what it returns.
    pub fn poll(&mut self, now: Instant) -> Option<PanelExit> {
        let code = self.panel.as_mut()?.poll()?;
        self.panel = None;
        let exit = PanelExit::from_code(code);
        if exit == PanelExit::FocusLost {
            self.lost_focus = Some(now);
        }
        Some(exit)
    }

    /// A left click on the icon: close the panel that is open, or open one,
    /// unless the click is the one that closed it ([`REOPEN_GUARD`]).
    pub fn click(&mut self, now: Instant) -> Toggle {
        if let Some(mut panel) = self.panel.take() {
            panel.stop();
            return Toggle::Close;
        }
        let guarded = self
            .lost_focus
            .is_some_and(|lost| now.saturating_duration_since(lost) < REOPEN_GUARD);
        if guarded {
            // Once: the next click is a real one.
            self.lost_focus = None;
            return Toggle::Skip;
        }
        Toggle::Open
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use marsa_core::platform::spawn::sibling_program;

    use super::*;

    /// How a stand-in panel ended, if it has: its exit code.
    type Ended = Rc<Cell<Option<Option<i32>>>>;

    /// A panel that is not a process: it ends when its owner says.
    #[derive(Default)]
    struct Fake {
        ended: Ended,
        stopped: Rc<Cell<bool>>,
    }

    impl Running for Fake {
        fn poll(&mut self) -> Option<Option<i32>> {
            self.ended.get()
        }

        fn stop(&mut self) {
            self.stopped.set(true);
        }
    }

    /// A slot with one open panel, and the handles that end it and show that
    /// it was stopped.
    fn open_slot() -> (Slot<Fake>, Ended, Rc<Cell<bool>>) {
        let fake = Fake::default();
        let (ended, stopped) = (Rc::clone(&fake.ended), Rc::clone(&fake.stopped));
        let mut slot = Slot::new();
        slot.start(fake);
        (slot, ended, stopped)
    }

    fn px(x: f64, y: f64, width: f64, height: f64) -> Px {
        Px {
            x,
            y,
            width,
            height,
        }
    }

    #[test]
    fn a_click_opens_a_panel_when_none_is_open_and_closes_the_one_that_is() {
        let now = Instant::now();
        let mut slot: Slot<Fake> = Slot::new();
        assert_eq!(slot.click(now), Toggle::Open);
        assert!(!slot.is_open());
        let (mut slot, _, stopped) = open_slot();
        assert!(slot.is_open());
        // A panel that runs is not opened again; the click closes it.
        assert_eq!(slot.click(now), Toggle::Close);
        assert!(stopped.get(), "the panel was ended");
        assert!(!slot.is_open());
        // The next click opens one.
        assert_eq!(slot.click(now), Toggle::Open);
    }

    #[test]
    fn a_panel_that_ended_gives_its_reason_once_and_frees_the_slot() {
        let now = Instant::now();
        let (mut slot, ended, _) = open_slot();
        assert_eq!(slot.poll(now), None, "it runs");
        assert!(slot.is_open());
        ended.set(Some(Some(i32::from(PanelExit::Quit.code()))));
        assert_eq!(slot.poll(now), Some(PanelExit::Quit));
        assert!(!slot.is_open());
        assert_eq!(slot.poll(now), None, "said once");
        // A panel that a signal ended, or that crashed, asked for nothing.
        let (mut slot, ended, _) = open_slot();
        ended.set(Some(None));
        assert_eq!(slot.poll(now), Some(PanelExit::Closed));
        let (mut slot, ended, _) = open_slot();
        ended.set(Some(Some(101)));
        assert_eq!(slot.poll(now), Some(PanelExit::Closed));
    }

    #[test]
    fn the_click_that_took_the_focus_from_the_panel_does_not_open_it_again() {
        let now = Instant::now();
        let (mut slot, ended, _) = open_slot();
        ended.set(Some(Some(i32::from(PanelExit::FocusLost.code()))));
        assert_eq!(slot.poll(now), Some(PanelExit::FocusLost));
        // The click is right behind it: it closed the panel, so it is no
        // request to open one.
        let soon = now + Duration::from_millis(40);
        assert_eq!(slot.click(soon), Toggle::Skip);
        // Only that one: the next click, however soon, is the user's.
        assert_eq!(slot.click(soon), Toggle::Open);
    }

    #[test]
    fn a_click_long_after_the_focus_was_lost_opens_the_panel() {
        let now = Instant::now();
        let (mut slot, ended, _) = open_slot();
        ended.set(Some(Some(i32::from(PanelExit::FocusLost.code()))));
        slot.poll(now);
        assert_eq!(slot.click(now + REOPEN_GUARD), Toggle::Open);
    }

    #[test]
    fn only_losing_the_focus_guards_the_next_click() {
        let now = Instant::now();
        for reason in [
            PanelExit::Closed,
            PanelExit::OpenWindow,
            PanelExit::StopMarsa,
            PanelExit::StartMarsa,
        ] {
            let (mut slot, ended, _) = open_slot();
            ended.set(Some(Some(i32::from(reason.code()))));
            assert_eq!(slot.poll(now), Some(reason));
            assert_eq!(slot.click(now), Toggle::Open, "{reason:?}");
        }
    }

    #[test]
    fn the_panel_is_started_with_its_flag_the_data_folder_and_the_place_of_the_icon() {
        let dir = Path::new("/data/marsa");
        let plain = args(dir, None);
        assert_eq!(plain, ["--panel", "--data-dir", "/data/marsa"]);
        let anchor = Anchor {
            icon: Rect::from_min_size(pos2(2146.0, 0.0), vec2(76.0, 68.0)),
            monitor: Rect::from_min_size(pos2(0.0, 0.0), vec2(3024.0, 1964.0)),
            scale: 2.0,
        };
        let placed = args(dir, Some(&anchor));
        assert_eq!(&placed[..3], &plain[..]);
        assert_eq!(
            &placed[3..],
            [
                "--icon",
                "2146,0,76,68",
                "--monitor",
                "0,0,3024,1964",
                "--scale",
                "2"
            ]
        );
        // What the panel reads back is what the tray sent.
        let text = |index: usize| placed[index].to_str().expect("text");
        assert_eq!(Anchor::parse(text(4), text(6), text(8)), Ok(anchor));
        // A data folder with spaces is one argument, unchanged.
        let spaced = args(Path::new("/data/My Marsa"), None);
        assert_eq!(spaced[2], OsString::from("/data/My Marsa"));
    }

    #[test]
    fn the_panel_program_is_the_window_program_next_to_the_running_one() {
        let program = sibling_program("marsa-gui").expect("a path");
        assert!(
            program.is_absolute(),
            "never found through PATH: {program:?}"
        );
        let exe = std::env::current_exe().expect("the test program");
        assert_eq!(program.parent(), exe.parent());
        let name = program.file_name().and_then(|name| name.to_str());
        assert_eq!(
            name,
            Some(format!("marsa-gui{}", std::env::consts::EXE_SUFFIX).as_str())
        );
    }

    /// What the test program does when a test below starts it again as a
    /// child, as a stand-in for the panel: it exits with the code of "Quit",
    /// or waits to be stopped. Run on its own, it does nothing and passes.
    #[test]
    fn child_stand_in_for_the_panel() {
        match std::env::var("MARSA_TRAY_TEST_CHILD").as_deref() {
            Ok("quit") => std::process::exit(i32::from(PanelExit::Quit.code())),
            Ok("wait") => std::thread::sleep(Duration::from_secs(120)),
            _ => {}
        }
    }

    /// The test program as a child that does what `how` says.
    fn child(how: &str) -> Child {
        Command::new(std::env::current_exe().expect("the test program"))
            .args([
                "--exact",
                "tray::panel::tests::child_stand_in_for_the_panel",
            ])
            .env("MARSA_TRAY_TEST_CHILD", how)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .expect("starts")
    }

    #[test]
    fn a_panel_that_cannot_be_started_is_an_error() {
        let missing = Path::new("/this/program/does/not/exist/marsa-gui");
        assert!(spawn(missing, &[]).is_err());
    }

    #[test]
    fn a_real_child_that_exits_with_a_code_gives_the_reason_of_that_code() {
        let mut slot = Slot::new();
        slot.start(child("quit"));
        let started = Instant::now();
        let exit = loop {
            if let Some(exit) = slot.poll(Instant::now()) {
                break exit;
            }
            assert!(started.elapsed() < Duration::from_secs(60), "it ended");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(exit, PanelExit::Quit);
        assert!(!slot.is_open());
    }

    #[test]
    fn a_real_child_that_runs_is_stopped_by_a_click_and_collected() {
        let mut slot = Slot::new();
        slot.start(child("wait"));
        assert_eq!(slot.poll(Instant::now()), None, "it runs");
        assert_eq!(slot.click(Instant::now()), Toggle::Close);
        assert!(!slot.is_open());
    }

    #[test]
    fn a_child_that_was_stopped_has_ended_by_a_signal_where_there_are_signals() {
        let mut waiting = child("wait");
        assert_eq!(Running::poll(&mut waiting), None);
        Running::stop(&mut waiting);
        let code = Running::poll(&mut waiting).expect("collected");
        assert_ne!(code, Some(i32::from(PanelExit::Quit.code())));
    }

    // -- Which monitor holds the icon --

    fn monitor(x: f64, y: f64, width: f64, height: f64, scale: f64) -> Monitor {
        Monitor {
            rect: px(x, y, width, height),
            scale,
        }
    }

    #[test]
    fn the_monitor_that_holds_the_icon_is_the_anchor() {
        // The spike's Mac: the icon at (2146, 0), 76 by 68 px, on a screen of
        // 3024 by 1964 px at scale 2.
        let mac = monitor(0.0, 0.0, 3024.0, 1964.0, 2.0);
        let held = anchor(px(2146.0, 0.0, 76.0, 68.0), &[mac]).expect("held");
        assert_eq!(held.scale, 2.0);
        assert_eq!(held.monitor.width(), 3024.0);
        assert_eq!(held.icon.left(), 2146.0);
        // A Windows icon on the second of two screens (physical, one space).
        let first = monitor(0.0, 0.0, 1920.0, 1080.0, 1.0);
        let second = monitor(1920.0, 0.0, 2560.0, 1440.0, 1.5);
        let icon = px(4300.0, 1400.0, 36.0, 36.0);
        let held = anchor(icon, &[first, second]).expect("held");
        assert_eq!((held.scale, held.monitor.left()), (1.5, 1920.0));
    }

    #[test]
    fn an_icon_with_no_size_or_on_no_monitor_gives_no_anchor() {
        let mac = monitor(0.0, 0.0, 3024.0, 1964.0, 2.0);
        // Linux, and an icon the system could not place: a rect of nothing.
        assert_eq!(anchor(px(0.0, 0.0, 0.0, 0.0), &[mac]), None);
        assert_eq!(anchor(px(10.0, 10.0, 24.0, 0.0), &[mac]), None);
        // Outside every monitor, or no monitor at all.
        assert_eq!(anchor(px(9000.0, 0.0, 76.0, 68.0), &[mac]), None);
        assert_eq!(anchor(px(100.0, 0.0, 76.0, 68.0), &[]), None);
        // A scale of 0 is no monitor.
        assert_eq!(
            anchor(
                px(100.0, 0.0, 76.0, 68.0),
                &[monitor(0.0, 0.0, 3024.0, 1964.0, 0.0)]
            ),
            None
        );
    }

    #[test]
    fn two_screens_of_different_scales_that_both_seem_to_hold_the_icon_give_the_right_one() {
        // macOS gives each screen its own pixels: a Retina screen of 1512
        // points (3024 px) with a plain screen of 1920 points (1920 px) to
        // its right at x = 1512 points. An icon on the plain screen at
        // 1600 points is at pixel 1600, which the Retina screen's pixels
        // (0 to 3024) also contain. Its height says which: 24 px is 12
        // points on the Retina screen, and an icon is not that small.
        let retina = monitor(0.0, 0.0, 3024.0, 1964.0, 2.0);
        let plain = monitor(1512.0, 0.0, 1920.0, 1080.0, 1.0);
        let on_plain = anchor(px(1600.0, 0.0, 28.0, 24.0), &[retina, plain]).expect("held");
        assert_eq!(on_plain.scale, 1.0, "the plain screen");
        // The other way round: the icon on the Retina screen is 68 px high,
        // which on the plain screen would be 68 points.
        let on_retina = anchor(px(2146.0, 0.0, 76.0, 68.0), &[plain, retina]).expect("held");
        assert_eq!(
            on_retina.scale, 2.0,
            "the Retina screen, though it is second"
        );
        // Neither height fits: the first screen that holds the icon.
        let odd = anchor(px(1700.0, 0.0, 10.0, 5.0), &[retina, plain]).expect("held");
        assert_eq!(odd.scale, 2.0);
    }
}
