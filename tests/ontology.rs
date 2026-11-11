use fieldkit::{
    Collected, Config, Workbench,
    model::{digest, text},
    store::Store,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn workspace() -> (tempfile::TempDir, Workbench) {
    let temp = tempfile::tempdir().unwrap();
    let workbench = Workbench {
        store: Store::open(temp.path()).unwrap(),
        config: Config::default(),
    };
    (temp, workbench)
}

fn capture(workbench: &Workbench, run: &str, target_type: &str) -> Value {
    let body = b"<a href='https://project.example/research'>Project</a>".to_vec();
    let hash = digest(&body);
    let report = json!({"run_id":run,"command":"http","target":"https://account.example/alice","status":"ok",
        "data":{"fixture":true},"sources":[{"source_id":"source-a","kind":"http","sha256":hash,"hash_scope":"source_bytes","observed_at":"2026-11-11T10:00:00Z"}],
        "entities":[{"type":"profile_url","value":"https://account.example/alice","source_ids":["source-a"]}],
        "relationships":[{"source":{"type":"profile_url","value":"https://account.example/alice"},"relation":"links_to",
            "target":{"type":target_type,"value":"https://project.example/research"},"source_ids":["source-a"]}],
        "timing":{"started_at":"2026-11-11T10:00:00Z","completed_at":"2026-11-11T10:00:01Z"}});
    workbench
        .store
        .save_report(&Collected {
            report: report.clone(),
            blobs: BTreeMap::from([(hash, body)]),
        })
        .unwrap();
    report
}

#[test]
fn claims_resolve_to_exact_preserved_report_and_source_hash() {
    let (_temp, wb) = workspace();
    let report = capture(&wb, "run-grounded", "url");
    let ontology = wb.sync_ontology().unwrap();
    assert_eq!(ontology["version"], 1);
    assert_eq!(ontology["concepts"].as_array().unwrap().len(), 2);
    let claim = &ontology["claims"][0];
    assert_eq!(claim["status"], "observed");
    assert_ne!(claim["source_id"], claim["target_id"]);
    assert_eq!(claim["evidence_ids"], json!(["evidence_run-grounded"]));
    let anchor = &claim["anchors"][0];
    assert_eq!(
        anchor["report_sha256"],
        wb.store.report_hash("run-grounded").unwrap()
    );
    assert_eq!(
        report.pointer(text(anchor, "json_pointer")).unwrap(),
        &report["relationships"][0]
    );
    assert_eq!(
        anchor["source_hashes"][0]["sha256"],
        report["sources"][0]["sha256"]
    );
    assert_eq!(wb.store.verify().unwrap()["ok"], true);
}

#[test]
fn vocabulary_evolves_idempotently_and_reviews_preserve_captures() {
    let (temp, wb) = workspace();
    let original = capture(&wb, "first", "url");
    let first = wb.sync_ontology().unwrap();
    let claim_id = text(&first["claims"][0], "id").to_owned();
    assert!(wb.review_claim(&claim_id, "accepted", "").is_err());
    wb.review_claim(
        &claim_id,
        "accepted",
        "Keep this explicit link as a source-attributed observation",
    )
    .unwrap();
    assert_eq!(wb.sync_ontology().unwrap()["version"], 1);
    assert_eq!(
        wb.ontology_snapshot().unwrap()["claims"][0]["status"],
        "accepted"
    );
    capture(&wb, "second", "organization");
    let next = wb.sync_ontology().unwrap();
    assert_eq!(next["version"], 2);
    assert_eq!(next["claims"].as_array().unwrap().len(), 2);
    assert_eq!(
        wb.store
            .reports(10)
            .unwrap()
            .iter()
            .find(|r| r["run_id"] == "first")
            .unwrap(),
        &original
    );
    let reopened = Workbench {
        store: Store::open(temp.path()).unwrap(),
        config: Config::default(),
    };
    assert_eq!(reopened.ontology_snapshot().unwrap()["version"], 2);
    assert!(
        reopened.ontology_snapshot().unwrap()["claims"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["status"] == "accepted")
    );
}

#[test]
fn failed_provider_results_never_become_observed_claims() {
    let (_temp, wb) = workspace();
    let mut report = capture(&wb, "original", "url");
    report["run_id"] = json!("blocked");
    report["status"] = json!("blocked");
    report["entities"][0]["type"] = json!("blocked_type");
    wb.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::new(),
        })
        .unwrap();
    let ontology = wb.sync_ontology().unwrap();
    assert!(
        !ontology["concepts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["type"] == "blocked_type")
    );
    assert_eq!(ontology["claims"][0]["run_ids"], json!(["original"]));
}

#[test]
fn additional_captures_extend_anchors_without_reopening_rejected_claims() {
    let (_temp, wb) = workspace();
    capture(&wb, "one", "url");
    let first = wb.sync_ontology().unwrap();
    let claim_id = text(&first["claims"][0], "id").to_owned();
    wb.review_claim(
        &claim_id,
        "rejected",
        "Source context contradicts the proposed interpretation",
    )
    .unwrap();
    capture(&wb, "two", "url");
    let ontology = wb.sync_ontology().unwrap();
    assert_eq!(ontology["version"], 1);
    assert_eq!(ontology["claims"].as_array().unwrap().len(), 1);
    assert_eq!(
        ontology["claims"][0]["anchors"].as_array().unwrap().len(),
        2
    );
    assert_eq!(ontology["claims"][0]["status"], "rejected");
}

#[cfg(unix)]
#[test]
fn ontology_lock_rejects_symlinks_and_hardlinks() {
    use std::os::unix::fs::symlink;
    let (temp, wb) = workspace();
    let other = temp.path().join("other-file");
    std::fs::write(&other, b"unchanged").unwrap();
    let lock = temp.path().join("ontology.lock");
    symlink(&other, &lock).unwrap();
    assert!(wb.sync_ontology().is_err());
    std::fs::remove_file(&lock).unwrap();
    std::fs::hard_link(&other, &lock).unwrap();
    assert!(wb.sync_ontology().is_err());
    assert_eq!(std::fs::read(&other).unwrap(), b"unchanged");
}
