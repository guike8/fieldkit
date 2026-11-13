//! Shared validated workspace operations for local HTTP and MCP.
use crate::{
    Job, Workbench,
    discovery::DiscoveryOptions,
    model::{strings, text},
};
use anyhow::{Result, bail, ensure};
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;

pub const OPERATIONS: &[(&str, &str)] = &[
    (
        "investigations/create",
        "Persist the user's Telegram question and up to five agent-planned historical searches; does not collect",
    ),
    (
        "investigations/show",
        "Inspect a shared investigation, pending proposals and human-reviewed summary",
    ),
    (
        "investigations/run",
        "Attempt one planned collection step once; no retries or pagination",
    ),
    (
        "investigations/evidence",
        "Inspect a selected step's full preserved report, including older evidence outside the recent case window",
    ),
    (
        "investigations/select",
        "Plan a public-post capture from an exact archived search result; does not collect",
    ),
    (
        "investigations/propose",
        "Propose an immutable cited finding; all kinds require local human review before summary inclusion",
    ),
    ("case", "Set case title and investigation question"),
    ("evidence", "Preserve analyst-supplied evidence"),
    (
        "evidence/review",
        "Assess evidence without changing original capture",
    ),
    ("notes", "Add an evidence-linked analyst note"),
    ("entities", "Add an analyst entity"),
    ("links", "Link two entities with required evidence IDs"),
    ("flows", "Create a research flow"),
    ("flows/steps", "Plan a collector step"),
    ("flows/run", "Run a selected step"),
    ("tools/run", "Run a collector and preserve its sources"),
    (
        "correlations/generate",
        "Find evidence-backed correlation leads",
    ),
    (
        "correlations/review",
        "Review a lead with rationale; no identity merge",
    ),
    (
        "discovery/create",
        "Plan bounded discovery from a username, profile, URL, domain or query",
    ),
    (
        "discovery/run",
        "Run or resume a bounded discovery queue, growing the ontology with evidence",
    ),
    (
        "ontology/sync",
        "Learn case vocabulary and grounded claims from preserved reports",
    ),
    ("claims/review", "Review a sourced claim with rationale"),
    (
        "discovery/configure",
        "Configure an existing discovery planner and total reasoning budget",
    ),
    (
        "nvidia/status",
        "Check the actual NVIDIA gateway, catalog and semantic-layer readiness",
    ),
    (
        "nvidia/export",
        "Refresh the separate read-only NVIDIA evidence projection",
    ),
    (
        "nvidia/ask",
        "Ask NVIDIA Auto Ontology about the current evidence and preserve its public assessment",
    ),
];

pub fn operation_schema(operation: &str) -> Value {
    let s = json!({"type":"string","maxLength":20000});
    let identifier = json!({"type":"string","minLength":1,"maxLength":256});
    let refs = json!({"type":"array","items":identifier,"maxItems":50});
    let (properties, required) = match operation {
        "investigations/create" => (
            json!({"question":{"type":"string","minLength":1,"maxLength":4000},"searches":{"type":"array","maxItems":5,"items":{"type":"object","properties":{"command":{"enum":["telegram-search"]},"target":{"type":"string","minLength":1,"maxLength":2048},"options":{"type":"object"}},"required":["command","target"],"additionalProperties":false}}}),
            vec!["question"],
        ),
        "investigations/show" => (json!({"id":identifier}), vec!["id"]),
        "investigations/run" | "investigations/evidence" => (
            json!({"id":identifier,"step_id":identifier}),
            vec!["id", "step_id"],
        ),
        "investigations/select" => (
            json!({"id":identifier,"step_id":identifier,"index":{"type":"integer","minimum":0,"maximum":99}}),
            vec!["id", "step_id", "index"],
        ),
        "investigations/propose" => (
            json!({"id":identifier,"content":{"type":"string","minLength":1,"maxLength":12000},"kind":{"enum":["observation","hypothesis","identity_sensitive"]},"citations":{"type":"array","maxItems":20,"items":{"type":"object","properties":{"step_id":identifier,"pointer":{"type":"string","minLength":1,"maxLength":128}},"required":["step_id","pointer"],"additionalProperties":false}}}),
            vec!["id", "content", "kind", "citations"],
        ),
        "case" => (json!({"title":s,"question":s}), vec!["title"]),
        "evidence" => (
            json!({"title":s,"content":s,"source_url":s,"kind":s,"observed_at":s}),
            vec!["title"],
        ),
        "evidence/review" => (
            json!({"evidence_id":identifier,"status":{"enum":["unreviewed","corroborated","contested","discarded"]},"note":s}),
            vec!["evidence_id", "status"],
        ),
        "notes" => (json!({"content":s,"evidence_ids":refs}), vec!["content"]),
        "entities" => (json!({"kind":s,"value":s,"label":s}), vec!["kind", "value"]),
        "links" => (
            json!({"source_id":identifier,"target_id":identifier,"relation":s,"evidence_ids":refs,"note":s}),
            vec!["source_id", "target_id", "relation", "evidence_ids"],
        ),
        "flows" => (
            json!({"title":s,"steps":{"type":"array","maxItems":20,"items":{"type":"object"}}}),
            vec!["title"],
        ),
        "flows/steps" => (
            json!({"flow_id":identifier,"command":s,"target":s,"options":{"type":"object"}}),
            vec!["flow_id", "command"],
        ),
        "flows/run" => (
            json!({"flow_id":identifier,"step_id":identifier}),
            vec!["flow_id", "step_id"],
        ),
        "tools/run" => (
            json!({"command":s,"target":s,"options":{"type":"object"}}),
            vec!["command"],
        ),
        "correlations/review" | "claims/review" => (
            json!({"id":identifier,"decision":{"enum":["accepted","rejected"]},"note":s}),
            vec!["id", "decision", "note"],
        ),
        "discovery/create" => (
            json!({"kind":{"enum":["username","profile","url","domain","search"]},"seed":s,"max_jobs":{"type":"integer","minimum":1,"maximum":100},"max_depth":{"type":"integer","minimum":0,"maximum":4},"domains":{"type":"array","items":s,"maxItems":30},"sites":{"type":"array","items":s,"maxItems":30},"web_search":{"type":"boolean"},"planner":{"enum":["local","nvidia"]},"reasoning_budget":{"type":"integer","minimum":1,"maximum":20}}),
            vec!["seed"],
        ),
        "discovery/run" => (json!({"id":identifier}), vec!["id"]),
        "discovery/configure" => (
            json!({"id":identifier,"planner":{"enum":["local","nvidia"]},"reasoning_budget":{"type":"integer","minimum":1,"maximum":20}}),
            vec!["id", "planner", "reasoning_budget"],
        ),
        "nvidia/ask" => (
            json!({"question":{"type":"string","minLength":1,"maxLength":4000}}),
            vec!["question"],
        ),
        _ => (json!({}), vec![]),
    };
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}

pub fn validate(value: &Value, schema: &Value) -> Result<()> {
    if let Some(options) = schema["enum"].as_array() {
        ensure!(options.contains(value), "Value is outside allowed choices");
    }
    match schema["type"].as_str() {
        Some("object") => {
            let obj = value
                .as_object()
                .ok_or_else(|| anyhow::anyhow!("Expected an object"))?;
            let props = schema["properties"].as_object();
            for key in schema["required"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                ensure!(obj.contains_key(key), "Missing required field {key}");
            }
            for (key, val) in obj {
                if let Some(field) = props.and_then(|p| p.get(key)) {
                    validate(val, field)?;
                } else {
                    ensure!(
                        schema["additionalProperties"] != false,
                        "Unsupported field {key}"
                    );
                }
            }
        }
        Some("string") => {
            let s = value
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Expected text"))?;
            ensure!(
                !s.contains('\0')
                    && s.len() >= schema["minLength"].as_u64().unwrap_or(0) as usize
                    && s.len() <= schema["maxLength"].as_u64().unwrap_or(65536) as usize,
                "Text length outside allowed range"
            );
        }
        Some("boolean") => ensure!(value.is_boolean(), "Expected true or false"),
        Some("integer") => {
            let n = value
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("Expected an unsigned integer"))?;
            ensure!(
                n >= schema["minimum"].as_u64().unwrap_or(0)
                    && n <= schema["maximum"].as_u64().unwrap_or(u64::MAX),
                "Integer outside allowed range"
            );
        }
        Some("array") => {
            let a = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("Expected an array"))?;
            ensure!(
                a.len() <= schema["maxItems"].as_u64().unwrap_or(100) as usize,
                "Too many array items"
            );
            for item in a {
                validate(item, &schema["items"])?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub fn apply(workbench: &Workbench, operation: &str, v: &Value) -> Result<Value> {
    ensure!(
        OPERATIONS.iter().any(|(name, _)| *name == operation),
        "Unknown workspace operation"
    );
    ensure!(
        v.to_string().len() <= 65536,
        "Operation input exceeds 64 KiB"
    );
    validate(v, &operation_schema(operation))?;
    match operation {
        "investigations/create" => workbench.create_investigation(
            text(v, "question"),
            serde_json::from_value(v.get("searches").cloned().unwrap_or(json!([])))?,
        ),
        "investigations/show" => workbench.investigation(text(v, "id")),
        "investigations/run" => workbench.run_investigation_step(text(v, "id"), text(v, "step_id")),
        "investigations/evidence" => {
            workbench.investigation_evidence(text(v, "id"), text(v, "step_id"))
        }
        "investigations/select" => workbench.select_investigation_post(
            text(v, "id"),
            text(v, "step_id"),
            v["index"].as_u64().unwrap() as usize,
        ),
        "investigations/propose" => workbench.propose_investigation_finding(
            text(v, "id"),
            text(v, "content"),
            text(v, "kind"),
            v["citations"].as_array().unwrap().clone(),
        ),
        "case" => workbench.update_case(text(v, "title"), text(v, "question")),
        "evidence" => workbench.add_evidence(
            text(v, "title"),
            text(v, "content"),
            text(v, "source_url"),
            v["kind"].as_str().unwrap_or("note"),
            v["observed_at"].as_str(),
        ),
        "evidence/review" => {
            workbench.review_evidence(text(v, "evidence_id"), text(v, "status"), text(v, "note"))
        }
        "notes" => workbench.add_note(text(v, "content"), strings(&v["evidence_ids"])),
        "entities" => workbench.add_entity(text(v, "kind"), text(v, "value"), text(v, "label")),
        "links" => workbench.add_link(
            text(v, "source_id"),
            text(v, "target_id"),
            text(v, "relation"),
            strings(&v["evidence_ids"]),
            text(v, "note"),
        ),
        "flows" => workbench.create_flow(
            text(v, "title"),
            serde_json::from_value(v.get("steps").cloned().unwrap_or(json!([])))?,
        ),
        "flows/steps" => workbench.add_step(
            text(v, "flow_id"),
            Job {
                command: text(v, "command").into(),
                target: v["target"].as_str().map(str::to_owned),
                options: v.get("options").cloned().unwrap_or(json!({})),
            },
        ),
        "flows/run" => workbench.run_step(text(v, "flow_id"), text(v, "step_id")),
        "tools/run" => workbench.run_tool(&serde_json::from_value(v.clone())?),
        "correlations/generate" => Ok(json!(workbench.correlations()?)),
        "correlations/review" => {
            workbench.review_correlation(text(v, "id"), text(v, "decision"), text(v, "note"))
        }
        "discovery/create" => {
            workbench.create_discovery(serde_json::from_value::<DiscoveryOptions>(v.clone())?)
        }
        "discovery/run" => workbench.run_discovery(text(v, "id"), &AtomicBool::new(false), |_| {}),
        "discovery/configure" => workbench.configure_discovery_planner(
            text(v, "id"),
            text(v, "planner"),
            v["reasoning_budget"].as_u64().unwrap_or(5) as usize,
        ),
        "nvidia/status" => workbench.nvidia_status(),
        "nvidia/export" => workbench.export_nvidia_projection(),
        "nvidia/ask" => workbench.nvidia_ask(text(v, "question"), &AtomicBool::new(false), |_| {}),
        "ontology/sync" => workbench.sync_ontology(),
        "claims/review" => {
            workbench.review_claim(text(v, "id"), text(v, "decision"), text(v, "note"))
        }
        _ => bail!("Unsupported operation"),
    }
}
