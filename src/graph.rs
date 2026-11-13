//! A disposable, source-attributed view of the ordinary workbench state.
//! No identity resolution, finding promotion, persistence, or collection happens here.
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or("")
}
fn rows<'a>(state: &'a Value, key: &str) -> impl Iterator<Item = &'a Value> {
    state[key].as_array().into_iter().flatten()
}
fn ids(value: &Value) -> BTreeSet<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Project the latest-500-report materialization, never analytical proposals.
/// Evidence/source pairs must intersect *within the same cited evidence record*;
/// membership in an unrelated valid collector run is not sufficient support.
pub fn project(state: &Value) -> Value {
    let allowed: BTreeMap<String, BTreeSet<String>> = rows(state, "evidence")
        .filter(|evidence| text(evidence, "collection_method") == "collector")
        .filter(|evidence| {
            matches!(
                text(evidence, "collector_status"),
                "ok" | "partial" | "not_found"
            )
        })
        .filter(|evidence| !text(evidence, "id").is_empty())
        .filter_map(|evidence| {
            let declared = ids(&evidence["source_ids"]);
            let sources: BTreeSet<String> = rows(evidence, "sources")
                .map(|source| text(source, "source_id"))
                .filter(|id| !id.is_empty() && declared.contains(*id))
                .map(str::to_owned)
                .collect();
            (!sources.is_empty()).then(|| (text(evidence, "id").to_owned(), sources))
        })
        .collect();
    let support = |record: &Value| -> (BTreeSet<String>, BTreeSet<String>) {
        let declared = ids(&record["source_ids"]);
        let mut evidence_ids = BTreeSet::new();
        let mut source_ids = BTreeSet::new();
        for id in ids(&record["evidence_ids"]) {
            if let Some(sources) = allowed.get(&id) {
                let intersection: BTreeSet<String> =
                    declared.intersection(sources).cloned().collect();
                if !intersection.is_empty() {
                    evidence_ids.insert(id);
                    source_ids.extend(intersection);
                }
            }
        }
        (evidence_ids, source_ids)
    };
    let mut nodes = BTreeMap::new();
    for entity in rows(state, "entities") {
        let (evidence_ids, source_ids) = support(entity);
        let id = text(entity, "id");
        let kind = text(entity, "type");
        let value = &entity["value"];
        if id.is_empty()
            || kind.is_empty()
            || !(value.is_string() || value.is_number())
            || evidence_ids.is_empty()
            || source_ids.is_empty()
        {
            continue;
        }
        // Analyst display labels can suggest an identity unsupported by a capture.
        let label = format!(
            "[{}] {}",
            kind,
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
        );
        nodes.insert(
            id.to_owned(),
            json!({"id":id,"type":kind,"value":value,"label":label,
            "evidence_ids":evidence_ids,"source_ids":source_ids}),
        );
    }
    let claims: BTreeMap<&str, &Value> = rows(&state["ontology"], "claims")
        .map(|claim| (text(claim, "id"), claim))
        .collect();
    let mut edges = BTreeMap::new();
    for link in rows(state, "links") {
        if text(link, "status") != "observed" {
            continue;
        }
        let id = text(link, "id");
        let source = text(link, "source_id");
        let target = text(link, "target_id");
        let relation = text(link, "relation");
        let (evidence_ids, source_ids) = support(link);
        if id.is_empty()
            || relation.is_empty()
            || !nodes.contains_key(source)
            || !nodes.contains_key(target)
            || evidence_ids.is_empty()
            || source_ids.is_empty()
        {
            continue;
        }
        let mut edge = json!({"id":id,"source_id":source,"target_id":target,"relation":relation,
            "evidence_ids":evidence_ids,"source_ids":source_ids,"status":"source_observation"});
        if let Some(hash) = id.strip_prefix("link_") {
            let claim_id = format!("claim_{hash}");
            if let Some(claim) = claims.get(claim_id.as_str())
                && matches!(text(claim, "status"), "accepted" | "rejected")
                && text(claim, "source_id") == source
                && text(claim, "target_id") == target
                && text(claim, "relation") == relation
            {
                edge["review"] = json!({"scope":"legacy_claim_review","claim_id":claim_id,
                    "status":claim["status"],"review_note":claim["review_note"],"reviewed_at":claim["reviewed_at"]});
            }
        }
        edges.insert(id.to_owned(), edge);
    }
    let requested = text(&state["graph_focus"], "entity_id");
    let focus_id = nodes
        .get_key_value(requested)
        .or_else(|| nodes.first_key_value())
        .map(|(id, _)| json!(id))
        .unwrap_or(Value::Null);
    json!({"nodes":nodes.into_values().collect::<Vec<_>>(),"edges":edges.into_values().collect::<Vec<_>>(),
    "focus_id":focus_id,"report_window":500,"limitations":[
        "Coverage: latest 500 reports in the workbench materialization, not the complete case history. Missing nodes or edges are not evidence of absence.",
        "Source relations are attributed observations, not verified identity, ownership, endorsement or independent corroboration. Typed node IDs match exactly; shared handles, names and domains never merge identities.",
        "Only collector evidence with ok, partial or not_found status and cited source membership is shown. Failed, manual and model assessments, analyst assertions, correlation leads and findings (even accepted findings) do not create graph edges.",
        "Legacy claim review is separate from trusted finding review; accepted or rejected claim reviews never confirm or rewrite a source observation."
    ]})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        json!({"evidence":[
            {"id":"ev_a","collection_method":"collector","collector_status":"ok","source_ids":["src_a"],"sources":[{"source_id":"src_a"}]},
            {"id":"ev_b","collection_method":"collector","collector_status":"partial","source_ids":["src_b"],"sources":[{"source_id":"src_b"}]},
            {"id":"ev_n","collection_method":"collector","collector_status":"not_found","source_ids":["src_n"],"sources":[{"source_id":"src_n"}]}
        ],"entities":[
            {"id":"username_exact","type":"username","value":"same","label":"Verified owner","evidence_ids":["ev_a"],"source_ids":["src_a"]},
            {"id":"profile_a","type":"profile_url","value":"https://a.example/same","evidence_ids":["ev_a"],"source_ids":["src_a"]},
            {"id":"profile_b","type":"profile_url","value":"https://b.example/same","evidence_ids":["ev_b"],"source_ids":["src_b"]},
            {"id":"absent_candidate","type":"username","value":"same","evidence_ids":["ev_n"],"source_ids":["src_n"]}
        ],"links":[
            {"id":"link_a","source_id":"username_exact","target_id":"profile_a","relation":"public_profile_at","status":"observed","evidence_ids":["ev_a"],"source_ids":["src_a"]},
            {"id":"link_b","source_id":"profile_b","target_id":"username_exact","relation":"declares_username","status":"observed","evidence_ids":["ev_b"],"source_ids":["src_b"]}
        ]})
    }
    #[test]
    fn candidates_are_distinct_exact_typed_nodes_with_both_edge_directions() {
        let state = fixture();
        let graph = project(&state);
        assert_eq!(graph["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(graph["edges"].as_array().unwrap().len(), 2);
        assert_eq!(graph["edges"][0]["source_id"], "username_exact");
        assert_eq!(graph["edges"][1]["target_id"], "username_exact");
        assert!(!graph.to_string().contains("Verified owner"));
        assert_eq!(graph["nodes"][3]["label"], "[username] same");
        assert_eq!(graph["report_window"], 500);
        assert!(graph["limitations"].to_string().contains("latest 500"));
        assert_eq!(state, fixture(), "projection must not mutate its input");
    }
    #[test]
    fn failed_manual_model_and_sourceless_evidence_never_support_graph_records() {
        for (method, status, sources, declared) in [
            (
                "collector",
                "failed",
                json!([{"source_id":"src_a"}]),
                json!(["src_a"]),
            ),
            (
                "analyst_supplied",
                "ok",
                json!([{"source_id":"src_a"}]),
                json!(["src_a"]),
            ),
            (
                "model_assessment_not_independent_evidence",
                "ok",
                json!([{"source_id":"src_a"}]),
                json!(["src_a"]),
            ),
            ("collector", "ok", json!([]), json!(["src_a"])),
            (
                "collector",
                "ok",
                json!([{"source_id":"wrong"}]),
                json!(["src_a"]),
            ),
            ("collector", "ok", json!([{"source_id":"src_a"}]), json!([])),
        ] {
            let mut state = fixture();
            state["evidence"][0]["collection_method"] = json!(method);
            state["evidence"][0]["collector_status"] = json!(status);
            state["evidence"][0]["sources"] = sources;
            state["evidence"][0]["source_ids"] = declared;
            let graph = project(&state);
            assert_eq!(graph["edges"], json!([]), "{method}/{status}");
            assert_eq!(graph["nodes"].as_array().unwrap().len(), 2);
        }
    }
    #[test]
    fn source_membership_must_belong_to_the_same_cited_evidence() {
        let mut state = fixture();
        state["links"][0]["source_ids"] = json!(["src_b"]);
        state["links"][0]["evidence_ids"] = json!(["ev_a"]);
        assert_eq!(project(&state)["edges"].as_array().unwrap().len(), 1);
        state["entities"][0]["source_ids"] = json!(["src_b"]);
        assert!(project(&state)["edges"].as_array().unwrap().is_empty());
        state = fixture();
        state["links"][0]["source_ids"] = json!(["src_a", "src_b", "unknown"]);
        state["links"][0]["evidence_ids"] = json!(["ev_a", "missing"]);
        let graph = project(&state);
        assert_eq!(graph["edges"][0]["source_ids"], json!(["src_a"]));
        assert_eq!(graph["edges"][0]["evidence_ids"], json!(["ev_a"]));
    }
    #[test]
    fn analytical_identity_proposals_never_generate_edges_or_merge_nodes() {
        let mut state = fixture();
        state["correlations"] =
            json!([{"id":"lead","status":"accepted","entity_ids":["profile_a","profile_b"]}]);
        state["investigations"] = json!([{"findings":[{"kind":"identity_sensitive","status":"accepted","source_id":"profile_a","target_id":"profile_b","relation":"same_person"}]}]);
        let mut analyst = state["links"][0].clone();
        analyst["id"] = json!("link_analyst");
        analyst["status"] = json!("analyst_assertion");
        state["links"].as_array_mut().unwrap().push(analyst.clone());
        analyst["id"] = json!("link_lead");
        analyst["status"] = json!("accepted");
        state["links"].as_array_mut().unwrap().push(analyst);
        let graph = project(&state);
        assert_eq!(graph["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(graph["edges"].as_array().unwrap().len(), 2);
        assert!(!graph["edges"].to_string().contains("same_person"));
    }
    #[test]
    fn legacy_claim_review_is_metadata_never_source_confirmation() {
        for status in ["accepted", "rejected"] {
            let mut state = fixture();
            state["ontology"] = json!({"claims":[{"id":"claim_a","source_id":"username_exact","target_id":"profile_a","relation":"public_profile_at","status":status,"review_note":"Legacy assessment","reviewed_at":"then"}]});
            let graph = project(&state);
            assert_eq!(graph["edges"][0]["status"], "source_observation");
            assert_eq!(graph["edges"][0]["review"]["status"], status);
            assert_eq!(graph["edges"][0]["review"]["scope"], "legacy_claim_review");
            assert!(graph["edges"][1].get("review").is_none());
        }
    }
    #[test]
    fn projection_order_focus_and_empty_or_malformed_states_are_deterministic() {
        let mut state = fixture();
        let graph = project(&state);
        for key in ["entities", "links", "evidence"] {
            state[key].as_array_mut().unwrap().reverse();
        }
        assert_eq!(project(&state), graph);
        state["graph_focus"] = json!({"id":"graph","entity_id":"profile_b","updated_at":"now"});
        assert_eq!(project(&state)["focus_id"], "profile_b");
        state["graph_focus"]["entity_id"] = json!("PROFILE_B");
        assert_eq!(project(&state)["focus_id"], "absent_candidate");
        for state in [
            Value::Null,
            json!({}),
            json!({"entities":[null,{}],"links":[{}],"evidence":[{}]}),
        ] {
            let graph = project(&state);
            assert_eq!(graph["nodes"], json!([]));
            assert_eq!(graph["edges"], json!([]));
            assert_eq!(graph["focus_id"], Value::Null);
        }
    }
}
