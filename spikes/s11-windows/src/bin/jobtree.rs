//! The console build of the job tree test. See `s11_windows::jobtree`.

#[cfg(windows)]
fn main() {
    s11_windows::jobtree::main();
}

#[cfg(not(windows))]
fn main() {}
