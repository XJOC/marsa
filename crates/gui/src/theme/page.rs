//! Sizes that belong to one screen (a column width, a field width), kept here
//! so no size is written outside `theme` (rules.md 2). They are not shared
//! design tokens like [`super::Size`]; a size moves there when a second screen
//! needs it.

/// The width of the site name field of the link form.
pub const NAME_FIELD_WIDTH: f32 = 180.0;

/// The Services screen: the columns after the name and its
/// line: the state pill, and the buttons ("Show log" and Start, or Restart
/// and Stop, side by side). The mockup's buttons are wider than they need,
/// and the name and its two lines are the ones that need the room.
pub const SERVICES_STATE_COLUMN: f32 = 130.0;
pub const SERVICES_ACTIONS_COLUMN: f32 = 150.0;

/// The Sites screen: the search field of the header, the
/// columns after the site (the PHP menu with its reason, the connection, and
/// the Open button, so it ends at the card's edge), and the Forget column of
/// the parked folders.
pub const SITES_SEARCH_WIDTH: f32 = 230.0;
pub const SITES_PHP_COLUMN: f32 = 200.0;
pub const SITES_CONNECTION_COLUMN: f32 = 84.0;
pub const SITES_OPEN_COLUMN: f32 = 58.0;
pub const SITES_FORGET_COLUMN: f32 = 80.0;
/// The reason column of the skipped folders.
pub const SITES_WHY_COLUMN: f32 = 340.0;
/// How many rows of parked and of skipped folders show before the list
/// scrolls inside its card.
pub const SITES_FOLDER_ROWS: f32 = 5.0;

/// The Home sites list: the PHP column fits `PHP 8.4`, and the
/// last column fits the Open button, so the button ends at the card's edge.
pub const HOME_PHP_COLUMN: f32 = 56.0;
pub const HOME_OPEN_COLUMN: f32 = 58.0;

/// The first-run panel's column: wide enough for the steps'
/// lines, narrower than the window.
pub const FIRST_RUN_WIDTH: f32 = 560.0;

/// A site's page: the column of the labels in its facts card
/// (the mockup's 130).
pub const SITE_FACT_LABEL: f32 = 130.0;

/// The Runtimes screen: the list on the start side (layout.md,
/// Master and detail: 190 to 220), and the columns of the Installed table
/// after the version, as the mockup has them (the menu column is an icon
/// button wide).
pub const RUNTIMES_LIST_WIDTH: f32 = 220.0;
pub const RUNTIMES_VERSION_COLUMN: f32 = 110.0;
pub const RUNTIMES_TAG_COLUMN: f32 = 90.0;

/// The Settings screen: the width of its column (the mockup's
/// 680), and the least room the name and hint of a row keep next to its
/// control, so a long value is cut short instead of squeezing the text.
pub const SETTINGS_WIDTH: f32 = 680.0;
pub const SETTINGS_TEXT_MIN: f32 = 220.0;
