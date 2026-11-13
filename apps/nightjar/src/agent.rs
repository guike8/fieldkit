//! Process-only integration. The provider client and scheduler remain in fieldkit-agent.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{
    io::Read,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_REPORT: u64 = 8 * 1024 * 1024;
#[derive(Clone, Debug)]
pub struct AgentConfig {
    pub sidecar: String,
    pub binary: String,
    pub base_url: String,
    pub model: String,
    pub key_env: String,
}
impl Default for AgentConfig {
    fn default() -> Self {
        let directory = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        Self {
            sidecar: directory.join("fieldkit-agent").display().to_string(),
            binary: directory.join("fieldkit").display().to_string(),
            base_url: "https://api.together.ai/v1".into(),
            model: "thinkingmachines/Inkling-2-Small".into(),
            key_env: "INKLING_API_KEY".into(),
        }
    }
}
impl AgentConfig {
    pub fn availability(&self) -> Result<()> {
        ensure!(
            Path::new(&self.sidecar).is_absolute() && Path::new(&self.sidecar).is_file(),
            "Choose an absolute path to the fieldkit-agent executable"
        );
        ensure!(
            Path::new(&self.binary).is_absolute() && Path::new(&self.binary).is_file(),
            "Choose an absolute path to the fieldkit executable"
        );
        ensure!(
            !self.key_env.is_empty()
                && self.key_env.len() <= 128
                && self
                    .key_env
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "Choose a valid API key environment variable name, not a secret"
        );
        ensure!(
            std::env::var(&self.key_env).is_ok_and(|key| !key.is_empty()),
            "Named key environment variable is absent or empty in the app process; launch from an environment with the key exported"
        );
        ensure!(
            self.base_url.starts_with("https://")
                && !self.base_url.contains(['@', '?', '#'])
                && !self.base_url.chars().any(char::is_whitespace),
            "Endpoint must be HTTPS without credentials, query or fragment"
        );
        ensure!(
            !self.model.trim().is_empty() && self.model.len() <= 256,
            "Enter the configured model identifier"
        );
        Ok(())
    }
}

/// Child group is terminated on cancel, timeout, shutdown or any error path.
struct Running(Child);
impl Drop for Running {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(-(self.0.id() as i32), libc::SIGKILL);
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn run(
    config: &AgentConfig,
    case: &Path,
    question: &str,
    mode: &str,
    seeds: &[String],
    collection: bool,
    cancel: &AtomicBool,
) -> Result<Value> {
    config.availability()?;
    let workspace = case.parent().context("Case must have a parent workspace")?;
    ensure!(case.is_absolute(), "Loaded case path must be absolute");
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let output: PathBuf = case.join(format!("nightjar-agent-{stamp}.json"));
    let mut command = Command::new(&config.sidecar);
    command
        .args(["--binary", &config.binary, "--workspace"])
        .arg(workspace)
        .arg("--case")
        .arg(case)
        .args([
            "--mode",
            mode,
            "--base-url",
            &config.base_url,
            "--model",
            &config.model,
            "--api-key-env",
            &config.key_env,
            "--timeout",
            "60",
            "--max-tokens",
            "2048",
            "--max-turns",
            "8",
            "--max-tool-calls",
            "16",
            "--max-total-tokens",
            "16384",
        ])
        .arg("--output")
        .arg(&output);
    for seed in seeds {
        command.arg("--seed").arg(seed);
    }
    if collection {
        command.arg("--allow-collection");
    }
    // End of options keeps a question beginning with '-' a positional question.
    command
        .arg("--")
        .arg(question)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = Running(
        command
            .spawn()
            .context("Could not launch configured sidecar")?,
    );
    let started = Instant::now();
    let status = loop {
        ensure!(
            !cancel.load(Ordering::Relaxed),
            "Agent cancelled. Already persisted case observations are retained; refresh to inspect them"
        );
        ensure!(
            started.elapsed() < Duration::from_secs(600),
            "Agent exceeded the 10-minute desktop deadline; stopped process group. Refresh to inspect partial case results"
        );
        if let Ok(metadata) = std::fs::symlink_metadata(&output) {
            ensure!(
                metadata.is_file() && metadata.len() <= MAX_REPORT,
                "Agent output is not a bounded regular JSON report"
            );
        }
        if let Some(status) = child.0.try_wait()? {
            break status;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let metadata = std::fs::symlink_metadata(&output).with_context(|| {
        format!(
            "Sidecar exited ({status}) without a report; check configuration. No success is assumed"
        )
    })?;
    ensure!(
        metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= MAX_REPORT,
        "Agent report is not a bounded regular file"
    );
    let mut bytes = Vec::new();
    std::fs::File::open(&output)?
        .take(MAX_REPORT + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_REPORT,
        "Agent report exceeds 8 MiB"
    );
    let mut report: Value =
        serde_json::from_slice(&bytes).context("Sidecar report is not valid JSON")?;
    validate_report(&report)?;
    // Defense in depth for an executable configured by the operator: never display its key.
    let key = std::env::var(&config.key_env).context("Named key disappeared")?;
    redact(&mut report, &key);
    Ok(
        serde_json::json!({"process_success":status.success(),"exit_code":status.code(),"report_path":output,"report":report,
        "notice":"Actual sidecar output; findings remain proposals until deliberate local review. Nonzero exits and stop_reason are not success."}),
    )
}

fn validate_report(report: &Value) -> Result<()> {
    ensure!(
        report["stop_reason"]
            .as_str()
            .is_some_and(|s| !s.is_empty())
            && report["final"].is_string()
            && report["trace"].is_array()
            && report["metrics"].is_object(),
        "Sidecar JSON is missing its actual stop reason, final text, trace or metrics; no successful investigation is assumed"
    );
    Ok(())
}

fn redact(value: &mut Value, key: &str) {
    match value {
        Value::String(s) => *s = s.replace(key, "[REDACTED]"),
        Value::Array(values) => {
            for value in values {
                redact(value, key);
            }
        }
        Value::Object(values) => {
            let original = std::mem::take(values);
            for (name, mut value) in original {
                redact(&mut value, key);
                values.insert(name.replace(key, "[REDACTED]"), value);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn malformed_report_is_not_success_and_secrets_are_redacted_structurally() {
        assert!(validate_report(&serde_json::json!({"success":true})).is_err());
        let mut value = serde_json::json!({"key\"value": ["key\"value"]});
        redact(&mut value, "key\"value");
        assert_eq!(value, serde_json::json!({"[REDACTED]":["[REDACTED]"]}));
    }
    #[test]
    fn missing_configuration_never_launches() {
        let config = AgentConfig {
            sidecar: "/definitely/missing/fieldkit-agent".into(),
            ..Default::default()
        };
        assert!(config.availability().is_err());
    }
    #[cfg(unix)]
    #[test]
    fn mock_sidecar_reports_actual_nonzero_status_without_network() {
        use std::os::unix::fs::PermissionsExt;
        let temp = tempfile::tempdir().unwrap();
        let sidecar = temp.path().join("mock-agent");
        // Rust-owned test fixture; no provider/client execution.
        std::fs::write(&sidecar, "#!/bin/sh\nwhile [ \"$#\" -gt 0 ]; do if [ \"$1\" = '--output' ]; then shift; output=$1; fi; shift; done\nprintf '%s' '{\"stop_reason\":\"budget_exhausted\",\"final\":\"SYNTHETIC mock sidecar fixture\",\"trace\":[],\"metrics\":{}}' > \"$output\"\nexit 2\n").unwrap();
        std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o700)).unwrap();
        let case = temp.path().join("case");
        std::fs::create_dir(&case).unwrap();
        // PATH is already present; this test never mutates the process environment.
        let config = AgentConfig {
            sidecar: sidecar.display().to_string(),
            binary: sidecar.display().to_string(),
            key_env: "PATH".into(),
            ..Default::default()
        };
        let result = run(
            &config,
            &case,
            "mock question",
            "person",
            &["mock".into()],
            false,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(result["process_success"], false);
        assert_eq!(result["report"]["stop_reason"], "budget_exhausted");
        assert!(
            result["report"]["final"]
                .as_str()
                .unwrap()
                .starts_with("SYNTHETIC")
        );
    }
    #[cfg(unix)]
    #[test]
    fn cancellation_stops_mock_sidecar() {
        let temp = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        let sidecar = temp.path().join("mock-wait");
        std::fs::write(&sidecar, "#!/bin/sh\nwhile :; do sleep 1; done\n").unwrap();
        std::fs::set_permissions(&sidecar, std::fs::Permissions::from_mode(0o700)).unwrap();
        let config = AgentConfig {
            sidecar: sidecar.display().to_string(),
            binary: sidecar.display().to_string(),
            key_env: "PATH".into(),
            ..Default::default()
        };
        let case = temp.path().join("case");
        std::fs::create_dir(&case).unwrap();
        let start = Instant::now();
        assert!(
            run(
                &config,
                &case,
                "mock",
                "person",
                &["mock".into()],
                false,
                &AtomicBool::new(true)
            )
            .is_err()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
