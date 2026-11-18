//! The sizes of the old one-page window (now `screens/`), unchanged, kept
//! here so no size is written outside `theme` (rules.md 2). They are not
//! design tokens: the screens of M5-D4 to M5-D10 replace them with the tokens
//! of [`super::Size`], and each goes with its last use.

/// The widest the confirmation of a removal grows; its text wraps.
pub const CONFIRM_WIDTH: f32 = 380.0;
/// The width of an install's progress bar.
pub const PROGRESS_WIDTH: f32 = 420.0;
/// The width of a version field in the Runtimes section.
pub const VERSION_FIELD_WIDTH: f32 = 70.0;
/// The width of the site name field of the link form.
pub const NAME_FIELD_WIDTH: f32 = 180.0;

/// The services table: the narrowest service column, the first width and
/// the narrowest of the state column, and the narrowest PID column.
pub const SERVICE_COLUMN_MIN: f32 = 80.0;
pub const STATE_COLUMN_WIDTH: f32 = 260.0;
pub const STATE_COLUMN_MIN: f32 = 80.0;
pub const PID_COLUMN_MIN: f32 = 50.0;

/// The sites table: the narrowest and widest name and URL columns, the
/// narrowest kind, PHP, and folder columns.
pub const SITE_NAME_COLUMN_MIN: f32 = 80.0;
pub const SITE_NAME_COLUMN_MAX: f32 = 240.0;
pub const SITE_URL_COLUMN_MIN: f32 = 120.0;
pub const SITE_URL_COLUMN_MAX: f32 = 320.0;
pub const SITE_KIND_COLUMN_MIN: f32 = 60.0;
/// The narrowest PHP column: a closed list with a version in it.
pub const PHP_COLUMN_WIDTH: f32 = 110.0;
pub const SITE_FOLDER_COLUMN_MIN: f32 = 80.0;
/// The sites table keeps at least this many lines of height.
pub const SITES_MIN_LINES: f32 = 2.0;
