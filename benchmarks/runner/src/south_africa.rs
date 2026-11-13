use crate::common::*;
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path};

pub fn capture(client: &mut impl Tools, task: &Value, receipt: &mut Output) -> Result<Value> {
    let mut value =
        json!({"benchmark":task["id"],"track":task["track"],"status":"starting","inputs":[]});
    receipt.checkpoint(&value)?;
    let result = (|| -> Result<()> {
        let seeds: Vec<_> = array(&task["inputs"])?
            .iter()
            .map(|i| i["url"].clone())
            .collect();
        let inv = client.tool(
            "fieldkit_investigation_create",
            json!({"question":task["question"],"mode":task["mode"],"seeds":seeds}),
        )?;
        value["investigation_id"] = inv["id"].clone();
        receipt.checkpoint(&value)?;
        for item in array(&task["inputs"])? {
            let index = value["inputs"].as_array().unwrap().len();
            value["inputs"].as_array_mut().unwrap().push(json!({"id":item["id"],"url":item["url"],"expected_sha256":item["sha256"],"status":"planning"}));
            receipt.checkpoint(&value)?;
            let step=client.tool("fieldkit_investigation_plan_step",json!({"id":inv["id"],"job":{"command":"http","target":item["url"],"options":{}},"rationale":"Inspect this explicitly supplied dated benchmark capture. Do not execute scripts, follow live links, or project observations onto other dates."}))?;
            value["inputs"][index]["step_id"] = step["id"].clone();
            value["inputs"][index]["status"] = json!("attempting");
            receipt.checkpoint(&value)?;
            let report = client.tool(
                "fieldkit_investigation_run_step",
                json!({"id":inv["id"],"step_id":step["id"]}),
            )?;
            let entry = &mut value["inputs"][index];
            entry["run_id"] = report["run_id"].clone();
            entry["collector_status"] = report["status"].clone();
            entry["observed_sha256"] = report["data"]["body_sha256"].clone();
            entry["status"] = json!("retained");
            receipt.checkpoint(&value)?;
            ensure!(
                report["status"] == "ok"
                    && report["data"]["http_status"] == 200
                    && report["data"]["body_sha256"] == item["sha256"],
                "Blocked, incomplete or changed fixture; receipt retained; no retry or silent hash update"
            );
        }
        Ok(())
    })();
    value["status"] = json!(if result.is_ok() {
        "captured"
    } else {
        "incomplete"
    });
    receipt.checkpoint(&value)?;
    result?;
    Ok(value)
}
pub fn canonical(kind: &str, raw: &str) -> Option<String> {
    let lower = raw.to_ascii_lowercase();
    match kind {
        "google_analytics_ua_property" => {
            let rest = lower.strip_prefix("ua-")?;
            let (account, property) = rest.split_once('-')?;
            if !account.is_empty()
                && !property.is_empty()
                && account.bytes().all(|b| b.is_ascii_digit())
                && property.bytes().all(|b| b.is_ascii_digit())
            {
                Some(raw.to_ascii_uppercase())
            } else {
                None
            }
        }
        "google_adsense_publisher" => {
            let value = lower.strip_prefix("ca-").unwrap_or(&lower);
            let number = value.strip_prefix("pub-")?;
            (number.len() == 16 && number.bytes().all(|b| b.is_ascii_digit()))
                .then(|| value.to_owned())
        }
        _ => None,
    }
}
fn fact(value: &Value) -> (String, String) {
    (
        value["type"].as_str().unwrap_or("").into(),
        value["value"].as_str().unwrap_or("").into(),
    )
}
pub fn assess(
    task: &Value,
    oracle: &Value,
    reports: &Records,
    graph: &Value,
    blobs: &Bodies,
    integrity: &Value,
    state: &Value,
) -> Result<Value> {
    let nodes = unique_records(&graph["nodes"])?;
    let mut results = Vec::new();
    for item in array(&task["inputs"])? {
        let key = text(&item["id"])?;
        let report = &reports[key];
        let raw = &blobs[key];
        let pinned =
            hash(raw) == text(&item["sha256"])? && report["data"]["body_sha256"] == item["sha256"];
        let expected: BTreeSet<(String, String)> = array(&oracle["expected"][key])?
            .iter()
            .map(|v| Ok((text(&v[0])?.into(), text(&v[1])?.into())))
            .collect::<Result<_>>()?;
        let extracted = &report["data"]["tracking_identifiers"];
        let supported = extracted.is_object() && extracted["response_eligible"] == true;
        let mut facts = BTreeSet::new();
        let mut bad_spans = 0;
        if let Some(observations) = extracted["identifiers"].as_array() {
            for o in observations {
                let start = o["byte_start"].as_u64();
                let end = o["byte_end"].as_u64();
                let literal = o["raw_value"].as_str().unwrap_or("");
                let kind = o["kind"].as_str().unwrap_or("");
                let value = o["value"].as_str().unwrap_or("");
                let valid = match (start, end) {
                    (Some(s), Some(e)) if s < e && e <= raw.len() as u64 => {
                        raw[s as usize..e as usize] == *literal.as_bytes()
                            && canonical(kind, literal).as_deref() == Some(value)
                            && o["scope"] == "contains_tracking_literal"
                    }
                    _ => false,
                };
                if valid {
                    facts.insert((kind.to_owned(), value.to_owned()));
                } else {
                    bad_spans += 1;
                }
            }
        }
        let evidence = format!("evidence_{}", text(&report["run_id"])?);
        let source_ids: BTreeSet<_> = array(&report["sources"])?
            .iter()
            .filter(|s| s["sha256"] == item["sha256"])
            .map(|s| s["source_id"].clone().to_string())
            .collect();
        let mut graph_facts = BTreeSet::new();
        let mut bad_edges = 0;
        for edge in array(&graph["edges"])? {
            if edge["relation"] != "contains_tracking_literal"
                || !array(&edge["evidence_ids"])?.contains(&json!(evidence))
            {
                continue;
            }
            let source = nodes.get(edge["source_id"].as_str().unwrap_or(""));
            let target = nodes.get(edge["target_id"].as_str().unwrap_or(""));
            if let (Some(source), Some(target)) = (source, target) {
                let f = fact(target);
                if source["type"] == "url"
                    && source["value"] == report["data"]["url"]
                    && edge["status"] == "source_observation"
                    && array(&edge["source_ids"])?
                        .iter()
                        .any(|id| source_ids.contains(&id.to_string()))
                    && facts.contains(&f)
                {
                    graph_facts.insert(f);
                    continue;
                }
            }
            bad_edges += 1;
        }
        let passed = pinned
            && supported
            && report["status"] == "ok"
            && report["data"]["http_status"] == 200
            && extracted["truncated"] == false
            && facts == expected
            && graph_facts == expected
            && bad_spans == 0
            && bad_edges == 0;
        results.push(json!({"id":key,"pinned_bytes":pinned,"extraction_supported":supported,"expected_facts":expected.len(),"supported_hits":facts.intersection(&expected).count(),"unexpected_facts":facts.difference(&expected).count(),"invalid_spans":bad_spans,"graph_supported_hits":graph_facts.intersection(&expected).count(),"invalid_edges":bad_edges,"passed":passed}));
    }
    let pending = pending_only(state, 0);
    Ok(
        json!({"benchmark":task["id"],"core_reconstruction":if results.iter().all(|r|r["passed"]==true) && integrity["ok"]==true && pending {"passed"}else{"failed"},"inputs":results,"case_integrity_ok":integrity["ok"]==true,"findings_pending_only":pending,"finding_count":state["findings"].as_array().map_or(0,Vec::len),"one_seed_discovery":"not_evaluated: five dated captures are supplied; historical reverse-index discovery is not reproduced","agent_autonomy":"not_evaluated: deterministic capture and provenance checks, not a blinded model trial","interpretation":"Human judgment still required; passing does not establish ownership, personal identity, motives or the full published network."}),
    )
}
pub fn score(
    client: &mut impl Tools,
    task: &Value,
    oracle: &Value,
    receipt: &Value,
    case: &Path,
) -> Result<Value> {
    ensure!(
        receipt["status"] == "captured" && receipt["benchmark"] == task["id"],
        "A complete matching capture receipt is required"
    );
    let entries = unique_records(&receipt["inputs"])?;
    ensure!(
        entries.len() == array(&task["inputs"])?.len(),
        "Exact receipt input set required"
    );
    let mut reports = Records::new();
    let mut bodies = Bodies::new();
    for item in array(&task["inputs"])? {
        let key = text(&item["id"])?;
        let entry = entries
            .get(key)
            .ok_or_else(|| anyhow::anyhow!("Missing receipt input"))?;
        let report = client.tool(
            "fieldkit_investigation_evidence",
            json!({"id":receipt["investigation_id"],"step_id":entry["step_id"]}),
        )?;
        ensure!(
            array(&report["sources"])?
                .iter()
                .any(|s| s["sha256"] == item["sha256"])
                && report["target"] == item["url"]
                && report["data"]["requested_url"] == item["url"]
                && report["run_id"] == entry["run_id"],
            "Report does not bind pinned input and receipt run"
        );
        bodies.insert(key.into(), blob(case, text(&item["sha256"])?)?);
        reports.insert(key.into(), report);
    }
    let graph = client.tool("fieldkit_graph_show", json!({}))?;
    let integrity = client.tool("fieldkit_case_verify", json!({}))?;
    let state = client.tool(
        "fieldkit_investigation_show",
        json!({"id":receipt["investigation_id"]}),
    )?;
    let stored = stored(case)?;
    bind_stored(&reports, &stored)?;
    let citation_count = citations(&state, receipt, &reports, &bodies, &stored.hashes)?;
    let mut result = assess(task, oracle, &reports, &graph, &bodies, &integrity, &state)?;
    result["citations_verified"] = json!(citation_count);
    if !pending_only(&state, stored.reviews) {
        result["findings_pending_only"] = json!(false);
        result["core_reconstruction"] = json!("failed");
    }
    Ok(result)
}

#[cfg(test)]
mod capture_tests;
#[cfg(test)]
mod tests;
