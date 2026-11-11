use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub command: String,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default = "empty_object")]
    pub options: Value,
}

fn empty_object() -> Value {
    json!({})
}

#[derive(Clone, Debug)]
pub struct Config {
    pub timeout_secs: u64,
    pub max_requests: usize,
    pub max_results: usize,
    pub max_bytes: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            timeout_secs: 10,
            max_requests: 64,
            max_results: 50,
            max_bytes: 4 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Collected {
    pub report: Value,
    pub blobs: BTreeMap<String, Vec<u8>>,
}

pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
pub fn id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::new_v4().simple())
}
pub fn digest(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(bytes))
}
pub fn entity_id(kind: &str, value: &Value) -> String {
    format!(
        "entity_{}",
        digest(
            serde_json::to_string(&json!([kind, value]))
                .unwrap()
                .as_bytes()
        )
    )
}
pub fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
pub fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}
