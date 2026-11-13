//! Agent-neutral Telegram plans and immutable, locally reviewed analytical proposals.
use crate::{
    Job, Workbench, collectors,
    model::{digest, id, now, text},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

fn bounded(value: &str, maximum: usize) -> Result<()> {
    ensure!(
        !value.trim().is_empty() && value.len() <= maximum && !value.contains('\0'),
        "Supply 1–{maximum} bytes of text"
    );
    Ok(())
}

impl Workbench {
    fn investigation_lock(&self, identifier: &str) -> Result<std::fs::File> {
        self.store.lock(&format!(
            "investigation-{}.lock",
            digest(identifier.as_bytes())
        ))
    }

    pub fn create_investigation(&self, question: &str, searches: Vec<Job>) -> Result<Value> {
        bounded(question, 4000)?;
        ensure!(searches.len() <= 5, "Plan at most five archive searches");
        let steps = searches.into_iter().map(|job| {
            ensure!(job.command == "telegram-search", "Initial steps must be Telegram archive searches");
            collectors::validate_job(&job)?;
            Ok(json!({"id":id("step"),"command":job.command,"target":job.target,"options":job.options,"status":"pending","run_id":null,"error":null}))
        }).collect::<Result<Vec<_>>>()?;
        let record = json!({"id":id("investigation"),"title":question.chars().take(160).collect::<String>(),"question":question,"platform":"telegram","steps":steps,"created_at":now(),"coverage":"Historical third-party index leads; selected public-post captures may be unavailable or changed. No complete coverage or identity attribution.","review_policy":"All analytical findings require local human review; proposals are not source evidence"});
        self.store.put("investigations", &record)?;
        self.store.event(
            "investigation.created",
            "Telegram investigation planned",
            json!({"id":record["id"]}),
        )?;
        self.investigation(text(&record, "id"))
    }

    pub fn investigation(&self, identifier: &str) -> Result<Value> {
        let mut record = self.store.get("investigations", identifier)?;
        let reviews = self.store.list("investigation_reviews")?;
        let mut findings = Vec::new();
        for mut finding in self.store.list("investigation_findings")? {
            if finding["investigation_id"] != identifier {
                continue;
            }
            let hash = digest(serde_json::to_string(&finding)?.as_bytes());
            finding["status"] = json!("pending_review");
            if let Some(review) = reviews
                .iter()
                .find(|review| review["id"] == finding["id"] && review["proposal_sha256"] == hash)
            {
                // A review never changes the preserved proposal or its citations.
                finding["status"] = review["decision"].clone();
                finding["review"] = review.clone();
            }
            findings.push(finding);
        }
        record["summary"] = json!(
            findings
                .iter()
                .filter(|f| f["status"] == "accepted")
                .collect::<Vec<_>>()
        );
        record["findings"] = json!(findings);
        Ok(record)
    }

    pub fn investigations(&self) -> Result<Vec<Value>> {
        self.store
            .list("investigations")?
            .iter()
            .map(|v| self.investigation(text(v, "id")))
            .collect()
    }

    pub fn run_investigation_step(&self, identifier: &str, step_id: &str) -> Result<Value> {
        self.run_investigation_step_with(identifier, step_id, |job| self.collect_preserved(job))
    }

    fn run_investigation_step_with(
        &self,
        identifier: &str,
        step_id: &str,
        collect: impl FnOnce(&Job) -> Result<Value>,
    ) -> Result<Value> {
        let _lock = self.investigation_lock(identifier)?;
        let mut record = self.store.get("investigations", identifier)?;
        let step = record["steps"]
            .as_array_mut()
            .context("Malformed investigation")?
            .iter_mut()
            .find(|v| v["id"] == step_id)
            .context("Unknown investigation step")?;
        ensure!(
            step["status"] == "pending",
            "Step already attempted; inspect its report. Interrupted/failed steps are never automatically retried"
        );
        let job: Job = serde_json::from_value(
            json!({"command":step["command"],"target":step["target"],"options":step["options"]}),
        )?;
        ensure!(
            ["telegram-search", "telegram-post"].contains(&job.command.as_str()),
            "Unsupported investigation collector"
        );
        collectors::validate_job(&job)?;
        step["status"] = json!("running");
        step["started_at"] = json!(now());
        self.store.put("investigations", &record)?;
        let result = collect(&job);
        let step = record["steps"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|v| v["id"] == step_id)
            .unwrap();
        match &result {
            Ok(report) => {
                step["run_id"] = report["run_id"].clone();
                step["status"] = report["status"].clone();
                step["error"] = report["errors"].clone();
            }
            Err(error) => {
                step["status"] = json!("failed");
                step["error"] = json!(error.to_string());
            }
        }
        step["completed_at"] = json!(now());
        // Bind the preserved report before optional indexing can fail.
        self.store.put("investigations", &record)?;
        if result.is_ok()
            && let Err(error) = self.sync_ontology()
        {
            let step = record["steps"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|v| v["id"] == step_id)
                .unwrap();
            step["indexing_error"] = json!(error.to_string());
            self.store.put("investigations", &record)?;
        }
        self.store.event(
            "investigation.step_finished",
            "Investigation collection attempted",
            json!({"id":identifier,"step_id":step_id}),
        )?;
        result
    }

    pub fn investigation_evidence(&self, identifier: &str, step_id: &str) -> Result<Value> {
        let record = self.store.get("investigations", identifier)?;
        let step = record["steps"]
            .as_array()
            .context("Malformed investigation")?
            .iter()
            .find(|v| v["id"] == step_id)
            .context("Unknown investigation step")?;
        let run = step["run_id"]
            .as_str()
            .context("Step has no preserved report")?;
        self.store.report(run)
    }

    pub fn select_investigation_post(
        &self,
        identifier: &str,
        search_step: &str,
        index: usize,
    ) -> Result<Value> {
        let _lock = self.investigation_lock(identifier)?;
        let report = self.investigation_evidence(identifier, search_step)?;
        ensure!(
            report["command"] == "telegram-search",
            "Select a result from an archive search"
        );
        let message = report["data"]["messages"]
            .as_array()
            .and_then(|rows| rows.get(index))
            .context("Unknown archive result")?;
        let target = message["url"]
            .as_str()
            .context("Result has no supported public Telegram post URL")?;
        let job = Job {
            command: "telegram-post".into(),
            target: Some(target.into()),
            options: json!({}),
        };
        collectors::validate_job(&job)?;
        let mut record = self.store.get("investigations", identifier)?;
        let steps = record["steps"]
            .as_array_mut()
            .context("Malformed investigation")?;
        ensure!(
            steps.len() < 10,
            "Investigation accepts at most ten collection steps"
        );
        ensure!(
            !steps
                .iter()
                .any(|s| s["command"] == "telegram-post" && s["target"] == target),
            "Post already selected"
        );
        let step = json!({"id":id("step"),"command":job.command,"target":target,"options":{},"status":"pending","run_id":null,"error":null,"selected_from":{"step_id":search_step,"index":index,"run_id":report["run_id"]}});
        steps.push(step.clone());
        self.store.put("investigations", &record)?;
        self.store.event(
            "investigation.post_selected",
            "Public post capture planned",
            json!({"id":identifier,"step":step}),
        )?;
        Ok(step)
    }

    fn investigation_citation(&self, identifier: &str, citation: &Value) -> Result<Value> {
        let step_id = text(citation, "step_id");
        let pointer = text(citation, "pointer");
        let report = self.investigation_evidence(identifier, step_id)?;
        ensure!(
            ["ok", "partial"].contains(&text(&report, "status")),
            "Cite an observed result, not a failed/absent capture"
        );
        let observation = if report["command"] == "telegram-search" {
            let index = pointer
                .strip_prefix("/data/messages/")
                .and_then(|s| s.parse::<usize>().ok())
                .context("Cite /data/messages/INDEX for archive observations")?;
            ensure!(
                pointer == format!("/data/messages/{index}"),
                "Use a canonical observation pointer"
            );
            report["data"]["messages"]
                .as_array()
                .and_then(|v| v.get(index))
                .context("Unknown archive observation")?
        } else {
            ensure!(
                report["command"] == "telegram-post" && pointer == "/data",
                "Cite /data for a public post capture"
            );
            &report["data"]
        };
        let sources = report["sources"]
            .as_array()
            .context("Capture has no sources")?;
        ensure!(!sources.is_empty(), "Capture has no source bytes");
        for source in sources {
            self.store.source_bytes(text(source, "sha256"))?;
        }
        Ok(
            json!({"step_id":step_id,"run_id":report["run_id"],"evidence_id":format!("evidence_{}",text(&report,"run_id")),"report_sha256":self.store.report_hash(text(&report,"run_id"))?,"pointer":pointer,"observation_sha256":digest(serde_json::to_string(observation)?.as_bytes()),"sources":sources,"scope":if report["command"]=="telegram-search"{"provider_archive_observation_not_direct_verification"}else{"public_page_capture_not_identity_attribution"}}),
        )
    }

    pub fn propose_investigation_finding(
        &self,
        identifier: &str,
        content: &str,
        kind: &str,
        citations: Vec<Value>,
    ) -> Result<Value> {
        bounded(content, 12000)?;
        ensure!(
            ["observation", "hypothesis", "identity_sensitive"].contains(&kind),
            "Unknown finding kind"
        );
        ensure!(
            !citations.is_empty() && citations.len() <= 20,
            "Supply 1–20 observation citations"
        );
        let _lock = self.investigation_lock(identifier)?;
        self.store.get("investigations", identifier)?;
        ensure!(
            self.store
                .list("investigation_findings")?
                .iter()
                .filter(|f| f["investigation_id"] == identifier)
                .count()
                < 100,
            "Investigation finding limit reached"
        );
        let citations = citations
            .iter()
            .map(|c| self.investigation_citation(identifier, c))
            .collect::<Result<Vec<_>>>()?;
        let evidence_ids = citations
            .iter()
            .map(|c| text(c, "evidence_id").to_owned())
            .collect::<std::collections::BTreeSet<_>>();
        let finding = json!({"id":id("finding"),"investigation_id":identifier,"content":content,"kind":kind,"citations":citations,"evidence_ids":evidence_ids,"created_at":now(),"origin":"analytical_proposal","status":"pending_review","source_evidence":false});
        self.store.put("investigation_findings", &finding)?;
        self.store.event(
            "investigation.finding_proposed",
            "Cited analytical proposal awaits human review",
            json!({"id":finding["id"],"investigation_id":identifier}),
        )?;
        Ok(finding)
    }

    /// Trusted local TUI entry point. Deliberately absent from HTTP operations and MCP.
    /// This is not isolation from an agent with shell/database access to the case.
    pub fn review_investigation_finding(
        &self,
        identifier: &str,
        finding_id: &str,
        decision: &str,
        note: &str,
    ) -> Result<Value> {
        ensure!(
            ["accepted", "rejected"].contains(&decision),
            "Decision must be accepted or rejected"
        );
        bounded(note, 4000)?;
        let _lock = self.investigation_lock(identifier)?;
        let finding = self.store.get("investigation_findings", finding_id)?;
        ensure!(
            finding["investigation_id"] == identifier,
            "Finding belongs to another investigation"
        );
        let citations = finding["citations"]
            .as_array()
            .context("Missing citations")?;
        ensure!(
            !citations.is_empty() && citations.len() <= 20,
            "Finding requires preserved citations"
        );
        for citation in citations {
            ensure!(
                self.investigation_citation(identifier, citation)? == *citation,
                "Citation changed; review cannot be applied"
            );
        }
        let review = json!({"id":finding_id,"investigation_id":identifier,"proposal_sha256":digest(serde_json::to_string(&finding)?.as_bytes()),"decision":decision,"note":note,"reviewed_at":now(),"authority":"local_human_review"});
        self.store.put("investigation_reviews", &review)?;
        self.store.event(
            "investigation.finding_reviewed",
            "Local human finding review recorded",
            review.clone(),
        )?;
        Ok(review)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Collected, Config};
    use std::collections::BTreeMap;

    #[test]
    fn preserved_step_survives_ontology_indexing_failure() {
        let directory = tempfile::tempdir().unwrap();
        let w = Workbench::new(directory.path().join("case"), Config::default()).unwrap();
        let plan = w
            .create_investigation(
                "A topic question",
                vec![Job {
                    command: "telegram-search".into(),
                    target: Some("research".into()),
                    options: json!({}),
                }],
            )
            .unwrap();
        let identifier = text(&plan, "id");
        let step_id = text(&plan["steps"][0], "id");
        let saved = json!({"run_id":"run_preserved","command":"telegram-search","status":"ok","sources":[],"data":{"messages":[]},"entities":[],"relationships":[],"timing":{}});
        w.store
            .save_report(&Collected {
                report: saved.clone(),
                blobs: BTreeMap::new(),
            })
            .unwrap();
        w.store.save_report(&Collected {report:json!({"run_id":"run_bad","command":"doctor","status":"ok","sources":[],"data":{},"timing":{}}),blobs:BTreeMap::new()}).unwrap();
        w.store
            .connection()
            .unwrap()
            .execute(
                "UPDATE runs SET report_sha256='altered' WHERE run_id='run_bad'",
                [],
            )
            .unwrap();
        let returned = w
            .run_investigation_step_with(identifier, step_id, |_| Ok(saved.clone()))
            .unwrap();
        assert_eq!(returned, saved);
        let plan = w.investigation(identifier).unwrap();
        assert_eq!(plan["steps"][0]["run_id"], "run_preserved");
        assert_eq!(plan["steps"][0]["status"], "ok");
        assert!(
            plan["steps"][0]["indexing_error"]
                .as_str()
                .unwrap()
                .contains("altered")
        );
        assert_eq!(
            w.investigation_evidence(identifier, step_id).unwrap(),
            saved
        );
    }
}
