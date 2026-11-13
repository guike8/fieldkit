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
fn empty_investigation() -> (tempfile::TempDir, Workbench, Value) {
    let dir = tempfile::tempdir().unwrap();
    let w = Workbench::new(dir.path().join("case"), Config::default()).unwrap();
    let investigation = w
        .create_investigation("Synthetic capture attachment test", vec![])
        .unwrap();
    (dir, w, investigation)
}

fn synthetic_capture(
    w: &Workbench,
    run_id: &str,
    command: &str,
    status: &str,
    media_type: &str,
) -> (Value, String) {
    let raw = br#"{"pairs":[{"label":"synthetic preserved observation","count":7}],"a/b":{"~key":"synthetic escaped value"}}"#.to_vec();
    let hash = digest(&raw);
    let messages = json!([
        {"url":"https://t.me/examplechannel/42","text":"Synthetic preview one","source_ids":[format!("src_{run_id}")]},
        {"url":"https://t.me/examplechannel/43","text":"Synthetic preview two","source_ids":[format!("src_{run_id}")]}
    ]);
    let data = if command == "http" {
        // Deliberately differs from the preserved JSON: HTTP citations address raw JSON.
        json!({"pairs":[{"label":"normalized report value","count":999}]})
    } else {
        json!({"messages":messages})
    };
    let report = json!({
        "run_id":run_id,"command":command,"target":"https://example.invalid/synthetic",
        "status":status,"data":data,
        "sources":[{"source_id":format!("src_{run_id}"),"sha256":hash,"kind":"http","media_type":media_type}],
        "entities":[],"relationships":[],"errors":[],
        "timing":{"completed_at":"2024-03-01T00:00:00Z"}
    });
    w.store
        .save_report(&Collected {
            report: report.clone(),
            blobs: BTreeMap::from([(hash.clone(), raw)]),
        })
        .unwrap();
    (report, hash)
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

#[test]
fn attached_http_cites_exact_preserved_json_and_stays_pending() {
    for (index, media_type) in [
        "application/json",
        "application/json; charset=utf-8",
        "application/vnd.synthetic+json",
    ]
    .iter()
    .enumerate()
    {
        let (dir, w, inv) = empty_investigation();
        let identifier = text(&inv, "id");
        let run_id = format!("run_http_{index}");
        let (report, hash) = synthetic_capture(&w, &run_id, "http", "partial", media_type);
        let step = w.attach_investigation_capture(identifier, &run_id).unwrap();
        assert_eq!(step["run_id"], run_id);
        assert_eq!(step["command"], "http");
        assert_eq!(step["status"], "partial");
        assert_eq!(
            w.investigation_evidence(identifier, text(&step, "id"))
                .unwrap(),
            report
        );
        assert!(w.attach_investigation_capture(identifier, &run_id).is_err());
        assert!(
            w.run_investigation_step(identifier, text(&step, "id"))
                .is_err()
        );
        let finding = w
            .propose_investigation_finding(
                identifier,
                "Synthetic proposal only, not an accepted finding",
                "observation",
                vec![json!({"step_id":step["id"],"source_id":report["sources"][0]["source_id"],"pointer":"/pairs/0"})],
            )
            .unwrap();
        let citation = &finding["citations"][0];
        let decoded: Value = serde_json::from_slice(&w.store.source_bytes(&hash).unwrap()).unwrap();
        assert_eq!(citation["source_id"], report["sources"][0]["source_id"]);
        assert_eq!(citation["pointer"], "/pairs/0");
        assert_eq!(citation["run_id"], run_id);
        assert_eq!(citation["step_id"], step["id"]);
        assert_eq!(
            citation["report_sha256"],
            w.store.report_hash(&run_id).unwrap()
        );
        assert_eq!(citation["sources"], report["sources"]);
        assert_eq!(citation["sources"][0]["sha256"], hash);
        assert_eq!(
            citation["observation_sha256"],
            digest(
                serde_json::to_string(&decoded["pairs"][0])
                    .unwrap()
                    .as_bytes()
            )
        );
        assert_ne!(
            citation["observation_sha256"],
            digest(
                serde_json::to_string(&report["data"]["pairs"][0])
                    .unwrap()
                    .as_bytes()
            )
        );
        assert_eq!(
            citation["scope"],
            "external_json_observation_not_identity_attribution"
        );
        assert_eq!(finding["status"], "pending_review");
        assert_eq!(finding["source_evidence"], false);
        assert_eq!(w.store.reports(10).unwrap(), vec![report]);
        let reopened = Workbench::new(dir.path().join("case"), Config::default()).unwrap();
        let view = reopened.investigation(identifier).unwrap();
        assert_eq!(view["steps"], json!([step]));
        assert_eq!(view["findings"][0]["citations"], finding["citations"]);
        assert_eq!(view["summary"], json!([]));
        assert!(
            reopened
                .store
                .list("investigation_reviews")
                .unwrap()
                .is_empty()
        );
        assert_eq!(reopened.store.verify().unwrap()["ok"], true);
    }
}

#[test]
fn attached_telegram_messages_require_exact_message_and_only_plan_post_capture() {
    let (_dir, w, inv) = empty_investigation();
    let identifier = text(&inv, "id");
    let (report, _) = synthetic_capture(&w, "run_preview", "telegram-messages", "ok", "text/html");
    let step = w
        .attach_investigation_capture(identifier, "run_preview")
        .unwrap();
    for pointer in [
        "/data",
        "/data/messages",
        "/data/messages/00",
        "/data/messages/+0",
        "/data/messages/0/text",
        "/data/messages/99",
        "/data/messages/-",
    ] {
        assert!(
            w.propose_investigation_finding(
                identifier,
                "Synthetic claim",
                "observation",
                vec![json!({"step_id":step["id"],"pointer":pointer})],
            )
            .is_err(),
            "accepted nonexact Telegram pointer {pointer}"
        );
    }
    let finding = w
        .propose_investigation_finding(
            identifier,
            "Synthetic preview observation",
            "observation",
            vec![json!({"step_id":step["id"],"pointer":"/data/messages/0"})],
        )
        .unwrap();
    assert_eq!(finding["citations"][0]["pointer"], "/data/messages/0");
    assert_eq!(
        finding["citations"][0]["observation_sha256"],
        digest(
            serde_json::to_string(&report["data"]["messages"][0])
                .unwrap()
                .as_bytes()
        )
    );
    assert!(
        w.select_investigation_post(identifier, text(&step, "id"), 2)
            .is_err()
    );
    let planned = w
        .select_investigation_post(identifier, text(&step, "id"), 0)
        .unwrap();
    assert_eq!(planned["command"], "telegram-post");
    assert_eq!(planned["target"], "https://t.me/examplechannel/42");
    assert_eq!(planned["status"], "pending");
    assert!(planned["run_id"].is_null());
    assert_eq!(
        planned["selected_from"],
        json!({"step_id":step["id"],"index":0,"run_id":"run_preview"})
    );
    assert!(
        w.select_investigation_post(identifier, text(&step, "id"), 0)
            .is_err()
    );
    assert_eq!(w.store.reports(10).unwrap(), vec![report]);
    assert_eq!(w.investigation(identifier).unwrap()["summary"], json!([]));
}

#[test]
fn attachment_accepts_only_supported_completed_captures() {
    let (_dir, w, inv) = empty_investigation();
    let identifier = text(&inv, "id");
    for (index, command) in [
        "http",
        "telegram-messages",
        "telegram-post",
        "telegram-search",
    ]
    .iter()
    .enumerate()
    {
        let run_id = format!("run_allowed_{index}");
        let status = if index % 2 == 0 { "ok" } else { "partial" };
        synthetic_capture(&w, &run_id, command, status, "application/json");
        let step = w.attach_investigation_capture(identifier, &run_id).unwrap();
        assert_eq!(step["command"], *command);
        assert_eq!(step["status"], status);
    }
    let before = w.investigation(identifier).unwrap()["steps"].clone();
    for (index, command) in [
        "doctor",
        "analysis.nvidia",
        "telegram",
        "telegram-members",
        "file",
        "unknown",
    ]
    .iter()
    .enumerate()
    {
        let run_id = format!("run_disallowed_{index}");
        synthetic_capture(&w, &run_id, command, "ok", "application/json");
        assert!(
            w.attach_investigation_capture(identifier, &run_id).is_err(),
            "accepted {command}"
        );
    }
    for (index, status) in ["failed", "error", "running", "pending", ""]
        .iter()
        .enumerate()
    {
        let run_id = format!("run_incomplete_{index}");
        synthetic_capture(&w, &run_id, "http", status, "application/json");
        assert!(
            w.attach_investigation_capture(identifier, &run_id).is_err(),
            "accepted status {status}"
        );
    }
    assert_eq!(w.investigation(identifier).unwrap()["steps"], before);
}

#[test]
fn attachment_rejects_missing_foreign_and_altered_reports_without_mutating_plan() {
    let (_dir, w, inv) = empty_investigation();
    let identifier = text(&inv, "id");
    let (_other_dir, other, _) = empty_investigation();
    synthetic_capture(&other, "run_foreign_case", "http", "ok", "application/json");
    for run_id in ["run_missing", "run_foreign_case", ""] {
        assert!(w.attach_investigation_capture(identifier, run_id).is_err());
    }
    synthetic_capture(&w, "run_altered_report", "http", "ok", "application/json");
    w.store
        .connection()
        .unwrap()
        .execute(
            "UPDATE runs SET result_json='{}' WHERE run_id='run_altered_report'",
            [],
        )
        .unwrap();
    assert!(
        w.attach_investigation_capture(identifier, "run_altered_report")
            .is_err()
    );
    assert!(
        w.attach_investigation_capture("investigation_missing", "run_altered_report")
            .is_err()
    );
    assert_eq!(w.investigation(identifier).unwrap()["steps"], json!([]));
}

#[test]
fn attachment_requires_nonempty_sources_and_verifies_every_source() {
    for failure in [
        "empty",
        "missing_digest",
        "missing_bytes",
        "altered_second_source",
    ] {
        let (_dir, w, inv) = empty_investigation();
        let (mut report, _) = synthetic_capture(&w, "run_seed", "http", "ok", "application/json");
        report["run_id"] = json!("run_invalid_sources");
        let mut blobs = BTreeMap::new();
        match failure {
            "empty" => report["sources"] = json!([]),
            "missing_digest" => {
                report["sources"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("sha256");
            }
            "missing_bytes" => {}
            "altered_second_source" => {
                let raw = b"second synthetic source".to_vec();
                let hash = digest(&raw);
                report["sources"].as_array_mut().unwrap().push(json!({
                    "source_id":"src_second","sha256":hash,"kind":"http","media_type":"text/plain"
                }));
                blobs.insert(hash, raw);
            }
            _ => unreachable!(),
        }
        w.store
            .save_report(&Collected {
                report: report.clone(),
                blobs,
            })
            .unwrap();
        if failure == "missing_bytes" {
            std::fs::remove_file(
                w.store
                    .path
                    .join("blobs")
                    .join(text(&report["sources"][0], "sha256")),
            )
            .unwrap();
        } else if failure == "altered_second_source" {
            std::fs::write(
                w.store
                    .path
                    .join("blobs")
                    .join(text(&report["sources"][1], "sha256")),
                b"altered",
            )
            .unwrap();
        }
        assert!(
            w.attach_investigation_capture(text(&inv, "id"), "run_invalid_sources")
                .is_err(),
            "accepted {failure}"
        );
        assert_eq!(
            w.investigation(text(&inv, "id")).unwrap()["steps"],
            json!([])
        );
    }
}

#[test]
fn http_citations_reject_missing_foreign_plaintext_and_bad_raw_pointers() {
    let (_dir, w, inv) = empty_investigation();
    let identifier = text(&inv, "id");
    let (report, hash) = synthetic_capture(&w, "run_json", "http", "ok", "application/json");
    let step = w
        .attach_investigation_capture(identifier, "run_json")
        .unwrap();
    synthetic_capture(&w, "run_other_source", "http", "ok", "application/json");
    let source_id = &report["sources"][0]["source_id"];
    let mut invalid = vec![
        json!({"step_id":step["id"],"pointer":"/pairs/0"}),
        json!({"step_id":step["id"],"source_id":"src_run_other_source","pointer":"/pairs/0"}),
        json!({"step_id":step["id"],"source_id":"src_absent","pointer":"/pairs/0"}),
    ];
    for pointer in [
        "/data/pairs/0",
        "/pairs/99",
        "/pairs/00",
        "/pairs/0/missing",
        "pairs/0",
        "/a~2b",
    ] {
        invalid.push(json!({"step_id":step["id"],"source_id":source_id,"pointer":pointer}));
    }
    for citation in invalid {
        assert!(
            w.propose_investigation_finding(
                identifier,
                "Synthetic claim",
                "observation",
                vec![citation.clone()]
            )
            .is_err(),
            "accepted {citation}"
        );
    }
    let (plain, _) = synthetic_capture(&w, "run_plain", "http", "ok", "text/plain");
    let plain_step = w
        .attach_investigation_capture(identifier, "run_plain")
        .unwrap();
    assert!(w.propose_investigation_finding(identifier, "Synthetic claim", "observation", vec![json!({
        "step_id":plain_step["id"],"source_id":plain["sources"][0]["source_id"],"pointer":"/pairs/0"
    })]).is_err());
    let escaped = w
        .propose_investigation_finding(
            identifier,
            "Synthetic escaped-key observation",
            "observation",
            vec![json!({
                "step_id":step["id"],"source_id":source_id,"pointer":"/a~1b/~0key"
            })],
        )
        .unwrap();
    assert_eq!(
        escaped["citations"][0]["observation_sha256"],
        digest(
            serde_json::to_string("synthetic escaped value")
                .unwrap()
                .as_bytes()
        )
    );
    let before = w.store.list("investigation_findings").unwrap();
    std::fs::write(
        w.store.path.join("blobs").join(hash),
        b"altered after attachment",
    )
    .unwrap();
    assert!(
        w.propose_investigation_finding(
            identifier,
            "Synthetic claim",
            "observation",
            vec![json!({
                "step_id":step["id"],"source_id":source_id,"pointer":"/pairs/0"
            })]
        )
        .is_err()
    );
    assert_eq!(w.store.list("investigation_findings").unwrap(), before);
    assert_eq!(w.investigation(identifier).unwrap()["summary"], json!([]));
}

#[test]
fn attachments_and_planned_collections_share_the_ten_step_limit() {
    let (_dir, w, _) = empty_investigation();
    let searches = (0..5)
        .map(|_| Job {
            command: "telegram-search".into(),
            target: Some("synthetic research".into()),
            options: json!({}),
        })
        .collect();
    let inv = w
        .create_investigation("Synthetic mixed step limit", searches)
        .unwrap();
    let identifier = text(&inv, "id");
    synthetic_capture(
        &w,
        "run_limit_preview",
        "telegram-messages",
        "ok",
        "text/html",
    );
    let preview = w
        .attach_investigation_capture(identifier, "run_limit_preview")
        .unwrap();
    w.select_investigation_post(identifier, text(&preview, "id"), 0)
        .unwrap();
    for index in 0..3 {
        let run_id = format!("run_limit_{index}");
        synthetic_capture(&w, &run_id, "http", "ok", "application/json");
        w.attach_investigation_capture(identifier, &run_id).unwrap();
    }
    let before = w.investigation(identifier).unwrap()["steps"].clone();
    assert_eq!(before.as_array().unwrap().len(), 10);
    synthetic_capture(&w, "run_over_limit", "http", "ok", "application/json");
    assert!(
        w.attach_investigation_capture(identifier, "run_over_limit")
            .is_err()
    );
    assert!(
        w.select_investigation_post(identifier, text(&preview, "id"), 1)
            .is_err()
    );
    assert_eq!(w.investigation(identifier).unwrap()["steps"], before);
    assert_eq!(w.store.reports(20).unwrap().len(), 5);
}

#[test]
fn attachment_api_is_local_and_rejects_all_extra_approval_or_status_keys() {
    let (_dir, w, inv) = empty_investigation();
    synthetic_capture(&w, "run_api_attach", "http", "ok", "application/json");
    let valid = json!({"id":inv["id"],"run_id":"run_api_attach"});
    for key in [
        "approved", "approval", "status", "decision", "review", "unknown",
    ] {
        let mut payload = valid.clone();
        payload[key] = if key == "approved" {
            json!(true)
        } else {
            json!("accepted")
        };
        assert!(
            api::apply(&w, "investigations/attach", &payload).is_err(),
            "accepted extra key {key}"
        );
        assert_eq!(
            w.investigation(text(&inv, "id")).unwrap()["steps"],
            json!([])
        );
    }
    for payload in [json!({"id":inv["id"]}), json!({"run_id":"run_api_attach"})] {
        assert!(api::apply(&w, "investigations/attach", &payload).is_err());
    }
    let step = api::apply(&w, "investigations/attach", &valid).unwrap();
    assert_eq!(step["run_id"], "run_api_attach");
    assert_eq!(step["status"], "ok");
    assert!(api::apply(&w, "investigations/attach", &valid).is_err());
    let view = w.investigation(text(&inv, "id")).unwrap();
    assert_eq!(view["steps"], json!([step]));
    assert_eq!(view["summary"], json!([]));
    assert_eq!(view["findings"], json!([]));
    assert!(w.store.list("investigation_reviews").unwrap().is_empty());
    assert_eq!(w.store.reports(10).unwrap().len(), 1);
}
