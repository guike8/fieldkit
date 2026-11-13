use fieldkit_agent::{
    Chat,
    benchmark::{REPLAY_MODEL, ScriptedClient},
};
use serde_json::{Value, json};
use std::{
    os::unix::fs::PermissionsExt,
    process::{Command, Output},
    time::Duration,
};
fn command() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_fieldkit-agent-benchmark"));
    c.env_remove("INKLING_API_KEY")
        .env_remove("OPENAI_API_KEY")
        .env_remove("INKLING_BASE_URL")
        .env_remove("INKLING_MODEL");
    c
}
fn parse(o: &Output) -> Value {
    assert!(
        o.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    serde_json::from_slice(&o.stdout).unwrap()
}
#[test]
fn crypto_scope_is_accepted_without_starting_a_provider_or_collection() {
    use clap::Parser;
    use fieldkit_agent::cli::AgentArgs;
    let target = format!("ethereum:0x{}", "a".repeat(40));
    let args = AgentArgs::try_parse_from([
        "fieldkit-agent",
        "What is directly observable for this seed?",
        "--binary",
        "/not-executed/fieldkit",
        "--workspace",
        "/not-opened",
        "--case",
        "/not-opened/case",
        "--mode",
        "crypto",
        "--seed",
        &target,
    ])
    .unwrap();
    assert_eq!(args.mode, "crypto");
    assert_eq!(args.seeds, vec![target]);
    assert!(!args.allow_collection);
}
#[test]
fn absolute_binary_replay_needs_neither_python_credentials_nor_working_directory() {
    let out = command()
        .current_dir("/")
        .args(["--scenario", "seeded_infrastructure"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let r = parse(&out);
    assert_eq!(r["aggregate"]["passed"], 1);
    assert_eq!(r["model"], REPLAY_MODEL);
    assert_eq!(r["config"]["base_url_used"], false);
    assert!(r["scenarios"][0].get("trace").is_none());
}
#[test]
fn exclusive_private_full_trace_output_and_no_stdout() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("report.json");
    let out = command()
        .args(["--scenario", "citation_fidelity", "--output"])
        .arg(&p)
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
    let before = std::fs::read(&p).unwrap();
    let r: Value = serde_json::from_slice(&before).unwrap();
    assert_eq!(r["scenarios"][0]["passed"], true);
    assert!(!r["scenarios"][0]["trace"].as_array().unwrap().is_empty());
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let out = command().arg("--output").arg(&p).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(std::fs::read(&p).unwrap(), before);
}
#[test]
fn missing_live_key_invalid_constructor_and_report_url_scrubbing() {
    let out = command()
        .args([
            "--mode",
            "live",
            "--api-key-env",
            "NONEXISTENT_SYNTHETIC_KEY",
        ])
        .env_remove("NONEXISTENT_SYNTHETIC_KEY")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    let out = command()
        .env("INKLING_API_KEY", "synthetic-secret")
        .args([
            "--mode",
            "live",
            "--base-url",
            "https://user:synthetic-secret@host/v1",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(out.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("synthetic-secret"));
    let out = command()
        .args([
            "--scenario",
            "blocked_stop",
            "--base-url",
            "https://name:password@model.example/v1?key=secret#fragment",
        ])
        .output()
        .unwrap();
    assert!(out.status.success());
    let r = parse(&out);
    assert_eq!(r["config"]["base_url"], "https://model.example/v1");
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(!text.contains("password"));
    assert!(!text.contains("key=secret"));
}
#[test]
fn failed_budgets_and_invalid_cli_limits_have_nonzero_status() {
    let out = command()
        .args(["--scenario", "citation_fidelity", "--max-turns", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(parse(&out)["scenarios"][0]["stop_reason"], "turn_limit");
    for args in [
        ["--max-turns", "0"],
        ["--timeout", "NaN"],
        ["--timeout", "inf"],
        ["--timeout", "0"],
        ["--max-total-tokens", "-1"],
    ] {
        let out = command().args(args).output().unwrap();
        assert_eq!(out.status.code(), Some(2));
    }
}
#[test]
fn live_cli_uses_only_mock_http_and_synthetic_tools_scrubs_echoed_escaped_key() {
    let secret = "synthetic-secret-\"\\token";
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", server.server_addr());
    let worker = std::thread::spawn(move || {
        let mut client = ScriptedClient::new("citation_fidelity").unwrap();
        let mut requests = 0;
        loop {
            let mut request = server
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
                .expect("mock request");
            assert_eq!(request.url(), "/v1/chat/completions");
            assert_eq!(
                request
                    .headers()
                    .iter()
                    .find(|h| h.field.equiv("Authorization"))
                    .unwrap()
                    .value
                    .as_str(),
                format!("Bearer {secret}")
            );
            let payload: Value = serde_json::from_reader(request.as_reader()).unwrap();
            assert_eq!(payload["model"], "test-model");
            assert_eq!(payload["parallel_tool_calls"], false);
            assert_eq!(payload["stream"], false);
            let mut reply = client
                .complete(
                    payload["messages"].as_array().unwrap(),
                    payload["tools"].as_array().unwrap(),
                )
                .unwrap();
            let done = reply["choices"][0]["message"]["tool_calls"]
                .as_array()
                .unwrap()
                .is_empty();
            if done {
                let text = reply["choices"][0]["message"]["content"].as_str().unwrap();
                reply["choices"][0]["message"]["content"] = json!(format!("{text} {secret}"));
            }
            request
                .respond(tiny_http::Response::from_string(reply.to_string()))
                .unwrap();
            requests += 1;
            if done {
                return requests;
            }
        }
    });
    let out = command()
        .env("SYNTHETIC_KEY", secret)
        .args([
            "--mode",
            "live",
            "--scenario",
            "citation_fidelity",
            "--model",
            "test-model",
            "--api-key-env",
            "SYNTHETIC_KEY",
            "--base-url",
            &base,
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let r = parse(&out);
    assert_eq!(r["mode"], "live");
    assert_eq!(r["model"], "test-model");
    assert_eq!(r["aggregate"]["passed"], 1);
    assert!(
        !r["scenarios"][0]["final"]
            .as_str()
            .unwrap()
            .contains(secret)
    );
    assert!(
        r["scenarios"][0]["final"]
            .as_str()
            .unwrap()
            .contains("[REDACTED]")
    );
    assert!(worker.join().unwrap() > 1);
}
#[test]
fn mock_provider_failure_is_failed_json_with_unknown_usage_not_key_details() {
    let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", server.server_addr());
    let worker = std::thread::spawn(move || {
        let request = server
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        request
            .respond(
                tiny_http::Response::from_string("provider leaked synthetic-secret")
                    .with_status_code(429),
            )
            .unwrap();
    });
    let out = command()
        .env("INKLING_API_KEY", "synthetic-secret")
        .args([
            "--mode",
            "live",
            "--scenario",
            "blocked_stop",
            "--base-url",
            &base,
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let r = parse(&out);
    assert_eq!(r["aggregate"]["failed"], 1);
    assert_eq!(r["aggregate"]["token_usage_complete"], false);
    assert!(r["scenarios"][0]["metrics"]["prompt_tokens"].is_null());
    assert_eq!(r["scenarios"][0]["stop_reason"], "error");
    assert!(!String::from_utf8_lossy(&out.stdout).contains("synthetic-secret"));
    worker.join().unwrap();
}
