//! The Visual C++ runtime that Windows reports as installed.

use std::ffi::OsStr;
use std::mem::size_of;
use std::ptr::null_mut;

use super::wide;

/// The Microsoft Visual C++ runtime (x64) that Windows reports as
/// installed, `(major, minor, build)`, from the key Microsoft documents for
/// it; `None` when it reports none (ADR 0019). Read only.
///
/// The key is in the 64-bit view on some machines, in the 32-bit one
/// (`Wow6432Node`, where Microsoft's page names it) on others, and in both
/// on the rest: both are read, and the newer installed version counts.
pub fn vc_runtime_version() -> Option<(u32, u32, u32)> {
    use windows_sys::Win32::System::Registry::{
        REG_ROUTINE_FLAGS, RRF_SUBKEY_WOW6432KEY, RRF_SUBKEY_WOW6464KEY,
    };
    [RRF_SUBKEY_WOW6464KEY, RRF_SUBKEY_WOW6432KEY]
        .into_iter()
        .filter_map(|view: REG_ROUTINE_FLAGS| vc_runtime_in(view))
        .max()
}

/// [`vc_runtime_version`] in one registry view.
fn vc_runtime_in(
    view: windows_sys::Win32::System::Registry::REG_ROUTINE_FLAGS,
) -> Option<(u32, u32, u32)> {
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::System::Registry::{
        HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RegGetValueW,
    };
    const KEY: &str = r"SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64";
    let key = wide(OsStr::new(KEY)).ok()?;
    let read = |name: &str| -> Option<u32> {
        let name = wide(OsStr::new(name)).ok()?;
        let mut value: u32 = 0;
        let mut size = size_of::<u32>() as u32;
        // SAFETY: `key` and `name` are NUL-terminated and live through the
        // call; `RRF_RT_REG_DWORD` takes a 4-byte value, which `value` is,
        // and `size` says so. The type out pointer may be null.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                name.as_ptr(),
                RRF_RT_REG_DWORD | view,
                null_mut(),
                (&raw mut value).cast(),
                &mut size,
            )
        };
        (status == ERROR_SUCCESS).then_some(value)
    };
    if read("Installed")? != 1 {
        return None;
    }
    Some((read("Major")?, read("Minor")?, read("Bld")?))
}
