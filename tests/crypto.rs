//! Offline synthetic fixtures only. No public wallet/transaction requests are authorized.
use fieldkit::{
    Collected, Config, Job, Workbench, api, collectors,
    model::{digest, text},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

const ADDRESS: &str = "ethereum:0xAa00000000000000000000000000000000000001";
const OTHER: &str = "ethereum:0xbb00000000000000000000000000000000000002";
const TX: &str = "ethereum:0xCc00000000000000000000000000000000000000000000000000000000000003";

fn job(command: &str, target: &str) -> Job {
    Job {
        command: command.into(),
        target: Some(target.into()),
        options: json!({}),
    }
}
fn workbench() -> (tempfile::TempDir, Workbench) {
    let dir = tempfile::tempdir().unwrap();
    let w = Workbench::new(
        dir.path().join("case"),
        Config {
            max_requests: 0,
            ..Config::default()
        },
    )
    .unwrap();
    (dir, w)
}
fn capture_report(run: &str, command: &str, target: &str) -> (Value, Vec<u8>) {
    let raw = json!({"hash":target.strip_prefix("ethereum:").unwrap_or(target),
        "items":[{"hash":TX.strip_prefix("ethereum:").unwrap(),"from":{"hash":ADDRESS.strip_prefix("ethereum:").unwrap()},"to":{"hash":OTHER.strip_prefix("ethereum:").unwrap()}}],
        "fixture":"Synthetic preserved fixture, not a live provider response"});
    let bytes = serde_json::to_vec(&raw).unwrap();
    let hash = digest(&bytes);
    let report = json!({"run_id":run,"command":command,"target":target,"status":"ok",
        "data":{"address":{"target":target,"provider_fields":{"name":OTHER}},
            "transactions":[{"target":TX,"from_target":ADDRESS,"to_target":OTHER,"amount_wei":"17","confirmation_status":"pending_or_unconfirmed"}],
            "untrusted_link":OTHER,"history_complete":false},
        "sources":[{"source_id":"src_crypto_fixture","sha256":hash,"kind":"http","http_status":200,"media_type":"application/json"}],
        "entities":[],"relationships":[],"errors":[],"timing":{}});
    (report, bytes)
}
fn save(w: &Workbench, run: &str, command: &str, target: &str) -> (Value, Vec<u8>) {
    let (report, bytes) = capture_report(run, command, target);
    w.store
        .save_report(&Collected {
            report: report.clone(),
            blobs: BTreeMap::from([(digest(&bytes), bytes.clone())]),
        })
        .unwrap();
    (report, bytes)
}
fn fixture() -> (tempfile::TempDir, Workbench, Value, Value, Vec<u8>) {
    let (dir, w) = workbench();
    let inv = w
        .create_recon_investigation(
            "Inspect synthetic public Ethereum transaction roles, not ownership",
            "crypto",
            vec![ADDRESS.into()],
        )
        .unwrap();
    let (_, bytes) = save(&w, "run_crypto_fixture", "evm-address", ADDRESS);
    let step = w
        .attach_investigation_capture(text(&inv, "id"), "run_crypto_fixture")
        .unwrap();
    (dir, w, inv, step, bytes)
}

#[test]
fn crypto_creation_preserves_scope_and_strict_mode_without_collecting() {
    let (_dir, w) = workbench();
    let question = "A synthetic forensic question, not an authorized live investigation";
    let inv = api::apply(
        &w,
        "investigations/create",
        &json!({"question":question,"mode":"crypto","seeds":[ADDRESS,TX]}),
    )
    .unwrap();
    assert_eq!(inv["question"], question);
    assert_eq!(inv["mode"], "crypto");
    assert_eq!(inv["seeds"], json!([ADDRESS, TX]));
    assert_eq!(inv["steps"], json!([]));
    assert_eq!(inv["summary"], json!([]));
    assert!(w.store.reports(10).unwrap().is_empty());
    for invalid in [
        json!({"question":"q","mode":"crypto","seeds":[]}),
        json!({"question":"q","mode":"crypto","seeds":["0x0000000000000000000000000000000000000000"]}),
        json!({"question":"q","mode":"crypto","seeds":["polygon:0x0000000000000000000000000000000000000000"]}),
        json!({"question":"q","mode":"crypto","seeds":[ADDRESS],"searches":[]}),
        json!({"question":"q","mode":null,"seeds":[ADDRESS]}),
        json!({"question":"q","mode":["crypto"],"seeds":[ADDRESS]}),
        json!({"question":"q","mode":"bitcoin","seeds":[ADDRESS]}),
        json!({"question":"q","mode":"crypto","seeds":[ADDRESS],"approved":true}),
    ] {
        assert!(
            api::apply(&w, "investigations/create", &invalid).is_err(),
            "accepted {invalid}"
        );
    }
    assert_eq!(w.investigations().unwrap().len(), 1);
}

#[test]
fn target_chain_type_hex_length_and_options_fail_before_network() {
    let bare = ADDRESS.strip_prefix("ethereum:").unwrap();
    let mut cases = Vec::new();
    for command in ["evm-address", "evm-transaction"] {
        for target in [
            bare.to_owned(),
            format!("polygon:{bare}"),
            format!("eip155:1:{bare}"),
            format!("ethereum:{bare} "),
            format!("Ethereum:{bare}"),
            format!("ethereum:{bare}/transactions"),
            format!("ethereum:0x{}", "g".repeat(40)),
            format!("ethereum:0x{}", "a".repeat(39)),
            format!("ethereum:0x{}", "a".repeat(65)),
            format!("ethereum:0x{}", "雪".repeat(40)),
        ] {
            cases.push(job(command, &target));
        }
        let target = if command == "evm-address" {
            ADDRESS
        } else {
            TX
        };
        for options in [
            json!({"endpoint":"https://example.org"}),
            json!({"chain":"polygon"}),
            json!({"page":2}),
            json!({"api_key":"not-allowed"}),
            json!({"retry":true}),
            json!({"before":"cursor"}),
        ] {
            let mut j = job(command, target);
            j.options = options;
            cases.push(j);
        }
    }
    cases.push(job("evm-address", TX));
    cases.push(job("evm-transaction", ADDRESS));
    for options in [
        json!({"limit":0}),
        json!({"limit":51}),
        json!({"limit":1.5}),
        json!({"limit":"1"}),
    ] {
        let mut j = job("evm-address", ADDRESS);
        j.options = options;
        cases.push(j);
    }
    let mut tx_limit = job("evm-transaction", TX);
    tx_limit.options = json!({"limit":1});
    cases.push(tx_limit);
    for j in cases {
        assert!(collectors::validate_job(&j).is_err(), "accepted {j:?}");
        let collected = collectors::collect(&j, &Config::default()).unwrap();
        assert_eq!(collected.report["status"], "invalid");
        assert_eq!(collected.report["requests"], 0);
        assert_eq!(collected.report["sources"], json!([]));
        assert!(collected.blobs.is_empty());
    }
    for j in [job("evm-address", ADDRESS), job("evm-transaction", TX)] {
        collectors::validate_job(&j).unwrap();
        collectors::validate_crypto_target(j.target.as_deref().unwrap()).unwrap();
        let cfg = Config {
            max_results: 0,
            ..Config::default()
        };
        let collected = collectors::collect(&j, &cfg).unwrap();
        assert_eq!(collected.report["status"], "invalid");
        assert_eq!(collected.report["requests"], 0);
        let cfg = Config {
            max_requests: 0,
            ..Config::default()
        };
        let collected = collectors::collect(&j, &cfg).unwrap();
        assert_eq!(collected.report["requests"], 0);
        assert_eq!(collected.report["target"], j.target.unwrap());
        assert_eq!(
            collected.report["data"]["coverage"]["history_complete"],
            false
        );
        assert_eq!(collected.report["errors"][0]["code"], "REQUEST_LIMIT");
        assert_eq!(collected.report["errors"][0]["retryable"], false);
    }
}

#[test]
fn crypto_mode_is_exclusive_in_plans_attachments_selection_and_execution() {
    let (_dir, w, inv, _capture, _bytes) = fixture();
    let id = text(&inv, "id");
    for command in [
        "http",
        "archive",
        "username",
        "profile",
        "dns",
        "telegram-post",
    ] {
        assert!(
            w.plan_investigation_step(id, job(command, ADDRESS), "Excluded mode collector", None)
                .is_err()
        );
    }
    for mode in ["person", "organization", "infrastructure"] {
        let other = w
            .create_recon_investigation("Other mode", mode, vec![ADDRESS.into(), TX.into()])
            .unwrap();
        for j in [job("evm-address", ADDRESS), job("evm-transaction", TX)] {
            assert!(
                w.plan_investigation_step(text(&other, "id"), j, "Not a crypto plan", None)
                    .is_err()
            );
        }
        assert!(
            w.attach_investigation_capture(text(&other, "id"), "run_crypto_fixture")
                .is_err()
        );
    }
    save(&w, "run_profile", "profile", ADDRESS);
    assert!(w.attach_investigation_capture(id, "run_profile").is_err());
    let legacy = w.create_investigation("Legacy archive", vec![]).unwrap();
    assert!(
        w.attach_investigation_capture(text(&legacy, "id"), "run_crypto_fixture")
            .is_err()
    );
    // Directly forged persisted steps are rechecked before the collection closure.
    for (mode, command, target) in [
        ("crypto", "http", ADDRESS),
        ("person", "evm-transaction", TX),
    ] {
        let other = w
            .create_recon_investigation("Revalidate execution", mode, vec![ADDRESS.into()])
            .unwrap();
        let mut record = w.store.get("investigations", text(&other, "id")).unwrap();
        record["steps"] = json!([{"id":"step_forged","command":command,"target":target,"options":{},"status":"pending"}]);
        w.store.put("investigations", &record).unwrap();
        assert!(
            w.run_investigation_step(text(&other, "id"), "step_forged")
                .is_err()
        );
    }
    let mut record = w.store.get("investigations", id).unwrap();
    let (mut report, bytes) = capture_report("run_preview", "telegram-messages", ADDRESS);
    report["data"]["messages"] = json!([{"url":"https://t.me/synthetic/1"}]);
    w.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::from([(digest(&bytes), bytes)]),
        })
        .unwrap();
    record["steps"].as_array_mut().unwrap().push(json!({"id":"step_preview","command":"telegram-messages","run_id":"run_preview","status":"ok"}));
    w.store.put("investigations", &record).unwrap();
    assert!(w.select_investigation_post(id, "step_preview", 0).is_err());
}

#[test]
fn exact_same_investigation_grounded_pivots_exclude_labels_links_case_changes_and_foreign_steps() {
    let (_dir, w, inv, capture, _bytes) = fixture();
    let id = text(&inv, "id");
    let lead = json!({"step_id":capture["id"],"pointer":"/data/transactions/0/target"});
    let step = w
        .plan_investigation_step(
            id,
            job("evm-transaction", TX),
            "Inspect an observed transaction, not ownership",
            Some(lead.clone()),
        )
        .unwrap();
    assert_eq!(step["grounding"]["origin"], "observed_lead_not_identity");
    assert_eq!(
        step["grounding"]["report_sha256"],
        w.store.report_hash("run_crypto_fixture").unwrap()
    );
    assert_eq!(
        step["grounding"]["observation_sha256"],
        digest(serde_json::to_string(TX).unwrap().as_bytes())
    );
    for pointer in [
        "/data/address/provider_fields/name",
        "/data/untrusted_link",
        "/data/transactions/0",
        "/data/transactions/00/to_target",
        "/target",
    ] {
        assert!(
            w.plan_investigation_step(
                id,
                job("evm-address", OTHER),
                "Must not pivot on labels",
                Some(json!({"step_id":capture["id"],"pointer":pointer}))
            )
            .is_err(),
            "accepted {pointer}"
        );
    }
    for (target, lead) in [
        (OTHER.to_owned(), None),
        (
            OTHER.to_ascii_lowercase(),
            Some(json!({"step_id":"missing","pointer":"/data/transactions/0/to_target"})),
        ),
        (
            ADDRESS.to_ascii_lowercase(),
            Some(json!({"step_id":capture["id"],"pointer":"/data/transactions/0/from_target"})),
        ),
        (
            OTHER.replace("ethereum:", "polygon:"),
            Some(json!({"step_id":capture["id"],"pointer":"/data/transactions/0/to_target"})),
        ),
    ] {
        assert!(
            w.plan_investigation_step(
                id,
                job("evm-address", &target),
                "Require exact scoped target",
                lead
            )
            .is_err()
        );
    }
    let other = w
        .create_recon_investigation("Foreign scope", "crypto", vec![ADDRESS.into()])
        .unwrap();
    assert!(
        w.plan_investigation_step(
            text(&other, "id"),
            job("evm-transaction", TX),
            "Cannot use foreign step",
            Some(lead)
        )
        .is_err()
    );
    let address = w
        .plan_investigation_step(
            id,
            job("evm-address", OTHER),
            "Inspect literal recipient role only",
            Some(json!({"step_id":capture["id"],"pointer":"/data/transactions/0/to_target"})),
        )
        .unwrap();
    assert_eq!(address["target"], OTHER);
    save(&w, "run_unscoped", "evm-address", OTHER);
    assert!(w.attach_investigation_capture(id, "run_unscoped").is_err());
}

#[test]
fn attempts_are_once_only_and_cannot_retry_through_limit_changes_or_attachments() {
    let (_dir, w) = workbench();
    let inv = w
        .create_recon_investigation(
            "Synthetic once-only budget",
            "crypto",
            vec![ADDRESS.into(), ADDRESS.to_ascii_lowercase()],
        )
        .unwrap();
    let id = text(&inv, "id");
    let step = w
        .plan_investigation_step(
            id,
            job("evm-address", ADDRESS),
            "Bounded synthetic attempt",
            None,
        )
        .unwrap();
    let report = w.run_investigation_step(id, text(&step, "id")).unwrap();
    assert_eq!(report["requests"], 0); // Actual Context request guard, no DNS/provider call.
    assert_eq!(report["errors"][0]["code"], "REQUEST_LIMIT");
    assert!(w.run_investigation_step(id, text(&step, "id")).is_err());
    let mut changed = job("evm-address", ADDRESS);
    changed.options = json!({"limit":1});
    assert!(
        w.plan_investigation_step(id, changed, "No limit-changing retry", None)
            .is_err()
    );
    assert!(
        w.plan_investigation_step(
            id,
            job("evm-address", &ADDRESS.to_ascii_lowercase()),
            "No case-changing retry",
            None
        )
        .is_err()
    );
    save(&w, "run_retry", "evm-address", ADDRESS);
    assert!(w.attach_investigation_capture(id, "run_retry").is_err());
    assert_eq!(
        w.investigation(id).unwrap()["steps"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn source_reader_proposals_and_revalidation_preserve_hashes_without_identity_approval() {
    let (dir, w, inv, capture, bytes) = fixture();
    let id = text(&inv, "id");
    let source = w
        .investigation_source(id, text(&capture, "id"), "src_crypto_fixture", 0, 16384)
        .unwrap();
    assert_eq!(source["text"], String::from_utf8(bytes.clone()).unwrap());
    assert_eq!(source["source_sha256"], digest(&bytes));
    assert_eq!(source["untrusted_content"], true);
    let before = w.case_graph().unwrap();
    let finding = api::apply(&w,"investigations/propose",&json!({"id":id,"kind":"identity_sensitive","content":"A synthetic unverified provider association hypothesis; not source ownership evidence","citations":[{"step_id":capture["id"],"pointer":"/data/transactions/0"}]})).unwrap();
    assert_eq!(finding["status"], "pending_review");
    assert_eq!(finding["source_evidence"], false);
    assert_eq!(
        finding["citations"][0]["scope"],
        "ethereum_provider_observation_not_transfer_finality_identity_or_ownership"
    );
    assert_eq!(
        finding["citations"][0]["sources"][0]["sha256"],
        digest(&bytes)
    );
    assert_eq!(w.investigation(id).unwrap()["summary"], json!([]));
    assert!(w.store.list("investigation_reviews").unwrap().is_empty());
    assert_eq!(w.case_graph().unwrap(), before);
    let reopened = Workbench::new(dir.path().join("case"), Config::default()).unwrap();
    assert_eq!(reopened.investigation(id).unwrap()["findings"][0], finding);
    let planned = w
        .plan_investigation_step(
            id,
            job("evm-transaction", TX),
            "Check preserved source integrity",
            Some(json!({"step_id":capture["id"],"pointer":"/data/transactions/0/target"})),
        )
        .unwrap();
    std::fs::write(w.store.path.join("blobs").join(digest(&bytes)), b"tampered").unwrap();
    assert!(w.run_investigation_step(id, text(&planned, "id")).is_err());
    assert!(
        w.investigation_source(id, text(&capture, "id"), "src_crypto_fixture", 0, 16384)
            .is_err()
    );
    assert!(
        w.review_investigation_finding(
            id,
            text(&finding, "id"),
            "accepted",
            "Tampered sources cannot be reviewed"
        )
        .is_err()
    );
    assert!(w.store.list("investigation_reviews").unwrap().is_empty());
}

#[test]
fn shared_ten_step_budget_includes_preserved_captures() {
    let (_dir, w) = workbench();
    let inv = w
        .create_recon_investigation("Ten synthetic steps", "crypto", vec![ADDRESS.into()])
        .unwrap();
    let (mut report, _) = capture_report("run_crypto_budget", "evm-address", ADDRESS);
    report["data"]["transactions"] = json!(
        (0..10)
            .map(|index| json!({"target":format!("ethereum:0x{index:064x}")}))
            .collect::<Vec<_>>()
    );
    // Synthetic fixture setup before preservation, never changing a saved capture.
    let bytes = serde_json::to_vec(&report["data"]).unwrap();
    let hash = digest(&bytes);
    report["sources"][0]["sha256"] = json!(hash);
    w.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash, bytes)]),
        })
        .unwrap();
    let capture = w
        .attach_investigation_capture(text(&inv, "id"), "run_crypto_budget")
        .unwrap();
    for index in 0..9 {
        w.plan_investigation_step(text(&inv,"id"),job("evm-transaction",&format!("ethereum:0x{index:064x}")),"Inspect one grounded transaction",Some(json!({"step_id":capture["id"],"pointer":format!("/data/transactions/{index}/target")}))).unwrap();
    }
    assert!(
        w.plan_investigation_step(
            text(&inv, "id"),
            job("evm-transaction", &format!("ethereum:0x{:064x}", 9)),
            "Over budget",
            Some(json!({"step_id":capture["id"],"pointer":"/data/transactions/9/target"}))
        )
        .is_err()
    );
    assert_eq!(
        w.investigation(text(&inv, "id")).unwrap()["steps"]
            .as_array()
            .unwrap()
            .len(),
        10
    );
}

#[test]
fn capabilities_api_and_mcp_bind_crypto_schemas_and_caveats() {
    let (dir, w) = workbench();
    let catalog = collectors::capabilities();
    assert_eq!(catalog["crypto"]["chain_id"], "eip155:1");
    assert_eq!(catalog["crypto"]["max_pages"], 1);
    assert_eq!(catalog["crypto"]["live_availability_verified"], false);
    assert_eq!(catalog["crypto"]["automatic_retries"], false);
    let schema = api::operation_schema("investigations/create");
    assert!(
        schema["properties"]["mode"]["enum"]
            .as_array()
            .unwrap()
            .contains(&json!("crypto"))
    );
    assert_eq!(schema["properties"]["mode"]["type"], "string");
    let broker = fieldkit::mcp::Broker::new(
        dir.path().to_owned(),
        Some("case".into()),
        Config {
            max_requests: 0,
            ..Config::default()
        },
    )
    .unwrap();
    for (command, target) in [("evm-address", ADDRESS), ("evm-transaction", TX)] {
        let tool_name = format!("fieldkit_{}", command.replace('-', "_"));
        let tool = broker
            .tools()
            .into_iter()
            .find(|t| t["name"] == tool_name)
            .unwrap();
        assert_eq!(
            tool["inputSchema"]["properties"]["target"],
            collectors::command_schema(command)["properties"]["target"]
        );
        assert_eq!(tool["inputSchema"]["additionalProperties"], false);
        assert!(tool["description"].as_str().unwrap().contains("Ethereum"));
        assert!(
            broker
                .invoke(&tool_name, &json!({"target":target,"chain":"polygon"}))
                .is_err()
        );
        assert!(
            broker
                .invoke(
                    &tool_name,
                    &json!({"target":target.replace("ethereum:","polygon:")})
                )
                .is_err()
        );
    }
    let created = broker
        .invoke(
            "fieldkit_investigation_create",
            &json!({"question":"Offline MCP scope","mode":"crypto","seeds":[ADDRESS]}),
        )
        .unwrap();
    assert_eq!(created["data"]["mode"], "crypto");
    assert_eq!(w.investigations().unwrap().len(), 1);
    let mut initialized = false;
    let init = fieldkit::mcp::dispatch(&broker,&json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18"}}),&mut initialized).unwrap();
    let instructions = init["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("Ethereum-mainnet-only"));
    assert!(instructions.contains("failed/pending"));
}
