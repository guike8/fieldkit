use fieldkit::{
    Config, Job, Workbench,
    model::{Collected, digest, entity_id},
    store::Store,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn workspace() -> (tempfile::TempDir, Workbench) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let w = Workbench::new(root.join("case"), Config::default()).unwrap();
    (temp, w)
}

fn observation(w: &Workbench, run: &str, site: &str, reference: &str) {
    let source = format!("src_{run}");
    let body = format!("{site} explicitly links to {reference}").into_bytes();
    let hash = digest(&body);
    let username =
        json!({"type":"username","value":format!("{site}:example"),"source_ids":[source]});
    let profile = json!({"type":"profile_url","value":format!("https://{site}.example/u/example"),"source_ids":[source]});
    let url = json!({"type":"url","value":reference,"source_ids":[source]});
    let report = json!({"schema_version":"1.0","run_id":run,"command":"profile","target":profile["value"],"status":"ok","data":{"username":"example","reference":reference},"sources":[{"source_id":source,"sha256":hash,"url":profile["value"],"bytes":body.len()}],"entities":[username,profile,url],"relationships":[{"source":username,"relation":"public_profile_at","target":profile,"source_ids":[source]},{"source":profile,"relation":"links_to","target":url,"source_ids":[source]}],"errors":[],"timing":{"started_at":"2026-11-11T00:00:00Z","completed_at":"2026-11-11T00:00:01Z"}});
    w.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash, body)]),
        })
        .unwrap();
}

#[test]
fn correlates_with_two_sided_evidence_and_keeps_identities_distinct() {
    let (_temp, w) = workspace();
    observation(&w, "run_a", "one", "https://project.example/");
    observation(&w, "run_b", "two", "https://project.example/");
    w.sync_ontology().unwrap();
    let before = w.snapshot().unwrap();
    let leads = before["correlations"].as_array().unwrap();
    assert_eq!(leads.len(), 2);
    for lead in leads {
        assert_eq!(lead["evidence_ids"].as_array().unwrap().len(), 2);
        assert_eq!(lead["basis"].as_array().unwrap().len(), 2);
        for side in lead["basis"].as_array().unwrap() {
            assert!(!side["evidence_ids"].as_array().unwrap().is_empty());
            assert!(!side["anchors"].as_array().unwrap().is_empty());
            for anchor in side["anchors"].as_array().unwrap() {
                let report = w
                    .store
                    .reports(10)
                    .unwrap()
                    .into_iter()
                    .find(|r| r["run_id"] == anchor["run_id"])
                    .unwrap();
                assert!(
                    report
                        .pointer(anchor["json_pointer"].as_str().unwrap())
                        .is_some()
                );
                assert_eq!(
                    anchor["report_sha256"],
                    w.store
                        .report_hash(anchor["run_id"].as_str().unwrap())
                        .unwrap()
                );
            }
        }
    }
    let lead = &leads[0];
    assert!(
        w.review_correlation(lead["id"].as_str().unwrap(), "accepted", "")
            .is_err()
    );
    w.review_correlation(
        lead["id"].as_str().unwrap(),
        "accepted",
        "Working lead: shared project link; ownership remains unestablished",
    )
    .unwrap();
    let after = w.snapshot().unwrap();
    assert_eq!(before["entities"], after["entities"]);
    assert!(
        after["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["status"] == "analyst_accepted_lead")
    );
    assert_eq!(w.store.verify().unwrap()["ok"], true);
}

#[test]
fn identical_page_representations_do_not_correlate_with_each_other() {
    let (_temp, w) = workspace();
    observation(&w, "run_a", "one", "https://project.example/");
    let source = "src_html";
    let body = b"<a href='https://project.example/'>project</a>".to_vec();
    let hash = digest(&body);
    let report = json!({"schema_version":"1.0","run_id":"run_html","command":"http","target":"https://one.example/u/example/","status":"ok","data":{},"sources":[{"source_id":source,"sha256":hash}],"entities":[{"type":"url","value":"https://one.example/u/example/","source_ids":[source]}],"relationships":[{"source":{"type":"url","value":"https://one.example/u/example/"},"relation":"links_to","target":{"type":"url","value":"https://project.example/"},"source_ids":[source]}],"errors":[],"timing":{}});
    w.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash, body)]),
        })
        .unwrap();
    w.sync_ontology().unwrap();
    assert!(w.correlations().unwrap().is_empty());
}

#[test]
fn evidence_assessment_is_separate_from_original_capture() {
    let (_temp, w) = workspace();
    let evidence = w
        .add_evidence(
            "A source",
            "<untrusted> quoted text",
            "https://example.org/",
            "note",
            None,
        )
        .unwrap();
    let original = w.store.reports(10).unwrap();
    w.review_evidence(
        evidence["id"].as_str().unwrap(),
        "contested",
        "Alternative explanation",
    )
    .unwrap();
    assert_eq!(original, w.store.reports(10).unwrap());
    assert_eq!(w.snapshot().unwrap()["evidence"][0]["status"], "contested");
    assert!(w.add_note("Note", vec!["missing".into()]).is_err());
    assert_eq!(w.store.verify().unwrap()["ok"], true);
}

#[test]
fn collector_runs_grow_ontology_and_preserve_raw_evidence_without_python() {
    let (_temp, w) = workspace();
    let e = w
        .add_evidence(
            "Public link",
            "A quoted page",
            "https://example.org/",
            "note",
            None,
        )
        .unwrap();
    let report = w
        .run_tool(&Job {
            command: "doctor".into(),
            target: None,
            options: json!({}),
        })
        .unwrap();
    assert_eq!(report["status"], "ok");
    let snapshot = w.snapshot().unwrap();
    assert!(
        snapshot["ontology"]["concepts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["type"] == "url")
    );
    assert!(
        snapshot["evidence"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["id"] == e["id"])
    );
    assert_eq!(w.store.verify().unwrap()["ok"], true);
}

#[test]
fn detects_report_and_artifact_tampering() {
    let (_temp, w) = workspace();
    observation(&w, "run_a", "one", "https://example.org/");
    let conn = w.store.connection().unwrap();
    conn.execute("UPDATE runs SET result_json='{}' WHERE run_id='run_a'", [])
        .unwrap();
    assert_eq!(w.store.verify().unwrap()["ok"], false);
    assert!(w.store.reports(10).is_err());
}

#[test]
fn typed_entity_ids_remain_legacy_compatible() {
    assert_eq!(
        entity_id("username", &json!("github:octocat")),
        format!("entity_{}", digest(br#"["username","github:octocat"]"#))
    );
    assert_ne!(
        entity_id("url", &json!("x")),
        entity_id("username", &json!("x"))
    );
}

#[test]
fn concurrent_flow_attempts_cannot_collect_the_same_step_twice() {
    let (_temp, w) = workspace();
    let flow = w
        .create_flow(
            "Offline check",
            vec![Job {
                command: "doctor".into(),
                target: None,
                options: json!({}),
            }],
        )
        .unwrap();
    let guard = w.store.lock("flows.lock").unwrap();
    assert!(
        w.run_step(
            flow["id"].as_str().unwrap(),
            flow["steps"][0]["id"].as_str().unwrap()
        )
        .is_err()
    );
    assert!(w.store.reports(10).unwrap().is_empty());
    drop(guard);
    assert_eq!(
        w.run_step(
            flow["id"].as_str().unwrap(),
            flow["steps"][0]["id"].as_str().unwrap()
        )
        .unwrap()["status"],
        "ok"
    );
}

#[test]
fn opens_existing_v1_collection_schema_and_keeps_report_hashes() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let path = root.join("old-case");
    std::fs::create_dir(&path).unwrap();
    std::fs::create_dir(path.join("blobs")).unwrap();
    let conn = rusqlite::Connection::open(path.join("case.sqlite3")).unwrap();
    conn.execute_batch("CREATE TABLE case_meta(key TEXT PRIMARY KEY,value TEXT NOT NULL);INSERT INTO case_meta VALUES('schema_version','1');CREATE TABLE runs(run_id TEXT PRIMARY KEY,command TEXT NOT NULL,target TEXT,status TEXT NOT NULL,started_at TEXT,result_json TEXT NOT NULL,report_sha256 TEXT NOT NULL);CREATE TABLE artifacts(sha256 TEXT PRIMARY KEY,size_bytes INTEGER NOT NULL,relative_path TEXT NOT NULL);CREATE TABLE run_artifacts(run_id TEXT NOT NULL REFERENCES runs(run_id),sha256 TEXT NOT NULL REFERENCES artifacts(sha256),PRIMARY KEY(run_id,sha256));").unwrap();
    let legacy = r#"{"command":"doctor","data":{"literal":"café"},"entities":[],"errors":[],"relationships":[],"run_id":"run_legacy","sources":[],"status":"ok","target":null,"timing":{}}"#;
    conn.execute(
        "INSERT INTO runs VALUES('run_legacy','doctor',NULL,'ok',NULL,?1,?2)",
        rusqlite::params![legacy, digest(legacy.as_bytes())],
    )
    .unwrap();
    drop(conn);
    let store = Store::open(path).unwrap();
    assert_eq!(
        store.report_hash("run_legacy").unwrap(),
        digest(legacy.as_bytes())
    );
    assert_eq!(store.reports(10).unwrap()[0]["data"]["literal"], "café");
    assert_eq!(store.verify().unwrap()["ok"], true);
}

#[test]
fn exported_ontology_claims_resolve_to_original_reports() {
    let (_temp, w) = workspace();
    observation(&w, "run_a", "one", "https://example.org/");
    w.sync_ontology().unwrap();
    let exported: Value = serde_json::from_str(&w.export("json").unwrap()).unwrap();
    for claim in exported["workspace"]["ontology"]["claims"]
        .as_array()
        .unwrap()
    {
        for anchor in claim["anchors"].as_array().unwrap() {
            let report = exported["collector_ledger"]["reports"]
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["run_id"] == anchor["run_id"])
                .unwrap();
            let referenced = report
                .pointer(anchor["json_pointer"].as_str().unwrap())
                .unwrap();
            assert_eq!(referenced["relation"], claim["relation"]);
            assert_eq!(
                anchor["report_sha256"],
                w.store
                    .report_hash(anchor["run_id"].as_str().unwrap())
                    .unwrap()
            );
        }
    }
}
