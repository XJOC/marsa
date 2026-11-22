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
// build reads no extra environment variable.
#[cfg(debug_assertions)]
pub mod demo;
// What a line of a program's output says: its level and its message.
pub mod logline;
// The measurement mode: made-up data and a frame count, for a release build
// with no daemon. It needs an empty data folder. Behind the
// cargo feature `measure`, off by default, so a shipped binary has no made-up
// data in it.
#[cfg(feature = "measure")]
pub mod measure;
pub mod model;
// The menu-bar panel's window mode, `marsa-gui --panel`.
pub mod panel;
// The review aids (demo, screenshot, gallery, measurement), behind the
// `cfg`s that keep them out of a shipped binary.
pub(crate) mod review;
// The system's opener of a folder: only `app` runs it.
pub(crate) mod opener;
// The window's own preferences, in `config/gui.toml`.
pub mod prefs;
// The window's own screens: only `shell` draws them.
pub(crate) mod screens;
// Debug builds only: a design-review aid, so a release build reads no extra
// environment variable.
#[cfg(debug_assertions)]
pub mod screenshot;
// The sidebar and the router: only `app` draws them.
pub(crate) mod shell;
pub mod startup;
pub mod strings;
pub mod theme;
pub mod tray;
pub mod widgets;
