//! The roots Windows trusts for HTTPS servers, for PHP's CA bundle.

use std::ffi::OsStr;
use std::io;
use std::mem::size_of;
use std::ptr::{null, null_mut};

use super::wide;

/// The roots in Windows' stores, for PHP's CA bundle (ADR 0020).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Roots {
    /// The DER of each root that Windows trusts for HTTPS servers now.
    pub trusted: Vec<Vec<u8>>,
    /// The roots left out: not for HTTPS servers, expired or not valid yet,
    /// also in the `Disallowed` store, or not a certificate.
    pub left_out: usize,
}

/// The roots this user's Windows trusts for HTTPS servers. The `ROOT` store
/// of the current user holds the machine's roots (`LocalMachine\Root`,
/// where `marsa setup` puts Marsa's CA), group policy's, and the user's
/// own. Windows keeps a root's limits in the store, not in the certificate,
/// so the ones that matter here are applied: the enhanced key usage, the
/// dates, and the `Disallowed` store. Windows' newer distrust data (the
/// disallowed list it downloads, distrust dates) is not, as in curl's own
/// import. Read only.
///
/// Windows may add a public root only when its own certificate check first
/// needs one (automatic root update), so a fresh machine may lack a root
/// for a while.
///
/// # Errors
///
/// A store cannot be opened or read.
pub fn root_certificates() -> io::Result<Roots> {
    use windows_sys::Win32::Foundation::ERROR_FILE_NOT_FOUND;
    use windows_sys::Win32::Security::Cryptography::{
        CERT_SYSTEM_STORE_CURRENT_USER, CERT_SYSTEM_STORE_LOCAL_MACHINE,
    };
    let missing = |err: &io::Error| err.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32);
    // The user's `Disallowed` holds the machine's too. A profile without one
    // falls back to the machine's own; with neither, nothing is disallowed.
    let disallowed = match store_certificates("Disallowed", CERT_SYSTEM_STORE_CURRENT_USER) {
        Err(err) if missing(&err) => {
            match store_certificates("Disallowed", CERT_SYSTEM_STORE_LOCAL_MACHINE) {
                Err(err) if missing(&err) => Vec::new(),
                other => other?,
            }
        }
        other => other?,
    };
    let disallowed: Vec<Vec<u8>> = disallowed.into_iter().map(|(der, _)| der).collect();
    let mut roots = Roots::default();
    for (der, for_servers) in store_certificates("ROOT", CERT_SYSTEM_STORE_CURRENT_USER)? {
        if for_servers && !disallowed.contains(&der) && crate::helper::ca::is_certificate(&der) {
            roots.trusted.push(der);
        } else {
            roots.left_out += 1;
        }
    }
    Ok(roots)
}

/// The DER of each certificate in the system store `name` of `location`
/// (`CERT_SYSTEM_STORE_CURRENT_USER` or `_LOCAL_MACHINE`), and whether it is
/// valid now for HTTPS servers ([`for_https_servers`]). The store is opened
/// read-only, and only if it exists.
pub(super) fn store_certificates(name: &str, location: u32) -> io::Result<Vec<(Vec<u8>, bool)>> {
    use windows_sys::Win32::Foundation::{CRYPT_E_NOT_FOUND, ERROR_NO_MORE_FILES};
    use windows_sys::Win32::Security::Cryptography::{
        CERT_CONTEXT, CERT_STORE_OPEN_EXISTING_FLAG, CERT_STORE_PROV_SYSTEM_W,
        CERT_STORE_READONLY_FLAG, CertCloseStore, CertEnumCertificatesInStore, CertOpenStore,
    };
    let name = wide(OsStr::new(name))?;
    let flags = location | CERT_STORE_READONLY_FLAG | CERT_STORE_OPEN_EXISTING_FLAG;
    // SAFETY: the system-store provider takes `pvPara` as a NUL-terminated
    // UTF-16 name, which `name` is, and it lives through the call. No
    // encoding type and no provider (0) for a system store. The store is
    // checked for null and closed below.
    let store =
        unsafe { CertOpenStore(CERT_STORE_PROV_SYSTEM_W, 0, 0, flags, name.as_ptr().cast()) };
    if store.is_null() {
        return Err(io::Error::last_os_error());
    }
    let mut found = Vec::new();
    let mut previous: *const CERT_CONTEXT = null();
    let end = loop {
        // SAFETY: `store` is open, and `previous` is null or the context
        // the last call gave, which this call frees.
        let context = unsafe { CertEnumCertificatesInStore(store, previous) };
        if context.is_null() {
            // Read at once: the end of the store, or why the read stopped.
            break io::Error::last_os_error();
        }
        // SAFETY: `context` is valid until the next call frees it, and its
        // `pbCertEncoded` points at `cbCertEncoded` bytes, copied out here
        // (a null pointer is skipped: a slice needs a real one).
        let der = unsafe {
            let cert = &*context;
            (!cert.pbCertEncoded.is_null()).then(|| {
                std::slice::from_raw_parts(cert.pbCertEncoded, cert.cbCertEncoded as usize).to_vec()
            })
        };
        if let Some(der) = der {
            // SAFETY: `context` is valid until the next enumeration.
            found.push((der, unsafe { for_https_servers(context) }));
        }
        previous = context;
    };
    // SAFETY: `store` is open and closed once, here. Every context was freed
    // by the call after it, and the last call returned none. The result is
    // not read: a read-only store with no context left has nothing to lose.
    unsafe {
        CertCloseStore(store, 0);
    }
    match end.raw_os_error() {
        Some(code) if code == CRYPT_E_NOT_FOUND || code == ERROR_NO_MORE_FILES as i32 => Ok(found),
        _ => Err(end),
    }
}

/// Whether Windows trusts the certificate of `context` for HTTPS servers
/// now: within its dates, and with an enhanced key usage (the certificate's
/// own, narrowed by the store's property) that is "any" or names server
/// authentication. Any failure leaves the certificate out.
///
/// # Safety
///
/// `context` points at a valid certificate context for the whole call.
pub(super) unsafe fn for_https_servers(
    context: *const windows_sys::Win32::Security::Cryptography::CERT_CONTEXT,
) -> bool {
    use std::ffi::CStr;
    use windows_sys::Win32::Foundation::{CRYPT_E_NOT_FOUND, SetLastError};
    use windows_sys::Win32::Security::Cryptography::{
        CTL_USAGE, CertGetEnhancedKeyUsage, CertVerifyTimeValidity,
    };
    const SERVER_AUTH: &[u8] = b"1.3.6.1.5.5.7.3.1";
    // SAFETY: `context` is valid for this call (the caller holds it until
    // its next enumeration), and so is its `pCertInfo`. A null time means
    // now.
    if unsafe { CertVerifyTimeValidity(null(), (*context).pCertInfo) } != 0 {
        return false;
    }
    let mut size: u32 = 0;
    // SAFETY: a null buffer asks for the size only, written to `size`.
    if unsafe { CertGetEnhancedKeyUsage(context, 0, null_mut(), &mut size) } == 0 {
        return false;
    }
    // `u64`s, so the buffer is aligned for the pointer in `CTL_USAGE`, and
    // never smaller than one `CTL_USAGE`.
    let words = (size as usize).max(size_of::<CTL_USAGE>()).div_ceil(8);
    let mut buffer = vec![0_u64; words];
    let usage = buffer.as_mut_ptr().cast::<CTL_USAGE>();
    // SAFETY: `buffer` holds at least `size` bytes and a whole `CTL_USAGE`,
    // aligned for it, and lives through the call and the reads below. The
    // last error is cleared first, so the answer read below is this call's.
    let read = unsafe {
        SetLastError(0);
        CertGetEnhancedKeyUsage(context, 0, usage, &mut size)
    };
    if read == 0 {
        return false;
    }
    // Read at once: with no usage listed, it says "all uses" or "none".
    let reason = io::Error::last_os_error().raw_os_error();
    // SAFETY: Windows filled `usage`: `cUsageIdentifier` pointers to
    // NUL-terminated OID strings, all inside `buffer`.
    unsafe {
        let usage = &*usage;
        if usage.cUsageIdentifier == 0 {
            return reason == Some(CRYPT_E_NOT_FOUND);
        }
        std::slice::from_raw_parts(usage.rgpszUsageIdentifier, usage.cUsageIdentifier as usize)
            .iter()
            .any(|&oid| CStr::from_ptr(oid.cast()).to_bytes() == SERVER_AUTH)
    }
}
