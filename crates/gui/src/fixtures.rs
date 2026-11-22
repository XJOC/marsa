//! Test data shared by the screens' tests: a connected model and an absolute
//! path on any system.

use std::path::PathBuf;

use marsa_core::types::{Health, Site, SiteKind, Source};
use semver::Version;

use crate::model::{Model, Update};

/// A model that has heard from the daemon.
pub(crate) fn connected() -> Model {
    let mut model = Model::default();
    model.update(Update::Connected(Health {
        app_version: "0.0.0".into(),
        api_version: 1,
        pid: 7,
    }));
    model
}

/// An absolute path on this system, from a Unix-style one: a drive on
/// Windows, where a path that starts with `/` is not absolute.
pub(crate) fn abs(unix: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(format!("C:{}", unix.replace('/', "\\")))
    } else {
        PathBuf::from(unix)
    }
}

/// A Laravel site `name` under `/code`, with PHP `php` from `source`.
pub(crate) fn site(name: &str, php: Option<&str>, source: Option<Source>) -> Site {
    let root = abs("/code").join(name);
    Site {
        name: name.into(),
        docroot: root.join("public"),
        root,
        kind: SiteKind::Laravel,
        php: php.map(|text| text.parse::<Version>().expect("a version")),
        php_source: source,
        secure: true,
    }
}
