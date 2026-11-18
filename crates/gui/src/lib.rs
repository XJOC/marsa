//! The code of the two desktop programs (architecture.md section 3): the
//! window `marsa-gui` (`app`, `bridge`, `model`, `shell`, `screens`,
//! `theme`, `widgets`) and the menu-bar icon `marsa-tray` (`tray`). Both are
//! clients of the daemon through `marsa_core::client`; neither changes state
//! by itself. `startup` and `strings` serve both.
//!
//! This library exists for the two binaries of this crate only. It is not a
//! public API.

pub mod app;
pub mod bridge;
// Debug builds only: fixed fake data for the design review, so a release
// build reads no extra environment variable (task M5-D5).
#[cfg(debug_assertions)]
pub mod demo;
pub mod model;
// The system's opener of a folder: only `app` runs it.
pub(crate) mod opener;
// The window's own preferences, in `config/gui.toml` (task M5-D9).
pub mod prefs;
// The window's own screens: only `shell` draws them.
pub(crate) mod screens;
// Debug builds only: a design-review aid, so a release build reads no extra
// environment variable (task M5-D1).
#[cfg(debug_assertions)]
pub mod screenshot;
// The sidebar and the router: only `app` draws them.
pub(crate) mod shell;
pub mod startup;
pub mod strings;
pub mod theme;
pub mod tray;
pub mod widgets;
