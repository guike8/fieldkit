use crate::{
    collectors,
    model::{Collected, Config, Job, digest, entity_id, id, now, strings, text},
    store::Store,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Clone)]
pub struct Workbench {
    pub store: Store,
    pub config: Config,
}

fn checked<'a>(s: &'a str, name: &str, max: usize, empty: bool) -> Result<&'a str> {
    ensure!(
        s.len() <= max && !s.contains('\0') && (empty || !s.trim().is_empty()),
        "{name} requires {}–{max} bytes",
        if empty { 0 } else { 1 }
    );
    Ok(s.trim())
}

fn merge_ids(target: &mut Value, key: &str, values: impl IntoIterator<Item = String>) {
    let mut current = strings(&target[key]);
    for value in values {
        if !current.contains(&value) && current.len() < 128 {
            current.push(value);
        }
    }
    target[key] = json!(current);
}

impl Workbench {
    pub fn new(path: impl AsRef<Path>, config: Config) -> Result<Self> {
        Ok(Self {
            store: Store::open(path)?,
            config,
        })
    }

    pub fn update_case(&self, title: &str, question: &str) -> Result<Value> {
        let title = checked(title, "Title", 160, false)?;
        let question = checked(question, "Question", 4000, true)?;
        let mut metadata = self.store.meta()?;
        metadata["title"] = json!(title);
        metadata["question"] = json!(question);
        self.store.set_meta(metadata.clone())?;
        self.store.event(
            "case.updated",
            "Investigation question updated",
            json!({"title":title,"question":question}),
        )?;
        Ok(metadata)
    }

    pub(crate) fn collect_preserved(&self, job: &Job) -> Result<Value> {
        let collected = collectors::collect(job, &self.config)?;
        self.store.save_report(&collected)?;
        Ok(collected.report)
    }

    pub fn run_tool(&self, job: &Job) -> Result<Value> {
        let report = self.collect_preserved(job)?;
        self.store.event("collector.completed","Collector result preserved",json!({"command":job.command,"target":job.target,"run_id":report["run_id"],"status":report["status"]}))?;
        self.sync_ontology()?;
        Ok(report)
    }

    pub fn references(&self, values: Vec<String>, required: bool) -> Result<Vec<String>> {
        ensure!(
            values.len() <= 50 && (!required || !values.is_empty()),
            "Supply {}–50 supporting evidence IDs",
            if required { 1 } else { 0 }
        );
        let mut unique = Vec::new();
        for value in values {
            ensure!(
                self.store.has_evidence(&value)?,
                "Unknown supporting evidence ID"
            );
            if !unique.contains(&value) {
                unique.push(value);
            }
        }
        Ok(unique)
    }

    pub fn add_evidence(
        &self,
        title: &str,
        content: &str,
        source_url: &str,
        kind: &str,
        observed_at: Option<&str>,
    ) -> Result<Value> {
        checked(title, "Title", 240, false)?;
        checked(content, "Content", 20000, true)?;
        checked(kind, "Kind", 40, false)?;
        if !source_url.is_empty() {
            let url = url::Url::parse(source_url)?;
            ensure!(
                ["https", "http"].contains(&url.scheme())
                    && url.username().is_empty()
                    && url.password().is_none(),
                "Source URL must be a public HTTP(S) reference"
            );
        }
        if let Some(time) = observed_at.filter(|s| !s.is_empty()) {
            chrono::DateTime::parse_from_rfc3339(time).context("Observed time must be RFC3339")?;
        }
        let identifier = id("evidence");
        let run = id("run");
        let source = id("src");
        let timestamp = now();
        let hash = digest(content.as_bytes());
        let sources = json!([{"source_id":source,"kind":"analyst_supplied","name":title,"url":source_url,"observed_at":timestamp,"sha256":hash,"bytes":content.len(),"untrusted_content":true,"collection_method":"manual_import"}]);
        let entities = if source_url.is_empty() {
            json!([])
        } else {
            json!([{"type":"url","value":source_url,"source_ids":[source]}])
        };
        let report = json!({"schema_version":"1.0","tool":"fieldkit","version":env!("CARGO_PKG_VERSION"),"run_id":run,"command":"evidence.note","target":source_url,"status":"ok","data":{"evidence_id":identifier,"title":title,"content":content},"sources":sources,"entities":entities,"relationships":[],"errors":[],"timing":{"started_at":timestamp,"completed_at":timestamp},"untrusted_content":true});
        let record = json!({"id":identifier,"title":title,"content":content,"source_url":source_url,"kind":kind,"observed_at":observed_at,"collected_at":timestamp,"created_at":timestamp,"sha256":hash,"artifact_sha256":hash,"hash_scope":"source_bytes","status":"unreviewed","review_note":"","run_id":run,"source_ids":[source],"sources":sources,"source_count":1,"collection_method":"analyst_supplied"});
        self.store.save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash, content.as_bytes().to_vec())]),
        })?;
        self.store.put("evidence", &record)?;
        self.store.event(
            "evidence.added",
            "Evidence note preserved",
            json!({"evidence_id":identifier}),
        )?;
        self.sync_ontology()?;
        Ok(record)
    }

    pub fn review_evidence(&self, identifier: &str, status: &str, note: &str) -> Result<Value> {
        ensure!(
            ["unreviewed", "corroborated", "contested", "discarded"].contains(&status),
            "Unknown evidence assessment"
        );
        self.references(vec![identifier.to_owned()], true)?;
        checked(note, "Assessment note", 4000, true)?;
        let record = json!({"id":identifier,"status":status,"review_note":note,"reviewed_at":now(),"scope":"analyst_assessment"});
        self.store.put("reviews", &record)?;
        self.store.event(
            "evidence.reviewed",
            "Evidence assessment recorded",
            record.clone(),
        )?;
        Ok(record)
    }

    pub fn add_note(&self, content: &str, references: Vec<String>) -> Result<Value> {
        checked(content, "Note", 20000, false)?;
        let record = json!({"id":id("note"),"content":content,"evidence_ids":self.references(references,false)?,"created_at":now()});
        self.store.put("notes", &record)?;
        self.store.event(
            "note.added",
            "Analyst note added",
            json!({"id":record["id"],"evidence_ids":record["evidence_ids"]}),
        )?;
        Ok(record)
    }

    pub fn add_entity(&self, kind: &str, value: &str, label: &str) -> Result<Value> {
        checked(kind, "Type", 40, false)?;
        checked(value, "Entity value", 2048, false)?;
        checked(label, "Label", 160, true)?;
        ensure!(
            kind.bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_'),
            "Entity type must be a lowercase identifier"
        );
        let record = json!({"id":entity_id(kind,&json!(value)),"type":kind,"value":value,"label":if label.is_empty(){value}else{label},"evidence_ids":[],"source_ids":[],"origin":"analyst","created_at":now()});
        self.store.put("entities", &record)?;
        self.store
            .event("entity.added", "Entity annotated", record.clone())?;
        Ok(record)
    }

    pub fn add_link(
        &self,
        source: &str,
        target: &str,
        relation: &str,
        references: Vec<String>,
        note: &str,
    ) -> Result<Value> {
        checked(relation, "Relation", 80, false)?;
        checked(note, "Link note", 4000, true)?;
        let state = self.materialized()?;
        let entities: BTreeSet<&str> = state["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| text(e, "id"))
            .collect();
        ensure!(
            source != target && entities.contains(source) && entities.contains(target),
            "Select two distinct existing entities"
        );
        let record = json!({"id":id("link"),"source_id":source,"target_id":target,"relation":relation,"evidence_ids":self.references(references,true)?,"source_ids":[],"status":"analyst_assertion","note":note,"created_at":now()});
        self.store.put("links", &record)?;
        self.store.event(
            "link.added",
            "Evidence-backed analyst link recorded",
            record.clone(),
        )?;
        self.sync_ontology()?;
        Ok(record)
    }

    pub fn materialized(&self) -> Result<Value> {
        let reports = self.store.reports(500)?;
        let manual = self.store.list("evidence")?;
        let by_run: BTreeMap<String, String> = manual
            .iter()
            .map(|e| (text(e, "run_id").to_owned(), text(e, "id").to_owned()))
            .collect();
        let reviews: BTreeMap<String, Value> = self
            .store
            .list("reviews")?
            .into_iter()
            .map(|e| (text(&e, "id").to_owned(), e))
            .collect();
        let mut evidence = manual;
        let mut entities: BTreeMap<String, Value> = BTreeMap::new();
        let mut links: BTreeMap<String, Value> = BTreeMap::new();
        for report in &reports {
            let run = text(report, "run_id");
            let ev = by_run
                .get(run)
                .cloned()
                .unwrap_or_else(|| format!("evidence_{run}"));
            if !by_run.contains_key(run) {
                let sources = report["sources"].as_array().cloned().unwrap_or_default();
                let hash = self.store.report_hash(run)?;
                let body = serde_json::to_string_pretty(&report["data"])?;
                let preview: String = body.chars().take(16000).collect();
                evidence.push(json!({"id":ev,"title":format!("{} · {}",text(report,"command"),text(report,"target")),"kind":if report["command"]=="analysis.nvidia"{"model_assessment"}else{"collector"},"content":preview,"content_truncated":preview.len()<body.len(),"source_url":sources.first().map(|s|s["url"].clone()).unwrap_or(Value::Null),"collected_at":report["timing"]["completed_at"],"sha256":sources.first().and_then(|s|s["sha256"].as_str()).unwrap_or(&hash),"report_sha256":hash,"hash_scope":sources.first().and_then(|s|s["hash_scope"].as_str()).unwrap_or(if sources.first().is_some_and(|s|s["sha256"].is_string()) {"source_bytes"} else {"collector_report"}),"collector_status":report["status"],"status":"unreviewed","review_note":"","run_id":run,"source_ids":sources.iter().filter_map(|s|s["source_id"].as_str()).collect::<Vec<_>>(),"sources":sources,"source_count":sources.len(),"collection_method":if report["command"]=="analysis.nvidia"{"model_assessment_not_independent_evidence"}else{"collector"}}));
            }
            let allowed_sources: BTreeSet<String> = report["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["source_id"].as_str().map(str::to_owned))
                .collect();
            let source_filter = |v: &Value| -> Vec<String> {
                strings(v)
                    .into_iter()
                    .filter(|s| allowed_sources.contains(s))
                    .collect()
            };
            let mut add_entity = |entity: &Value, refs: Vec<String>| -> Option<String> {
                let kind = text(entity, "type");
                let value = &entity["value"];
                if kind.is_empty()
                    || kind.len() > 128
                    || (!value.is_string() && !value.is_number())
                    || value.to_string().len() > 4096
                {
                    return None;
                }
                let identifier = entity_id(kind, value);
                let entry = entities.entry(identifier.clone()).or_insert_with(||json!({"id":identifier,"type":kind,"value":value,"label":value.as_str().map(str::to_owned).unwrap_or_else(||value.to_string()),"source_ids":[],"evidence_ids":[],"origin":"observed"}));
                merge_ids(entry, "source_ids", refs);
                merge_ids(entry, "evidence_ids", [ev.clone()]);
                Some(identifier)
            };
            for entity in report["entities"].as_array().into_iter().flatten() {
                add_entity(entity, source_filter(&entity["source_ids"]));
            }
            for relationship in report["relationships"].as_array().into_iter().flatten() {
                let relation = text(relationship, "relation");
                if relation.is_empty() || relation.len() > 128 {
                    continue;
                }
                let refs = source_filter(&relationship["source_ids"]);
                let source = add_entity(
                    relationship
                        .get("source")
                        .or_else(|| relationship.get("from"))
                        .unwrap_or(&Value::Null),
                    refs.clone(),
                );
                let target = add_entity(
                    relationship
                        .get("target")
                        .or_else(|| relationship.get("to"))
                        .unwrap_or(&Value::Null),
                    refs.clone(),
                );
                if let (Some(source), Some(target)) = (source, target) {
                    let identifier = format!(
                        "link_{}",
                        digest(json!([source, relation, target]).to_string().as_bytes())
                    );
                    let entry = links.entry(identifier.clone()).or_insert_with(||json!({"id":identifier,"source_id":source,"target_id":target,"relation":relation,"source_ids":[],"evidence_ids":[],"status":"observed","note":""}));
                    merge_ids(entry, "source_ids", refs);
                    merge_ids(entry, "evidence_ids", [ev.clone()]);
                }
            }
        }
        for e in &mut evidence {
            if let Some(review) = reviews.get(text(e, "id")) {
                for (key, value) in review.as_object().unwrap() {
                    if key != "id" {
                        e[key] = value.clone();
                    }
                }
            }
        }
        evidence.sort_by(|a, b| text(b, "collected_at").cmp(text(a, "collected_at")));
        for e in self.store.list("entities")? {
            if let Some(entry) = entities.get_mut(text(&e, "id")) {
                entry["label"] = e["label"].clone();
            } else {
                entities.insert(text(&e, "id").to_owned(), e);
            }
        }
        for e in self.store.list("links")? {
            links.insert(text(&e, "id").to_owned(), e);
        }
        for link in links.values() {
            for endpoint in ["source_id", "target_id"] {
                if let Some(entity) = entities.get_mut(text(link, endpoint)) {
                    merge_ids(entity, "evidence_ids", strings(&link["evidence_ids"]));
                }
            }
        }
        let count: i64 =
            self.store
                .connection()?
                .query_row("SELECT count(*) FROM runs", [], |r| r.get(0))?;
        Ok(
            json!({"evidence":evidence,"entities":entities.values().collect::<Vec<_>>(),"links":links.values().collect::<Vec<_>>(),"summary":{"run_count":count},"truncated":count>reports.len() as i64}),
        )
    }

    pub fn correlations(&self) -> Result<Vec<Value>> {
        self.correlations_from(&self.materialized()?)
    }

    fn correlations_from(&self, state: &Value) -> Result<Vec<Value>> {
        let entities: BTreeMap<&str, &Value> = state["entities"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (text(e, "id"), e))
            .collect();
        let evidence_aliases: BTreeMap<String, String> = self
            .store
            .list("evidence")?
            .into_iter()
            .map(|v| (text(&v, "run_id").to_owned(), text(&v, "id").to_owned()))
            .collect();
        let mut entity_anchors: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for report in self.store.reports(500)? {
            let run = text(&report, "run_id");
            let ev = evidence_aliases
                .get(run)
                .cloned()
                .unwrap_or_else(|| format!("evidence_{run}"));
            let hash = self.store.report_hash(run)?;
            let mut anchor_entity = |entity: &Value, pointer: String| {
                if text(entity, "type").is_empty() || entity["value"].is_null() {
                    return;
                }
                let identifier = entity_id(text(entity, "type"), &entity["value"]);
                let anchors = entity_anchors.entry(identifier).or_default();
                if anchors.len() < 128 {
                    anchors.push(json!({"evidence_id":ev,"run_id":run,"report_sha256":hash,"json_pointer":pointer,"source_ids":entity["source_ids"],"scope":"structured_collector_report"}));
                }
            };
            for (i, e) in report["entities"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                anchor_entity(e, format!("/entities/{i}"));
            }
            for (i, r) in report["relationships"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                for side in ["source", "target", "from", "to"] {
                    if let Some(e) = r.get(side) {
                        anchor_entity(e, format!("/relationships/{i}/{side}"));
                    }
                }
            }
        }
        let claims: BTreeMap<String, Value> = self
            .store
            .list("ontology_claims")?
            .into_iter()
            .map(|v| (text(&v, "id").to_owned(), v))
            .collect();
        let mut groups: BTreeMap<(String, String), BTreeMap<String, Value>> = BTreeMap::new();
        for link in state["links"].as_array().unwrap() {
            if link["relation"] != "links_to" {
                continue;
            }
            if let (Some(source), Some(target)) = (
                entities.get(text(link, "source_id")),
                entities.get(text(link, "target_id")),
            ) {
                if !["url", "profile_url", "domain", "email"].contains(&text(target, "type"))
                    || strings(&link["evidence_ids"]).is_empty()
                {
                    continue;
                }
                let group = groups
                    .entry((
                        "shared_reference".to_owned(),
                        target["value"].as_str().unwrap_or("").to_owned(),
                    ))
                    .or_default();
                let basis=group.entry(text(source,"id").to_owned()).or_insert_with(||json!({"entity_id":source["id"],"evidence_ids":[],"source_ids":[],"link_ids":[],"claim_ids":[],"anchors":[],"origin":link["status"]}));
                merge_ids(basis, "evidence_ids", strings(&link["evidence_ids"]));
                merge_ids(basis, "source_ids", strings(&link["source_ids"]));
                merge_ids(basis, "link_ids", [text(link, "id").to_owned()]);
                let claim_id = text(link, "id").replacen("link_", "claim_", 1);
                if let Some(claim) = claims.get(&claim_id) {
                    merge_ids(basis, "claim_ids", [claim_id]);
                    for anchor in claim["anchors"].as_array().into_iter().flatten() {
                        let anchors = basis["anchors"].as_array_mut().unwrap();
                        if anchors.len() < 128 && !anchors.contains(anchor) {
                            anchors.push(anchor.clone());
                        }
                    }
                }
            }
        }
        for entity in entities.values() {
            if !["username", "handle"].contains(&text(entity, "type"))
                || strings(&entity["evidence_ids"]).is_empty()
            {
                continue;
            }
            if let Some((scope, handle)) = text(entity, "value").split_once(':') {
                if scope.is_empty()
                    || handle.is_empty()
                    || handle.len() > 64
                    || !handle
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                {
                    continue;
                }
                groups.entry(("shared_handle".to_owned(),handle.to_owned())).or_default().insert(text(entity,"id").to_owned(),json!({"entity_id":entity["id"],"scope":scope,"evidence_ids":entity["evidence_ids"],"source_ids":entity["source_ids"],"anchors":entity_anchors.get(text(entity,"id")).cloned().unwrap_or_default(),"origin":entity["origin"]}));
            }
        }
        let reviews: BTreeMap<String, Value> = self
            .store
            .list("correlation_reviews")?
            .into_iter()
            .map(|r| (text(&r, "id").to_owned(), r))
            .collect();
        let mut results = Vec::new();
        for ((signal, value), members) in groups {
            if !(2..=20).contains(&members.len()) {
                continue;
            }
            let members: Vec<_> = members.into_iter().collect();
            for i in 0..members.len() {
                for j in i + 1..members.len() {
                    let (first, left) = &members[i];
                    let (second, right) = &members[j];
                    if signal == "shared_handle" && left["scope"] == right["scope"] {
                        continue;
                    }
                    if signal == "shared_reference"
                        && same_page(entities[first.as_str()], entities[second.as_str()])
                    {
                        continue;
                    }
                    let all_refs: BTreeSet<String> = strings(&left["evidence_ids"])
                        .into_iter()
                        .chain(strings(&right["evidence_ids"]))
                        .collect();
                    let refs: Vec<String> = strings(&left["evidence_ids"])
                        .into_iter()
                        .take(25)
                        .chain(strings(&right["evidence_ids"]).into_iter().take(25))
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect();
                    if self.references(refs.clone(), true).is_err() {
                        continue;
                    }
                    let identifier = format!(
                        "correlation_{}",
                        digest(json!([first, second, signal, value]).to_string().as_bytes())
                    );
                    let reason = if signal == "shared_reference" {
                        format!("Both records explicitly reference {value}")
                    } else {
                        format!(
                            "The same literal handle appears in distinct service scopes: {value}"
                        )
                    };
                    let mut record = json!({"id":identifier,"source_id":first,"target_id":second,"signal":signal,"value":value,"reason":reason,"evidence_ids":refs,"evidence_refs_truncated":all_refs.len()>refs.len(),"basis":[left,right],"strength":if signal=="shared_handle"{"weak"}else{"reference_match"},"status":"candidate","review_note":"","interpretation":"Investigation lead; no identity or ownership attribution"});
                    if let Some(review) = reviews.get(&identifier) {
                        record["status"] = review["status"].clone();
                        record["review_note"] = review["review_note"].clone();
                        record["reviewed_at"] = review["reviewed_at"].clone();
                    }
                    results.push(record);
                    if results.len() >= 200 {
                        return Ok(results);
                    }
                }
            }
        }
        Ok(results)
    }

    pub fn review_correlation(
        &self,
        identifier: &str,
        decision: &str,
        note: &str,
    ) -> Result<Value> {
        ensure!(
            ["accepted", "rejected"].contains(&decision),
            "Decision must be accepted or rejected"
        );
        checked(note, "Review rationale", 4000, false)?;
        let mut candidate = self
            .correlations()?
            .into_iter()
            .find(|r| text(r, "id") == identifier)
            .context("Unknown correlation")?;
        self.references(strings(&candidate["evidence_ids"]), true)?;
        candidate["status"] = json!(decision);
        candidate["review_note"] = json!(note);
        candidate["reviewed_at"] = json!(now());
        self.store.put("correlation_reviews", &candidate)?;
        self.store.event(
            "correlation.reviewed",
            "Correlation lead reviewed",
            candidate.clone(),
        )?;
        Ok(candidate)
    }

    pub fn create_flow(&self, title: &str, jobs: Vec<Job>) -> Result<Value> {
        checked(title, "Flow title", 160, false)?;
        ensure!(jobs.len() <= 20, "A flow accepts up to 20 steps");
        let steps: Vec<_> = jobs
            .into_iter()
            .map(|j| self.step(j))
            .collect::<Result<_>>()?;
        let flow = json!({"id":id("flow"),"title":title,"steps":steps,"status":"pending","created_at":now()});
        self.store.put("flows", &flow)?;
        self.store.event(
            "flow.created",
            "Research flow created",
            json!({"flow_id":flow["id"]}),
        )?;
        Ok(flow)
    }

    fn step(&self, job: Job) -> Result<Value> {
        collectors::validate_job(&job)?;
        Ok(
            json!({"id":id("step"),"command":job.command,"target":job.target,"options":job.options,"status":"pending","attempts":0,"run_id":null,"error":null}),
        )
    }

    pub fn add_step(&self, flow_id: &str, job: Job) -> Result<Value> {
        let _lock = self.store.lock("flows.lock")?;
        let mut flow = self.store.get("flows", flow_id)?;
        let step = self.step(job)?;
        let steps = flow["steps"].as_array_mut().context("Malformed flow")?;
        ensure!(steps.len() < 20, "A flow accepts up to 20 steps");
        steps.push(step.clone());
        flow["status"] = json!("pending");
        self.store.put("flows", &flow)?;
        self.store.event(
            "flow.step_added",
            "Collection step planned",
            json!({"flow_id":flow_id,"step_id":step["id"]}),
        )?;
        Ok(step)
    }

    pub fn run_step(&self, flow_id: &str, step_id: &str) -> Result<Value> {
        let _lock = self.store.lock("flows.lock")?;
        let mut flow = self.store.get("flows", flow_id)?;
        let step = flow["steps"]
            .as_array_mut()
            .context("Malformed flow")?
            .iter_mut()
            .find(|s| text(s, "id") == step_id)
            .context("Unknown flow step")?;
        ensure!(step["status"] != "running", "Step is already running");
        let job: Job = serde_json::from_value(
            json!({"command":step["command"],"target":step["target"],"options":step["options"]}),
        )?;
        step["status"] = json!("running");
        step["started_at"] = json!(now());
        step["attempts"] = json!(step["attempts"].as_u64().unwrap_or(0) + 1);
        flow["status"] = json!("running");
        self.store.put("flows", &flow)?;
        let result = self.run_tool(&job);
        let mut flow = self.store.get("flows", flow_id)?;
        let step = flow["steps"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|s| text(s, "id") == step_id)
            .context("Flow changed while running")?;
        match &result {
            Ok(report) => {
                step["run_id"] = report["run_id"].clone();
                step["status"] = json!(if ["ok", "not_found"].contains(&text(report, "status")) {
                    "completed"
                } else {
                    "failed"
                });
                step["error"] = if step["status"] == "failed" {
                    report["status"].clone()
                } else {
                    Value::Null
                };
            }
            Err(error) => {
                step["status"] = json!("failed");
                step["error"] = json!(error.to_string());
            }
        }
        step["completed_at"] = json!(now());
        flow["status"] = json!(if flow["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["status"] == "completed")
        {
            "completed"
        } else {
            "pending"
        });
        self.store.put("flows", &flow)?;
        self.store.event(
            "flow.step_finished",
            "Collection step finished",
            json!({"flow_id":flow_id,"step_id":step_id}),
        )?;
        result
    }

    pub fn snapshot(&self) -> Result<Value> {
        let mut state = self.materialized()?;
        let correlations = self.correlations_from(&state)?;
        for c in &correlations {
            if c["status"] == "accepted" {
                state["links"].as_array_mut().unwrap().push(json!({"id":format!("reviewed_{}",text(c,"id")),"source_id":c["source_id"],"target_id":c["target_id"],"relation":"possible_association","evidence_ids":c["evidence_ids"],"status":"analyst_accepted_lead","note":c["review_note"]}));
            }
        }
        state["metadata"] = self.store.meta()?;
        state["case_path"] = json!(self.store.path);
        state["schema_version"] = json!("1.0");
        state["correlations"] = json!(correlations);
        state["investigations"] = json!(self.investigations()?);
        state["flows"] = json!(self.store.list("flows")?);
        state["notes"] = json!(self.store.list("notes")?);
        state["activity"] = json!(self.store.activity()?);
        state["discovery"] = json!(self.discoveries()?);
        state["frontier"] = json!(self.store.list("discovery_jobs")?);
        state["ontology"] = self.ontology_snapshot()?;
        state["reasoning"] = json!(self.store.list("reasoning")?);
        state["nvidia"] = crate::reasoning::summary();
        state["stats"] = json!({"evidence":state["evidence"].as_array().unwrap().len(),"entities":state["entities"].as_array().unwrap().len(),"links":state["links"].as_array().unwrap().len(),"correlations":correlations.len(),"flows":state["flows"].as_array().unwrap().len(),"notes":state["notes"].as_array().unwrap().len(),"candidate_leads":correlations.iter().filter(|c|c["status"]=="candidate").count(),"runs":state["summary"]["run_count"]});
        Ok(state)
    }

    pub fn export(&self, format: &str) -> Result<String> {
        ensure!(
            ["json", "markdown"].contains(&format),
            "Choose json or markdown"
        );
        let payload = json!({"format":"fieldkit-investigation-v2","exported_at":now(),"workspace":self.snapshot()?,"collector_ledger":{"reports":self.store.reports(500)?},"integrity":self.store.verify()?});
        if format == "json" {
            return Ok(serde_json::to_string_pretty(&payload)? + "\n");
        }
        let mut out = format!(
            "# {}\n\nInvestigation question: {}\n\n",
            markdown(text(&payload["workspace"]["metadata"], "title")),
            markdown(text(&payload["workspace"]["metadata"], "question"))
        );
        for key in [
            "investigations",
            "evidence",
            "correlations",
            "ontology",
            "discovery",
            "frontier",
            "reasoning",
            "entities",
            "links",
            "flows",
            "notes",
            "activity",
        ] {
            let serialized = serde_json::to_string_pretty(&payload["workspace"][key])?;
            let fence = "`".repeat(
                serialized
                    .split(|c| c != '`')
                    .map(str::len)
                    .max()
                    .unwrap_or(0)
                    .max(2)
                    + 1,
            );
            out.push_str(&format!(
                "## {key}\n\n{fence}json\n{serialized}\n{fence}\n\n"
            ));
        }
        out.push_str(&format!("Integrity: {}\n\nCorrelation leads preserve distinct entities. Source bytes remain in the case blobs directory.\n",payload["integrity"]["ok"]));
        Ok(out)
    }
}

fn markdown(value: &str) -> String {
    value
        .replace(['\r', '\n'], " ")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn same_page(first: &Value, second: &Value) -> bool {
    if !["url", "profile_url"].contains(&text(first, "type"))
        || !["url", "profile_url"].contains(&text(second, "type"))
    {
        return false;
    }
    let normalize = |value: &str| -> Option<String> {
        let mut url = url::Url::parse(value).ok()?;
        url.set_fragment(None);
        let path = url.path().trim_end_matches('/').to_owned();
        url.set_path(if path.is_empty() { "/" } else { &path });
        Some(url.to_string())
    };
    match (
        normalize(text(first, "value")),
        normalize(text(second, "value")),
    ) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}
