//! A guard that lets one job run at a time: the setup and the uninstall share
//! one, and the installs have their own (a runtime's removal takes the
//! installs' guard).
//!
//! One atomic, no lock. The value says who holds the guard: `0` is nobody,
//! and a [`NonZeroU8`] flag names the holder, so the caller can tell a second
//! request which job runs now (the 409 of the other one).

use std::num::NonZeroU8;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

/// The guard. Free until someone [`take`](OneAtATime::take)s it.
#[derive(Debug, Default)]
pub struct OneAtATime(AtomicU8);

/// The right to run. Dropping it lets the next job start, also if the job
/// panics.
#[derive(Debug)]
pub struct Turn(Arc<OneAtATime>);

impl OneAtATime {
    pub const fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    /// The turn for the job named `flag`, or the flag of the job that runs
    /// now. One compare-and-swap.
    pub fn take(this: &Arc<Self>, flag: NonZeroU8) -> Result<Turn, u8> {
        this.0
            .compare_exchange(0, flag.get(), Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Turn(Arc::clone(this)))
    }

    /// The flag of the job that runs now, or `0`.
    pub fn holder(&self) -> u8 {
        self.0.load(Ordering::Acquire)
    }

    /// Whether a job runs now.
    pub fn is_running(&self) -> bool {
        self.holder() != 0
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        self.0.0.store(0, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(n: u8) -> NonZeroU8 {
        NonZeroU8::new(n).expect("not zero")
    }

    #[test]
    fn one_job_runs_and_the_other_learns_which() {
        let guard = Arc::new(OneAtATime::new());
        assert!(!guard.is_running());
        let turn = OneAtATime::take(&guard, flag(2)).expect("free");
        assert_eq!(guard.holder(), 2);
        assert!(guard.is_running());
        assert_eq!(OneAtATime::take(&guard, flag(1)).unwrap_err(), 2);
        assert_eq!(OneAtATime::take(&guard, flag(2)).unwrap_err(), 2);
        drop(turn);
        assert!(!guard.is_running());
        assert!(OneAtATime::take(&guard, flag(1)).is_ok());
    }

    #[test]
    fn a_job_that_panics_frees_the_guard() {
        let guard = Arc::new(OneAtATime::new());
        let turn = OneAtATime::take(&guard, flag(1)).expect("free");
        let result = std::thread::spawn(move || {
            let _turn = turn;
            panic!("the job failed");
        })
        .join();
        assert!(result.is_err());
        assert!(!guard.is_running());
    }
}
