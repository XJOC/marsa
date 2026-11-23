//! Xdebug state and request, with their JSON tests.

use serde::{Deserialize, Serialize};

/// `GET /v1/php/xdebug`, and the answer to `PUT /v1/php/xdebug`:
/// whether the `php.ini` files Marsa writes load Xdebug, and for which PHP.
///
/// Each PHP line (`8.4`) has one `php.ini`, so each list has at most one
/// version per line: the one its php-fpm runs, or the newest installed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct XdebugState {
    /// `[php] xdebug` in `global.toml`.
    pub enabled: bool,
    /// The PHP versions that load Xdebug, oldest first, in sites and in the
    /// terminal. Empty while `enabled` is `false`. Full semver text
    /// (`8.4.23`).
    #[serde(default)]
    pub loaded: Vec<String>,
    /// The PHP versions that have no `ext/xdebug.so` (the official Windows
    /// build, or an older install), oldest first. They run without Xdebug
    /// also while `enabled` is `true`.
    #[serde(default)]
    pub missing: Vec<String>,
}

/// The body of `PUT /v1/php/xdebug`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct XdebugRequest {
    /// `true` loads Xdebug in every PHP that has it, `false` in none.
    pub enabled: bool,
}

/// The JSON of the Xdebug types. A module of its own, like `autostart_tests`.
#[cfg(test)]
mod xdebug_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn xdebug_state_and_request() {
        let state = XdebugState {
            enabled: true,
            loaded: vec!["8.4.23".into()],
            missing: vec!["8.3.1".into()],
        };
        let text = json!({ "enabled": true, "loaded": ["8.4.23"], "missing": ["8.3.1"] });
        assert_eq!(serde_json::to_value(&state).unwrap(), text);
        assert_eq!(serde_json::from_value::<XdebugState>(text).unwrap(), state);
        // The lists may be left out.
        let off: XdebugState = serde_json::from_value(json!({ "enabled": false })).unwrap();
        assert_eq!(off, XdebugState::default());

        let request = XdebugRequest { enabled: true };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({ "enabled": true })
        );
        for bad in [json!({}), json!({ "enabled": "on" })] {
            assert!(serde_json::from_value::<XdebugRequest>(bad).is_err());
        }
    }
}
