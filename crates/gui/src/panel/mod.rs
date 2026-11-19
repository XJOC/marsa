//! The menu-bar panel as a window: `marsa-gui --panel` (ADR 0022). A
//! short-lived process that `marsa-tray` starts under its icon on a click and
//! that closes itself on focus loss or Esc.
//!
//! The panel never starts a program and never changes the tray. It tells the
//! tray what the user asked for by the way it exits: [`PanelExit`] is that
//! contract, and the tray, which waits for the child, reads it. No socket and
//! no file are involved, and the exit code carries no data.

pub mod app;
pub mod place;

use std::process::ExitStatus;

/// Why the panel closed. The exit code of `marsa-gui --panel`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PanelExit {
    /// Esc, or the icon was clicked again (the tray ended it). Exit code 0.
    Closed,
    /// Another window took the focus. The tray ignores a click on its icon
    /// that comes right after it: that click was the one that closed it.
    FocusLost,
    /// "Open Marsa" (or the status): the tray starts the window.
    OpenWindow,
    /// "Stop Marsa": the tray asks the daemon to shut down, as its menu does.
    StopMarsa,
    /// "Start Marsa", when the daemon is not running: the tray starts it, as
    /// its menu does.
    StartMarsa,
    /// "Quit": the tray closes its icon. The daemon and the sites go on.
    Quit,
}

impl PanelExit {
    /// The exit code. 1 is the code of a failure (`ExitCode::FAILURE`), and
    /// the codes of a program that crashed are the system's: all of them
    /// differ from these.
    pub const fn code(self) -> u8 {
        match self {
            Self::Closed => 0,
            Self::FocusLost => 20,
            Self::OpenWindow => 21,
            Self::StopMarsa => 22,
            Self::StartMarsa => 23,
            Self::Quit => 24,
        }
    }

    /// The reason that has this exit code, if any. Any other code, and a
    /// panel that a signal ended, count as [`PanelExit::Closed`]: nothing
    /// was asked of the tray.
    pub const fn from_code(code: Option<i32>) -> Self {
        match code {
            Some(20) => Self::FocusLost,
            Some(21) => Self::OpenWindow,
            Some(22) => Self::StopMarsa,
            Some(23) => Self::StartMarsa,
            Some(24) => Self::Quit,
            _ => Self::Closed,
        }
    }

    /// What the tray reads from a panel that has ended.
    pub fn of(status: ExitStatus) -> Self {
        Self::from_code(status.code())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [PanelExit; 6] = [
        PanelExit::Closed,
        PanelExit::FocusLost,
        PanelExit::OpenWindow,
        PanelExit::StopMarsa,
        PanelExit::StartMarsa,
        PanelExit::Quit,
    ];

    #[test]
    fn every_reason_comes_back_from_its_own_exit_code() {
        for reason in ALL {
            assert_eq!(PanelExit::from_code(Some(i32::from(reason.code()))), reason);
        }
        // Each code is its own.
        let mut codes: Vec<u8> = ALL.iter().map(|reason| reason.code()).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), ALL.len());
    }

    #[test]
    fn a_failure_a_crash_or_a_signal_asks_nothing_of_the_tray() {
        // 1 is `ExitCode::FAILURE`; 101 is a Rust panic; 130 is Ctrl+C.
        for code in [Some(1), Some(101), Some(130), Some(-1), Some(255), None] {
            assert_eq!(PanelExit::from_code(code), PanelExit::Closed, "{code:?}");
        }
    }
}
