//! Autostart (login item) state and request, with the JSON tests of both.

use serde::{Deserialize, Serialize};

/// `GET /v1/autostart`, and the answer to `PUT /v1/autostart`:
/// whether the tray icon starts at login. At most one of the two is `true`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartState {
    /// The login file is there and starts this Marsa's `marsa-tray`.
    pub enabled: bool,
    /// A login file is there, but not the one this Marsa writes: it starts a
    /// `marsa-tray` at another path (the app moved), or it was changed. Login
    /// may not start Marsa. `PUT` with `enabled: true` writes it again.
    #[serde(default)]
    pub stale: bool,
}

/// The body of `PUT /v1/autostart`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutostartRequest {
    /// `true` writes the login file, `false` deletes it.
    pub enabled: bool,
}

/// The JSON of the autostart types. A module of its own, next to them, so
/// this task and the others that edit this file do not touch the same lines.
#[cfg(test)]
mod autostart_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn autostart_state_and_request() {
        let stale = AutostartState {
            enabled: false,
            stale: true,
        };
        assert_eq!(
            serde_json::to_value(stale).unwrap(),
            json!({ "enabled": false, "stale": true })
        );
        let back: AutostartState =
            serde_json::from_value(json!({ "enabled": false, "stale": true })).unwrap();
        assert_eq!(back, stale);
        // `stale` may be left out.
        let on: AutostartState = serde_json::from_value(json!({ "enabled": true })).unwrap();
        assert_eq!(
            on,
            AutostartState {
                enabled: true,
                stale: false
            }
        );

        let request = AutostartRequest { enabled: true };
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({ "enabled": true })
        );
        for bad in [
            json!({}),
            json!({ "enabled": "yes" }),
            json!({ "enabled": 1 }),
        ] {
            assert!(serde_json::from_value::<AutostartRequest>(bad).is_err());
        }
    }
}
