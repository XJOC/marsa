//! The JSON shapes of `manifest.json` as `build` writes them.

use std::collections::BTreeMap;

use anyhow::Result;
use marsa_core::runtimes::Runtime;
use serde::{Deserialize, Serialize};

/// The JSON that `marsa_core::installer::manifest` reads. Field
/// order is the output order, and maps are `BTreeMap`, so the same inputs
/// give the same bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::manifest) struct ManifestJson {
    pub schema: u64,
    pub serial: u64,
    pub runtimes: RuntimesJson,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(in crate::manifest) struct RuntimesJson {
    #[serde(default)]
    pub php: Vec<ReleaseJson>,
    #[serde(default)]
    pub node: Vec<ReleaseJson>,
    #[serde(default)]
    pub composer: Vec<ReleaseJson>,
    #[serde(default)]
    pub caddy: Vec<ReleaseJson>,
    #[serde(default)]
    pub postgres: Vec<ReleaseJson>,
    /// Left out while empty, so a manifest without MySQL stays as it was.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mysql: Vec<ReleaseJson>,
    /// Left out while empty, as MySQL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub valkey: Vec<ReleaseJson>,
    /// Left out while empty, as MySQL.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mailpit: Vec<ReleaseJson>,
}

impl RuntimesJson {
    pub(in crate::manifest) fn get(&self, runtime: Runtime) -> &Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &self.php,
            Runtime::Node => &self.node,
            Runtime::Composer => &self.composer,
            Runtime::Caddy => &self.caddy,
            Runtime::Postgres => &self.postgres,
            Runtime::Mysql => &self.mysql,
            Runtime::Valkey => &self.valkey,
            Runtime::Mailpit => &self.mailpit,
        }
    }

    pub(super) fn get_mut(&mut self, runtime: Runtime) -> &mut Vec<ReleaseJson> {
        match runtime {
            Runtime::Php => &mut self.php,
            Runtime::Node => &mut self.node,
            Runtime::Composer => &mut self.composer,
            Runtime::Caddy => &mut self.caddy,
            Runtime::Postgres => &mut self.postgres,
            Runtime::Mysql => &mut self.mysql,
            Runtime::Valkey => &mut self.valkey,
            Runtime::Mailpit => &mut self.mailpit,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::manifest) struct ReleaseJson {
    pub version: String,
    pub platforms: BTreeMap<String, Vec<SourceJson>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::manifest) struct SourceJson {
    pub name: String,
    pub urls: Vec<String>,
    /// 64 lowercase hex characters.
    pub sha256: String,
    pub size: u64,
}

/// The manifest as written to disk: pretty JSON and a final newline.
pub(in crate::manifest) fn to_json(manifest: &ManifestJson) -> Result<String> {
    Ok(serde_json::to_string_pretty(manifest)? + "\n")
}
