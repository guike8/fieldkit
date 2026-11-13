use anyhow::{Result, bail};
use fieldkit_agent::{agent::ToolFailure, *};
use serde_json::{Value, json};
use std::{collections::VecDeque, time::Duration};

struct Script {
    responses: VecDeque<Value>,
    requests: Vec<(Vec<Value>, Vec<Value>)>,
}
impl Script {
    fn new(r: Vec<Value>) -> Self {
        Self {
            responses: r.into(),
            requests: vec![],
        }
    }
}
impl Chat for Script {
    fn complete(&mut self, m: &[Value], t: &[Value]) -> Result<Value> {
        self.requests.push((m.to_vec(), t.to_vec()));
        Ok(self.responses.pop_front().expect("response"))
    }
}
fn reply(content: Value, calls: Value) -> Value {
    fieldkit_agent::benchmark::scripted_reply(content, calls)
}
fn call(name: &str, args: &str, id: &str) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args}})
}
fn graph() -> Value {
    call("fieldkit_graph_show", "{}", "c1")
}
fn tools() -> Vec<Value> {
    vec![
        json!({"name":"fieldkit_graph_show","description":"Inspect graph","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}),
    ]
}
fn execute(client: &mut dyn Chat, b: Budgets) -> Result<Value> {
    run_agent(
        client,
        &tools(),
        |_, _| Ok(json!({"isError":false})),
        "Question",
        "",
        b,
    )
}
#[test]
fn tool_round_trip_metrics_and_compact_context() {
    let observation = json!({"isError":false,"structuredContent":{"data":{"exact":"evidence"}},"content":[{"type":"text","text":"duplicate evidence"}]});
    let mut c = Script::new(vec![
        reply(Value::Null, json!([graph()])),
        reply(json!("No identity established."), json!([])),
    ]);
    let mut observed = vec![];
    let report = run_agent(
        &mut c,
        &tools(),
        |n, a| {
            observed.push((n.to_owned(), a.clone()));
            Ok(observation.clone())
        },
        "Map evidence",
        "",
        Budgets::default(),
    )
    .unwrap();
    assert_eq!(report["stop_reason"], "completed");
    assert_eq!(observed, vec![("fieldkit_graph_show".into(), json!({}))]);
    assert_eq!(report["metrics"]["tool_calls"], 1);
    assert_eq!(report["metrics"]["prompt_tokens"], 160);
    assert_eq!(report["metrics"]["completion_tokens"], 40);
    let sent = &c.requests[1].0.last().unwrap();
    assert_eq!(sent["role"], "tool");
    assert_eq!(sent["tool_call_id"], "c1");
    let body: Value = serde_json::from_str(sent["content"].as_str().unwrap()).unwrap();
    assert!(body.get("content").is_none());
    assert_eq!(body["structuredContent"], observation["structuredContent"]);
    assert_eq!(report["trace"][0]["result"], observation);
}
#[test]
fn legacy_write_review_shell_and_collectors_not_advertised() {
    let mut advertised = tools();
    for n in [
        "fieldkit_note_add",
        "fieldkit_claim_review",
        "fieldkit_http",
        "shell",
    ] {
        advertised.push(json!({"name":n,"inputSchema":{}}));
    }
    let mut c = Script::new(vec![reply(json!("Insufficient evidence."), json!([]))]);
    run_agent(
        &mut c,
        &advertised,
        |_, _| panic!("unexpected tool"),
        "Question",
        "",
        Budgets::default(),
    )
    .unwrap();
    assert_eq!(c.requests[0].1.len(), 1);
    assert_eq!(
        c.requests[0].1[0]["function"]["name"],
        "fieldkit_graph_show"
    );
    for invalid in [json!({"name":"fieldkit_graph_show"}), tools()[0].clone()] {
        let mut bad = tools();
        bad.push(invalid);
        assert!(run_agent(&mut c, &bad, |_, _| panic!(), "Q", "", Budgets::default()).is_err());
    }
}
#[test]
fn validate_whole_batch_before_side_effects() {
    let cases = vec![
        call("fieldkit_note_add", "{}", "c2"),
        call("fieldkit_graph_show", "[]", "c2"),
        call("fieldkit_graph_show", "{bad", "c2"),
        call("fieldkit_graph_show", "{\"v\":NaN}", "c2"),
        call("fieldkit_graph_show", "{}", "c1"),
        call("fieldkit_graph_show", &"x".repeat(65537), "c2"),
        json!({"id":"c2","type":"shell"}),
    ];
    for bad in cases {
        let mut c = Script::new(vec![reply(Value::Null, json!([graph(), bad]))]);
        assert!(
            run_agent(
                &mut c,
                &tools(),
                |_, _| panic!("must have no effects"),
                "Question",
                "",
                Budgets::default()
            )
            .is_err()
        );
    }
}
#[test]
fn cumulative_ids_and_usage_are_strict() {
    let mut c = Script::new(vec![
        reply(Value::Null, json!([graph()])),
        reply(Value::Null, json!([graph()])),
    ]);
    let mut calls = 0;
    assert!(
        run_agent(
            &mut c,
            &tools(),
            |_, _| {
                calls += 1;
                Ok(json!({}))
            },
            "Q",
            "",
            Budgets::default()
        )
        .is_err()
    );
    assert_eq!(calls, 1);
    for usage in [
        Value::Null,
        json!({}),
        json!({"prompt_tokens":true,"completion_tokens":2}),
        json!({"prompt_tokens":-1,"completion_tokens":2}),
        json!({"prompt_tokens":1.5,"completion_tokens":2}),
    ] {
        let mut r = reply(Value::Null, json!([graph()]));
        r["usage"] = usage;
        assert!(
            run_agent(
                &mut Script::new(vec![r]),
                &tools(),
                |_, _| panic!("invalid usage effects"),
                "Q",
                "",
                Budgets::default()
            )
            .is_err()
        );
    }
}
#[test]
fn budgets_stop_before_partial_batches() {
    let mut c = Script::new(vec![reply(
        Value::Null,
        json!([graph(), call("fieldkit_graph_show", "{}", "c2")]),
    )]);
    let b = Budgets {
        max_tool_calls: 1,
        ..Default::default()
    };
    let r = run_agent(&mut c, &tools(), |_, _| panic!(), "Q", "", b).unwrap();
    assert_eq!(r["stop_reason"], "tool_limit");
    assert_eq!(r["trace"], json!([]));
    let mut c = Script::new(vec![reply(Value::Null, json!([graph()]))]);
    let r = run_agent(
        &mut c,
        &tools(),
        |_, _| panic!(),
        "Q",
        "",
        Budgets {
            max_total_tokens: 99,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r["stop_reason"], "token_limit");
    let mut c = Script::new(vec![reply(Value::Null, json!([graph()]))]);
    let r = run_agent(
        &mut c,
        &tools(),
        |_, _| Err(ToolFailure::Observation),
        "Q",
        "",
        Budgets {
            max_turns: 1,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(r["stop_reason"], "turn_limit");
    assert_eq!(r["trace"][0]["result"]["isError"], true);
}
#[test]
fn fatal_transport_failure_never_retried() {
    let mut c = Script::new(vec![reply(Value::Null, json!([graph()]))]);
    assert!(
        run_agent(
            &mut c,
            &tools(),
            |_, _| Err(ToolFailure::Fatal(anyhow::anyhow!("uncertain action"))),
            "Q",
            "",
            Budgets::default()
        )
        .unwrap_err()
        .to_string()
        .contains("uncertain")
    );
    assert_eq!(c.requests.len(), 1);
}
#[test]
fn incomplete_refused_and_malformed_model_responses_execute_nothing() {
    let mut cases = vec![
        reply(json!(""), json!([])),
        json!({"choices":[]}),
        reply(json!([]), json!([])),
        reply(Value::Null, json!({})),
        reply(Value::Null, json!([graph()])),
    ];
    cases.last_mut().unwrap()["choices"][0]["finish_reason"] = json!("length");
    for r in cases {
        assert!(
            run_agent(
                &mut Script::new(vec![r]),
                &tools(),
                |_, _| panic!("must stop"),
                "Q",
                "",
                Budgets::default()
            )
            .is_err()
        );
    }
    let mut r = reply(Value::Null, json!([graph()]));
    r["choices"][0]["message"]["role"] = json!("system");
    assert!(execute(&mut Script::new(vec![r]), Budgets::default()).is_err());
}
#[test]
fn oversize_observation_is_not_silently_truncated() {
    let mut c = Script::new(vec![reply(Value::Null, json!([graph()]))]);
    assert!(
        run_agent(
            &mut c,
            &tools(),
            |_, _| Ok(json!({"data":"x".repeat(MAX_MESSAGE)})),
            "Q",
            "",
            Budgets::default()
        )
        .unwrap_err()
        .to_string()
        .contains("1 MiB")
    );
}
#[test]
fn private_exclusive_report_and_recursive_credential_scrubbing() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("report.json");
    save_report(&p, &json!({"ok":true})).unwrap();
    assert_eq!(
        std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(save_report(&p, &json!({"ok":false})).is_err());
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&p).unwrap()).unwrap(),
        json!({"ok":true})
    );
    let link = d.path().join("symlink");
    std::os::unix::fs::symlink(d.path().join("absent"), &link).unwrap();
    assert!(save_report(&link, &json!({})).is_err());
    let secret = "synthetic-\"\\key";
    let v = json!({secret:[format!("echo {secret}")]});
    let clean = redact(&v, secret);
    assert_eq!(clean, json!({"[REDACTED]":["echo [REDACTED]"]}));
}

#[derive(Default)]
struct Fake {
    calls: Vec<(String, Value)>,
    fail: bool,
}
impl ToolBackend for Fake {
    fn tools(&self) -> Vec<Value> {
        ALLOWED_TOOLS
            .iter()
            .map(|n| json!({"name":n,"inputSchema":{"type":"object"}}))
            .collect()
    }
    fn call(&mut self, n: &str, a: &Value) -> Result<Value> {
        self.calls.push((n.into(), a.clone()));
        if self.fail {
            bail!("uncertain");
        }
        Ok(json!({"isError":false,"structuredContent":{"data":{"id":"inv_scoped"}}}))
    }
}
fn scoped(collection: bool) -> ScopedTools<Fake> {
    ScopedTools::new(
        Fake::default(),
        "Question",
        "person",
        vec!["synthetic_handle".into()],
        collection,
    )
}
fn create() -> Value {
    json!({"question":"Question","mode":"person","seeds":["synthetic_handle"]})
}
#[test]
fn exact_operator_scope_and_single_creation() {
    let mut s = scoped(false);
    for (key, v) in [
        ("question", json!("Changed")),
        ("mode", json!("organization")),
        ("seeds", json!(["other"])),
    ] {
        let mut args = create();
        args[key] = v;
        assert_eq!(
            s.call("fieldkit_investigation_create", &args).unwrap()["isError"],
            true
        );
    }
    let mut args = create();
    args["searches"] = json!([]);
    assert_eq!(
        s.call("fieldkit_investigation_create", &args).unwrap()["isError"],
        true
    );
    assert!(s.backend.calls.is_empty());
    assert_eq!(
        s.call("fieldkit_investigation_create", &create()).unwrap()["isError"],
        false
    );
    assert_eq!(s.investigation_id.as_deref(), Some("inv_scoped"));
    assert_eq!(
        s.call("fieldkit_investigation_create", &create()).unwrap()["isError"],
        true
    );
    assert_eq!(s.backend.calls.len(), 1);
}
#[test]
fn collection_disabled_source_reader_scoped_and_no_approval_surface() {
    let mut s = scoped(false);
    assert!(
        !s.tools
            .iter()
            .any(|t| t["name"] == "fieldkit_investigation_run_step")
    );
    assert!(
        s.tools
            .iter()
            .any(|t| t["name"] == "fieldkit_investigation_source")
    );
    for n in [
        "fieldkit_investigation_run_step",
        "fieldkit_note_add",
        "fieldkit_claim_review",
    ] {
        assert_eq!(s.call(n, &json!({})).unwrap()["isError"], true);
    }
    let args = json!({"id":"inv_scoped","step_id":"step_1","source_id":"src_1"});
    assert_eq!(
        s.call("fieldkit_investigation_source", &args).unwrap()["isError"],
        true
    );
    assert!(s.backend.calls.is_empty());
    s.call("fieldkit_investigation_create", &create()).unwrap();
    let mut foreign = args.clone();
    foreign["id"] = json!("foreign");
    assert_eq!(
        s.call("fieldkit_investigation_source", &foreign).unwrap()["isError"],
        true
    );
    assert_eq!(
        s.call("fieldkit_investigation_source", &args).unwrap()["isError"],
        false
    );
    assert_eq!(
        s.backend.calls.last().unwrap(),
        &("fieldkit_investigation_source".into(), args)
    );
}
#[test]
fn attempted_steps_and_uncertain_creation_cannot_replay() {
    let mut s = scoped(true);
    s.call("fieldkit_investigation_create", &create()).unwrap();
    assert_eq!(
        s.call("fieldkit_investigation_show", &json!({"id":"foreign"}))
            .unwrap()["isError"],
        true
    );
    let args = json!({"id":"inv_scoped","step_id":"step_1"});
    assert_eq!(
        s.call("fieldkit_investigation_run_step", &args).unwrap()["isError"],
        false
    );
    assert_eq!(
        s.call("fieldkit_investigation_run_step", &args).unwrap()["isError"],
        true
    );
    assert_eq!(s.backend.calls.len(), 2);
    let mut s = scoped(true);
    s.backend.fail = true;
    assert!(s.call("fieldkit_investigation_create", &create()).is_err());
    assert_eq!(
        s.call("fieldkit_investigation_create", &create()).unwrap()["isError"],
        true
    );
    assert_eq!(s.backend.calls.len(), 1);
}
#[test]
fn native_mcp_plan_only_review_boundary_and_compact_schema_lookup() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let binary = std::env::var_os("FIELDKIT_TEST_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| root.join("target/debug/fieldkit"));
    if !binary.is_file() {
        eprintln!("SKIP native MCP integration: build Fieldkit or set FIELDKIT_TEST_BINARY");
        return;
    }
    let d = tempfile::tempdir().unwrap();
    // macOS temp paths can include /var -> /private/var; MCP rejects symlinked scope.
    let workspace = d.path().canonicalize().unwrap();
    let mcp = McpClient::connect(
        &binary,
        &workspace,
        &workspace.join("case"),
        Duration::from_secs(10),
        "INKLING_API_KEY",
    )
    .unwrap();
    let mut s = ScopedTools::new(
        mcp,
        "Map synthetic infrastructure",
        "infrastructure",
        vec!["example.org".into()],
        false,
    );
    assert!(!s.tools.iter().any(|t| t["name"] == "fieldkit_note_add"));
    let desc = s
        .tools
        .iter()
        .find(|t| t["name"] == "fieldkit_capabilities")
        .unwrap();
    assert_eq!(desc["inputSchema"]["required"], json!(["command"]));
    for a in [json!({}), json!({"command":"invented"})] {
        assert_eq!(
            s.call("fieldkit_capabilities", &a).unwrap()["isError"],
            true
        );
    }
    let schema = s
        .call("fieldkit_capabilities", &json!({"command":"dns"}))
        .unwrap();
    assert_eq!(schema["isError"], false);
    let commands = schema["structuredContent"]["data"]["commands"]
        .as_array()
        .unwrap();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0]["name"], "dns");
    assert!(commands[0]["input_schema"].is_object());
    assert!(schema.to_string().len() < 5000);
    assert!(s.investigation_id.is_none());
    let r=s.call("fieldkit_investigation_create",&json!({"question":"Map synthetic infrastructure","mode":"infrastructure","seeds":["example.org"]})).unwrap();
    assert_eq!(r["isError"], false);
    let inv = s.investigation_id.clone().unwrap();
    let r=s.call("fieldkit_investigation_plan_step",&json!({"id":inv,"job":{"command":"dns","target":"example.org","options":{"types":["A"]}},"rationale":"Plan only; no network request authorized"})).unwrap();
    assert_eq!(r["isError"], false, "{r}");
    let show = s
        .call("fieldkit_investigation_show", &json!({"id":inv}))
        .unwrap();
    let state = &show["structuredContent"]["data"];
    assert_eq!(state["summary"], json!([]));
    assert_eq!(state["steps"][0]["status"], "pending");
    let rejected=s.call("fieldkit_investigation_propose",&json!({"id":inv,"content":"Unverified assertion","kind":"identity_sensitive","citations":[{"step_id":state["steps"][0]["id"],"pointer":"/data"}]})).unwrap();
    assert_eq!(rejected["isError"], true);
}
