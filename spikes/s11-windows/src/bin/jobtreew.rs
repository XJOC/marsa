//! The job tree parent built for the windows subsystem, like `marsad`
//! (architecture.md section 3): it has no console of its own.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
fn main() {
    s11_windows::jobtree::main();
}

#[cfg(not(windows))]
fn main() {}
