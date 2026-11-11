//! The code of the two desktop programs (architecture.md section 3): the
//! window `marsa-gui` (`app`, `bridge`, `model`) and the menu-bar icon
//! `marsa-tray` (`tray`). Both are clients of the daemon through
//! `marsa_core::client`; neither changes state by itself. `startup` and
//! `strings` serve both.
//!
//! This library exists for the two binaries of this crate only. It is not a
//! public API.

pub mod app;
pub mod bridge;
pub mod model;
pub mod startup;
pub mod strings;
pub mod tray;
