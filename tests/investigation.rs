use fieldkit::{
    Collected, Config, Job, Workbench, api,
    mcp::Broker,
    model::{digest, text},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn fixture() -> (tempfile::TempDir, Workbench, Value, String) {
    let dir = tempfile::tempdir().unwrap();
    let w = Workbench::new(dir.path().join("case"), Config::default()).unwrap();
    let mut investigation = w
        .create_investigation(
            "How did this narrative spread?",
            vec![Job {
                command: "telegram-search".into(),
                target: Some("research".into()),
                options: json!({"since":"2024-01-01","until":"2024-02-01","limit":2}),
            }],
        )
        .unwrap();
    let raw = br#"{"hits":{"hits":[{"_source":{"message":"A public research report"}}]}}"#.to_vec();
    let hash = digest(&raw);
    let report = json!({"run_id":"run_fixture","command":"telegram-search","target":"research","status":"ok","data":{"messages":[{"url":"https://t.me/examplechannel/42","text":"A public research report","directly_verified":false,"source_ids":["src_fixture"],"source_pointer":"/hits/hits/0/_source"}]},"sources":[{"source_id":"src_fixture","sha256":hash,"kind":"http"}],"entities":[],"relationships":[],"errors":[],"timing":{"completed_at":"2024-03-01T00:00:00Z"}});
    w.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash.clone(), raw)]),
        })
        .unwrap();
    investigation.as_object_mut().unwrap().remove("findings");
    investigation.as_object_mut().unwrap().remove("summary");
    investigation["steps"][0]["run_id"] = json!("run_fixture");
    investigation["steps"][0]["status"] = json!("ok");
    w.store.put("investigations", &investigation).unwrap();
    (dir, w, investigation, hash)
}
fn propose(w: &Workbench, investigation: &Value, kind: &str) -> Value {
    w.propose_investigation_finding(
        text(investigation, "id"),
        "This is a proposed assessment, not an independent fact.",
        kind,
        vec![json!({"step_id":investigation["steps"][0]["id"],"pointer":"/data/messages/0"})],
    )
    .unwrap()
}

#[test]
fn planning_is_local_validated_and_persistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("case");
    let w = Workbench::new(&path, Config::default()).unwrap();
    for payload in [
        json!({"question":" "}),
        json!({"question":"q","searches":[{"command":"doctor"}]}),
        json!({"question":"q","approved":true}),
    ] {
        assert!(api::apply(&w, "investigations/create", &payload).is_err());
    }
    assert!(w.store.list("investigations").unwrap().is_empty());
    let created = api::apply(
        &w,
        "investigations/create",
        &json!({"question":"Follow a public narrative"}),
    )
    .unwrap();
    assert_eq!(created["steps"], json!([]));
    assert!(w.store.reports(10).unwrap().is_empty());
    let reopened = Workbench::new(path, Config::default()).unwrap();
    assert_eq!(
        reopened.snapshot().unwrap()["investigations"][0]["question"],
        created["question"]
    );
}

#[test]
fn exact_result_selection_plans_but_does_not_collect_or_retry() {
    let (_dir, w, inv, _hash) = fixture();
    let identifier = text(&inv, "id");
    let search = text(&inv["steps"][0], "id");
    assert!(w.select_investigation_post(identifier, search, 10).is_err());
    let step = w.select_investigation_post(identifier, search, 0).unwrap();
    assert_eq!(step["command"], "telegram-post");
    assert_eq!(step["target"], "https://t.me/examplechannel/42");
    assert_eq!(step["status"], "pending");
    assert_eq!(w.store.reports(10).unwrap().len(), 1);
    assert!(w.select_investigation_post(identifier, search, 0).is_err());
    assert!(w.run_investigation_step(identifier, search).is_err());
    let _guard = w
        .store
        .lock(&format!(
            "investigation-{}.lock",
            digest(identifier.as_bytes())
        ))
        .unwrap();
    assert!(
        w.run_investigation_step(identifier, text(&step, "id"))
            .is_err()
    );
}

#[test]
fn every_finding_kind_is_pending_and_only_local_review_promotes_it() {
    let (_dir, w, inv, _hash) = fixture();
    for kind in ["observation", "hypothesis", "identity_sensitive"] {
        let finding = propose(&w, &inv, kind);
        assert_eq!(finding["status"], "pending_review");
        assert_eq!(
            finding["citations"][0]["scope"],
            "provider_archive_observation_not_direct_verification"
        );
        assert_eq!(
            w.investigation(text(&inv, "id")).unwrap()["summary"],
            json!([])
        );
        assert!(
            w.review_investigation_finding(text(&inv, "id"), text(&finding, "id"), "accepted", " ")
                .is_err()
        );
        w.review_investigation_finding(
            text(&inv, "id"),
            text(&finding, "id"),
            "accepted",
            "Reviewed source and uncertainty",
        )
        .unwrap();
        let view = w.investigation(text(&inv, "id")).unwrap();
        assert!(
            view["summary"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v["id"] == finding["id"])
        );
        assert_eq!(
            w.store
                .get("investigation_findings", text(&finding, "id"))
                .unwrap(),
            finding
        );
        w.review_investigation_finding(
            text(&inv, "id"),
            text(&finding, "id"),
            "rejected",
            "Insufficient support",
        )
        .unwrap();
    }
    assert_eq!(
        w.investigation(text(&inv, "id")).unwrap()["summary"],
        json!([])
    );
    assert!(w.store.verify().unwrap()["ok"] == true);
}

#[test]
fn citations_reject_foreign_missing_invalid_and_altered_sources() {
    let (_dir, w, inv, hash) = fixture();
    let identifier = text(&inv, "id");
    for pointer in [
        "/data/messages/99",
        "/data/messages/00",
        "/data",
        "/sources/0",
    ] {
        assert!(
            w.propose_investigation_finding(
                identifier,
                "claim",
                "hypothesis",
                vec![json!({"step_id":inv["steps"][0]["id"],"pointer":pointer})]
            )
            .is_err()
        );
    }
    let other = w.create_investigation("Another question", vec![]).unwrap();
    assert!(
        w.propose_investigation_finding(
            text(&other, "id"),
            "claim",
            "observation",
            vec![json!({"step_id":inv["steps"][0]["id"],"pointer":"/data/messages/0"})]
        )
        .is_err()
    );
    assert!(
        w.propose_investigation_finding(identifier, "claim", "observation", vec![])
            .is_err()
    );
    let finding = propose(&w, &inv, "identity_sensitive");
    std::fs::write(w.store.path.join("blobs").join(hash), b"altered").unwrap();
    assert!(
        w.review_investigation_finding(identifier, text(&finding, "id"), "accepted", "Reviewed")
            .is_err()
    );
    assert_eq!(w.investigation(identifier).unwrap()["summary"], json!([]));
}

#[test]
fn review_binds_exact_immutable_proposal_and_cannot_cross_investigations() {
    let (_dir, w, inv, _hash) = fixture();
    let mut finding = propose(&w, &inv, "hypothesis");
    let other = w.create_investigation("Other question", vec![]).unwrap();
    assert!(
        w.review_investigation_finding(
            text(&other, "id"),
            text(&finding, "id"),
            "accepted",
            "Reviewed"
        )
        .is_err()
    );
    w.review_investigation_finding(
        text(&inv, "id"),
        text(&finding, "id"),
        "accepted",
        "Reviewed",
    )
    .unwrap();
    finding["content"] = json!("Changed after review");
    w.store.put("investigation_findings", &finding).unwrap();
    let view = w.investigation(text(&inv, "id")).unwrap();
    assert_eq!(view["summary"], json!([]));
    assert_eq!(view["findings"][0]["status"], "pending_review");
}

#[test]
fn agent_and_http_operations_cannot_review_or_forge_approval() {
    let (dir, w, inv, _hash) = fixture();
    let finding = propose(&w, &inv, "identity_sensitive");
    let broker = Broker::new(
        dir.path().to_path_buf(),
        Some("case".into()),
        Config::default(),
    )
    .unwrap();
    for operation in [
        "investigations/review",
        "investigations/approve",
        "investigation_reviews",
    ] {
        assert!(
            api::apply(
                &w,
                operation,
                &json!({"id":finding["id"],"decision":"accepted"})
            )
            .is_err()
        );
    }
    assert!(
        broker
            .invoke(
                "fieldkit_investigation_review",
                &json!({"id":finding["id"],"decision":"accepted"})
            )
            .is_err()
    );
    assert!(broker.invoke("fieldkit_investigation_propose",&json!({"id":inv["id"],"content":"claim","kind":"observation","citations":[],"status":"accepted"})).is_err());
    let result = broker
        .invoke("fieldkit_investigation_show", &json!({"id":inv["id"]}))
        .unwrap();
    assert_eq!(result["data"]["summary"], json!([]));
    let report = broker
        .invoke(
            "fieldkit_investigation_evidence",
            &json!({"id":inv["id"],"step_id":inv["steps"][0]["id"]}),
        )
        .unwrap();
    assert_eq!(
        report["data"]["data"]["messages"][0]["text"],
        "A public research report"
    );
}

#[test]
fn evidence_lookup_does_not_depend_on_recent_report_window() {
    let (_dir, w, inv, _hash) = fixture();
    for i in 0..501 {
        w.store.save_report(&Collected {report:json!({"run_id":format!("run_later_{i}"),"command":"doctor","status":"ok","sources":[],"data":{},"timing":{}}),blobs:BTreeMap::new()}).unwrap();
    }
    assert!(
        w.store
            .reports(500)
            .unwrap()
            .iter()
            .all(|r| r["run_id"] != "run_fixture")
    );
    let finding = propose(&w, &inv, "hypothesis");
    w.review_investigation_finding(
        text(&inv, "id"),
        text(&finding, "id"),
        "accepted",
        "Reviewed older retained report",
    )
    .unwrap();
    assert_eq!(
        w.investigation(text(&inv, "id")).unwrap()["summary"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
