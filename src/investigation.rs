//! Agent-neutral investigation plans and immutable, locally reviewed analytical proposals.
use crate::{
    Job, Workbench, collectors,
    model::{digest, id, now, text},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

pub const RECON_COLLECTORS: &[&str] = &[
    "username",
    "profile",
    "dns",
    "rdap",
    "ct",
    "tls",
    "http",
    "archive",
    "telegram-profile",
    "telegram-messages",
    "telegram-post",
    "evm-address",
    "evm-transaction",
];

fn recon_mode(mode: &str) -> bool {
    ["person", "organization", "infrastructure", "crypto"].contains(&mode)
}

fn crypto_collector(command: &str) -> bool {
    ["evm-address", "evm-transaction"].contains(&command)
}

fn mode_allows(mode: &str, command: &str) -> bool {
    if mode == "crypto" {
        crypto_collector(command)
    } else {
        !crypto_collector(command) && external_collector(command)
    }
}

fn crypto_lead_pointer(pointer: &str) -> bool {
    if pointer == "/data/address/target" {
        return true;
    }
    let parts = pointer.split('/').collect::<Vec<_>>();
    parts.len() == 5
        && parts[1] == "data"
        && parts[2] == "transactions"
        && parts[3]
            .parse::<usize>()
            .is_ok_and(|index| parts[3] == index.to_string())
        && ["target", "from_target", "to_target"].contains(&parts[4])
}

fn external_collector(command: &str) -> bool {
    RECON_COLLECTORS.contains(&command) || command == "telegram-search"
}

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

    pub fn create_recon_investigation(
        &self,
        question: &str,
        mode: &str,
        seeds: Vec<String>,
    ) -> Result<Value> {
        bounded(question, 4000)?;
        ensure!(recon_mode(mode), "Unknown recon mode");
        ensure!(
            !seeds.is_empty() && seeds.len() <= 5,
            "Supply 1–5 user-scoped seeds"
        );
        for seed in &seeds {
            bounded(seed, 2048)?;
            ensure!(
                !seed.chars().any(char::is_control),
                "Seeds cannot contain control characters"
            );
            if mode == "crypto" {
                collectors::validate_crypto_target(seed)?;
            }
        }
        let record = json!({"id":id("investigation"),"title":question.chars().take(160).collect::<String>(),"question":question,"mode":mode,"platform":"public_sources","seeds":seeds,"steps":[],"created_at":now(),"coverage":if mode == "crypto" { "Bounded Ethereum mainnet outer transaction observations from Blockscout; one page, no retries. Provider availability is unverified and public instance APIs may be retired or denied. No complete history, token/internal transfers, finality, identity, ownership or proof of funds transferred." } else { "Bounded public-source recon, not comprehensive surveillance. Source observations and candidate associations do not establish identity or ownership." },"review_policy":"All analytical findings require local human review; proposals are not source evidence"});
        self.store.put("investigations", &record)?;
        self.store.event(
            "investigation.created",
            "Recon investigation scoped",
            json!({"id":record["id"],"mode":mode}),
        )?;
        self.investigation(text(&record, "id"))
    }

    fn recon_grounding(
        &self,
        identifier: &str,
        record: &Value,
        target: &str,
        lead: Option<&Value>,
    ) -> Result<Value> {
        if let Some(lead) = lead {
            let pointer = text(lead, "pointer");
            bounded(pointer, 128)?;
            ensure!(
                pointer.starts_with("/data/"),
                "Lead pointer must select an exact observed data string"
            );
            let report = self.investigation_evidence(identifier, text(lead, "step_id"))?;
            ensure!(
                mode_allows(text(record, "mode"), text(&report, "command")),
                "Capture is not allowed in this investigation mode"
            );
            if record["mode"] == "crypto" {
                ensure!(
                    crypto_lead_pointer(pointer),
                    "Crypto leads must select normalized source-bound hash targets, never provider labels or synthesized links"
                );
            }
            ensure!(
                external_collector(text(&report, "command"))
                    && ["ok", "partial"].contains(&text(&report, "status")),
                "Lead must come from an external observed capture"
            );
            let observed = report
                .pointer(pointer)
                .and_then(Value::as_str)
                .context("Lead must select a string, not a summary/object")?;
            ensure!(
                observed == target,
                "Target must equal the cited observation exactly"
            );
            let sources = report["sources"]
                .as_array()
                .context("Lead has no sources")?;
            ensure!(!sources.is_empty(), "Lead has no source bytes");
            for source in sources {
                self.store.source_bytes(text(source, "sha256"))?;
            }
            Ok(
                json!({"origin":"observed_lead_not_identity","step_id":lead["step_id"],"run_id":report["run_id"],"pointer":pointer,"report_sha256":self.store.report_hash(text(&report,"run_id"))?,"observation_sha256":digest(serde_json::to_string(observed)?.as_bytes()),"sources":sources}),
            )
        } else {
            ensure!(
                record["seeds"]
                    .as_array()
                    .is_some_and(|seeds| seeds.iter().any(|s| s == target)),
                "New targets require an exact observed lead in this investigation; do not invent scope"
            );
            Ok(json!({"origin":"provided_seed","value":target}))
        }
    }

    /// The external agent chooses the next job; Fieldkit checks its literal grounding.
    /// A source link is a lead, not authorization or an identity conclusion.
    pub fn plan_investigation_step(
        &self,
        identifier: &str,
        job: Job,
        rationale: &str,
        lead: Option<Value>,
    ) -> Result<Value> {
        bounded(rationale, 4000)?;
        ensure!(
            RECON_COLLECTORS.contains(&job.command.as_str()),
            "Unsupported recon collector"
        );
        collectors::validate_job(&job)?;
        let target = job.target.as_deref().context("Supply a scoped target")?;
        let _lock = self.investigation_lock(identifier)?;
        let mut record = self.store.get("investigations", identifier)?;
        ensure!(
            recon_mode(text(&record, "mode")),
            "Plan recon steps in a recon investigation"
        );
        ensure!(
            mode_allows(text(&record, "mode"), &job.command),
            "Collector is not allowed in this investigation mode"
        );
        let crypto = record["mode"] == "crypto";
        let grounding = self.recon_grounding(identifier, &record, target, lead.as_ref())?;
        let steps = record["steps"]
            .as_array_mut()
            .context("Malformed investigation")?;
        ensure!(
            steps.len() < 10,
            "Investigation accepts at most ten collection steps"
        );
        ensure!(
            !steps.iter().any(|s| s["command"] == job.command
                && if crypto {
                    text(s, "target").eq_ignore_ascii_case(target)
                } else {
                    s["target"] == target && s["options"] == job.options
                }),
            "Job already planned or attempted; no automatic retries"
        );
        let step = json!({"id":id("step"),"command":job.command,"target":target,"options":job.options,"status":"pending","run_id":null,"error":null,"rationale":rationale,"grounding":grounding,"planned_at":now()});
        steps.push(step.clone());
        self.store.put("investigations", &record)?;
        self.store.event(
            "investigation.step_planned",
            "Agent planned a grounded recon step",
            json!({"id":identifier,"step":step}),
        )?;
        Ok(step)
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
            .as_array()
            .context("Malformed investigation")?
            .iter()
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
            external_collector(&job.command),
            "Unsupported investigation collector"
        );
        ensure!(
            mode_allows(text(&record, "mode"), &job.command),
            "Collector is not allowed in this investigation mode"
        );
        collectors::validate_job(&job)?;
        if recon_mode(text(&record, "mode")) {
            let grounding = &step["grounding"];
            let lead = (grounding["origin"] == "observed_lead_not_identity").then_some(grounding);
            ensure!(
                self.recon_grounding(
                    identifier,
                    &record,
                    job.target.as_deref().unwrap_or(""),
                    lead
                )? == *grounding,
                "Step grounding changed; inspect evidence before replanning"
            );
        }
        let step = record["steps"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|step| step["id"] == step_id)
            .unwrap();
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

    /// Read a bounded, original UTF-8 source slice belonging to this investigation.
    /// This never collects, writes case state, or accepts a filesystem path.
    pub fn investigation_source(
        &self,
        identifier: &str,
        step_id: &str,
        source_id: &str,
        byte_start: usize,
        max_bytes: usize,
    ) -> Result<Value> {
        bounded(source_id, 256)?;
        ensure!(
            (1..=16384).contains(&max_bytes),
            "Read at most 16384 bytes per request"
        );
        // report() and source_bytes() verify the preserved report and blob hashes.
        let report = self.investigation_evidence(identifier, step_id)?;
        ensure!(
            external_collector(text(&report, "command")),
            "Read sources from external investigation captures only"
        );
        let source = report["sources"]
            .as_array()
            .context("Capture has no sources")?
            .iter()
            .find(|source| source["source_id"] == source_id)
            .context("Source does not belong to this capture")?;
        let bytes = self.store.source_bytes(text(source, "sha256"))?;
        let original = std::str::from_utf8(&bytes).context("Source is not valid UTF-8")?;
        ensure!(
            original.is_char_boundary(byte_start),
            "byte_start is out of bounds or splits a UTF-8 character"
        );
        let mut byte_end = byte_start.saturating_add(max_bytes).min(bytes.len());
        while !original.is_char_boundary(byte_end) {
            byte_end -= 1;
        }
        ensure!(
            byte_end > byte_start || byte_start == bytes.len(),
            "max_bytes is too small for the next UTF-8 character"
        );
        Ok(json!({
            "step_id":step_id,"run_id":report["run_id"],"source_id":source_id,
            "report_sha256":self.store.report_hash(text(&report,"run_id"))?,
            "source_sha256":source["sha256"],"media_type":source["media_type"],
            "encoding":"utf-8","byte_start":byte_start,"byte_end":byte_end,
            "total_bytes":bytes.len(),"text":&original[byte_start..byte_end],
            "truncated":byte_end < bytes.len(),
            "next_byte_start":(byte_end < bytes.len()).then_some(byte_end),
            "untrusted_content":true
        }))
    }

    /// Attach only a preserved external capture from this case; never analyst/model output.
    pub fn attach_investigation_capture(&self, identifier: &str, run_id: &str) -> Result<Value> {
        bounded(run_id, 256)?;
        let _lock = self.investigation_lock(identifier)?;
        let mut record = self.store.get("investigations", identifier)?;
        let report = self.store.report(run_id)?;
        ensure!(
            external_collector(text(&report, "command")),
            "Only preserved external recon captures can be attached"
        );
        ensure!(
            mode_allows(text(&record, "mode"), text(&report, "command")),
            "Capture is not allowed in this investigation mode"
        );
        if record["mode"] == "crypto" {
            // Attachment has no explicit lead argument: it may only attach an original
            // scoped seed. Further pivots must use plan + exact same-investigation lead.
            self.recon_grounding(identifier, &record, text(&report, "target"), None)?;
            collectors::validate_job(&Job {
                command: text(&report, "command").into(),
                target: Some(text(&report, "target").into()),
                options: json!({}),
            })?;
            ensure!(
                !record["steps"]
                    .as_array()
                    .context("Malformed investigation")?
                    .iter()
                    .any(|s| s["command"] == report["command"]
                        && text(s, "target").eq_ignore_ascii_case(text(&report, "target"))),
                "Job already planned or attempted; no retries through attachments"
            );
        }
        ensure!(
            ["ok", "partial"].contains(&text(&report, "status")),
            "Attach an observed result, not a failed/absent capture"
        );
        let sources = report["sources"]
            .as_array()
            .context("Capture has no sources")?;
        ensure!(!sources.is_empty(), "Capture has no source bytes");
        for source in sources {
            self.store.source_bytes(text(source, "sha256"))?;
        }
        let steps = record["steps"]
            .as_array_mut()
            .context("Malformed investigation")?;
        ensure!(
            steps.len() < 10,
            "Investigation accepts at most ten collection steps"
        );
        ensure!(
            !steps.iter().any(|s| s["run_id"] == run_id),
            "Capture already attached"
        );
        let step = json!({"id":id("step"),"command":report["command"],"target":report["target"],"options":{},"status":report["status"],"run_id":run_id,"error":report["errors"],"attached_at":now(),"origin":"preserved_case_capture","report_sha256":self.store.report_hash(run_id)?});
        steps.push(step.clone());
        if record["mode"] != "crypto" {
            record["coverage"] = json!(
                "Bounded preserved external captures; archive results are historical third-party leads and public pages may be unavailable or changed. No complete coverage or identity attribution."
            );
        }
        self.store.put("investigations", &record)?;
        self.store.event(
            "investigation.capture_attached",
            "Preserved external capture attached",
            json!({"id":identifier,"step":step}),
        )?;
        Ok(step)
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
            ["telegram-search", "telegram-messages"].contains(&text(&report, "command"))
                && ["ok", "partial"].contains(&text(&report, "status")),
            "Select a result from an observed archive search or public preview"
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
        ensure!(
            mode_allows(text(&record, "mode"), &job.command),
            "Collector is not allowed in this investigation mode"
        );
        let grounding = if recon_mode(text(&record, "mode")) {
            Some(self.recon_grounding(
                identifier,
                &record,
                target,
                Some(
                    &json!({"step_id":search_step,"pointer":format!("/data/messages/{index}/url")}),
                ),
            )?)
        } else {
            None
        };
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
        let mut step = json!({"id":id("step"),"command":job.command,"target":target,"options":{},"status":"pending","run_id":null,"error":null,"selected_from":{"step_id":search_step,"index":index,"run_id":report["run_id"]}});
        if let Some(grounding) = grounding {
            step["grounding"] = grounding;
        }
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
        let byte_range =
            if citation.get("byte_start").is_some() || citation.get("byte_end").is_some() {
                ensure!(
                    citation.get("pointer").is_none(),
                    "Do not mix JSON pointers and original-body byte ranges"
                );
                bounded(text(citation, "source_id"), 256)?;
                let start = citation["byte_start"]
                    .as_u64()
                    .context("Supply paired unsigned integer byte_start and byte_end")?;
                let end = citation["byte_end"]
                    .as_u64()
                    .context("Supply paired unsigned integer byte_start and byte_end")?;
                ensure!(
                    start < end && end - start <= 16384,
                    "Supply a nonempty half-open byte range of at most 16384 bytes"
                );
                Some((usize::try_from(start)?, usize::try_from(end)?))
            } else {
                bounded(pointer, 128)?;
                None
            };
        let report = self.investigation_evidence(identifier, step_id)?;
        ensure!(
            ["ok", "partial"].contains(&text(&report, "status")),
            "Cite an observed result, not a failed/absent capture"
        );
        let raw_observation;
        let observation = if byte_range.is_some()
            || (report["command"] == "http" && citation["source_id"].is_string())
        {
            ensure!(
                report["command"] == "http",
                "Byte ranges require an HTTP capture"
            );
            let source_id = text(citation, "source_id");
            bounded(source_id, 256)?;
            let source = report["sources"]
                .as_array()
                .context("Capture has no sources")?
                .iter()
                .find(|source| source["source_id"] == source_id)
                .context("Source does not belong to this capture")?;
            let media_type = text(source, "media_type")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if let Some((start, end)) = byte_range {
                ensure!(
                    source["kind"] == "http"
                        && source["http_status"]
                            .as_u64()
                            .is_some_and(|s| (200..300).contains(&s)),
                    "Byte ranges require a retained successful HTTP 2xx source"
                );
                ensure!(
                    ["text/html", "application/xhtml+xml", "text/plain"]
                        .contains(&media_type.as_str()),
                    "Byte ranges require an explicit HTML or plain-text source media type"
                );
                let bytes = self.store.source_bytes(text(source, "sha256"))?;
                let original = std::str::from_utf8(&bytes).context("Source is not valid UTF-8")?;
                let slice = original
                    .get(start..end)
                    .context("Byte range is out of bounds or splits a UTF-8 character")?;
                raw_observation = json!(slice);
                &raw_observation
            } else {
                ensure!(
                    media_type == "application/json"
                        || (media_type.starts_with("application/")
                            && media_type.ends_with("+json")),
                    "Cite a retained JSON response source"
                );
                raw_observation = serde_json::from_slice::<Value>(
                    &self.store.source_bytes(text(source, "sha256"))?,
                )
                .context("Source is not valid JSON")?;
                ensure!(pointer.starts_with('/'), "Supply a JSON source pointer");
                raw_observation
                    .pointer(pointer)
                    .context("Unknown JSON source observation")?
            }
        } else if ["telegram-search", "telegram-messages"].contains(&text(&report, "command")) {
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
        } else if report["command"] == "telegram-post" {
            ensure!(pointer == "/data", "Cite /data for a public post capture");
            &report["data"]
        } else {
            ensure!(
                external_collector(text(&report, "command")),
                "Cite external source observations only"
            );
            ensure!(
                pointer == "/data" || pointer.starts_with("/data/"),
                "Cite /data or an exact collector data pointer; raw HTTP JSON requires source_id"
            );
            report
                .pointer(pointer)
                .context("Unknown collector observation")?
        };
        let sources = report["sources"]
            .as_array()
            .context("Capture has no sources")?;
        ensure!(!sources.is_empty(), "Capture has no source bytes");
        for source in sources {
            self.store.source_bytes(text(source, "sha256"))?;
        }
        let scope = match text(&report, "command") {
            "http" if byte_range.is_some() => {
                "http_original_text_byte_range_not_execution_or_attribution"
            }
            "evm-address" | "evm-transaction" => {
                "ethereum_provider_observation_not_transfer_finality_identity_or_ownership"
            }
            "telegram-search" => "provider_archive_observation_not_direct_verification",
            "http" if citation["source_id"].is_string() => {
                "external_json_observation_not_identity_attribution"
            }
            "http"
                if pointer == "/data/tracking_identifiers"
                    || pointer.starts_with("/data/tracking_identifiers/") =>
            {
                "http_static_tracking_extraction_not_execution_or_attribution"
            }
            "http" if pointer == "/data/json_urls" || pointer.starts_with("/data/json_urls/") => {
                "http_json_url_observation_not_fetch_or_attribution"
            }
            "http" => "http_response_metadata_not_page_content",
            "telegram-post" | "telegram-messages" | "telegram-profile" => {
                "public_page_capture_not_identity_attribution"
            }
            _ => "collector_observation_not_identity_attribution",
        };
        let mut resolved = json!({"step_id":step_id,"run_id":report["run_id"],"evidence_id":format!("evidence_{}",text(&report,"run_id")),"report_sha256":self.store.report_hash(text(&report,"run_id"))?,"observation_sha256":digest(serde_json::to_string(observation)?.as_bytes()),"sources":sources,"scope":scope});
        if let Some((start, end)) = byte_range {
            resolved["byte_start"] = json!(start);
            resolved["byte_end"] = json!(end);
        } else {
            resolved["pointer"] = json!(pointer);
        }
        if report["command"] == "http" && citation["source_id"].is_string() {
            resolved["source_id"] = citation["source_id"].clone();
        }
        Ok(resolved)
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
