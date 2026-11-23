//! The report of `marsa doctor` as the daemon sees it.

use serde::{Deserialize, Serialize};

use super::{DoctorCheck, DoctorVerdict, DoctorWarning};

/// `GET /v1/doctor`: the checks of `marsa doctor`, as the daemon sees them.
/// The `PATH` check is not here: it judges a terminal's `PATH`, which only
/// the CLI has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorReport {
    /// In the order `marsa doctor` prints them.
    pub lines: Vec<DoctorLine>,
    /// Things that work now but will cause trouble; they fail nothing.
    pub warnings: Vec<DoctorWarning>,
}

/// One check and what it found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorLine {
    pub check: DoctorCheck,
    pub verdict: DoctorVerdict,
}
