use super::*;

struct Fixture {
    task: Value,
    oracle: Value,
    receipt: Value,
    reports: Records,
    bodies: Bodies,
    state: Value,
    graph: Value,
    integrity: Value,
    hashes: BTreeMap<String, String>,
    reviews: u64,
}
impl Fixture {
    fn new() -> Self {
        let sha = "a".repeat(40);
        let seed = format!("https://api.github.com/repos/dominictarr/event-stream/commits/{sha}");
        let mut f = Self {
            task: json!({"id":"synthetic","seeds":[seed]}),
            oracle: json!({"commit_sha":sha,"dependency":"sample-dependency","declared_range":"^0.1.0","locked_version":"0.1.0","locked_url":"https://registry.example/sample.tgz","manifest_snapshot_version":"1.0.0","runtime_fragments":["require('sample-dependency')","exports.example = example"],"patch_fragments":{"package.json":["+added dependency"],"index.js":["+added import"]},"raw_sha256":{}}),
            receipt: json!({"benchmark":"synthetic","status":"captured","investigation_id":"inv","inputs":[]}),
            reports: Records::new(),
            bodies: Bodies::new(),
            state: json!({"id":"inv","seeds":[seed],"steps":[],"findings":[],"summary":[]}),
            graph: json!({"nodes":[],"edges":[]}),
            integrity: json!({"ok":true}),
            hashes: BTreeMap::new(),
            reviews: 0,
        };
        let mut commit = json!({"sha":sha,"files":[]});
        for filename in FILENAMES {
            commit["files"].as_array_mut().unwrap().push(json!({"filename":filename,"raw_url":format!("https://github.com/dominictarr/event-stream/raw/{sha}/{filename}"),"patch":if filename=="package.json"{"+added dependency"}else{"+added import"}}));
        }
        f.bodies
            .insert("commit".into(), serde_json::to_vec(&commit).unwrap());
        f.bodies.insert(
            "package.json".into(),
            serde_json::to_vec(
                &json!({"version":"1.0.0","dependencies":{"sample-dependency":"^0.1.0"}}),
            )
            .unwrap(),
        );
        f.bodies.insert("package-lock.json".into(),serde_json::to_vec(&json!({"dependencies":{"sample-dependency":{"version":"0.1.0","resolved":"https://registry.example/sample.tgz"}}})).unwrap());
        f.bodies.insert(
            "index.js".into(),
            b"const example = require('sample-dependency');\nexports.example = example;\n".to_vec(),
        );
        for (index, key) in ["commit", FILENAMES[0], FILENAMES[1], FILENAMES[2]]
            .into_iter()
            .enumerate()
        {
            let url = if key == "commit" {
                seed.clone()
            } else {
                text(&commit["files"][index - 1]["raw_url"]).unwrap().into()
            };
            let final_url = if key == "commit" {
                url.clone()
            } else {
                url.replace("https://github.com/", "https://raw.githubusercontent.com/")
                    .replace("/raw/", "/")
            };
            let digest = hash(&f.bodies[key]);
            let entry = json!({"id":key,"step_id":format!("step{index}"),"run_id":format!("run{index}"),"url":url,"source_id":format!("src{index}"),"sha256":digest,"status":"ok"});
            f.receipt["inputs"]
                .as_array_mut()
                .unwrap()
                .push(entry.clone());
            let urls: Vec<_> = if key == "commit" {
                commit["files"].as_array().unwrap().iter().enumerate().map(|(i,file)|json!({"url":file["raw_url"],"source_pointer":format!("/files/{i}/raw_url")})).collect()
            } else {
                Vec::new()
            };
            let report = json!({"run_id":entry["run_id"],"command":"http","target":url,"status":"ok","sources":[{"source_id":entry["source_id"],"sha256":digest,"kind":"http","http_status":200,"media_type":if key=="commit"{"application/json"}else{"text/plain"}}],"data":{"requested_url":url,"url":final_url,"http_status":200,"body_sha256":digest,"source_ids":[entry["source_id"]],"json_urls":{"urls":urls,"valid_json":key=="commit","response_eligible":key=="commit","truncated":false}}});
            f.hashes
                .insert(format!("run{index}"), observation_hash(&report).unwrap());
            f.reports.insert(key.into(), report);
            if key != "commit" {
                f.oracle["raw_sha256"][key] = json!(digest);
            }
        }
        for entry in f.receipt["inputs"].as_array().unwrap().clone() {
            let key = text(&entry["id"]).unwrap();
            let mut step = json!({"id":entry["step_id"],"run_id":entry["run_id"],"target":entry["url"],"status":"ok"});
            if key != "commit" {
                let (_, pointer) = source_pivot(&commit, &f.reports["commit"], key).unwrap();
                step["grounding"] = json!({"origin":"observed_lead_not_identity","step_id":"step0","run_id":"run0","report_sha256":f.hashes["run0"],"pointer":pointer,"observation_sha256":observation_hash(&entry["url"]).unwrap()});
                let seed_entry = f.receipt["inputs"][0].clone();
                f.edge(
                    &json!(seed),
                    "contains_json_url",
                    &entry["url"],
                    &seed_entry,
                );
                let final_url = f.reports[key]["data"]["url"].clone();
                f.edge(&entry["url"], "request_resolved_to", &final_url, &entry);
                let raw = &f.bodies[key];
                let citation = json!({"step_id":entry["step_id"],"run_id":entry["run_id"],"source_id":entry["source_id"],"report_sha256":f.hashes[text(&entry["run_id"]).unwrap()],"sources":f.reports[key]["sources"],"byte_start":0,"byte_end":raw.len(),"observation_sha256":observation_hash(&json!(std::str::from_utf8(raw).unwrap())).unwrap(),"scope":"http_original_text_byte_range_not_execution_or_attribution"});
                f.state["findings"].as_array_mut().unwrap().push(json!({"status":"pending_review","source_evidence":false,"origin":"analytical_proposal","citations":[citation]}));
            }
            f.state["steps"].as_array_mut().unwrap().push(step);
        }
        f
    }
    fn node(&mut self, url: &Value) -> String {
        if let Some(n) = self.graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["value"] == *url)
        {
            return text(&n["id"]).unwrap().into();
        }
        let id = format!("node{}", self.graph["nodes"].as_array().unwrap().len());
        self.graph["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!({"id":id,"type":"url","value":url}));
        id
    }
    fn edge(&mut self, source: &Value, relation: &str, target: &Value, entry: &Value) {
        let source = self.node(source);
        let target = self.node(target);
        self.graph["edges"].as_array_mut().unwrap().push(json!({"source_id":source,"target_id":target,"relation":relation,"status":"source_observation","evidence_ids":[format!("evidence_{}",text(&entry["run_id"]).unwrap())],"source_ids":[entry["source_id"]]}));
    }
    fn score(&self) -> Value {
        assess(
            &self.task,
            &self.oracle,
            &self.receipt,
            &self.reports,
            &self.bodies,
            &self.state,
            &self.graph,
            &self.integrity,
            &self.hashes,
            self.reviews,
        )
    }
    fn failed(&self) {
        assert_eq!(
            self.score()["technical_reconstruction"],
            "failed",
            "{}",
            self.score()
        );
    }
    fn replace_body(&mut self, key: &str, raw: Vec<u8>) {
        let digest = hash(&raw);
        self.bodies.insert(key.into(), raw);
        let entry = self.receipt["inputs"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|e| e["id"] == key)
            .unwrap();
        entry["sha256"] = json!(digest);
        self.reports.get_mut(key).unwrap()["sources"][0]["sha256"] = json!(digest);
        self.reports.get_mut(key).unwrap()["data"]["body_sha256"] = json!(digest);
        self.oracle["raw_sha256"][key] = json!(digest);
    }
}
#[test]
fn complete_synthetic_source_pivots_citations_and_graph() {
    let r = Fixture::new().score();
    assert_eq!(r["technical_reconstruction"], "passed", "{r}");
    assert_eq!(r["grounded_file_pivots"], 3);
    assert_eq!(r["citations_verified"], 3);
    assert!(r["agent_performance_score"].is_null());
}
#[test]
fn source_mutation_fails() {
    let mut f = Fixture::new();
    f.bodies
        .get_mut("index.js")
        .unwrap()
        .extend_from_slice(b"changed");
    f.failed();
}
#[test]
fn missing_extraction_cannot_pass() {
    let mut f = Fixture::new();
    f.reports.get_mut("commit").unwrap()["data"]["json_urls"]["valid_json"] = json!(false);
    f.failed();
}
#[test]
fn wrong_source_pointer_cannot_ground_pivot() {
    let mut f = Fixture::new();
    f.reports.get_mut("commit").unwrap()["data"]["json_urls"]["urls"][0]["source_pointer"] =
        json!("/unrelated");
    f.failed();
}
#[test]
fn unobserved_or_foreign_target_fails() {
    let mut f = Fixture::new();
    f.receipt["inputs"][1]["url"] = json!("https://other.example/file");
    f.failed();
}
#[test]
fn grounding_binds_exact_parent_report() {
    let mut f = Fixture::new();
    f.state["steps"][1]["grounding"]["report_sha256"] = json!("wrong");
    f.failed();
}
#[test]
fn missing_request_response_binding_fails() {
    let mut f = Fixture::new();
    f.graph["edges"]
        .as_array_mut()
        .unwrap()
        .retain(|e| e["relation"] != "request_resolved_to");
    f.failed();
}
#[test]
fn wrong_graph_direction_and_report_provenance_fail() {
    let mut f = Fixture::new();
    let saved = f.graph.clone();
    let e = &mut f.graph["edges"][0];
    let source = e["source_id"].clone();
    e["source_id"] = e["target_id"].clone();
    e["target_id"] = source;
    f.failed();
    f.graph = saved;
    f.graph["edges"][0]["source_ids"] = json!(["foreign_source"]);
    f.failed();
}
#[test]
fn later_payload_version_cannot_replace_observed_lock() {
    let mut f = Fixture::new();
    let raw = std::str::from_utf8(&f.bodies["package-lock.json"])
        .unwrap()
        .replace("\"version\":\"0.1.0\"", "\"version\":\"0.1.1\"")
        .into_bytes();
    f.replace_body("package-lock.json", raw);
    f.failed();
    assert_eq!(f.score()["checks"]["locked_version_and_archive_url"], false);
}
#[test]
fn import_text_required_not_just_manifest() {
    let mut f = Fixture::new();
    f.replace_body("index.js", b"// no import\n".to_vec());
    f.failed();
    assert_eq!(
        f.score()["checks"]["runtime_source_import_and_export"],
        false
    );
}
#[test]
fn citation_must_recover_original_bytes_and_scope() {
    let mut f = Fixture::new();
    let end = f.state["findings"][0]["citations"][0]["byte_end"]
        .as_u64()
        .unwrap();
    f.state["findings"][0]["citations"][0]["byte_end"] = json!(end - 1);
    f.failed();
    f.state["findings"][0]["citations"][0]["byte_end"] = json!(end);
    f.state["findings"][0]["citations"][0]["scope"] =
        json!("http_response_metadata_not_page_content");
    f.failed();
}
#[test]
fn reviews_accepted_summary_or_integrity_failure_fail() {
    let mut f = Fixture::new();
    f.reviews = 1;
    f.failed();
    f.reviews = 0;
    f.state["summary"] = json!([{"content":"accepted"}]);
    f.failed();
    f.state["summary"] = json!([]);
    f.integrity["ok"] = json!(false);
    f.failed();
}
struct Fake<'a> {
    fixture: &'a Fixture,
    index: usize,
    receipt: std::path::PathBuf,
    fail: bool,
}
impl Tools for Fake<'_> {
    fn tool(&mut self, name: &str, _: Value) -> Result<Value> {
        match name {
            "fieldkit_investigation_create" => Ok(json!({"id":"inv"})),
            "fieldkit_investigation_plan_step" => Ok(json!({"id":format!("step{}",self.index)})),
            "fieldkit_investigation_run_step" => {
                let receipt = read_json(&self.receipt)?;
                ensure!(
                    receipt["inputs"][self.index]["status"] == "attempting",
                    "Attempt not checkpointed before collection"
                );
                if self.fail {
                    anyhow::bail!("Synthetic collection failure");
                }
                let key = ["commit", FILENAMES[0], FILENAMES[1], FILENAMES[2]][self.index];
                self.index += 1;
                Ok(self.fixture.reports[key].clone())
            }
            _ => anyhow::bail!("Unexpected tool {name}"),
        }
    }
}
#[test]
fn capture_uses_no_oracle_and_checkpoints_before_collection() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("blobs")).unwrap();
    for (key, raw) in &f.bodies {
        std::fs::write(
            root.path()
                .join("blobs")
                .join(text(&f.reports[key]["data"]["body_sha256"]).unwrap()),
            raw,
        )
        .unwrap();
    }
    let path = root.path().join("receipt.json");
    let mut out = Output::new(&path).unwrap();
    let mut client = Fake {
        fixture: &f,
        index: 0,
        receipt: path,
        fail: false,
    };
    let mut task = f.task.clone();
    task["question"] = json!("Synthetic source question");
    task["mode"] = json!("infrastructure");
    task["step_budget"] = json!(6);
    let receipt = capture(&mut client, &task, &mut out, root.path()).unwrap();
    assert_eq!(receipt["status"], "captured");
    assert_eq!(receipt["inputs"].as_array().unwrap().len(), 4);
    assert_eq!(client.index, 4);
    assert!(!root.path().join("oracle.json").exists());
}
#[test]
fn real_task_supplies_one_commit_not_dependency_answer() {
    let task =
        read_json(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../event-stream-2018/task.json"))
            .unwrap();
    assert_eq!(task["seeds"].as_array().unwrap().len(), 1);
    assert_eq!(task["step_budget"], 6);
    let text = task.to_string();
    for answer in ["flatmap-stream", "^0.1.0", "0.1.1", "3.3.5"] {
        assert!(!text.contains(answer));
    }
    assert!(task.get("inputs").is_none() && task.get("expected").is_none());
}
#[test]
fn failed_capture_stops_once_and_keeps_attempt_checkpoint() {
    let f = Fixture::new();
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("receipt.json");
    let mut out = Output::new(&path).unwrap();
    let mut client = Fake {
        fixture: &f,
        index: 0,
        receipt: path.clone(),
        fail: true,
    };
    let mut task = f.task.clone();
    task["step_budget"] = json!(6);
    assert!(capture(&mut client, &task, &mut out, root.path()).is_err());
    let r = read_json(&path).unwrap();
    assert_eq!(r["status"], "incomplete");
    assert_eq!(r["inputs"].as_array().unwrap().len(), 1);
    assert_eq!(r["inputs"][0]["status"], "attempting");
    assert!(Output::new(&path).is_err());
}
#[test]
fn exact_pivot_rejects_normalized_lookalikes_and_duplicate_observations() {
    let f = Fixture::new();
    let mut commit: Value = serde_json::from_slice(&f.bodies["commit"]).unwrap();
    for prefix in [
        "https://github.com:443/",
        "https://user@github.com/",
        "https://GITHUB.com/",
    ] {
        let mut c = commit.clone();
        c["files"][0]["raw_url"] = json!(
            text(&c["files"][0]["raw_url"])
                .unwrap()
                .replace("https://github.com/", prefix)
        );
        assert!(source_pivot(&c, &f.reports["commit"], "package.json").is_err());
    }
    let mut report = f.reports["commit"].clone();
    let duplicate = report["data"]["json_urls"]["urls"][0].clone();
    report["data"]["json_urls"]["urls"]
        .as_array_mut()
        .unwrap()
        .push(duplicate);
    assert!(source_pivot(&commit, &report, "package.json").is_err());
    let duplicate = commit["files"][0].clone();
    commit["files"].as_array_mut().unwrap().push(duplicate);
    assert!(source_pivot(&commit, &f.reports["commit"], "package.json").is_err());
}
#[test]
fn duplicate_records_and_accepted_findings_fail() {
    let mut f = Fixture::new();
    let e = f.receipt["inputs"][0].clone();
    f.receipt["inputs"].as_array_mut().unwrap().push(e);
    f.failed();
    let mut f = Fixture::new();
    f.state["findings"][0]["status"] = json!("accepted");
    f.failed();
}
#[test]
fn json_pointer_citation_and_empty_pending_findings_are_valid() {
    let mut f = Fixture::new();
    let entry = &f.receipt["inputs"][0];
    let body: Value = serde_json::from_slice(&f.bodies["commit"]).unwrap();
    let citation = json!({"step_id":entry["step_id"],"run_id":entry["run_id"],"source_id":entry["source_id"],"report_sha256":f.hashes["run0"],"sources":f.reports["commit"]["sources"],"pointer":"/sha","observation_sha256":observation_hash(&body["sha"]).unwrap(),"scope":"external_json_observation_not_identity_attribution"});
    f.state["findings"][0]["citations"] = json!([citation]);
    assert_eq!(
        f.score()["technical_reconstruction"],
        "passed",
        "{}",
        f.score()
    );
    f.state["findings"] = json!([]);
    assert_eq!(f.score()["technical_reconstruction"], "passed");
}
