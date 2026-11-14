//! php-cgi for the sites on Windows, which has no php-fpm (ADR 0016). Task
//! M4-19b.
//!
//! Each PHP line gets one php-cgi on `127.0.0.1:<fallback port>` that keeps
//! its own workers (`PHP_FCGI_CHILDREN`) and replaces each one after its
//! request limit. So the daemon runs it like a php-fpm pool: one service
//! (`php-cgi-<minor>`), one address for Caddy, and one `php.ini`. php-cgi
//! has no config file of its own, so the pool's `fpm_conf` is empty.
//!
//! When the supervisor stops php-cgi (a kill on Windows), its workers end
//! with it: php-cgi keeps them in a Job Object of its own (spike S-11). And
//! the daemon's own Job Object ends all of them if the daemon dies (ADR 0017).

use std::path::Path;

use marsa_core::caddy::Dial;
use marsa_core::php::{PhpError, PhpFiles, cgi_pool, php_ini};
use marsa_core::recipes::Platform;
use marsa_core::services::ServiceSpec;
use semver::Version;

use super::{Pool, Skip, fallback_port, fpm_id};
use crate::daemon::caddy::{RESTART, STOP_GRACE};

/// The php-cgi pool of the installed PHP `version` under `data_dir`. Pure:
/// nothing is read or written. `php.ini` points to `ca_bundle` when it is
/// given.
///
/// # Errors
///
/// `php_path_not_allowed` if a path cannot go in `php.ini`.
pub fn pool_for(
    data_dir: &Path,
    version: &Version,
    ca_bundle: Option<&Path>,
) -> Result<Pool, Skip> {
    let mut files = PhpFiles::new(data_dir, version);
    files.ca_bundle = ca_bundle.map(Path::to_path_buf);
    pool_with(files)
}

/// [`pool_for`] with `files` as they are, so `php.ini` loads the Xdebug they
/// name (task M5-11).
///
/// # Errors
///
/// `php_path_not_allowed` if a path cannot go in `php.ini`.
pub fn pool_with(files: PhpFiles) -> Result<Pool, Skip> {
    let php_error = |err: PhpError| Skip {
        code: err.code(),
        reason: err.to_string(),
    };
    let ini = php_ini(Platform::WindowsX64, &files).map_err(php_error)?;
    let port = fallback_port(files.minor);
    let plan = cgi_pool(&files, port).map_err(php_error)?;
    let spec = ServiceSpec {
        id: fpm_id(files.minor),
        program: plan.program,
        args: plan.args,
        env: plan.env,
        cwd: None,
        // Like Caddy and php-fpm. A worker that reaches its request limit is
        // replaced by php-cgi itself; only the end of php-cgi counts here.
        restart: RESTART,
        stop_grace: STOP_GRACE,
    };
    Ok(Pool {
        files,
        fpm_conf: String::new(),
        php_ini: ini,
        spec,
        dial: Dial::Tcp(port),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn a_pool_is_one_php_cgi_on_the_fallback_port() {
        let data = PathBuf::from("/data");
        let version = Version::new(8, 4, 27);
        let bundle = data.join("caddy").join("ca-bundle.pem");
        let pool = pool_for(&data, &version, Some(&bundle)).unwrap();

        let files = PhpFiles::new(&data, &version);
        assert_eq!(pool.dial, Dial::Tcp(19084));
        assert!(pool.fpm_conf.is_empty(), "php-cgi has no config file");
        assert_eq!(pool.spec.id, fpm_id(files.minor));
        assert_eq!(pool.spec.program, files.php_cgi);
        let ini = files.ini_file.to_str().unwrap();
        assert_eq!(pool.spec.args, ["-b", "127.0.0.1:19084", "-c", ini]);
        let variables = BTreeMap::from([
            ("PHP_FCGI_CHILDREN".to_owned(), "4".to_owned()),
            ("PHP_FCGI_MAX_REQUESTS".to_owned(), "500".to_owned()),
        ]);
        assert_eq!(pool.spec.env, variables);
        assert_eq!(pool.spec.cwd, None);
        assert_eq!(pool.spec.restart, RESTART);

        // The Windows php.ini: the extensions, and the CA bundle.
        let ext = format!("extension_dir = \"{}\"", files.ext_dir.to_str().unwrap());
        assert!(pool.php_ini.contains(&ext), "{}", pool.php_ini);
        assert!(pool.php_ini.contains("extension = pdo_sqlite"));
        let cainfo = format!("curl.cainfo = \"{}\"", bundle.to_str().unwrap());
        assert!(pool.php_ini.contains(&cainfo));
        assert_eq!(pool.files.ca_bundle, Some(bundle));
    }

    #[test]
    fn a_path_that_php_ini_cannot_hold_gives_no_pool() {
        let data = PathBuf::from("/da\"ta");
        let skip = pool_for(&data, &Version::new(8, 4, 27), None).unwrap_err();
        assert_eq!(skip.code, "php_path_not_allowed");
    }
}
