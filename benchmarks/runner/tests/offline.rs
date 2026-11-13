//! Synthetic offline tests. No historical bodies or network requests are involved.
use fieldkit_benchmarks::{common::*, transport::Client};
use serde_json::json;
use std::{
    path::Path,
    process::Command,
    time::{Duration, Instant},
};
fn command(mode: &str) -> Vec<String> {
    vec![
        env!("CARGO_BIN_EXE_benchmark-test-peer").into(),
        mode.into(),
    ]
}
#[test]
fn real_subprocess_roundtrip_and_private_wire_log() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("wire.jsonl");
    let mut c = Client::new(command("ok"), Some(Output::new(&path).unwrap())).unwrap();
    assert_eq!(
        c.tool("fieldkit_case_verify", json!({})).unwrap(),
        json!({"ok":true})
    );
    c.close().unwrap();
    let lines = std::fs::read_to_string(&path).unwrap();
    assert_eq!(lines.lines().count(), 2);
    for line in lines.lines() {
        let v: serde_json::Value = serde_json::from_str(line).unwrap();
        assert_eq!(v["request"]["id"], v["response"]["id"]);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn partial_line_deadline_covers_blocked_read() {
    let start = Instant::now();
    assert!(Client::with_timeout(command("partial"), None, Duration::from_millis(150)).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
}
#[test]
fn malformed_closed_wrong_id_rpc_error_and_oversized_are_rejected() {
    for mode in ["malformed", "closed", "wrong-id", "rpc-error", "oversized"] {
        assert!(
            Client::with_timeout(command(mode), None, Duration::from_secs(3)).is_err(),
            "mode {mode}"
        );
    }
}
#[test]
fn mcp_tool_error_is_not_promoted_to_data() {
    let mut c = Client::new(command("tool-error"), None).unwrap();
    assert!(c.tool("fieldkit_case_verify", json!({})).is_err());
    c.close().unwrap();
}
#[test]
fn exclusive_outputs_are_private_and_existing_files_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipt");
    let mut output = Output::new(&path).unwrap();
    output
        .checkpoint(&json!({"status":"attempting","long":"previous contents"}))
        .unwrap();
    output.checkpoint(&json!({"status":"incomplete"})).unwrap();
    assert_eq!(read_json(&path).unwrap(), json!({"status":"incomplete"}));
    assert!(Output::new(&path).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
#[test]
fn source_blobs_reject_mutation_oversize_symlinks_and_traversal() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("blobs")).unwrap();
    let raw = b"original";
    let digest = hash(raw);
    let path = root.path().join("blobs").join(&digest);
    std::fs::write(&path, raw).unwrap();
    assert_eq!(blob(root.path(), &digest).unwrap(), raw);
    std::fs::write(&path, b"mutation").unwrap();
    assert!(blob(root.path(), &digest).is_err());
    assert!(blob(root.path(), "../../other").is_err());
    let oversized = root.path().join("big");
    std::fs::File::create(&oversized)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    assert!(bounded_read(&oversized).is_err());
    #[cfg(unix)]
    {
        std::fs::remove_file(&path).unwrap();
        std::os::unix::fs::symlink(&oversized, &path).unwrap();
        assert!(blob(root.path(), &digest).is_err());
    }
}
fn store_fixture(root: &Path) -> serde_json::Value {
    std::fs::create_dir(root.join("blobs")).unwrap();
    let digest = hash(b"source");
    std::fs::write(root.join("blobs").join(&digest), b"source").unwrap();
    let report = json!({"run_id":"run","sources":[{"source_id":"src","sha256":digest}]});
    let raw = serde_json::to_string(&report).unwrap();
    let db = rusqlite::Connection::open(root.join("case.sqlite3")).unwrap();
    db.execute_batch("CREATE TABLE runs(run_id TEXT,result_json TEXT,report_sha256 TEXT);CREATE TABLE wb_items(kind TEXT);").unwrap();
    db.execute(
        "INSERT INTO runs VALUES(?1,?2,?3)",
        rusqlite::params!["run", raw, hash(raw.as_bytes())],
    )
    .unwrap();
    report
}
#[test]
fn independent_sqlite_hashes_and_mcp_equality() {
    let root = tempfile::tempdir().unwrap();
    let report = store_fixture(root.path());
    let before = std::fs::read(root.path().join("case.sqlite3")).unwrap();
    let s = stored(root.path()).unwrap();
    assert_eq!(s.reviews, 0);
    let reports = Records::from([("item".into(), report)]);
    bind_stored(&reports, &s).unwrap();
    assert_eq!(
        std::fs::read(root.path().join("case.sqlite3")).unwrap(),
        before
    );
    let mut forged = reports.clone();
    forged.get_mut("item").unwrap()["status"] = json!("ok");
    assert!(bind_stored(&forged, &s).is_err());
    let db = rusqlite::Connection::open(root.path().join("case.sqlite3")).unwrap();
    db.execute("UPDATE runs SET result_json='{}'", []).unwrap();
    assert!(stored(root.path()).is_err());
}
#[test]
fn independent_sqlite_source_hashes_and_reviews() {
    let root = tempfile::tempdir().unwrap();
    store_fixture(root.path());
    let db = rusqlite::Connection::open(root.path().join("case.sqlite3")).unwrap();
    db.execute("INSERT INTO wb_items VALUES('investigation_reviews')", [])
        .unwrap();
    assert_eq!(stored(root.path()).unwrap().reviews, 1);
    std::fs::write(root.path().join("blobs").join(hash(b"source")), b"changed").unwrap();
    assert!(stored(root.path()).is_err());
}
#[test]
fn readonly_sqlite_never_creates_missing_database() {
    let root = tempfile::tempdir().unwrap();
    assert!(stored(root.path()).is_err());
    assert!(!root.path().join("case.sqlite3").exists());
}
#[test]
fn source_requires_exactly_one_canonical_body_binding() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("blobs")).unwrap();
    let digest = hash(b"body");
    std::fs::write(root.path().join("blobs").join(&digest), b"body").unwrap();
    let mut report = json!({"data":{"body_sha256":digest,"source_ids":["s"]},"sources":[{"source_id":"s","sha256":digest}]});
    original_source(&report, root.path()).unwrap();
    let duplicate = report["sources"][0].clone();
    report["sources"].as_array_mut().unwrap().push(duplicate);
    assert!(original_source(&report, root.path()).is_err());
    report["data"]["body_sha256"] = json!("../".repeat(22));
    assert!(original_source(&report, root.path()).is_err());
}
#[test]
fn json_pointer_strict_escapes_arrays_and_nonroot() {
    let value = json!({"a/b":{"~":["ok"]}});
    assert_eq!(pointer_value(&value, "/a~1b/~0/0").unwrap(), "ok");
    for p in ["", "/a~2b", "/a~1b/~0/-1", "/missing"] {
        assert!(pointer_value(&value, p).is_err());
    }
}
#[test]
fn cli_rejects_reused_receipt_before_launching_server() {
    let root = tempfile::tempdir().unwrap();
    let receipt = root.path().join("receipt");
    std::fs::write(&receipt, b"sentinel").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fieldkit-benchmarks"))
        .args([
            "south-africa",
            "capture",
            "--command-json",
            "[\"/nonexistent/no-server\"]",
            "--receipt",
        ])
        .arg(&receipt)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("Create new private output"), "{stderr}");
    assert_eq!(std::fs::read(receipt).unwrap(), b"sentinel");
}
#[test]
fn cli_capture_does_not_need_oracle_even_when_initialization_fails() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("task.json"), b"{\"id\":\"synthetic\"}").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_fieldkit-benchmarks"))
        .args(["south-africa", "capture", "--command-json"])
        .arg(serde_json::to_string(&command("rpc-error")).unwrap())
        .arg("--receipt")
        .arg(root.path().join("receipt"))
        .arg("--task-dir")
        .arg(root.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("Mismatched or failed MCP response"),
        "{stderr}"
    );
    assert!(!root.path().join("oracle.json").exists());
}
