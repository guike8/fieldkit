//! A case-local vocabulary learned from preserved, structured collector output.
//!
//! The vocabulary describes what was collected. Claims retain pointers into the
//! immutable reports; learning a type never certifies its meaning or ownership.
use crate::{
    Workbench,
    model::{digest, entity_id, now, strings, text},
};
use anyhow::{Context, Result, bail, ensure};
use fs2::FileExt;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{File, OpenOptions},
    path::Path,
};

pub(crate) fn private_lock(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options
        .open(path)
        .context("Cannot open a private case lock")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && !std::fs::symlink_metadata(path)?.file_type().is_symlink(),
        "Case locks must be regular files, not symlinks"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        ensure!(metadata.nlink() == 1, "Case locks cannot be hard linked");
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 80
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn endpoint(value: &Value) -> Option<(String, Value)> {
    let kind = text(value, "type");
    let data = value.get("value")?;
    if !valid_name(kind) || !(data.is_string() || data.is_number()) || data.to_string().len() > 4096
    {
        return None;
    }
    Some((kind.to_owned(), data.clone()))
}

fn merge_strings(record: &mut Value, key: &str, values: impl IntoIterator<Item = String>) {
    let mut merged: BTreeSet<String> = strings(&record[key]).into_iter().collect();
    merged.extend(values);
    record[key] = json!(merged.into_iter().take(128).collect::<Vec<_>>());
}

fn concept_definition(kind: &str) -> String {
    match kind {
        "username" | "handle" => "A service-scoped public handle. Matching handles do not establish common ownership.".into(),
        "profile_url" => "The URL of a profile observed by a collector; source-specific verification applies.".into(),
        "url" => "A public URL explicitly returned in a preserved observation.".into(),
        "domain" => "A domain name observed in public records; registration and control are separate claims.".into(),
        "person" => "A person label appearing in a source or analyst record; identity remains a separate assessment.".into(),
        _ => format!("The {kind} entity type appeared in structured collector output. Its definition is provisional and case-local."),
    }
}

fn relation_definition(relation: &str) -> String {
    match relation {
        "links_to" => "The captured source explicitly links to the target. This does not establish endorsement or ownership.".into(),
        "public_profile_at" => "The collector reports the scoped handle at this public profile URL.".into(),
        "declares_username" => "A captured source declares this username; the declaration is attributed to that source.".into(),
        _ => format!("The collector emitted the explicit {relation} relationship. Review its source and collection limitations."),
    }
}

impl Workbench {
    /// Synchronize the vocabulary and claim index from immutable collector runs.
    /// This is idempotent: reviews and original captures are never overwritten.
    pub fn sync_ontology(&self) -> Result<Value> {
        let lock = private_lock(&self.store.path.join("ontology.lock"))?;
        lock.lock_exclusive()?;
        let reports = self.store.reports(500)?;
        let mut concepts: BTreeMap<String, Value> = self
            .store
            .list("ontology_concepts")?
            .into_iter()
            .map(|v| (text(&v, "id").to_owned(), v))
            .collect();
        let mut relations: BTreeMap<String, Value> = self
            .store
            .list("ontology_relations")?
            .into_iter()
            .map(|v| (text(&v, "id").to_owned(), v))
            .collect();
        let mut claims: BTreeMap<String, Value> = self
            .store
            .list("ontology_claims")?
            .into_iter()
            .map(|v| (text(&v, "id").to_owned(), v))
            .collect();
        let original_concepts = concepts.clone();
        let original_relations = relations.clone();
        let original_claims = claims.clone();
        let evidence_aliases: BTreeMap<String, String> = self
            .store
            .list("evidence")?
            .iter()
            .filter_map(|v| {
                let run = text(v, "run_id");
                let id = text(v, "id");
                (!run.is_empty() && !id.is_empty()).then(|| (run.to_owned(), id.to_owned()))
            })
            .collect();
        for report in reports {
            let run_id = text(&report, "run_id");
            if run_id.is_empty()
                || !matches!(text(&report, "status"), "ok" | "partial" | "not_found")
            {
                continue;
            }
            let evidence_id = evidence_aliases
                .get(run_id)
                .cloned()
                .unwrap_or_else(|| format!("evidence_{run_id}"));
            let report_hash = self.store.report_hash(run_id)?;
            let source_map: BTreeMap<String, Value> = report["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|s| !text(s, "source_id").is_empty())
                .map(|s| (text(s, "source_id").to_owned(), s.clone()))
                .collect();
            let observed_at = report["timing"]["completed_at"]
                .as_str()
                .or_else(|| report["started_at"].as_str())
                .unwrap_or("")
                .to_owned();
            let mut occurrences: Vec<(String, Value, Vec<String>, String)> = Vec::new();
            for (index, value) in report["entities"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if let Some((kind, data)) = endpoint(value) {
                    occurrences.push((
                        kind,
                        data,
                        strings(&value["source_ids"]),
                        format!("/entities/{index}"),
                    ));
                }
            }
            for (index, value) in report["relationships"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let relation = text(value, "relation");
                if !valid_name(relation) {
                    continue;
                }
                let source_key = if value.get("source").is_some() {
                    "source"
                } else {
                    "from"
                };
                let target_key = if value.get("target").is_some() {
                    "target"
                } else {
                    "to"
                };
                let (Some((source_type, source_value)), Some((target_type, target_value))) =
                    (endpoint(&value[source_key]), endpoint(&value[target_key]))
                else {
                    continue;
                };
                let source_id = entity_id(&source_type, &source_value);
                let target_id = entity_id(&target_type, &target_value);
                let refs: Vec<String> = strings(&value["source_ids"])
                    .into_iter()
                    .filter(|s| source_map.contains_key(s))
                    .collect();
                occurrences.push((
                    source_type.clone(),
                    source_value.clone(),
                    refs.clone(),
                    format!("/relationships/{index}/{source_key}"),
                ));
                occurrences.push((
                    target_type.clone(),
                    target_value.clone(),
                    refs.clone(),
                    format!("/relationships/{index}/{target_key}"),
                ));
                let relation_id = format!(
                    "ontology_relation_{}",
                    digest(
                        json!([source_type, relation, target_type])
                            .to_string()
                            .as_bytes()
                    )
                );
                if !relations.contains_key(&relation_id) && relations.len() >= 2000 {
                    continue;
                }
                let schema = relations.entry(relation_id.clone()).or_insert_with(|| json!({
                    "id":relation_id,"label":format!("{source_type} {relation} {target_type}"),
                    "relation":relation,"source_type":source_type,"target_type":target_type,
                    "definition":relation_definition(relation),"status":"observed_schema","origin":"structured_collector",
                    "evidence_ids":[],"source_ids":[],"created_at":now()
                }));
                merge_strings(schema, "evidence_ids", [evidence_id.clone()]);
                merge_strings(schema, "source_ids", refs.clone());
                let claim_id = format!(
                    "claim_{}",
                    digest(
                        json!([source_id, relation, target_id])
                            .to_string()
                            .as_bytes()
                    )
                );
                if !claims.contains_key(&claim_id) && claims.len() >= 4000 {
                    continue;
                }
                let source_hashes: Vec<Value> = refs.iter().filter_map(|s| {
                    let source = &source_map[s];
                    source.get("sha256").and_then(Value::as_str).map(|hash| json!({"source_id":s,"sha256":hash,"hash_scope":source.get("hash_scope").cloned().unwrap_or(json!("source_bytes"))}))
                }).collect();
                let anchor = json!({"evidence_id":evidence_id,"run_id":run_id,"report_sha256":report_hash,
                    "json_pointer":format!("/relationships/{index}"),"source_ids":refs,"source_hashes":source_hashes,
                    "collector_status":report["status"],"observed_at":observed_at,"scope":"structured_collector_report"});
                let claim = claims.entry(claim_id.clone()).or_insert_with(|| json!({
                    "id":claim_id,"label":format!("{} {} {}",source_value,relation,target_value),
                    "source_id":source_id,"target_id":target_id,"source_type":source_type,"target_type":target_type,
                    "source_value":source_value,"target_value":target_value,"relation":relation,"relation_id":relation_id,
                    "reason":"Explicit relationship in preserved collector output; inspect the source before interpreting it.",
                    "status":"observed","origin":"structured_collector","evidence_ids":[],"source_ids":[],"run_ids":[],"anchors":[],
                    "interpretation":"Observation attributed to its sources; no inferred identity or ownership.","created_at":now()
                }));
                merge_strings(claim, "evidence_ids", [evidence_id.clone()]);
                merge_strings(claim, "source_ids", refs);
                merge_strings(claim, "run_ids", [run_id.to_owned()]);
                let anchors = claim["anchors"]
                    .as_array_mut()
                    .expect("internal claim anchors");
                if !anchors.iter().any(|v| {
                    v["run_id"] == anchor["run_id"] && v["json_pointer"] == anchor["json_pointer"]
                }) {
                    if anchors.len() < 128 {
                        anchors.push(anchor);
                    } else {
                        claim["anchors_truncated"] = json!(true);
                    }
                }
            }
            for (kind, _value, refs, pointer) in occurrences {
                let concept_id = format!("ontology_concept_{kind}");
                if !concepts.contains_key(&concept_id) && concepts.len() >= 1000 {
                    continue;
                }
                let concept = concepts.entry(concept_id.clone()).or_insert_with(|| json!({
                    "id":concept_id,"type":kind,"label":kind,"definition":concept_definition(&kind),
                    "status":"observed_schema","origin":"structured_collector","evidence_ids":[],"source_ids":[],"examples":[],"created_at":now()
                }));
                merge_strings(concept, "evidence_ids", [evidence_id.clone()]);
                merge_strings(
                    concept,
                    "source_ids",
                    refs.into_iter().filter(|s| source_map.contains_key(s)),
                );
                let example = json!({"evidence_id":evidence_id,"run_id":run_id,"json_pointer":pointer,"report_sha256":report_hash});
                let examples = concept["examples"]
                    .as_array_mut()
                    .expect("internal concept examples");
                if examples.len() < 16
                    && !examples.iter().any(|v| {
                        v["run_id"] == example["run_id"]
                            && v["json_pointer"] == example["json_pointer"]
                    })
                {
                    examples.push(example);
                }
            }
        }
        let new_schema = concepts
            .keys()
            .filter(|key| !original_concepts.contains_key(*key))
            .count()
            + relations
                .keys()
                .filter(|key| !original_relations.contains_key(*key))
                .count();
        let mut changed = 0usize;
        for (kind, records, original) in [
            ("ontology_concepts", &concepts, &original_concepts),
            ("ontology_relations", &relations, &original_relations),
            ("ontology_claims", &claims, &original_claims),
        ] {
            for (id, value) in records {
                if original.get(id) != Some(value) {
                    self.store.put(kind, value)?;
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            let mut state = self
                .store
                .list("ontology_state")?
                .into_iter()
                .find(|v| v["id"] == "ontology_state")
                .unwrap_or_else(|| json!({"id":"ontology_state","version":0,"history":[]}));
            if new_schema > 0 {
                state["version"] = json!(state["version"].as_u64().unwrap_or(0) + 1);
                let version = state["version"].clone();
                let history = state["history"]
                    .as_array_mut()
                    .expect("internal ontology history");
                history.push(
                    json!({"version":version,"created_at":now(),"new_schema_items":new_schema}),
                );
                if history.len() > 100 {
                    history.remove(0);
                }
            }
            state["updated_at"] = json!(now());
            self.store.put("ontology_state", &state)?;
            self.store.event("ontology.updated","Ontology learned from preserved evidence",json!({"version":state["version"],"changed_records":changed,"new_schema_items":new_schema}))?;
        }
        self.ontology_snapshot()
    }

    pub fn ontology_snapshot(&self) -> Result<Value> {
        let state = self
            .store
            .list("ontology_state")?
            .into_iter()
            .find(|v| v["id"] == "ontology_state")
            .unwrap_or_else(|| json!({"version":0,"history":[]}));
        let mut claims = self.store.list("ontology_claims")?;
        let reviews: BTreeMap<String, Value> = self
            .store
            .list("ontology_reviews")?
            .into_iter()
            .map(|v| (text(&v, "id").to_owned(), v))
            .collect();
        for claim in &mut claims {
            if let Some(review) = reviews.get(text(claim, "id")) {
                claim["status"] = review["status"].clone();
                claim["review_note"] = review["review_note"].clone();
                claim["reviewed_at"] = review["reviewed_at"].clone();
            }
        }
        Ok(
            json!({"version":state["version"],"history":state["history"],"concepts":self.store.list("ontology_concepts")?,
            "relations":self.store.list("ontology_relations")?,"claims":claims,
            "scope":"A case-local vocabulary and source-attributed observations. Reviews do not merge identities or certify source claims.",
            "limits":{"reports_per_sync":500,"concepts":1000,"relations":2000,"claims":4000,"anchors_per_claim":128},
            "provider":"fieldkit_structured_evidence"}),
        )
    }

    pub fn review_claim(&self, id: &str, decision: &str, note: &str) -> Result<Value> {
        if !matches!(decision, "accepted" | "rejected") {
            bail!("Claim decision must be accepted or rejected");
        }
        if note.trim().is_empty() || note.len() > 4000 {
            bail!("Record a rationale of 1–4000 characters");
        }
        let mut claim = self.store.get("ontology_claims", id)?;
        if claim["evidence_ids"].as_array().is_none_or(Vec::is_empty)
            || claim["anchors"].as_array().is_none_or(Vec::is_empty)
        {
            bail!("A claim needs preserved supporting evidence before review");
        }
        for evidence_id in strings(&claim["evidence_ids"]) {
            if !self.store.has_evidence(&evidence_id)? {
                bail!("Claim supporting evidence no longer exists in this case");
            }
        }
        for anchor in claim["anchors"].as_array().expect("checked anchors") {
            if self.store.report_hash(text(anchor, "run_id"))? != text(anchor, "report_sha256") {
                bail!("Claim report anchor does not match the preserved report");
            }
        }
        let reviewed_at = now();
        let review =
            json!({"id":id,"status":decision,"review_note":note.trim(),"reviewed_at":reviewed_at});
        self.store.put("ontology_reviews", &review)?;
        self.store.event("claim.reviewed","Source-attributed claim reviewed",json!({"claim_id":id,"decision":decision,"note":note.trim(),"evidence_ids":claim["evidence_ids"]}))?;
        claim["status"] = json!(decision);
        claim["review_note"] = json!(note.trim());
        claim["reviewed_at"] = json!(reviewed_at);
        Ok(claim)
    }
}
