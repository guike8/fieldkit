use super::*;

struct Fixture {
    task: Value,
    oracle: Value,
    reports: Records,
    bodies: Bodies,
    graph: Value,
    state: Value,
    integrity: Value,
}
impl Fixture {
    fn new() -> Self {
        let mut f = Self {
            task: json!({"id":"synthetic","inputs":[]}),
            oracle: json!({"expected":{}}),
            reports: Records::new(),
            bodies: Bodies::new(),
            graph: json!({"nodes":[],"edges":[]}),
            state: json!({"summary":[],"findings":[]}),
            integrity: json!({"ok":true}),
        };
        for (key, literal) in [("earlier", Some("UA-101199457-4")), ("later", None)] {
            let body = literal
                .map_or_else(
                    || "<html>no literal</html>".to_owned(),
                    |v| format!("<script>'{v}'</script>"),
                )
                .into_bytes();
            let digest = hash(&body);
            f.task["inputs"]
                .as_array_mut()
                .unwrap()
                .push(json!({"id":key,"sha256":digest}));
            let mut identifiers = Vec::new();
            let mut expected = Vec::new();
            if let Some(literal) = literal {
                let kind = "google_analytics_ua_property";
                expected.push(json!([kind, literal]));
                let start = std::str::from_utf8(&body).unwrap().find(literal).unwrap();
                identifiers.push(json!({"kind":kind,"value":literal,"raw_value":literal,"byte_start":start,"byte_end":start+literal.len(),"scope":"contains_tracking_literal"}));
                f.graph["nodes"] = json!([{"id":"url","type":"url","value":"https://example.invalid/earlier"},{"id":"property","type":kind,"value":literal}]);
                f.graph["edges"] = json!([{"source_id":"url","target_id":"property","relation":"contains_tracking_literal","status":"source_observation","evidence_ids":["evidence_run_earlier"],"source_ids":["src_earlier"]}]);
            }
            f.oracle["expected"][key] = json!(expected);
            f.reports.insert(key.into(),json!({"run_id":format!("run_{key}"),"status":"ok","sources":[{"source_id":format!("src_{key}"),"sha256":digest}],"data":{"url":format!("https://example.invalid/{key}"),"body_sha256":digest,"http_status":200,"tracking_identifiers":{"identifiers":identifiers,"response_eligible":true,"truncated":false}}}));
            f.bodies.insert(key.into(), body);
        }
        f
    }
    fn score(&self) -> Value {
        assess(
            &self.task,
            &self.oracle,
            &self.reports,
            &self.graph,
            &self.bodies,
            &self.integrity,
            &self.state,
        )
        .unwrap()
    }
    fn failed(&self) {
        assert_eq!(self.score()["core_reconstruction"], "failed");
    }
}
#[test]
fn complete_synthetic_evidence_and_negative_control() {
    let r = Fixture::new().score();
    assert_eq!(r["core_reconstruction"], "passed");
    assert_eq!(r["inputs"][0]["supported_hits"], 1);
    assert!(
        r["agent_autonomy"]
            .as_str()
            .unwrap()
            .contains("not_evaluated")
    );
}
#[test]
fn absent_capability_does_not_pass_negative_control() {
    let mut f = Fixture::new();
    f.reports.get_mut("later").unwrap()["data"]
        .as_object_mut()
        .unwrap()
        .remove("tracking_identifiers");
    assert_eq!(f.score()["inputs"][1]["passed"], false);
    f.failed();
}
#[test]
fn source_mutation_blocks_credit() {
    let mut f = Fixture::new();
    f.bodies
        .get_mut("earlier")
        .unwrap()
        .extend_from_slice(b"changed");
    assert_eq!(f.score()["inputs"][0]["pinned_bytes"], false);
    f.failed();
}
#[test]
fn original_byte_range_required() {
    let mut f = Fixture::new();
    f.reports.get_mut("earlier").unwrap()["data"]["tracking_identifiers"]["identifiers"][0]["byte_start"] =
        json!(10);
    assert_eq!(f.score()["inputs"][0]["invalid_spans"], 1);
    f.failed();
}
#[test]
fn distinct_properties_not_collapsed_to_account() {
    let mut f = Fixture::new();
    f.oracle["expected"]["earlier"][0][1] = json!("UA-101199457-2");
    assert_eq!(f.score()["inputs"][0]["unexpected_facts"], 1);
    f.failed();
}
#[test]
fn temporal_carry_forward_fails() {
    let mut f = Fixture::new();
    let o = f.reports["earlier"]["data"]["tracking_identifiers"]["identifiers"][0].clone();
    f.reports.get_mut("later").unwrap()["data"]["tracking_identifiers"]["identifiers"]
        .as_array_mut()
        .unwrap()
        .push(o);
    f.failed();
    let mut e = f.graph["edges"][0].clone();
    e["evidence_ids"] = json!(["evidence_run_later"]);
    f.graph["edges"].as_array_mut().unwrap().push(e);
    assert!(f.score()["inputs"][1]["invalid_edges"].as_u64().unwrap() > 0);
}
#[test]
fn graph_requires_same_report_source_and_direction() {
    let mut f = Fixture::new();
    f.graph["edges"][0]["source_ids"] = json!(["foreign_source"]);
    f.failed();
    f.graph["edges"][0]["source_ids"] = json!(["src_earlier"]);
    f.graph["edges"][0]["source_id"] = json!("property");
    f.graph["edges"][0]["target_id"] = json!("url");
    f.failed();
}
#[test]
fn missing_graph_or_truncated_extraction_fails() {
    let mut f = Fixture::new();
    f.graph["edges"] = json!([]);
    f.failed();
    let mut f = Fixture::new();
    f.reports.get_mut("earlier").unwrap()["data"]["tracking_identifiers"]["truncated"] =
        json!(true);
    f.failed();
}
#[test]
fn approval_or_integrity_failure_blocks_result() {
    let mut f = Fixture::new();
    f.integrity["ok"] = json!(false);
    f.failed();
    f.integrity["ok"] = json!(true);
    f.state["findings"] =
        json!([{"status":"accepted","source_evidence":false,"review":{"decision":"accepted"}}]);
    f.failed();
}
#[test]
fn publisher_normalization_preserves_full_property() {
    assert_eq!(
        canonical("google_adsense_publisher", "Ca-PuB-8264869885899896").as_deref(),
        Some("pub-8264869885899896")
    );
    assert_eq!(
        canonical("google_analytics_ua_property", "ua-101199457-4").as_deref(),
        Some("UA-101199457-4")
    );
    assert_eq!(
        canonical("google_analytics_ua_property", "UA-101199457"),
        None
    );
}
fn data(name: &str) -> Value {
    read_json(
        &Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../south-africa-2017")
            .join(name),
    )
    .unwrap()
}
#[test]
fn seed_only_task_does_not_supply_answer_or_replay() {
    let task = data("discovery-task.json");
    assert_eq!(task["seeds"], json!(["http://www.publicopinion.co.za:80/"]));
    assert_eq!(task["step_budget"], 10);
    assert!(task.get("inputs").is_none() && task.get("expected").is_none());
    let text = task.to_string();
    for word in [
        "web.archive.org/web/",
        "pub-8264869885899896",
        "UA-101199457",
        "wmc-scams.com",
    ] {
        assert!(!text.contains(word));
    }
}
#[test]
fn blocked_measurement_never_claims_discovery_or_agent_credit() {
    let run = data("discovery-run.json");
    assert_eq!(run["status"], "discovery_blocked");
    assert_eq!(run["discovery_pass"], false);
    assert!(run["agent_performance_score"].is_null());
    assert_eq!(run["source_http_status"], 429);
    assert_eq!(run["outbound_collector_requests"], 1);
    for field in [
        "additional_websites_verified",
        "graph_edges",
        "findings",
        "human_reviews",
        "paid_api_calls",
    ] {
        assert_eq!(run[field], 0);
    }
    assert_eq!(run["historical_index_query_performed"], false);
    for source in data("discovery-feasibility.json")["sources"]
        .as_array()
        .unwrap()
    {
        assert_eq!(source["historical_data_retrieved"], false);
    }
}
#[test]
fn bool_and_negative_offsets_cannot_masquerade_as_byte_ranges() {
    for bad in [json!(true), json!(-1), json!(1.5)] {
        let mut f = Fixture::new();
        f.reports.get_mut("earlier").unwrap()["data"]["tracking_identifiers"]["identifiers"][0]["byte_start"] =
            bad;
        f.failed();
    }
}
