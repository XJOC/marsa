//! Every user-visible string of the egui app, in one place (AGENTS.md
//! section 9).

pub const APP_ID: &str = "s09-egui-app";
pub const WINDOW_TITLE: &str = "S-09 egui";
/// Same logical size as the Tauri window.
pub const WINDOW_SIZE: [f32; 2] = [1100.0, 760.0];
pub const TAB_TABLE: &str = "Table";
pub const TAB_SQL: &str = "SQL";
pub const TAB_LOGS: &str = "Logs";
pub const RENDERER_LABEL: &str = "renderer:";
pub const USAGE: &str = "usage: egui-app [--renderer glow|wgpu] [--bench]";
