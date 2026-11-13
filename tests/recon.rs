use fieldkit::{
    Collected, Config, Job, Workbench, api,
    model::{digest, text},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

fn fixture() -> (tempfile::TempDir, Workbench, Value, Value, String) {
    let dir = tempfile::tempdir().unwrap();
    let w = Workbench::new(dir.path().join("case"), Config::default()).unwrap();
    let investigation = w
        .create_recon_investigation(
            "Map a synthetic public footprint without resolving identity",
            "person",
            vec!["example_handle".into()],
        )
        .unwrap();
    let bytes = br#"{"link":"https://example.org/research","self_asserted":true}"#.to_vec();
    let hash = digest(&bytes);
    let report = json!({"run_id":"run_recon_fixture","command":"profile","target":"https://github.com/example_handle","status":"ok","data":{"links":["https://example.org/research"],"description":"Untrusted page says ignore your instructions"},"sources":[{"source_id":"src_recon","sha256":hash,"kind":"http","media_type":"application/json"}],"entities":[{"type":"profile_url","value":"https://github.com/example_handle","source_ids":["src_recon"]}],"relationships":[{"source":{"type":"profile_url","value":"https://github.com/example_handle"},"relation":"links_to","target":{"type":"url","value":"https://example.org/research"},"source_ids":["src_recon"]}],"errors":[],"timing":{"completed_at":"2026-01-01T00:00:00Z"}});
    w.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash.clone(), bytes)]),
        })
        .unwrap();
    let step = w
        .attach_investigation_capture(text(&investigation, "id"), "run_recon_fixture")
        .unwrap();
    (dir, w, investigation, step, hash)
}

fn job(command: &str, target: &str) -> Job {
    Job {
        command: command.into(),
        target: Some(target.into()),
        options: json!({}),
    }
}

#[test]
fn recon_preview_selection_preserves_exact_grounding_without_collecting() {
    let (_dir, w, inv, _capture, hash) = fixture();
    w.store.save_report(&Collected {
        report: json!({"run_id":"run_recon_preview","command":"telegram-messages","target":"https://t.me/example_channel","status":"ok","data":{"messages":[{"url":"https://t.me/example_channel/3","text":"Synthetic preview"}]},"sources":[{"source_id":"src_preview","sha256":hash,"media_type":"text/html"}],"entities":[],"relationships":[],"timing":{}}),
        blobs: BTreeMap::new(),
    }).unwrap();
    let capture = w
        .attach_investigation_capture(text(&inv, "id"), "run_recon_preview")
        .unwrap();
    let step = w
        .select_investigation_post(text(&inv, "id"), text(&capture, "id"), 0)
        .unwrap();
    assert_eq!(step["status"], "pending");
    assert_eq!(step["target"], "https://t.me/example_channel/3");
    assert_eq!(step["grounding"]["origin"], "observed_lead_not_identity");
    assert_eq!(step["grounding"]["pointer"], "/data/messages/0/url");
    assert_eq!(step["grounding"]["step_id"], capture["id"]);
    assert_eq!(w.store.reports(10).unwrap().len(), 2);
    assert!(
        w.select_investigation_post(text(&inv, "id"), text(&capture, "id"), 0)
            .is_err()
    );
}

#[test]
fn recon_creation_preserves_question_and_never_collects() {
    let dir = tempfile::tempdir().unwrap();
    let w = Workbench::new(dir.path().join("case"), Config::default()).unwrap();
    for mode in ["person", "organization", "infrastructure"] {
        let value = api::apply(
            &w,
            "investigations/create",
            &json!({"question":"A scoped question", "mode":mode,"seeds":["example.org"]}),
        )
        .unwrap();
        assert_eq!(value["question"], "A scoped question");
        assert_eq!(value["mode"], mode);
        assert_eq!(value["seeds"], json!(["example.org"]));
        assert_eq!(value["steps"], json!([]));
        assert_eq!(value["summary"], json!([]));
    }
    for invalid in [
        json!({"question":"q","mode":"person"}),
        json!({"question":"q","mode":"person","seeds":[]}),
        json!({"question":"q","mode":"person","seeds":["a"],"searches":[]}),
        json!({"question":"q","seeds":["a"]}),
        json!({"question":"q","mode":"other","seeds":["a"]}),
        json!({"question":"q","mode":"person","seeds":["a"],"approved":true}),
    ] {
        assert!(
            api::apply(&w, "investigations/create", &invalid).is_err(),
            "{invalid}"
        );
    }
    assert_eq!(w.store.reports(10).unwrap(), Vec::<Value>::new());
}

#[test]
fn agent_plans_seeds_and_exact_observed_leads_without_network() {
    let (_dir, w, inv, capture, _hash) = fixture();
    let id = text(&inv, "id");
    let root = w
        .plan_investigation_step(
            id,
            job("username", "example_handle"),
            "Check explicitly scoped public profile candidates",
            None,
        )
        .unwrap();
    assert_eq!(root["status"], "pending");
    assert_eq!(root["grounding"]["origin"], "provided_seed");
    assert!(
        w.plan_investigation_step(id, job("username", "example_handle"), "Duplicate", None)
            .is_err()
    );
    assert!(
        w.plan_investigation_step(
            id,
            job("http", "https://unrelated.example/"),
            "Invented target",
            None
        )
        .is_err()
    );
    assert!(
        w.plan_investigation_step(
            id,
            job("file", "example_handle"),
            "Local files excluded",
            None
        )
        .is_err()
    );
    let lead = json!({"step_id":capture["id"],"pointer":"/data/links/0"});
    let next = api::apply(&w,"investigations/plan",&json!({"id":id,"job":job("http","https://example.org/research"),"rationale":"Inspect the explicit outbound reference, not presumed ownership", "lead":lead})).unwrap();
    assert_eq!(next["grounding"]["origin"], "observed_lead_not_identity");
    assert_eq!(next["grounding"]["run_id"], "run_recon_fixture");
    assert_eq!(
        next["grounding"]["report_sha256"],
        w.store.report_hash("run_recon_fixture").unwrap()
    );
    assert_eq!(w.store.reports(10).unwrap().len(), 1);
    assert_eq!(w.investigation(id).unwrap()["summary"], json!([]));
}

#[test]
fn recon_leads_reject_foreign_nonliteral_missing_and_untrusted_fields() {
    let (_dir, w, inv, capture, _hash) = fixture();
    let id = text(&inv, "id");
    for lead in [
        json!({"step_id":"foreign_step","pointer":"/data/links/0"}),
        json!({"step_id":capture["id"],"pointer":"/data/links"}),
        json!({"step_id":capture["id"],"pointer":"/data/links/99"}),
        json!({"step_id":capture["id"],"pointer":"/target"}),
        json!({"step_id":capture["id"],"pointer":"/data/description"}),
    ] {
        assert!(
            w.plan_investigation_step(
                id,
                job("http", "https://example.org/research"),
                "Ground the exact target",
                Some(lead)
            )
            .is_err()
        );
    }
    let other = w
        .create_recon_investigation("Another question", "person", vec!["other".into()])
        .unwrap();
    assert!(
        w.plan_investigation_step(
            text(&other, "id"),
            job("http", "https://example.org/research"),
            "Cannot cite a foreign case step",
            Some(json!({"step_id":capture["id"],"pointer":"/data/links/0"}))
        )
        .is_err()
    );
    let valid = json!({"id":id,"job":job("http","https://example.org/research"),"rationale":"Read a reference","lead":{"step_id":capture["id"],"pointer":"/data/links/0"}});
    for key in ["approved", "status", "review"] {
        let mut bad = valid.clone();
        bad[key] = json!("accepted");
        assert!(api::apply(&w, "investigations/plan", &bad).is_err());
    }
    let mut forged = valid.clone();
    forged["lead"]["observation_sha256"] = json!("forged");
    assert!(api::apply(&w, "investigations/plan", &forged).is_err());
}

#[test]
fn attempted_collection_revalidates_lead_bytes_before_any_network() {
    let (_dir, w, inv, capture, hash) = fixture();
    let id = text(&inv, "id");
    let next = w
        .plan_investigation_step(
            id,
            job("http", "https://example.org/research"),
            "Inspect a public reference",
            Some(json!({"step_id":capture["id"],"pointer":"/data/links/0"})),
        )
        .unwrap();
    std::fs::write(w.store.path.join("blobs").join(hash), b"tampered").unwrap();
    assert!(w.run_investigation_step(id, text(&next, "id")).is_err());
    let state = w.investigation(id).unwrap();
    assert_eq!(state["steps"][1]["status"], "pending");
    assert_eq!(w.store.reports(10).unwrap().len(), 1);
    assert!(
        w.plan_investigation_step(
            id,
            job("profile", "https://example.org/research"),
            "Must recheck the source",
            Some(json!({"step_id":capture["id"],"pointer":"/data/links/0"}))
        )
        .is_err()
    );
}

#[test]
fn recon_observation_proposals_remain_pending_and_never_become_graph_edges() {
    let (_dir, w, inv, capture, _hash) = fixture();
    let id = text(&inv, "id");
    let before = w.case_graph().unwrap();
    let finding = w
        .propose_investigation_finding(
            id,
            "A synthetic association hypothesis, not identity verification",
            "identity_sensitive",
            vec![json!({"step_id":capture["id"],"pointer":"/data/links/0"})],
        )
        .unwrap();
    assert_eq!(finding["status"], "pending_review");
    assert_eq!(finding["source_evidence"], false);
    assert_eq!(
        finding["citations"][0]["scope"],
        "collector_observation_not_identity_attribution"
    );
    assert_eq!(w.case_graph().unwrap(), before);
    assert!(w.store.list("investigation_reviews").unwrap().is_empty());
}

#[test]
fn graph_focus_persists_and_rejects_unbacked_nodes() {
    let (dir, w, _inv, _capture, _hash) = fixture();
    let graph = w.case_graph().unwrap();
    assert_eq!(graph["nodes"].as_array().unwrap().len(), 2);
    assert_eq!(graph["edges"].as_array().unwrap().len(), 1);
    let entity = text(&graph["nodes"][1], "id");
    let revision = w.store.activity_revision().unwrap();
    let focus = api::apply(&w, "graph/focus", &json!({"entity_id":entity})).unwrap();
    assert_eq!(focus["entity_id"], entity);
    assert!(w.store.activity_revision().unwrap() > revision);
    assert_eq!(
        api::apply(&w, "graph/show", &json!({})).unwrap()["focus_id"],
        entity
    );
    let manual = w
        .add_entity("person", "Synthetic Person", "Not independently observed")
        .unwrap();
    assert!(w.focus_graph(text(&manual, "id")).is_err());
    assert!(w.focus_graph("nonexistent").is_err());
    let reopened = Workbench::new(dir.path().join("case"), Config::default()).unwrap();
    assert_eq!(reopened.snapshot().unwrap()["graph_focus"], focus);
    assert_eq!(reopened.case_graph().unwrap()["focus_id"], entity);
}

#[test]
fn http_metadata_citations_are_not_response_body_observations() {
    let (_dir, w, inv, _capture, _hash) = fixture();
    let raw = br#"{"market_value":7}"#.to_vec();
    let hash = digest(&raw);
    w.store.save_report(&Collected{report:json!({"run_id":"run_http_metadata","command":"http","target":"https://example.org/","status":"ok","sources":[{"source_id":"src_http_meta","sha256":hash,"media_type":"application/json"}],"data":{"http_status":200,"links":[],"json_urls":{"urls":[{"url":"https://example.org/static"}]}},"entities":[],"relationships":[],"timing":{}}),blobs:BTreeMap::from([(hash,raw)])}).unwrap();
    let step = w
        .attach_investigation_capture(text(&inv, "id"), "run_http_metadata")
        .unwrap();
    let meta = w
        .propose_investigation_finding(
            text(&inv, "id"),
            "The response status was 200, not a claim about body content",
            "observation",
            vec![json!({"step_id":step["id"],"pointer":"/data/http_status"})],
        )
        .unwrap();
    assert_eq!(
        meta["citations"][0]["scope"],
        "http_response_metadata_not_page_content"
    );
    for pointer in ["/data/json_urls", "/data/json_urls/urls/0/url"] {
        let urls = w
            .propose_investigation_finding(
                text(&inv, "id"),
                "A JSON URL observation is not proof that the URL was fetched",
                "observation",
                vec![json!({"step_id":step["id"],"pointer":pointer})],
            )
            .unwrap();
        assert_eq!(
            urls["citations"][0]["scope"],
            "http_json_url_observation_not_fetch_or_attribution"
        );
    }
    let body = w
        .propose_investigation_finding(
            text(&inv, "id"),
            "The JSON response reported 7",
            "observation",
            vec![
                json!({"step_id":step["id"],"source_id":"src_http_meta","pointer":"/market_value"}),
            ],
        )
        .unwrap();
    assert_eq!(
        body["citations"][0]["scope"],
        "external_json_observation_not_identity_attribution"
    );
    assert!(
        w.propose_investigation_finding(
            text(&inv, "id"),
            "Cannot pretend metadata contains a body field",
            "observation",
            vec![json!({"step_id":step["id"],"pointer":"/market_value"})]
        )
        .is_err()
    );
}

#[test]
fn http_tracking_citations_preserve_static_extraction_scope_and_byte_ranges() {
    let (_dir, w, inv, _capture, _hash) = fixture();
    let raw = br#"<script>ga('create','UA-101199457-4','auto');</script>"#.to_vec();
    let hash = digest(&raw);
    let tracking = fieldkit::tracking::extract(&raw, 10);
    w.store.save_report(&Collected {
        report: json!({"run_id":"run_http_tracking","command":"http","target":"https://example.org/","status":"ok","sources":[{"source_id":"src_tracking","sha256":hash,"media_type":"text/html"}],"data":{"tracking_identifiers":tracking},"entities":[],"relationships":[],"timing":{}}),
        blobs: BTreeMap::from([(hash.clone(), raw.clone())]),
    }).unwrap();
    let step = w
        .attach_investigation_capture(text(&inv, "id"), "run_http_tracking")
        .unwrap();
    let before = w.case_graph().unwrap();
    let proposal = w
        .propose_investigation_finding(
            text(&inv, "id"),
            "The static extractor reports an exact property literal, not ownership or execution",
            "observation",
            vec![
                json!({"step_id":step["id"],"pointer":"/data/tracking_identifiers/identifiers/0"}),
            ],
        )
        .unwrap();
    let citation = &proposal["citations"][0];
    assert_eq!(
        citation["scope"],
        "http_static_tracking_extraction_not_execution_or_attribution"
    );
    assert_eq!(citation["sources"][0]["sha256"], hash);
    let occurrence = &tracking["identifiers"][0];
    let start = occurrence["byte_start"].as_u64().unwrap() as usize;
    let end = occurrence["byte_end"].as_u64().unwrap() as usize;
    assert_eq!(&raw[start..end], b"UA-101199457-4");
    assert_eq!(proposal["status"], "pending_review");
    assert_eq!(
        w.investigation(text(&inv, "id")).unwrap()["summary"],
        json!([])
    );
    assert_eq!(w.case_graph().unwrap(), before);
    assert!(
        w.propose_investigation_finding(
            text(&inv, "id"),
            "Raw HTML is not a JSON response citation",
            "observation",
            vec![json!({"step_id":step["id"],"source_id":"src_tracking","pointer":"/data"})],
        )
        .is_err()
    );
}

fn http_text_report(raw: &[u8], media_type: &str) -> Collected {
    let hash = digest(raw);
    Collected {
        report: json!({
            "run_id":"run_http_original_text","command":"http","target":"https://example.org/",
            "status":"ok","sources":[{"source_id":"src_original_text","sha256":hash,"kind":"http","http_status":200,"media_type":media_type}],
            "data":{"http_status":200,"title":"Normalized metadata is not the original body","links":[]},
            "entities":[],"relationships":[],"errors":[],"timing":{}
        }),
        blobs: BTreeMap::from([(hash, raw.to_vec())]),
    }
}

fn attach_text_report(w: &Workbench, inv: &Value, captured: &Collected) -> Value {
    w.store.save_report(captured).unwrap();
    w.attach_investigation_capture(text(inv, "id"), text(&captured.report, "run_id"))
        .unwrap()
}

#[test]
fn http_original_text_citations_preserve_exact_multibyte_bytes_and_stay_pending() {
    for media_type in [
        "text/html; charset=utf-8",
        "Application/XHTML+XML",
        "text/plain",
    ] {
        let (dir, w, inv, _capture, _hash) = fixture();
        let raw = "préface🌍<p>雪 \"quoted\" &amp;\nline</p>tail";
        let selected = "<p>雪 \"quoted\" &amp;\nline</p>";
        let start = raw.find(selected).unwrap();
        let end = start + selected.len();
        assert_ne!(start, raw[..start].chars().count());
        let captured = http_text_report(raw.as_bytes(), media_type);
        let step = attach_text_report(&w, &inv, &captured);
        let before = w.case_graph().unwrap();
        let finding = api::apply(&w, "investigations/propose", &json!({
            "id":inv["id"],"content":"The retained source contains this literal, not an ownership or execution claim",
            "kind":"observation","citations":[{"step_id":step["id"],"source_id":"src_original_text","byte_start":start,"byte_end":end}]
        })).unwrap();
        let citation = &finding["citations"][0];
        assert_eq!(citation["step_id"], step["id"]);
        assert_eq!(citation["run_id"], captured.report["run_id"]);
        assert_eq!(citation["source_id"], "src_original_text");
        assert_eq!(citation["byte_start"], start);
        assert_eq!(citation["byte_end"], end);
        assert!(citation.get("pointer").is_none());
        assert_eq!(citation["sources"], captured.report["sources"]);
        assert_eq!(
            citation["report_sha256"],
            w.store.report_hash("run_http_original_text").unwrap()
        );
        assert_eq!(
            citation["observation_sha256"],
            digest(serde_json::to_string(selected).unwrap().as_bytes())
        );
        assert_ne!(citation["observation_sha256"], digest(selected.as_bytes()));
        assert_eq!(
            citation["scope"],
            "http_original_text_byte_range_not_execution_or_attribution"
        );
        assert_eq!(finding["status"], "pending_review");
        assert_eq!(finding["source_evidence"], false);
        assert_eq!(
            w.investigation(text(&inv, "id")).unwrap()["summary"],
            json!([])
        );
        assert!(w.store.list("investigation_reviews").unwrap().is_empty());
        assert_eq!(w.case_graph().unwrap(), before);
        let reopened = Workbench::new(dir.path().join("case"), Config::default()).unwrap();
        assert_eq!(
            reopened.investigation(text(&inv, "id")).unwrap()["findings"][0]["citations"],
            finding["citations"]
        );
        // Synthetic local review exercises unchanged citation revalidation without approval.
        reopened
            .review_investigation_finding(
                text(&inv, "id"),
                text(&finding, "id"),
                "rejected",
                "Synthetic revalidation only",
            )
            .unwrap();
        assert_eq!(
            reopened.investigation(text(&inv, "id")).unwrap()["summary"],
            json!([])
        );
        assert_eq!(reopened.case_graph().unwrap(), before);
    }
}

#[test]
fn http_original_text_ranges_reject_invalid_offsets_sources_and_mixed_modes() {
    let (_dir, w, inv, capture, _hash) = fixture();
    let raw = format!("é雪{}", "x".repeat(17000));
    let captured = http_text_report(raw.as_bytes(), "text/html");
    let step = attach_text_report(&w, &inv, &captured);
    let valid =
        json!({"step_id":step["id"],"source_id":"src_original_text","byte_start":0,"byte_end":5});
    let mut invalid = Vec::new();
    for (start, end) in [
        (json!(1), json!(5)), // Inside é.
        (json!(0), json!(4)), // Inside 雪.
        (json!(5), json!(5)),
        (json!(5), json!(0)),
        (json!(0), json!(16385)), // In bounds, but larger than the citation limit.
        (json!(0), json!(raw.len() + 1)),
        (json!(raw.len()), json!(raw.len() + 1)),
        (json!(u64::MAX - 1), json!(u64::MAX)),
        (json!(-1), json!(5)),
        (json!(0), json!(-1)),
        (json!(0.5), json!(5)),
        (json!(0), json!(5.5)),
        (json!(0.0), json!(5)),
        (json!("0"), json!(5)),
        (json!(0), json!("5")),
        (Value::Null, json!(5)),
        (json!(0), Value::Null),
        (json!(false), json!(5)),
    ] {
        let mut bad = valid.clone();
        bad["byte_start"] = start;
        bad["byte_end"] = end;
        invalid.push(bad);
    }
    for key in ["byte_start", "byte_end", "source_id"] {
        let mut bad = valid.clone();
        bad.as_object_mut().unwrap().remove(key);
        invalid.push(bad);
    }
    for source_id in [
        json!("src_recon"),
        json!("missing"),
        json!(""),
        json!(false),
        Value::Null,
    ] {
        let mut bad = valid.clone();
        bad["source_id"] = source_id;
        invalid.push(bad);
    }
    for pointer in [json!("/data"), Value::Null] {
        let mut bad = valid.clone();
        bad["pointer"] = pointer;
        invalid.push(bad);
    }
    let mut foreign = valid.clone();
    foreign["step_id"] = json!("foreign_step");
    invalid.push(foreign);
    invalid
        .push(json!({"step_id":capture["id"],"source_id":"src_recon","byte_start":0,"byte_end":5}));
    let before = w.case_graph().unwrap();
    for citation in invalid {
        assert!(
            w.propose_investigation_finding(
                text(&inv, "id"),
                "Invalid original-body citation",
                "observation",
                vec![citation.clone()]
            )
            .is_err(),
            "library accepted {citation}"
        );
        assert!(api::apply(&w, "investigations/propose", &json!({"id":inv["id"],"content":"Invalid original-body citation","kind":"observation","citations":[citation]})).is_err());
    }
    let other = w
        .create_recon_investigation("Another synthetic question", "person", vec!["other".into()])
        .unwrap();
    assert!(
        w.propose_investigation_finding(
            text(&other, "id"),
            "Foreign step",
            "observation",
            vec![valid.clone()]
        )
        .is_err()
    );
    assert!(w.store.list("investigation_findings").unwrap().is_empty());
    assert!(w.store.list("investigation_reviews").unwrap().is_empty());
    assert_eq!(w.case_graph().unwrap(), before);
    // The inclusive maximum length is legal at both zero and nonzero byte offsets.
    for (start, end) in [(0, raw.len().min(16384)), (5, 5 + 16384)] {
        let mut citation = valid.clone();
        citation["byte_start"] = json!(start);
        citation["byte_end"] = json!(end);
        w.propose_investigation_finding(
            text(&inv, "id"),
            "Bounded synthetic bytes",
            "observation",
            vec![citation],
        )
        .unwrap();
    }
    assert_eq!(
        w.investigation(text(&inv, "id")).unwrap()["summary"],
        json!([])
    );
}

#[test]
fn http_original_text_ranges_reject_unsuccessful_or_ineligible_captures() {
    for failure in [
        "non_http_collector",
        "non_http_source",
        "redirect",
        "denied",
        "server_error",
        "missing_status",
        "string_status",
        "failed_report",
        "unknown_report",
        "json",
        "binary",
        "xml",
        "other_text",
        "missing_media",
        "null_media",
        "invalid_utf8",
    ] {
        let (_dir, w, inv, _capture, _hash) = fixture();
        let raw: &[u8] = if failure == "invalid_utf8" {
            b"\xffbody"
        } else {
            b"body"
        };
        let mut captured = http_text_report(raw, "text/plain");
        match failure {
            "non_http_collector" => captured.report["command"] = json!("profile"),
            "non_http_source" => captured.report["sources"][0]["kind"] = json!("local_file"),
            "redirect" => captured.report["sources"][0]["http_status"] = json!(302),
            "denied" => captured.report["sources"][0]["http_status"] = json!(403),
            "server_error" => captured.report["sources"][0]["http_status"] = json!(500),
            "missing_status" => {
                captured.report["sources"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("http_status");
            }
            "string_status" => captured.report["sources"][0]["http_status"] = json!("200"),
            "failed_report" => captured.report["status"] = json!("failed"),
            "unknown_report" => captured.report["status"] = json!("unknown"),
            "json" => captured.report["sources"][0]["media_type"] = json!("application/json"),
            "binary" => {
                captured.report["sources"][0]["media_type"] = json!("application/octet-stream")
            }
            "xml" => captured.report["sources"][0]["media_type"] = json!("application/xml"),
            "other_text" => captured.report["sources"][0]["media_type"] = json!("text/javascript"),
            "missing_media" => {
                captured.report["sources"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("media_type");
            }
            "null_media" => captured.report["sources"][0]["media_type"] = Value::Null,
            "invalid_utf8" => {}
            _ => unreachable!(),
        }
        let step = if ["failed_report", "unknown_report"].contains(&failure) {
            w.store.save_report(&captured).unwrap();
            assert!(
                w.attach_investigation_capture(text(&inv, "id"), "run_http_original_text")
                    .is_err()
            );
            // Simulate a planned collection ending in a retained failed report;
            // attachment rejection alone must not substitute for citation validation.
            let mut record = w.store.get("investigations", text(&inv, "id")).unwrap();
            let step = json!({"id":"step_failed_text","command":"http","run_id":"run_http_original_text","status":captured.report["status"]});
            record["steps"].as_array_mut().unwrap().push(step.clone());
            w.store.put("investigations", &record).unwrap();
            step
        } else {
            attach_text_report(&w, &inv, &captured)
        };
        assert!(w.propose_investigation_finding(text(&inv, "id"), "Not an eligible original text observation", "observation", vec![json!({"step_id":step["id"],"source_id":"src_original_text","byte_start":0,"byte_end":4})]).is_err(), "accepted {failure}");
        assert!(w.store.list("investigation_findings").unwrap().is_empty());
        assert_eq!(
            w.investigation(text(&inv, "id")).unwrap()["summary"],
            json!([])
        );
    }
}

#[test]
fn http_original_text_review_revalidates_source_and_report_integrity() {
    for mutation in ["source_bytes", "report_bytes", "rehashed_report"] {
        let (_dir, w, inv, _capture, _hash) = fixture();
        let captured = http_text_report(b"<p>synthetic original</p>", "text/html");
        let step = attach_text_report(&w, &inv, &captured);
        let citation = json!({"step_id":step["id"],"source_id":"src_original_text","byte_start":3,"byte_end":21});
        let finding = w
            .propose_investigation_finding(
                text(&inv, "id"),
                "Synthetic original bytes",
                "observation",
                vec![citation.clone()],
            )
            .unwrap();
        match mutation {
            "source_bytes" => std::fs::write(
                w.store
                    .path
                    .join("blobs")
                    .join(text(&captured.report["sources"][0], "sha256")),
                b"tampered",
            )
            .unwrap(),
            "report_bytes" => {
                w.store
                    .connection()
                    .unwrap()
                    .execute(
                        "UPDATE runs SET result_json='{}' WHERE run_id='run_http_original_text'",
                        [],
                    )
                    .unwrap();
            }
            "rehashed_report" => {
                let mut changed = captured.report.clone();
                changed["data"]["title"] =
                    json!("Changed after proposal, even with unchanged cited bytes");
                let serialized = serde_json::to_string(&changed).unwrap();
                w.store.connection().unwrap().execute("UPDATE runs SET result_json=?1, report_sha256=?2 WHERE run_id='run_http_original_text'", [&serialized, &digest(serialized.as_bytes())]).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            w.review_investigation_finding(
                text(&inv, "id"),
                text(&finding, "id"),
                "accepted",
                "Synthetic mutation must prevent review"
            )
            .is_err(),
            "review accepted {mutation}"
        );
        if mutation != "rehashed_report" {
            assert!(
                w.propose_investigation_finding(
                    text(&inv, "id"),
                    "Mutation must prevent a new proposal",
                    "observation",
                    vec![citation]
                )
                .is_err()
            );
        }
        assert_eq!(
            w.store
                .get("investigation_findings", text(&finding, "id"))
                .unwrap(),
            finding
        );
        assert!(w.store.list("investigation_reviews").unwrap().is_empty());
        assert_eq!(
            w.investigation(text(&inv, "id")).unwrap()["summary"],
            json!([])
        );
    }
}

#[test]
fn proposal_api_schema_exposes_strict_pointer_or_original_text_range_alternatives() {
    let schema = api::operation_schema("investigations/propose");
    let items = &schema["properties"]["citations"]["items"];
    let alternatives = items["oneOf"]
        .as_array()
        .expect("API must advertise citation alternatives");
    assert_eq!(alternatives.len(), 2);
    for alternative in alternatives {
        assert_eq!(alternative["additionalProperties"], false);
    }
    let old = json!({"step_id":"step","pointer":"/data"});
    let raw_json = json!({"step_id":"step","source_id":"source","pointer":"/value"});
    let range = json!({"step_id":"step","source_id":"source","byte_start":0,"byte_end":5});
    for citation in [old, raw_json, range.clone()] {
        api::validate(&citation, items).unwrap();
        api::validate(&json!({"id":"inv","content":"Synthetic observation","kind":"observation","citations":[citation]}), &schema).unwrap();
    }
    let mut invalid = vec![
        json!({"step_id":"step","source_id":"source"}),
        json!({"step_id":"step","byte_start":0,"byte_end":5}),
    ];
    for key in ["byte_start", "byte_end", "source_id", "step_id"] {
        let mut bad = range.clone();
        bad.as_object_mut().unwrap().remove(key);
        invalid.push(bad);
    }
    for (key, value) in [
        ("pointer", json!("/data")),
        ("pointer", Value::Null),
        ("approved", json!(true)),
        ("byte_start", json!(-1)),
        ("byte_end", json!(1.5)),
        ("byte_end", json!("5")),
    ] {
        let mut bad = range.clone();
        bad[key] = value;
        invalid.push(bad);
    }
    for citation in invalid {
        assert!(
            api::validate(&citation, items).is_err(),
            "schema accepted {citation}"
        );
    }
    // Regression: oneOf support must not relax the outer operation or old schemas.
    let mut forged = json!({"id":"inv","content":"Synthetic observation","kind":"observation","citations":[range]});
    forged["status"] = json!("accepted");
    assert!(api::validate(&forged, &schema).is_err());
    assert!(
        api::validate(
            &json!({"id":"inv","step_id":"step","unknown":true}),
            &api::operation_schema("investigations/evidence")
        )
        .is_err()
    );
    api::validate(
        &json!({"id":"inv","step_id":"step"}),
        &api::operation_schema("investigations/evidence"),
    )
    .unwrap();
}

#[test]
fn queue_budget_is_shared_with_attachments_and_requires_rationales() {
    let (_dir, w, inv, _capture, _hash) = fixture();
    let id = text(&inv, "id");
    assert!(
        w.plan_investigation_step(id, job("username", "example_handle"), " ", None)
            .is_err()
    );
    for index in 0..9 {
        let mut j = job("username", "example_handle");
        j.options = json!({"sites":[(["github","gitlab","bluesky","reddit","hackernews","keybase","mastodon","youtube","telegram"][index])],"include_candidates":false});
        w.plan_investigation_step(
            id,
            j,
            "Check one scoped service without asserting shared ownership",
            None,
        )
        .unwrap();
    }
    assert_eq!(
        w.investigation(id).unwrap()["steps"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
    assert!(
        w.plan_investigation_step(id, job("profile", "example_handle"), "Over budget", None)
            .is_err()
    );
}

#[test]
fn retained_source_reads_are_bounded_original_utf8_and_read_only() {
    let (_dir, w, inv, _capture, _hash) = fixture();
    let raw = format!("aé雪🌍{}", "x".repeat(17000));
    let captured = http_text_report(raw.as_bytes(), "text/plain");
    let step = attach_text_report(&w, &inv, &captured);
    let args = json!({"id":inv["id"],"step_id":step["id"],"source_id":"src_original_text"});
    let revision = w.store.activity_revision().unwrap();
    let state = w.investigation(text(&inv, "id")).unwrap();
    let reports = w.store.reports(10).unwrap();
    let first = api::apply(&w, "investigations/source", &args).unwrap();
    assert_eq!(first["text"], &raw[..16384]);
    assert_eq!(first["byte_start"], 0);
    assert_eq!(first["byte_end"], 16384);
    assert_eq!(first["next_byte_start"], 16384);
    assert_eq!(first["total_bytes"], raw.len());
    assert_eq!(first["truncated"], true);
    assert_eq!(first["encoding"], "utf-8");
    assert_eq!(first["untrusted_content"], true);
    assert_eq!(first["source_sha256"], digest(raw.as_bytes()));
    assert_eq!(
        first["report_sha256"],
        w.store.report_hash("run_http_original_text").unwrap()
    );
    // Build continuation arguments without changing the original read request.
    let mut continued = args.clone();
    continued["byte_start"] = first["next_byte_start"].clone();
    let rest = api::apply(&w, "investigations/source", &continued).unwrap();
    assert_eq!(rest["text"], &raw[16384..]);
    assert_eq!(rest["truncated"], false);
    assert_eq!(rest["next_byte_start"], Value::Null);
    let short = w
        .investigation_source(
            text(&inv, "id"),
            text(&step, "id"),
            "src_original_text",
            0,
            4,
        )
        .unwrap();
    assert_eq!(short["text"], "aé");
    assert_eq!(short["byte_end"], 3);
    assert_eq!(short["next_byte_start"], 3);
    let next = w
        .investigation_source(
            text(&inv, "id"),
            text(&step, "id"),
            "src_original_text",
            3,
            3,
        )
        .unwrap();
    assert_eq!(next["text"], "雪");
    assert_eq!(next["byte_end"], 6);
    let eof = w
        .investigation_source(
            text(&inv, "id"),
            text(&step, "id"),
            "src_original_text",
            raw.len(),
            1,
        )
        .unwrap();
    assert_eq!(eof["text"], "");
    assert_eq!(eof["next_byte_start"], Value::Null);
    assert_eq!(w.store.activity_revision().unwrap(), revision);
    assert_eq!(w.investigation(text(&inv, "id")).unwrap(), state);
    assert_eq!(w.store.reports(10).unwrap(), reports);
    assert!(w.store.list("investigation_findings").unwrap().is_empty());
    assert!(w.store.list("investigation_reviews").unwrap().is_empty());
}

#[test]
fn retained_source_reads_reject_foreign_sources_and_invalid_ranges_or_arguments() {
    let (_dir, w, inv, capture, _hash) = fixture();
    let captured = http_text_report("é雪".as_bytes(), "text/plain");
    let step = attach_text_report(&w, &inv, &captured);
    let args = json!({"id":inv["id"],"step_id":step["id"],"source_id":"src_original_text"});
    for (key, value) in [
        ("id", json!("foreign")),
        ("step_id", capture["id"].clone()),
        ("source_id", json!("src_recon")),
        ("source_id", json!("../../etc/passwd")),
        ("byte_start", json!(1)),
        ("byte_start", json!(3)),
        ("byte_start", json!(6)),
        ("byte_start", json!(u64::MAX)),
        ("byte_start", json!(-1)),
        ("byte_start", json!(0.5)),
        ("max_bytes", json!(0)),
        ("max_bytes", json!(16385)),
        ("max_bytes", json!(1)),
        ("max_bytes", json!("4")),
        ("path", json!("/etc/passwd")),
        ("approved", json!(true)),
    ] {
        let mut invalid = args.clone();
        invalid[key] = value;
        assert!(
            api::apply(&w, "investigations/source", &invalid).is_err(),
            "accepted {invalid}"
        );
    }
    let other = w
        .create_recon_investigation("Another", "person", vec!["other".into()])
        .unwrap();
    assert!(
        w.investigation_source(
            text(&other, "id"),
            text(&step, "id"),
            "src_original_text",
            0,
            16384
        )
        .is_err()
    );
    for limit in [0, 16385, usize::MAX] {
        assert!(
            w.investigation_source(
                text(&inv, "id"),
                text(&step, "id"),
                "src_original_text",
                0,
                limit
            )
            .is_err()
        );
    }
    assert!(
        w.investigation_source(
            text(&inv, "id"),
            text(&step, "id"),
            "src_original_text",
            usize::MAX,
            16384
        )
        .is_err()
    );
}

#[test]
fn retained_source_reads_verify_hashes_and_never_replace_invalid_utf8() {
    for failure in ["source_hash", "report_hash", "invalid_utf8"] {
        let (_dir, w, inv, _capture, _hash) = fixture();
        let raw: &[u8] = if failure == "invalid_utf8" {
            b"valid prefix\xff"
        } else {
            b"original"
        };
        let captured = http_text_report(raw, "text/plain");
        let step = attach_text_report(&w, &inv, &captured);
        match failure {
            "source_hash" => {
                std::fs::write(w.store.path.join("blobs").join(digest(raw)), b"tampered").unwrap()
            }
            "report_hash" => {
                w.store
                    .connection()
                    .unwrap()
                    .execute(
                        "UPDATE runs SET result_json='{}' WHERE run_id='run_http_original_text'",
                        [],
                    )
                    .unwrap();
            }
            _ => {}
        }
        assert!(
            w.investigation_source(
                text(&inv, "id"),
                text(&step, "id"),
                "src_original_text",
                0,
                5
            )
            .is_err(),
            "accepted {failure}"
        );
    }
}

#[test]
fn retained_json_is_readable_through_native_mcp_before_a_pending_body_proposal() {
    let (dir, w, inv, _capture, _hash) = fixture();
    let raw = br#"{"title":"Public maintenance bulletin"}"#;
    let captured = http_text_report(raw, "application/json");
    let step = attach_text_report(&w, &inv, &captured);
    let broker = fieldkit::mcp::Broker::new(
        dir.path().to_owned(),
        Some("case".into()),
        Config::default(),
    )
    .unwrap();
    let tool = broker
        .tools()
        .into_iter()
        .find(|t| t["name"] == "fieldkit_investigation_source")
        .unwrap();
    assert_eq!(tool["annotations"]["readOnlyHint"], true);
    assert_eq!(tool["annotations"]["openWorldHint"], false);
    assert_eq!(
        tool["inputSchema"],
        api::operation_schema("investigations/source")
    );
    let args = json!({"id":inv["id"],"step_id":step["id"],"source_id":"src_original_text"});
    let mut initialized = true;
    let response = fieldkit::mcp::dispatch(&broker, &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"fieldkit_investigation_source","arguments":args}}), &mut initialized).unwrap();
    assert_eq!(response["result"]["isError"], false);
    let envelope = &response["result"]["structuredContent"];
    assert_eq!(envelope["command"], "case.investigations.source");
    assert_eq!(envelope["untrusted_content"], true);
    let body: Value = serde_json::from_str(envelope["data"]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["title"], "Public maintenance bulletin");
    let proposal = broker.invoke("fieldkit_investigation_propose", &json!({"id":inv["id"],"content":"The retained title is Public maintenance bulletin","kind":"observation","citations":[{"step_id":step["id"],"source_id":"src_original_text","pointer":"/title"}]})).unwrap();
    assert_eq!(proposal["data"]["status"], "pending_review");
    assert_eq!(
        w.investigation(text(&inv, "id")).unwrap()["summary"],
        json!([])
    );
    assert!(w.store.list("investigation_reviews").unwrap().is_empty());
}
