//! Bounded, provider-independent investigator; no approval or generic execution tools.
pub mod agent;
pub mod benchmark;
pub mod cli;
pub mod scope;
pub mod transport;

pub use agent::{Budgets, Chat, run_agent};
pub use scope::{ScopedTools, ToolBackend};
pub use transport::{ChatClient, McpClient};

use anyhow::Result;
use serde_json::Value;
use std::{fs::OpenOptions, io::Write, path::Path};

pub const MODEL: &str = "thinkingmachines/Inkling-2-Small";
pub const BASE_URL: &str = "https://api.together.ai/v1";
pub const MAX_MESSAGE: usize = 1024 * 1024;
pub const MAX_HTTP: u64 = 8 * MAX_MESSAGE as u64;
pub const ALLOWED_TOOLS: &[&str] = &[
    "fieldkit_capabilities",
    "fieldkit_investigation_create",
    "fieldkit_investigation_plan_step",
    "fieldkit_investigation_run_step",
    "fieldkit_investigation_evidence",
    "fieldkit_investigation_source",
    "fieldkit_investigation_select_post",
    "fieldkit_investigation_show",
    "fieldkit_investigation_propose",
    "fieldkit_graph_show",
    "fieldkit_graph_focus",
];

/// Exclusive creation, including dangling symlinks. Never relax the caller's umask.
pub fn save_report(path: &Path, report: &Value) -> Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    serde_json::to_writer(&mut file, report)?;
    file.write_all(b"\n")?;
    Ok(())
}

/// Redact values AND object keys before serializing, including JSON-escaped secrets.
pub fn redact(value: &Value, secret: &str) -> Value {
    if secret.is_empty() {
        return value.clone();
    }
    match value {
        Value::String(s) => Value::String(s.replace(secret, "[REDACTED]")),
        Value::Array(a) => a.iter().map(|v| redact(v, secret)).collect(),
        Value::Object(m) => Value::Object(
            m.iter()
                .map(|(k, v)| (k.replace(secret, "[REDACTED]"), redact(v, secret)))
                .collect(),
        ),
        _ => value.clone(),
    }
}
