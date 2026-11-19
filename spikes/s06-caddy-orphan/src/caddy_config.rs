//! The Caddy JSON config that the spike loads.

use serde_json::{Value, json};

/// The id that marks a server as ours. `GET /id/marsa-srv` on the admin API
/// answers 200 only when this config is loaded.
pub const SERVER_ID: &str = "marsa-srv";

/// One server on `127.0.0.1:<app_port>` that answers with a fixed text, no
/// HTTPS, and an access log (so Caddy writes to stderr on every request). The
/// admin API listens on `127.0.0.1:<admin_port>` and does not persist config.
/// With `tagged = false` the `@id` is left out: that is the "foreign Caddy".
pub fn build(app_port: u16, admin_port: u16, body: &str, tagged: bool) -> Value {
    let mut server = json!({
        "listen": [format!("127.0.0.1:{app_port}")],
        "automatic_https": { "disable": true },
        "logs": {},
        "routes": [{
            "handle": [{
                "handler": "static_response",
                "status_code": 200,
                "body": body
            }]
        }]
    });
    if tagged && let Some(map) = server.as_object_mut() {
        map.insert("@id".to_string(), Value::String(SERVER_ID.to_string()));
    }
    json!({
        "admin": {
            "listen": format!("127.0.0.1:{admin_port}"),
            "config": { "persist": false }
        },
        "apps": { "http": { "servers": { "srv": server } } }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tag_is_present_only_when_requested() {
        let tagged = build(1, 2, "x", true);
        let plain = build(1, 2, "x", false);
        assert_eq!(tagged["apps"]["http"]["servers"]["srv"]["@id"], SERVER_ID);
        assert!(plain["apps"]["http"]["servers"]["srv"]["@id"].is_null());
    }

    #[test]
    fn everything_binds_loopback() {
        let cfg = build(18106, 2106, "x", true);
        assert_eq!(cfg["admin"]["listen"], "127.0.0.1:2106");
        assert_eq!(
            cfg["apps"]["http"]["servers"]["srv"]["listen"][0],
            "127.0.0.1:18106"
        );
    }
}
