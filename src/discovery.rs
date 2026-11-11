//! Persistent, bounded reconnaissance driven by explicit collector observations.
//! External text is data: it can supply public links, never executable actions.
use crate::{
    Job, Workbench,
    model::{digest, id, now, text},
};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};
use url::Url;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DiscoveryOptions {
    pub kind: String,
    pub seed: String,
    pub max_jobs: usize,
    pub max_depth: usize,
    pub domains: Vec<String>,
    pub sites: Vec<String>,
    pub web_search: bool,
    pub planner: String,
    pub reasoning_budget: usize,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            kind: "username".into(),
            seed: String::new(),
            max_jobs: 10,
            max_depth: 2,
            domains: Vec::new(),
            sites: Vec::new(),
            web_search: false,
            planner: "local".into(),
            reasoning_budget: 5,
        }
    }
}

fn normalized_url(value: &str) -> Result<String> {
    let mut url = Url::parse(value).context("Discovery URL must be absolute")?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("Discovery supports HTTP(S) URLs without embedded credentials");
    }
    url.set_fragment(None);
    Ok(url.to_string())
}

fn domain_name(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('.').to_ascii_lowercase();
    if value.is_empty()
        || value.len() > 253
        || !value.contains('.')
        || value.split('.').any(|part| {
            part.is_empty()
                || part.len() > 63
                || part.starts_with('-')
                || part.ends_with('-')
                || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
    {
        bail!("Domain scope must contain literal DNS names");
    }
    Ok(value)
}

fn inside_scope(target: &str, domains: &[String]) -> bool {
    if domains.is_empty() {
        return true;
    }
    let host = Url::parse(target)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_else(|| target.to_owned())
        .trim_end_matches('.')
        .to_ascii_lowercase();
    domains
        .iter()
        .any(|d| host == *d || host.ends_with(&format!(".{d}")))
}

fn job_key(session_id: &str, job: &Job) -> String {
    format!(
        "discovery_job_{}",
        digest(
            json!([session_id, job.command, job.target, job.options])
                .to_string()
                .as_bytes()
        )
    )
}

fn refs(report: &Value) -> (Vec<String>, Vec<Value>) {
    let run_id = text(report, "run_id");
    if run_id.is_empty() {
        return (vec![], vec![]);
    }
    let evidence_id = format!("evidence_{run_id}");
    (
        vec![evidence_id.clone()],
        vec![json!({"evidence_id":evidence_id,"run_id":run_id})],
    )
}

fn link_priority(relationship: &Value, target: &str) -> usize {
    if text(relationship, "relation") == "public_profile_at" {
        return 90;
    }
    let Ok(url) = Url::parse(target) else {
        return 10;
    };
    let path = url.path().to_ascii_lowercase();
    if [
        "/privacy", "/terms", "/login", "/signup", "/sign-in", "/share", "/search",
    ]
    .iter()
    .any(|p| path.starts_with(p))
    {
        return 10;
    }
    let source = relationship
        .get("source")
        .or_else(|| relationship.get("from"))
        .unwrap_or(&Value::Null);
    if text(source, "type") == "profile_url" {
        return 90;
    }
    let source_host = Url::parse(text(source, "value"))
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned));
    if source_host.as_deref() != url.host_str() {
        return 70;
    }
    if [
        "/about", "/contact", "/team", "/people", "/members", "/project",
    ]
    .iter()
    .any(|p| path.starts_with(p))
    {
        return 50;
    }
    30
}

impl Workbench {
    pub fn create_discovery(&self, mut options: DiscoveryOptions) -> Result<Value> {
        options.kind = options.kind.trim().to_ascii_lowercase();
        options.seed = options.seed.trim().to_owned();
        options.planner = options.planner.trim().to_ascii_lowercase();
        if !matches!(options.planner.as_str(), "local" | "nvidia")
            || !(1..=20).contains(&options.reasoning_budget)
        {
            bail!("Choose planner local or nvidia and a total reasoning budget from 1 to 20");
        }
        if options.seed.is_empty()
            || options.seed.len() > 8192
            || options.seed.chars().any(char::is_control)
        {
            bail!("Supply a discovery seed of 1–8192 characters");
        }
        if !(1..=100).contains(&options.max_jobs) || options.max_depth > 4 {
            bail!("Discovery accepts 1–100 jobs and depth 0–4");
        }
        if options.domains.len() > 30 || options.sites.len() > 30 {
            bail!("Use at most 30 scope domains and username sites");
        }
        options.domains = options
            .domains
            .iter()
            .map(|s| domain_name(s))
            .collect::<Result<BTreeSet<_>>>()?
            .into_iter()
            .collect();
        if options.sites.iter().any(|s| {
            s.is_empty()
                || s.len() > 80
                || !s
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        }) {
            bail!("Username sites must be provider identifiers");
        }
        let mut username_options = json!({"include_candidates":false});
        if !options.sites.is_empty() {
            username_options["sites"] = json!(options.sites);
        }
        let (command, target, job_options) = match options.kind.as_str() {
            "username" => {
                options.seed = options.seed.trim_start_matches('@').to_owned();
                if options.seed.is_empty()
                    || options.seed.len() > 256
                    || options.seed.contains(char::is_whitespace)
                {
                    bail!("Use a public username or federated handle");
                }
                ("username", options.seed.clone(), username_options)
            }
            "profile" => {
                options.seed = normalized_url(&options.seed)?;
                ("profile", options.seed.clone(), json!({}))
            }
            "url" => {
                options.seed = normalized_url(&options.seed)?;
                ("http", options.seed.clone(), json!({}))
            }
            "domain" => {
                options.seed = domain_name(&options.seed)?;
                (
                    "domain",
                    options.seed.clone(),
                    json!({"modules":["dns","rdap","http"]}),
                )
            }
            "search" => ("search", options.seed.clone(), json!({"limit":10})),
            _ => bail!("Seed kind must be username, profile, url, domain, or search"),
        };
        if matches!(options.kind.as_str(), "url" | "profile" | "domain")
            && !inside_scope(&target, &options.domains)
        {
            bail!("The seed is outside the requested domain scope");
        }
        if self.store.list("discovery")?.len() >= 50 {
            bail!("This case already has 50 discovery sessions");
        }
        let session_id = id("discovery");
        let session = json!({"id":session_id,"title":format!("{} · {}",options.kind,options.seed),
            "kind":options.kind,"seed":options.seed,"status":"pending","options":options,
            "created_at":now(),"updated_at":now(),"attempted":0,"completed":0,"failed":0,"queued":0,
            "stop_reason":null,"reasoning_used":0,"scope":"Public collector operations, evidence-grounded recommendations and bounded depth; no identity attribution."});
        self.store.put("discovery", &session)?;
        let job = Job {
            command: command.into(),
            target: Some(target),
            options: job_options,
        };
        self.enqueue_discovery(
            &session,
            &job,
            0,
            100,
            "User-supplied investigation seed",
            &[],
            &[],
            None,
        )?;
        if options.web_search && options.kind != "search" {
            self.enqueue_discovery(&session,&Job {command:"search".into(),target:Some(format!("\"{}\"",options.seed)),options:json!({"limit":10})},0,40,
                "Search public indexes for the user-supplied seed; results remain unverified candidates",&[],&[],None)?;
        }
        self.store.event(
            "discovery.created",
            "Bounded discovery planned",
            json!({"discovery_id":session_id,"options":options}),
        )?;
        self.update_discovery_counts(&session_id, None, None)
    }

    pub fn discoveries(&self) -> Result<Vec<Value>> {
        self.store.list("discovery")
    }

    pub fn discovery_jobs(&self, session_id: &str) -> Result<Vec<Value>> {
        self.store.get("discovery", session_id)?;
        let mut jobs: Vec<_> = self
            .store
            .list("discovery_jobs")?
            .into_iter()
            .filter(|j| text(j, "discovery_id") == session_id)
            .collect();
        jobs.sort_by(|a, b| {
            b["priority"]
                .as_u64()
                .cmp(&a["priority"].as_u64())
                .then(a["depth"].as_u64().cmp(&b["depth"].as_u64()))
                .then(text(a, "id").cmp(text(b, "id")))
        });
        Ok(jobs)
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "Queue entries carry the job and its evidence derivation together"
    )]
    pub(crate) fn enqueue_discovery(
        &self,
        session: &Value,
        job: &Job,
        depth: usize,
        priority: usize,
        reason: &str,
        evidence_ids: &[String],
        anchors: &[Value],
        pointer: Option<&str>,
    ) -> Result<()> {
        let session_id = text(session, "id");
        let identifier = job_key(session_id, job);
        let existing = self.discovery_jobs(session_id)?;
        if existing.iter().any(|j| text(j, "id") == identifier) || existing.len() >= 200 {
            return Ok(());
        }
        let options: DiscoveryOptions = serde_json::from_value(session["options"].clone())?;
        let out_of_scope = job.command != "search"
            && job.command != "username"
            && !inside_scope(job.target.as_deref().unwrap_or(""), &options.domains);
        let too_deep = depth > options.max_depth;
        let skip = if out_of_scope {
            Some("outside_domain_scope")
        } else if too_deep {
            Some("depth_limit")
        } else {
            None
        };
        let anchors: Vec<Value> = anchors
            .iter()
            .map(|anchor| {
                let mut anchor = anchor.clone();
                if let Some(pointer) = pointer {
                    anchor["json_pointer"] = json!(pointer);
                }
                anchor
            })
            .collect();
        self.store.put("discovery_jobs",&json!({"id":identifier,"discovery_id":session_id,
            "command":job.command,"target":job.target,"options":job.options,"depth":depth,"priority":priority,
            "status":if skip.is_some(){"skipped"}else{"pending"},"skip_reason":skip,
            "reason":reason,"evidence_ids":evidence_ids,"anchors":anchors,
            "attempts":0,"run_id":null,"created_at":now(),"started_at":null,"completed_at":null,"error":null}))
    }

    fn expand_discovery(&self, session: &Value, parent: &Value, report: &Value) -> Result<()> {
        if !matches!(text(report, "status"), "ok" | "partial" | "not_found") {
            return Ok(());
        }
        let depth = parent["depth"].as_u64().unwrap_or(0) as usize + 1;
        let (evidence_ids, mut anchors) = refs(report);
        if evidence_ids.is_empty() {
            return Ok(());
        }
        anchors[0]["report_sha256"] = json!(self.store.report_hash(text(report, "run_id"))?);
        anchors[0]["source_ids"] = json!(
            report["sources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["source_id"].as_str())
                .collect::<Vec<_>>()
        );
        for (index, entity) in report["entities"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            let kind = text(entity, "type");
            let value = text(entity, "value");
            if (kind == "profile_url"
                || (matches!(kind, "url" | "search_result_url")
                    && text(report, "command") == "search"))
                && let Ok(url) = normalized_url(value)
            {
                let command = if kind == "profile_url" {
                    "http"
                } else {
                    "profile"
                };
                let reason = if kind == "profile_url" {
                    "Inspect a profile URL explicitly observed by the preceding collector"
                } else {
                    "Inspect an unverified public search result before making any claim about it"
                };
                self.enqueue_discovery(
                    session,
                    &Job {
                        command: command.into(),
                        target: Some(url),
                        options: json!({}),
                    },
                    depth,
                    90,
                    reason,
                    &evidence_ids,
                    &anchors,
                    Some(&format!("/entities/{index}")),
                )?;
            }
        }
        for (index, relationship) in report["relationships"]
            .as_array()
            .into_iter()
            .flatten()
            .enumerate()
        {
            if !matches!(
                text(relationship, "relation"),
                "links_to" | "declares_canonical" | "public_profile_at"
            ) {
                continue;
            }
            let target = relationship
                .get("target")
                .or_else(|| relationship.get("to"))
                .unwrap_or(&Value::Null);
            let value = text(target, "value");
            if !matches!(text(target, "type"), "url" | "profile_url") {
                continue;
            }
            if let Ok(url) = normalized_url(value) {
                let priority = link_priority(relationship, &url);
                self.enqueue_discovery(session,&Job {command:"http".into(),target:Some(url),options:json!({})},depth,priority,
                    "Follow an explicit public link from captured evidence; a link does not establish ownership",&evidence_ids,&anchors,Some(&format!("/relationships/{index}")))?;
            }
        }
        // A supplied profile is a collection candidate. Its page can still be
        // preserved even when the specialized verifier cannot identify it.
        if text(parent, "command") == "profile"
            && let Ok(url) = normalized_url(text(parent, "target"))
        {
            self.enqueue_discovery(
                session,
                &Job {
                    command: "http".into(),
                    target: Some(url),
                    options: json!({}),
                },
                depth,
                80,
                "Capture the inspected public profile page and its explicit outbound links",
                &evidence_ids,
                &anchors,
                Some("/target"),
            )?;
        }
        Ok(())
    }

    fn update_discovery_counts(
        &self,
        session_id: &str,
        status: Option<&str>,
        stop_reason: Option<&str>,
    ) -> Result<Value> {
        let mut session = self.store.get("discovery", session_id)?;
        let jobs = self.discovery_jobs(session_id)?;
        session["attempted"] = json!(
            jobs.iter()
                .filter(|j| j["attempts"].as_u64().unwrap_or(0) > 0)
                .count()
        );
        session["completed"] = json!(
            jobs.iter()
                .filter(|j| text(j, "status") == "completed")
                .count()
        );
        session["failed"] = json!(
            jobs.iter()
                .filter(|j| matches!(text(j, "status"), "failed" | "interrupted"))
                .count()
        );
        session["queued"] = json!(
            jobs.iter()
                .filter(|j| text(j, "status") == "pending")
                .count()
        );
        session["skipped"] = json!(
            jobs.iter()
                .filter(|j| text(j, "status") == "skipped")
                .count()
        );
        if let Some(status) = status {
            session["status"] = json!(status);
        }
        session["stop_reason"] = json!(stop_reason);
        session["updated_at"] = json!(now());
        self.store.put("discovery", &session)?;
        Ok(session)
    }

    /// Runs until budget, exhaustion, or stop. An OS advisory lock prevents
    /// overlapping workers, and is released automatically after process death.
    pub fn run_discovery(
        &self,
        session_id: &str,
        stop: &AtomicBool,
        progress: impl Fn(Value),
    ) -> Result<Value> {
        let lock_path = self.store.path.join("discovery.lock");
        let lock = crate::ontology::private_lock(&lock_path)?;
        lock.try_lock_exclusive()
            .context("A discovery worker is already running for this case")?;
        let initial = self.store.get("discovery", session_id)?;
        let options: DiscoveryOptions = serde_json::from_value(initial["options"].clone())?;
        // Recovery is deliberately conservative: a collector may have committed
        // its immutable report just before the process died. Do not fetch again.
        for mut job in self.discovery_jobs(session_id)? {
            if text(&job, "status") == "running" {
                job["status"] = json!("interrupted");
                job["error"] = json!(
                    "Previous worker stopped during this collector. Preserved reports remain in Evidence; this job is not retried automatically."
                );
                job["completed_at"] = json!(now());
                self.store.put("discovery_jobs", &job)?;
            }
        }
        let mut session = self.update_discovery_counts(session_id, Some("running"), None)?;
        self.store.event(
            "discovery.started",
            "Discovery worker started",
            json!({"discovery_id":session_id}),
        )?;
        progress(
            json!({"discovery":session,"status":"running","message":format!("Discovery started · {} queued jobs",session["queued"])}),
        );
        if options.planner == "nvidia" && !stop.load(Ordering::Relaxed) {
            progress(
                json!({"status":"reasoning","message":"Checking NVIDIA gateway and compiled evidence catalog"}),
            );
            let ready = self.nvidia_status()?;
            if ready["ready"] != true {
                self.nvidia_setup_failure(session_id, text(&ready, "message"))?;
                let mut paused = self.update_discovery_counts(
                    session_id,
                    Some("needs_setup"),
                    Some("nvidia_not_ready"),
                )?;
                paused["message"] = ready["message"].clone();
                self.store.put("discovery", &paused)?;
                return Ok(paused);
            }
        }
        let outcome = (|| -> Result<Value> {
            loop {
                if stop.load(Ordering::Relaxed) {
                    return self.update_discovery_counts(
                        session_id,
                        Some("paused"),
                        Some("stop_requested"),
                    );
                }
                if session["attempted"].as_u64().unwrap_or(0) >= options.max_jobs as u64 {
                    return self.update_discovery_counts(
                        session_id,
                        Some("budget_exhausted"),
                        Some("job_budget"),
                    );
                }
                let pending = self
                    .discovery_jobs(session_id)?
                    .into_iter()
                    .filter(|j| text(j, "status") == "pending")
                    .collect::<Vec<_>>();
                let mut job = if options.planner == "nvidia" {
                    // Explicit user seeds precede model reasoning: there are no
                    // collected sources for the model to assess before them.
                    if let Some(seed) = pending.iter().find(|j| {
                        j["depth"] == 0 && j["evidence_ids"].as_array().is_some_and(Vec::is_empty)
                    }) {
                        seed.clone()
                    } else if options.max_depth == 0 && pending.is_empty() {
                        return self.update_discovery_counts(
                            session_id,
                            Some("completed"),
                            Some("depth_limit"),
                        );
                    } else {
                        let decision = self.nvidia_next_job(session_id, stop, &progress)?;
                        match text(&decision, "action") {
                            "collect" => decision["job"].clone(),
                            "stop" => {
                                return self.update_discovery_counts(
                                    session_id,
                                    Some("completed"),
                                    Some("nvidia_no_supported_followup"),
                                );
                            }
                            _ => {
                                let mut paused = self.update_discovery_counts(
                                    session_id,
                                    Some(text(&decision, "status")),
                                    Some(text(&decision, "stop_reason")),
                                )?;
                                paused["message"] = decision["message"].clone();
                                self.store.put("discovery", &paused)?;
                                return Ok(paused);
                            }
                        }
                    }
                } else if let Some(job) = pending.into_iter().next() {
                    job
                } else {
                    return self.update_discovery_counts(
                        session_id,
                        Some("completed"),
                        Some("frontier_exhausted"),
                    );
                };
                job["status"] = json!("running");
                job["started_at"] = json!(now());
                job["attempts"] = json!(job["attempts"].as_u64().unwrap_or(0) + 1);
                self.store.put("discovery_jobs", &job)?;
                progress(
                    json!({"discovery":session,"job":job,"status":"collecting","command":job["command"],"target":job["target"],
                    "message":format!("Collecting {} {} · depth {}",text(&job,"command"),text(&job,"target"),job["depth"])}),
                );
                let collection = self.run_tool(&Job {
                    command: text(&job, "command").to_owned(),
                    target: job["target"].as_str().map(str::to_owned),
                    options: job["options"].clone(),
                });
                match collection {
                    Ok(report) => {
                        job["run_id"] = report["run_id"].clone();
                        job["evidence_id"] = json!(format!("evidence_{}", text(&report, "run_id")));
                        job["collector_status"] = report["status"].clone();
                        let good = matches!(text(&report, "status"), "ok" | "not_found");
                        job["status"] = json!(if good { "completed" } else { "failed" });
                        if !good {
                            job["error"] = json!(format!(
                                "Collector returned {}; inspect preserved evidence for limitations",
                                text(&report, "status")
                            ));
                        }
                        // Partial collectors can still contain explicitly observed
                        // profiles or links. Preserve their status and provenance.
                        self.expand_discovery(&session, &job, &report)?;
                    }
                    Err(error) => {
                        job["status"] = json!("failed");
                        job["error"] =
                            json!(error.to_string().chars().take(1000).collect::<String>());
                    }
                }
                job["completed_at"] = json!(now());
                self.store.put("discovery_jobs", &job)?;
                session = self.update_discovery_counts(session_id, Some("running"), None)?;
                progress(
                    json!({"discovery":session,"job":job,"status":"updated","command":job["command"],"target":job["target"],
                    "message":format!("{} · {} · {} attempted / {} queued",text(&job,"command"),text(&job,"status"),session["attempted"],session["queued"])}),
                );
            }
        })();
        let session = match outcome {
            Ok(session) => session,
            Err(error) => {
                let _ =
                    self.update_discovery_counts(session_id, Some("paused"), Some("worker_error"));
                return Err(error);
            }
        };
        self.store.event("discovery.stopped","Discovery worker stopped",json!({"discovery_id":session_id,"status":session["status"],"stop_reason":session["stop_reason"],"attempted":session["attempted"]}))?;
        progress(
            json!({"discovery":session,"status":"stopped","message":format!("Discovery {} · {} attempted / {} queued",text(&session,"status"),session["attempted"],session["queued"])}),
        );
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Collected, Config, store::Store};
    use std::collections::BTreeMap;

    fn workspace() -> (tempfile::TempDir, Workbench) {
        let temp = tempfile::tempdir().unwrap();
        let wb = Workbench {
            store: Store::open(temp.path()).unwrap(),
            config: Config::default(),
        };
        (temp, wb)
    }

    fn preserve(
        wb: &Workbench,
        run: &str,
        command: &str,
        entities: Value,
        relationships: Value,
    ) -> Value {
        let report = json!({"run_id":run,"command":command,"target":"seed","status":"ok","sources":[{"source_id":"source-fixture","kind":"fixture"}],"entities":entities,"relationships":relationships,"data":{},"timing":{}});
        wb.store
            .save_report(&Collected {
                report: report.clone(),
                blobs: BTreeMap::new(),
            })
            .unwrap();
        report
    }

    #[test]
    fn explicit_profile_discovery_preserves_source_pointer_and_deduplicates_url_fragments() {
        let (_temp, wb) = workspace();
        let session = wb
            .create_discovery(DiscoveryOptions {
                seed: "alice".into(),
                ..Default::default()
            })
            .unwrap();
        let parent = wb.discovery_jobs(text(&session, "id")).unwrap().remove(0);
        let report = preserve(
            &wb,
            "profiles",
            "username",
            json!([
                {"type":"profile_url","value":"https://account.example/alice#top","source_ids":["source-fixture"]},
                {"type":"profile_url","value":"https://account.example/alice#bio","source_ids":["source-fixture"]}
            ]),
            json!([]),
        );
        wb.expand_discovery(&session, &parent, &report).unwrap();
        wb.expand_discovery(&session, &parent, &report).unwrap();
        let jobs = wb.discovery_jobs(text(&session, "id")).unwrap();
        assert_eq!(jobs.len(), 2);
        let child = jobs.iter().find(|v| v["command"] == "http").unwrap();
        assert_eq!(child["target"], "https://account.example/alice");
        assert_eq!(child["depth"], 1);
        assert_eq!(child["evidence_ids"], json!(["evidence_profiles"]));
        assert_eq!(
            child["anchors"][0]["report_sha256"],
            wb.store.report_hash("profiles").unwrap()
        );
        assert_eq!(
            report
                .pointer(text(&child["anchors"][0], "json_pointer"))
                .unwrap(),
            &report["entities"][0]
        );
    }

    #[test]
    fn scope_and_depth_exclusions_are_visible_in_frontier() {
        let (_temp, wb) = workspace();
        let session = wb
            .create_discovery(DiscoveryOptions {
                seed: "alice".into(),
                max_depth: 0,
                domains: vec!["allowed.example".into()],
                ..Default::default()
            })
            .unwrap();
        let parent = wb.discovery_jobs(text(&session, "id")).unwrap().remove(0);
        let report = preserve(
            &wb,
            "links",
            "http",
            json!([]),
            json!([
                {"source":{"type":"url","value":"https://allowed.example/"},"relation":"links_to","target":{"type":"url","value":"https://outside.example/"},"source_ids":["source-fixture"]},
                {"source":{"type":"url","value":"https://allowed.example/"},"relation":"links_to","target":{"type":"url","value":"https://allowed.example/about"},"source_ids":["source-fixture"]}
            ]),
        );
        wb.expand_discovery(&session, &parent, &report).unwrap();
        let jobs = wb.discovery_jobs(text(&session, "id")).unwrap();
        assert_eq!(jobs.len(), 3);
        assert!(
            jobs.iter()
                .any(|j| j["skip_reason"] == "outside_domain_scope" && j["status"] == "skipped")
        );
        assert!(
            jobs.iter()
                .any(|j| j["skip_reason"] == "depth_limit" && j["status"] == "skipped")
        );
    }

    #[test]
    fn search_index_urls_become_candidates_to_inspect_not_confirmed_accounts() {
        let (_temp, wb) = workspace();
        let session = wb
            .create_discovery(DiscoveryOptions {
                kind: "search".into(),
                seed: "public project".into(),
                ..Default::default()
            })
            .unwrap();
        let parent = wb.discovery_jobs(text(&session, "id")).unwrap().remove(0);
        let report = preserve(
            &wb,
            "search-hits",
            "search",
            json!([{"type":"search_result_url","value":"https://account.example/alice","source_ids":["source-fixture"]}]),
            json!([]),
        );
        wb.expand_discovery(&session, &parent, &report).unwrap();
        let jobs = wb.discovery_jobs(text(&session, "id")).unwrap();
        let candidate = jobs.iter().find(|j| j["command"] == "profile").unwrap();
        assert!(text(candidate, "reason").contains("unverified"));
        assert_eq!(candidate["evidence_ids"], json!(["evidence_search-hits"]));
        assert!(
            wb.ontology_snapshot().unwrap()["claims"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn another_worker_cannot_run_the_same_case_concurrently() {
        let (_temp, wb) = workspace();
        let session = wb
            .create_discovery(DiscoveryOptions {
                seed: "alice".into(),
                ..Default::default()
            })
            .unwrap();
        let lock = crate::ontology::private_lock(&wb.store.path.join("discovery.lock")).unwrap();
        lock.try_lock_exclusive().unwrap();
        assert!(
            wb.run_discovery(text(&session, "id"), &AtomicBool::new(false), |_| {})
                .is_err()
        );
        assert!(wb.store.reports(10).unwrap().is_empty());
    }
}
