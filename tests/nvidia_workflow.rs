//! Controller-level tests use child-process environment and a real loopback
//! gateway; they never mutate the test process's environment or call a model.
use fieldkit::{
    Collected, Config, Workbench,
    discovery::DiscoveryOptions,
    model::{digest, text},
    planner::validate_recommendation,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    io::Read,
    path::Path,
    process::{Command, Output},
    thread::{self, JoinHandle},
    time::Duration,
};
use tiny_http::{Header, Response, Server, StatusCode};

const TOKEN: &str = "workflow-test-token-not-a-real-credential";
struct Gateway {
    url: String,
    join: JoinHandle<Vec<Value>>,
}
impl Gateway {
    fn new(ready: bool, answer: Option<Value>) -> Self {
        let server = Server::http((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let url = format!("http://{}", server.server_addr());
        let count = 4 + usize::from(answer.is_some());
        let join = thread::spawn(move || {
            let mut requests = vec![];
            for _ in 0..count {
                let Some(mut request) = server.recv_timeout(Duration::from_secs(5)).unwrap() else {
                    break;
                };
                let path = request.url().to_owned();
                let authenticated = request.headers().iter().any(|header| {
                    header.field.equiv("x-api-key") && header.value.as_str() == TOKEN
                });
                let mut bytes = Vec::new();
                request
                    .as_reader()
                    .take(256 * 1024)
                    .read_to_end(&mut bytes)
                    .unwrap();
                let body = if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                };
                requests.push(json!({"path":path,"authenticated":authenticated,"body":body}));
                let (status, media, body) = match path.as_str() {
                    "/api/health" => (
                        if ready { 200 } else { 503 },
                        "application/json",
                        json!({"status":if ready{"ok"}else{"unavailable"}}).to_string(),
                    ),
                    "/api/semantic-compilation/status" => (
                        200,
                        "application/json",
                        json!({"calculated":ready,"running":false,"last_success_at":if ready{Some("2026-11-11T10:00:00Z")}else{None},"last_failure_at":null}).to_string(),
                    ),
                    "/api/connections" => (200, "application/json", json!({"count":1}).to_string()),
                    "/api/datasources/dbs" => (
                        200,
                        "application/json",
                        json!({"data":[{"name":"fieldkit"}]}).to_string(),
                    ),
                    "/api/chat/completions" => (
                        200,
                        "text/event-stream",
                        format!(
                            "data: {}\n\ndata: [DONE]\n\n",
                            json!({"type":"result","answer":answer.as_ref().expect("unexpected model request")})
                        ),
                    ),
                    _ => (404, "application/json", "{}".into()),
                };
                request
                    .respond(
                        Response::from_string(body)
                            .with_status_code(StatusCode(status))
                            .with_header(
                                Header::from_bytes(b"Content-Type", media.as_bytes()).unwrap(),
                            ),
                    )
                    .unwrap();
            }
            requests
        });
        Self { url, join }
    }
    fn finish(self) -> Vec<Value> {
        self.join.join().unwrap()
    }
}

fn child(case: &Path, projection: &Path, url: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_fieldkit"))
        .arg("--case")
        .arg(case)
        .args(args)
        .env("AUTO_ONTOLOGY_API_URL", url)
        .env("AUTO_ONTOLOGY_API_TOKEN", TOKEN)
        .env("AUTO_ONTOLOGY_TARGET_DB", "fieldkit")
        .env("AUTO_ONTOLOGY_TIMEOUT", "5")
        .env("FIELDKIT_AUTO_ONTOLOGY_PROJECTION", projection)
        .output()
        .unwrap()
}

fn observation(w: &Workbench, command: &str, source_kind: &str, value: &str) -> Value {
    let body = format!("Retained {source_kind} observation: {value}").into_bytes();
    let hash = digest(&body);
    let report = json!({"schema_version":"1.0","run_id":"run_observation","command":command,"target":"https://profile.example/fixture","status":"ok","data":{"url":value},
        "sources":[{"source_id":"src_observation","kind":source_kind,"sha256":hash,"hash_scope":"source_bytes"}],
        "entities":[],"relationships":[],"errors":[],"timing":{"started_at":"2026-11-11T00:00:00Z","completed_at":"2026-11-11T00:00:01Z"}});
    w.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash, body)]),
        })
        .unwrap();
    json!({"evidence_id":"evidence_run_observation","run_id":"run_observation","report_sha256":w.store.report_hash("run_observation").unwrap(),"json_pointer":"/data/url","source_ids":["src_observation"]})
}

#[test]
fn model_assessments_cannot_become_independent_grounding_for_new_jobs() {
    for (command, kind) in [
        ("analysis.nvidia", "model_assessment"),
        ("analysis.other", "model_assessment"),
        ("http", "model_assessment"),
    ] {
        let directory = tempfile::tempdir().unwrap();
        let w = Workbench::new(directory.path(), Config::default()).unwrap();
        let session = w
            .create_discovery(DiscoveryOptions {
                seed: "public_example".into(),
                ..Default::default()
            })
            .unwrap();
        let mut row = observation(
            &w,
            command,
            kind,
            "https://project.example/invented-by-model",
        );
        row["command"] = "http".into();
        row["target"] = "https://project.example/invented-by-model".into();
        row["options_json"] = "{}".into();
        let answer = json!({"response":"Follow the model's suggested URL","sql_code":"SELECT source_url FROM evidence","sql_response_from_db":[row],"rows_truncated":false,"row_count":1});
        assert!(
            validate_recommendation(&w, &session, &answer).is_err(),
            "{command} / {kind} must not authorize another collection"
        );
    }
}

#[test]
fn planner_settings_preserve_queue_and_usage_and_refuse_active_worker() {
    let directory = tempfile::tempdir().unwrap();
    let w = Workbench::new(directory.path(), Config::default()).unwrap();
    let mut session = w
        .create_discovery(DiscoveryOptions {
            seed: "public_example".into(),
            ..Default::default()
        })
        .unwrap();
    let id = text(&session, "id").to_owned();
    let jobs = w.discovery_jobs(&id).unwrap();
    session["reasoning_used"] = json!(3);
    w.store.put("discovery", &session).unwrap();
    let guard = w.store.lock("discovery.lock").unwrap();
    assert!(w.configure_discovery_planner(&id, "nvidia", 6).is_err());
    drop(guard);
    assert!(w.configure_discovery_planner(&id, "nvidia", 2).is_err());
    assert_eq!(
        w.store.get("discovery", &id).unwrap()["options"]["planner"],
        "local"
    );
    let changed = w.configure_discovery_planner(&id, "nvidia", 6).unwrap();
    assert_eq!(changed["reasoning_used"], 3);
    assert_eq!(changed["options"]["reasoning_budget"], 6);
    assert_eq!(w.discovery_jobs(&id).unwrap(), jobs);
}

#[test]
fn unavailable_gateway_stops_before_collection_without_local_fallback() {
    let directory = tempfile::tempdir().unwrap();
    let case = directory.path().join("case");
    let projection = directory.path().join("projection/fieldkit.sqlite");
    let w = Workbench::new(&case, Config::default()).unwrap();
    // This reserved address is rejected without I/O even if a regression tries
    // to run the seed. The correct path makes no collector attempt at all.
    let session = w
        .create_discovery(DiscoveryOptions {
            kind: "url".into(),
            seed: "https://192.0.2.1/".into(),
            planner: "nvidia".into(),
            ..Default::default()
        })
        .unwrap();
    let id = text(&session, "id").to_owned();
    let gateway = Gateway::new(false, None);
    let output = child(
        &case,
        &projection,
        &gateway.url,
        &["discover", "--resume", &id],
    );
    assert!(
        !output.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = w.store.get("discovery", &id).unwrap();
    assert_eq!(saved["status"], "needs_setup");
    assert_eq!(saved["attempted"], 0);
    assert!(w.store.reports(20).unwrap().is_empty());
    assert_eq!(w.discovery_jobs(&id).unwrap()[0]["status"], "pending");
    let decisions = w.store.list("reasoning").unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0]["status"], "unavailable");
    let requests = gateway.finish();
    assert_eq!(requests.len(), 4);
    assert!(
        requests
            .iter()
            .all(|request| request["authenticated"] == true)
    );
    assert!(
        !requests
            .iter()
            .any(|request| request["path"] == "/api/chat/completions")
    );
}

#[test]
fn actual_gateway_wire_path_preserves_reasoned_stop_as_assessment() {
    let directory = tempfile::tempdir().unwrap();
    let case = directory.path().join("case");
    let projection = directory.path().join("projection/fieldkit.sqlite");
    let w = Workbench::new(&case, Config::default()).unwrap();
    let session = w
        .create_discovery(DiscoveryOptions {
            kind: "url".into(),
            seed: "https://profile.example/fixture".into(),
            planner: "nvidia".into(),
            reasoning_budget: 2,
            ..Default::default()
        })
        .unwrap();
    let id = text(&session, "id").to_owned();
    let _anchor = observation(&w, "http", "http", "https://project.example/research");
    let mut seed = w.discovery_jobs(&id).unwrap().remove(0);
    seed["status"] = json!("completed");
    seed["attempts"] = json!(1);
    seed["run_id"] = json!("run_observation");
    w.store.put("discovery_jobs", &seed).unwrap();
    let gateway = Gateway::new(
        true,
        Some(
            json!({"response":"No supported useful follow-up remains in this bounded fixture","sql_code":"SELECT job_id FROM frontier WHERE status = 'pending' LIMIT 0","sql_response_from_db":[],"row_count":0}),
        ),
    );
    let output = child(
        &case,
        &projection,
        &gateway.url,
        &["discover", "--resume", &id],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = w.store.get("discovery", &id).unwrap();
    assert_eq!(saved["status"], "completed");
    assert_eq!(saved["reasoning_used"], 1);
    let decisions = w.store.list("reasoning").unwrap();
    assert_eq!(decisions.len(), 1);
    let record = &decisions[0];
    assert_eq!(record["status"], "stopped");
    assert_eq!(record["decision"]["action"], "stop");
    for key in [
        "analysis_sha256",
        "request_sha256",
        "response_sha256",
        "projection_sha256",
        "snapshot_sha256",
    ] {
        assert_eq!(record[key].as_str().unwrap().len(), 64, "{key}");
    }
    let reports = w.store.reports(20).unwrap();
    assert_eq!(reports.len(), 2);
    assert_eq!(
        reports
            .iter()
            .filter(|report| report["command"] == "http")
            .count(),
        1
    );
    let assessment = reports
        .iter()
        .find(|report| report["command"] == "analysis.nvidia")
        .unwrap();
    assert_eq!(assessment["sources"][0]["kind"], "model_assessment");
    assert_eq!(assessment["entities"], json!([]));
    assert_eq!(assessment["relationships"], json!([]));
    assert!(!assessment.to_string().contains(TOKEN));
    assert_eq!(w.store.verify().unwrap()["ok"], true);
    let requests = gateway.finish();
    assert_eq!(requests.len(), 5);
    let ask = requests
        .iter()
        .find(|request| request["path"] == "/api/chat/completions")
        .unwrap();
    assert_eq!(ask["authenticated"], true);
    assert_eq!(ask["body"]["target_db"], "fieldkit");
    assert!(ask["body"]["question"].as_str().unwrap().contains(&id));
    assert!(
        !ask["body"]["evidence"]
            .as_str()
            .unwrap()
            .contains("Retained http observation")
    );
}

#[test]
fn gateway_selection_runs_the_evidence_backed_job_instead_of_priority_order() {
    let directory = tempfile::tempdir().unwrap();
    let case = directory.path().join("case");
    let projection = directory.path().join("projection/fieldkit.sqlite");
    let w = Workbench::new(&case, Config::default()).unwrap();
    let session = w
        .create_discovery(DiscoveryOptions {
            seed: "public_example".into(),
            sites: vec!["instagram".into()],
            planner: "nvidia".into(),
            max_jobs: 2,
            reasoning_budget: 2,
            ..Default::default()
        })
        .unwrap();
    let id = text(&session, "id").to_owned();
    let body = b"Synthetic captured public text explicitly lists instagram:observed_alias".to_vec();
    let hash = digest(&body);
    let report = json!({"schema_version":"1.0","run_id":"run_alias_fixture","command":"http","target":"https://public.example/fixture","status":"ok","data":{"description":"Synthetic public observation"},
        "sources":[{"source_id":"src_alias_fixture","kind":"http","sha256":hash,"hash_scope":"source_bytes"}],
        "entities":[{"type":"username","value":"instagram:observed_alias","source_ids":["src_alias_fixture"]}],"relationships":[],"errors":[],"timing":{"started_at":"2026-11-11T00:00:00Z","completed_at":"2026-11-11T00:00:01Z"}});
    w.store
        .save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash, body)]),
        })
        .unwrap();
    w.sync_ontology().unwrap();
    let mut seed = w.discovery_jobs(&id).unwrap().remove(0);
    seed["status"] = json!("completed");
    seed["attempts"] = json!(1);
    seed["run_id"] = json!("run_alias_fixture");
    w.store.put("discovery_jobs", &seed).unwrap();
    let anchor = json!({"evidence_id":"evidence_run_alias_fixture","run_id":"run_alias_fixture","report_sha256":w.store.report_hash("run_alias_fixture").unwrap(),"json_pointer":"/entities/0","source_ids":["src_alias_fixture"]});
    let selected = json!({"id":"job_selected_alias","discovery_id":id,"command":"username","target":"observed_alias","options":{"sites":["instagram"],"include_candidates":false},"depth":1,"priority":1,"status":"pending","reason":"Investigate the explicitly observed service-scoped alias","evidence_ids":["evidence_run_alias_fixture"],"anchors":[anchor],"attempts":0,"created_at":"2026-11-11T00:00:02Z"});
    let mut other = selected.clone();
    other["id"] = json!("job_higher_priority");
    other["target"] = json!("other_alias");
    other["priority"] = json!(99);
    other["reason"] =
        json!("Higher-priority fixture that the NVIDIA recommendation does not select");
    w.store.put("discovery_jobs", &selected).unwrap();
    w.store.put("discovery_jobs", &other).unwrap();
    let before_entities = w.materialized().unwrap()["entities"].clone();
    let gateway = Gateway::new(
        true,
        Some(
            json!({"response":"Resolve the explicitly observed alias using the supported bounded collector","sql_code":"SELECT job_id, reason AS rationale FROM frontier WHERE job_id = 'job_selected_alias'","sql_response_from_db":[{"job_id":"job_selected_alias","rationale":"This job directly investigates the captured service-scoped alias"}],"row_count":1}),
        ),
    );
    let output = child(
        &case,
        &projection,
        &gateway.url,
        &["discover", "--resume", &id],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let saved = w.store.get("discovery", &id).unwrap();
    assert_eq!(saved["attempted"], 2);
    assert_eq!(saved["reasoning_used"], 1);
    assert_eq!(saved["status"], "budget_exhausted");
    let jobs = w.discovery_jobs(&id).unwrap();
    let chosen = jobs
        .iter()
        .find(|job| job["id"] == "job_selected_alias")
        .unwrap();
    let untouched = jobs
        .iter()
        .find(|job| job["id"] == "job_higher_priority")
        .unwrap();
    assert_eq!(chosen["attempts"], 1);
    assert_eq!(chosen["collector_status"], "partial");
    assert_eq!(untouched["status"], "pending");
    assert_eq!(untouched["attempts"], 0);
    let decisions = w.store.list("reasoning").unwrap();
    assert_eq!(decisions.len(), 1);
    let decision = &decisions[0];
    assert_eq!(decision["status"], "accepted");
    assert_eq!(decision["selected_job_id"], "job_selected_alias");
    assert_eq!(
        decision["evidence_ids"],
        json!(["evidence_run_alias_fixture"])
    );
    assert_eq!(decision["anchors"][0]["json_pointer"], "/entities/0");
    let reports = w.store.reports(20).unwrap();
    let usernames: Vec<_> = reports
        .iter()
        .filter(|report| report["command"] == "username")
        .collect();
    assert_eq!(usernames.len(), 1);
    let collected = usernames[0];
    assert_eq!(collected["target"], "observed_alias");
    assert_eq!(collected["status"], "partial");
    assert_eq!(collected["data"]["confirmed_count"], 0);
    assert_eq!(collected["data"]["profiles"][0]["status"], "unverified");
    assert_eq!(collected["sources"], json!([]));
    assert_eq!(collected["entities"], json!([]));
    assert_eq!(collected["relationships"], json!([]));
    assert_eq!(w.materialized().unwrap()["entities"], before_entities);
    assert_eq!(w.store.verify().unwrap()["ok"], true);
    let requests = gateway.finish();
    assert_eq!(requests.len(), 5);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["path"] == "/api/chat/completions")
            .count(),
        1
    );
}
