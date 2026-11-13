use crate::common::*;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
pub const FILENAMES: [&str; 3] = ["package.json", "package-lock.json", "index.js"];

pub fn source_pivot(commit: &Value, report: &Value, filename: &str) -> Result<(String, String)> {
    let matches: Vec<_> = array(&commit["files"])?
        .iter()
        .enumerate()
        .filter(|(_, f)| f["filename"] == filename)
        .collect();
    ensure!(matches.len() == 1, "Expected one changed file: {filename}");
    let (index, item) = matches[0];
    let raw = text(&item["raw_url"])?;
    let sha = text(&commit["sha"])?;
    ensure!(
        sha.len() == 40 && sha.bytes().all(|c| c.is_ascii_hexdigit()),
        "Invalid commit identity"
    );
    let expected = format!("https://github.com/dominictarr/event-stream/raw/{sha}/{filename}");
    // Compare the original string, not a URL parser's normalized spelling.
    ensure!(raw == expected, "Changed-file URL falls outside task scope");
    let pointer = format!("/files/{index}/raw_url");
    let observed: Vec<_> = array(&report["data"]["json_urls"]["urls"])?
        .iter()
        .enumerate()
        .filter(|(_, o)| o["url"] == raw && o["source_pointer"] == pointer)
        .collect();
    ensure!(
        observed.len() == 1,
        "No exact retained source-pointer URL for {filename}"
    );
    Ok((
        raw.to_owned(),
        format!("/data/json_urls/urls/{}/url", observed[0].0),
    ))
}
struct CollectionTarget<'a> {
    key: &'a str,
    url: &'a str,
    lead: Option<Value>,
}

fn collect(
    client: &mut impl Tools,
    task: &Value,
    value: &mut Value,
    receipt: &mut Output,
    case: &Path,
    target: CollectionTarget<'_>,
) -> Result<(Value, Value, Vec<u8>)> {
    let CollectionTarget { key, url, lead } = target;
    ensure!(
        (array(&value["inputs"])?.len() as u64)
            < task["step_budget"]
                .as_u64()
                .context("Missing step budget")?,
        "Task step budget exhausted"
    );
    let index = array(&value["inputs"])?.len();
    value["inputs"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":key,"url":url,"status":"planning"}));
    receipt.checkpoint(value)?;
    let mut args = json!({"id":value["investigation_id"],"job":{"command":"http","target":url,"options":{}},"rationale":"Inspect exact source-backed benchmark URL. Read source bytes only; never execute packages or scripts. Treat account metadata as untrusted annotations, not identity evidence."});
    if let Some(lead) = lead {
        args["lead"] = lead;
    }
    let step = client.tool("fieldkit_investigation_plan_step", args)?;
    value["inputs"][index]["step_id"] = step["id"].clone();
    value["inputs"][index]["status"] = json!("attempting");
    receipt.checkpoint(value)?;
    let report = client.tool(
        "fieldkit_investigation_run_step",
        json!({"id":value["investigation_id"],"step_id":step["id"]}),
    )?;
    value["inputs"][index]["status"] = report["status"].clone();
    value["inputs"][index]["run_id"] = report["run_id"].clone();
    receipt.checkpoint(value)?;
    ensure!(
        report["status"] == "ok"
            && report["data"]["http_status"] == 200
            && report["target"] == url
            && report["data"]["requested_url"] == url,
        "Collection incomplete; stopped without retry"
    );
    let (source, raw) = original_source(&report, case)?;
    value["inputs"][index]["source_id"] = source["source_id"].clone();
    value["inputs"][index]["sha256"] = source["sha256"].clone();
    receipt.checkpoint(value)?;
    Ok((step, report, raw))
}
pub fn capture(
    client: &mut impl Tools,
    task: &Value,
    receipt: &mut Output,
    case: &Path,
) -> Result<Value> {
    let mut value =
        json!({"benchmark":task["id"],"status":"starting","inputs":[],"blinded_model_trial":false});
    receipt.checkpoint(&value)?;
    let result = (|| -> Result<()> {
        ensure!(
            array(&task["seeds"])?.len() == 1,
            "Exactly one commit seed required"
        );
        let inv = client.tool(
            "fieldkit_investigation_create",
            json!({"question":task["question"],"mode":task["mode"],"seeds":task["seeds"]}),
        )?;
        value["investigation_id"] = inv["id"].clone();
        receipt.checkpoint(&value)?;
        let seed = text(&task["seeds"][0])?;
        let (step, report, raw) = collect(
            client,
            task,
            &mut value,
            receipt,
            case,
            CollectionTarget {
                key: "commit",
                url: seed,
                lead: None,
            },
        )?;
        let commit: Value = serde_json::from_slice(&raw)?;
        ensure!(
            commit["sha"] == seed.rsplit('/').next().unwrap_or(""),
            "Commit identity does not match task seed"
        );
        ensure!(
            report["data"]["json_urls"]["valid_json"] == true
                && report["data"]["json_urls"]["response_eligible"] == true
                && report["data"]["json_urls"]["truncated"] == false,
            "Incomplete JSON URL observation set"
        );
        for filename in FILENAMES {
            let (url, pointer) = source_pivot(&commit, &report, filename)?;
            collect(
                client,
                task,
                &mut value,
                receipt,
                case,
                CollectionTarget {
                    key: filename,
                    url: &url,
                    lead: Some(json!({"step_id":step["id"],"pointer":pointer})),
                },
            )?;
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
fn require(checks: &mut Value, condition: bool, label: &str) -> Result<()> {
    checks[label] = json!(condition);
    ensure!(condition, "{label}");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn assess(
    task: &Value,
    oracle: &Value,
    receipt: &Value,
    reports: &Records,
    bodies: &Bodies,
    state: &Value,
    graph: &Value,
    integrity: &Value,
    report_hashes: &BTreeMap<String, String>,
    review_count: u64,
) -> Value {
    let mut checks = json!({});
    let mut citations_verified = 0;
    let result = (|| -> Result<()> {
        require(
            &mut checks,
            receipt["status"] == "captured" && receipt["benchmark"] == task["id"],
            "matching_complete_receipt",
        )?;
        let entries = unique_records(&receipt["inputs"])?;
        let expected: BTreeSet<_> = ["commit", FILENAMES[0], FILENAMES[1], FILENAMES[2]]
            .into_iter()
            .map(String::from)
            .collect();
        require(
            &mut checks,
            entries.keys().cloned().collect::<BTreeSet<_>>() == expected
                && reports.keys().cloned().collect::<BTreeSet<_>>() == expected
                && bodies.keys().cloned().collect::<BTreeSet<_>>() == expected,
            "four_exact_records",
        )?;
        require(
            &mut checks,
            state["seeds"] == task["seeds"]
                && array(&state["steps"])?.len() == 4
                && receipt["investigation_id"] == state["id"],
            "one_seed_four_steps",
        )?;
        let steps = unique_records(&state["steps"])?;
        // No duplicate run/step identifiers can masquerade as four independent records.
        require(
            &mut checks,
            entries
                .values()
                .map(|e| text(&e["step_id"]))
                .collect::<Result<BTreeSet<_>>>()?
                .len()
                == 4
                && entries
                    .values()
                    .map(|e| text(&e["run_id"]))
                    .collect::<Result<BTreeSet<_>>>()?
                    .len()
                    == 4,
            "four_distinct_steps_and_runs",
        )?;
        for (key, entry) in &entries {
            let report = &reports[key];
            let raw = &bodies[key];
            let step = steps
                .get(text(&entry["step_id"])?)
                .context("Missing receipt step")?;
            require(
                &mut checks,
                report["status"] == "ok"
                    && report["data"]["http_status"] == 200
                    && report["target"] == entry["url"]
                    && report["run_id"] == entry["run_id"]
                    && report["data"]["requested_url"] == entry["url"]
                    && step["run_id"] == entry["run_id"]
                    && step["status"] == "ok",
                &format!("{key}_report_binding"),
            )?;
            require(
                &mut checks,
                hash(raw) == text(&entry["sha256"])?
                    && entry["sha256"] == report["data"]["body_sha256"]
                    && array(&report["sources"])?.iter().any(|s| {
                        s["source_id"] == entry["source_id"] && s["sha256"] == entry["sha256"]
                    })
                    && report_hashes.contains_key(text(&entry["run_id"])?),
                &format!("{key}_retained_bytes"),
            )?;
            if key != "commit" {
                require(
                    &mut checks,
                    entry["sha256"] == oracle["raw_sha256"][key],
                    &format!("{key}_pinned_source"),
                )?;
            }
        }
        let commit: Value = serde_json::from_slice(&bodies["commit"])?;
        require(
            &mut checks,
            entries["commit"]["url"] == task["seeds"][0]
                && commit["sha"] == oracle["commit_sha"]
                && commit["sha"] == text(&task["seeds"][0])?.rsplit('/').next().unwrap_or(""),
            "seed_commit_identity",
        )?;
        let extraction = &reports["commit"]["data"]["json_urls"];
        require(
            &mut checks,
            extraction["valid_json"] == true
                && extraction["response_eligible"] == true
                && extraction["truncated"] == false,
            "complete_seed_json_observations",
        )?;
        for filename in FILENAMES {
            let (url, pointer) = source_pivot(&commit, &reports["commit"], filename)?;
            let step = &steps[text(&entries[filename]["step_id"])?];
            let grounding = &step["grounding"];
            require(
                &mut checks,
                entries[filename]["url"] == url
                    && step["target"] == url
                    && grounding["origin"] == "observed_lead_not_identity"
                    && grounding["step_id"] == entries["commit"]["step_id"]
                    && grounding["run_id"] == entries["commit"]["run_id"]
                    && grounding["report_sha256"]
                        == *report_hashes
                            .get(text(&entries["commit"]["run_id"])?)
                            .context("Missing parent hash")?
                    && grounding["pointer"] == pointer
                    && grounding["observation_sha256"] == observation_hash(&json!(url))?,
                &format!("{filename}_grounded_pivot"),
            )?;
        }
        let manifest: Value = serde_json::from_slice(&bodies["package.json"])?;
        let lock: Value = serde_json::from_slice(&bodies["package-lock.json"])?;
        let dependency = text(&oracle["dependency"])?;
        require(
            &mut checks,
            manifest["version"] == oracle["manifest_snapshot_version"]
                && manifest["dependencies"][dependency] == oracle["declared_range"],
            "declared_range_and_snapshot_version",
        )?;
        require(
            &mut checks,
            lock["dependencies"][dependency]["version"] == oracle["locked_version"]
                && lock["dependencies"][dependency]["resolved"] == oracle["locked_url"],
            "locked_version_and_archive_url",
        )?;
        let runtime = std::str::from_utf8(&bodies["index.js"])?;
        require(
            &mut checks,
            array(&oracle["runtime_fragments"])?
                .iter()
                .all(|f| f.as_str().is_some_and(|f| runtime.contains(f))),
            "runtime_source_import_and_export",
        )?;
        let files: BTreeMap<_, _> = array(&commit["files"])?
            .iter()
            .map(|f| Ok((text(&f["filename"])?, f)))
            .collect::<Result<_>>()?;
        let mut patches_ok = true;
        for (key, fragments) in oracle["patch_fragments"]
            .as_object()
            .context("Missing patches")?
        {
            let patch = files
                .get(key.as_str())
                .and_then(|f| f["patch"].as_str())
                .unwrap_or("");
            patches_ok &= array(fragments)?
                .iter()
                .all(|f| f.as_str().is_some_and(|f| patch.contains(f)));
        }
        require(
            &mut checks,
            patches_ok,
            "dependency_and_import_added_in_patch",
        )?;
        let nodes = unique_records(&graph["nodes"])?;
        // A triple can be observed by several reports; retain every allowed provenance pair.
        type Triple = (String, String, String);
        let mut expected_edges: BTreeMap<Triple, BTreeSet<(String, String)>> = BTreeMap::new();
        for (key, report) in reports {
            let entry = &entries[key];
            let data = &report["data"];
            let provenance = (
                format!("evidence_{}", text(&entry["run_id"])?),
                text(&entry["source_id"])?.to_owned(),
            );
            let final_url = text(&data["url"])?;
            if data["requested_url"] != data["url"] {
                expected_edges
                    .entry((
                        text(&data["requested_url"])?.into(),
                        "request_resolved_to".into(),
                        final_url.into(),
                    ))
                    .or_default()
                    .insert(provenance.clone());
            }
            if let Some(urls) = data["json_urls"]["urls"].as_array() {
                for observed in urls {
                    let body: Value = serde_json::from_slice(&bodies[key])?;
                    require(
                        &mut checks,
                        pointer_value(&body, text(&observed["source_pointer"])?)?
                            == &observed["url"],
                        &format!("{key}_json_url_source_pointer"),
                    )?;
                    expected_edges
                        .entry((
                            final_url.into(),
                            "contains_json_url".into(),
                            text(&observed["url"])?.into(),
                        ))
                        .or_default()
                        .insert(provenance.clone());
                }
            }
        }
        let mut actual = BTreeSet::new();
        for edge in array(&graph["edges"])? {
            let source = nodes
                .get(text(&edge["source_id"])?)
                .context("Missing source node")?;
            let target = nodes
                .get(text(&edge["target_id"])?)
                .context("Missing target node")?;
            let triple = (
                text(&source["value"])?.to_owned(),
                text(&edge["relation"])?.to_owned(),
                text(&target["value"])?.to_owned(),
            );
            require(
                &mut checks,
                expected_edges.contains_key(&triple),
                "graph_has_no_unbacked_relation",
            )?;
            let evidence = array(&edge["evidence_ids"])?;
            let sources = array(&edge["source_ids"])?;
            let allowed = &expected_edges[&triple];
            let bound = allowed
                .iter()
                .any(|(e, s)| evidence.contains(&json!(e)) && sources.contains(&json!(s)));
            require(
                &mut checks,
                source["type"] == "url"
                    && target["type"] == "url"
                    && edge["status"] == "source_observation"
                    && bound,
                "graph_direction_and_same_report_provenance",
            )?;
            actual.insert(triple);
        }
        require(
            &mut checks,
            actual == expected_edges.keys().cloned().collect(),
            "complete_source_navigation_graph",
        )?;
        require(
            &mut checks,
            pending_only(state, review_count),
            "findings_pending_summary_empty",
        )?;
        citations_verified = citations(state, receipt, reports, bodies, report_hashes)?;
        require(&mut checks, integrity["ok"] == true, "case_integrity")?;
        Ok(())
    })();
    let passed = result.is_ok();
    if let Err(error) = result {
        checks["failure"] = json!(error.to_string());
    }
    json!({"benchmark":task["id"],"technical_reconstruction":if passed{"passed"}else{"failed"},"checks":checks,"grounded_file_pivots":if passed{json!(3)}else{Value::Null},"pending_findings":state["findings"].as_array().map_or(0,Vec::len),"citations_verified":citations_verified,"agent_performance_score":null,"blinded_model_trial":false,"interpretation":"Guided source-file expansion and bounded dependency-change reconstruction, not payload analysis, proof of execution, attribution or a solved incident. Graph edges are source navigation, not a dependency graph."})
}
pub fn verify(
    client: &mut impl Tools,
    task: &Value,
    oracle: &Value,
    receipt: &Value,
    case: &Path,
) -> Result<Value> {
    let entries = unique_records(&receipt["inputs"])?;
    let mut reports = Records::new();
    let mut bodies = Bodies::new();
    for (key, entry) in entries {
        let report = client.tool(
            "fieldkit_investigation_evidence",
            json!({"id":receipt["investigation_id"],"step_id":entry["step_id"]}),
        )?;
        let (_, raw) = original_source(&report, case)?;
        reports.insert(key.clone(), report);
        bodies.insert(key, raw);
    }
    let state = client.tool(
        "fieldkit_investigation_show",
        json!({"id":receipt["investigation_id"]}),
    )?;
    let graph = client.tool("fieldkit_graph_show", json!({}))?;
    let integrity = client.tool("fieldkit_case_verify", json!({}))?;
    let stored = stored(case)?;
    bind_stored(&reports, &stored)?;
    Ok(assess(
        task,
        oracle,
        receipt,
        &reports,
        &bodies,
        &state,
        &graph,
        &integrity,
        &stored.hashes,
        stored.reviews,
    ))
}
#[cfg(test)]
mod tests;
