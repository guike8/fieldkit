//! Validate model-selected collection jobs against the case's preserved facts.
//! SQL/prose is never executed here. A recommendation may select a frozen
//! pending job or propose a strictly grounded public-source collection target.
use crate::{
    Job, Workbench, collectors,
    model::{digest, strings, text},
};
use anyhow::{Result, anyhow, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::IpAddr,
};
use url::Url;

const PUBLIC: &[&str] = &[
    "username",
    "profile",
    "http",
    "search",
    "dns",
    "rdap",
    "ct",
    "tls",
    "archive",
    "domain",
    "email",
    "telegram-profile",
    "telegram-messages",
    "telegram-post",
    "bellingcat-telegram",
];
struct Citation {
    anchor: Value,
    literals: Vec<Literal>,
}
#[derive(Clone)]
struct Literal {
    value: String,
    kind: String,
}

pub fn validate_recommendation(w: &Workbench, session: &Value, answer: &Value) -> Result<Value> {
    let session_id = text(session, "id");
    ensure!(
        !session_id.is_empty(),
        "Recommendation requires an existing discovery session"
    );
    let persisted = w.store.get("discovery", session_id)?;
    ensure!(
        persisted["options"] == session["options"] && persisted["seed"] == session["seed"],
        "Discovery settings changed while NVIDIA was reasoning; retry against the current projection"
    );
    ensure!(
        !text(answer, "sql_code").trim().is_empty() && text(answer, "sql_code").len() <= 128 * 1024,
        "NVIDIA recommendation must include the executed SQL"
    );
    ensure!(
        answer["rows_truncated"] != true && answer["truncated"] != true,
        "Truncated NVIDIA rows cannot select an unambiguous job"
    );
    let rows = answer["sql_response_from_db"]
        .as_array()
        .ok_or_else(|| anyhow!("NVIDIA recommendation must contain normalized SQL record rows"))?;
    ensure!(
        rows.len() <= 500
            && answer["row_count"]
                .as_u64()
                .is_none_or(|n| n == rows.len() as u64),
        "NVIDIA result rows are truncated or exceed the row bound"
    );
    if rows.is_empty() {
        return Ok(
            json!({"action":"stop","job_id":null,"job":null,"rationale":rationale(&Value::Null,answer)?,"evidence_ids":[],"anchors":[],"depth":null}),
        );
    }
    let jobs = w.discovery_jobs(session_id)?;
    let reports = w.store.reports(500)?;
    let report_map: BTreeMap<&str, &Value> =
        reports.iter().map(|r| (text(r, "run_id"), r)).collect();
    let manual = w.store.list("evidence")?;
    let aliases: BTreeMap<&str, &str> = manual
        .iter()
        .map(|e| (text(e, "run_id"), text(e, "id")))
        .collect();
    let case_id = w.store.meta()?["case_id"].as_str().unwrap_or("").to_owned();
    let mut choices = BTreeMap::new();
    let mut verified_blobs = BTreeSet::new();
    for row in rows {
        ensure!(
            row.is_object(),
            "Each NVIDIA recommendation row must be an object"
        );
        if let Some(case) = row.get("case_id").filter(|v| !v.is_null()) {
            ensure!(
                case.as_str() == Some(&case_id),
                "NVIDIA row belongs to another case"
            );
        }
        if let Some(session) = row.get("discovery_id").filter(|v| !v.is_null()) {
            ensure!(
                session.as_str() == Some(session_id),
                "NVIDIA row belongs to another discovery session"
            );
        }
        let first = optional_string(row, "job_id")?;
        let alias = optional_string(row, "frontier_id")?;
        ensure!(
            first.is_none() || alias.is_none() || first == alias,
            "NVIDIA row has conflicting frontier identifiers"
        );
        let chosen = if let Some(identifier) = first.or(alias) {
            let pending = jobs
                .iter()
                .find(|j| text(j, "id") == identifier)
                .ok_or_else(|| anyhow!("NVIDIA selected a job outside this discovery session"))?;
            ensure!(
                pending["status"] == "pending",
                "NVIDIA selected a job that is no longer pending"
            );
            let mut job = job_from_record(pending)?;
            if let Some(command) = optional_string(row, "command")? {
                ensure!(
                    command == job.command,
                    "NVIDIA cannot change a frozen pending job's command"
                );
            }
            if let Some(target) = optional_string(row, "target")? {
                ensure!(
                    Some(target) == job.target.as_deref(),
                    "NVIDIA cannot change a frozen pending job's target"
                );
            }
            if row.get("options").is_some_and(|v| !v.is_null())
                || row.get("options_json").is_some_and(|v| !v.is_null())
            {
                ensure!(
                    options(row)? == job.options,
                    "NVIDIA cannot change a frozen pending job's options"
                );
            }
            let depth = pending["depth"]
                .as_u64()
                .ok_or_else(|| anyhow!("Pending job has no valid depth"))?
                as usize;
            let references = strings(&pending["evidence_ids"]);
            let anchor_values = pending["anchors"].as_array().cloned().unwrap_or_default();
            let seed = depth == 0
                && references.is_empty()
                && anchor_values.is_empty()
                && is_seed(&persisted, &job);
            let citations = if seed {
                vec![]
            } else {
                ensure!(
                    !references.is_empty() && !anchor_values.is_empty(),
                    "Pending follow-up job lacks supporting evidence and source anchors"
                );
                let valid = w.references(references.clone(), true)?;
                let citations = verify_anchors(
                    w,
                    &anchor_values,
                    &report_map,
                    &aliases,
                    &mut verified_blobs,
                )?;
                ensure!(
                    citations
                        .iter()
                        .all(|c| valid.contains(&text(&c.anchor, "evidence_id").to_owned()))
                        && valid
                            .iter()
                            .all(|id| citations.iter().any(|c| c.anchor["evidence_id"] == *id)),
                    "Pending job's evidence references do not match its anchors"
                );
                citations
            };
            validate_job_policy(&mut job, session, &citations, false, seed)?;
            if !seed {
                ensure!(
                    citations.iter().any(|c| grounds(&job, &c.literals)),
                    "Pending job target is not grounded by its cited structured source pointer"
                );
            }
            ensure!(
                depth <= session["options"]["max_depth"].as_u64().unwrap_or(0) as usize,
                "Pending job exceeds the discovery depth limit"
            );
            let reason = if seed {
                format!("User-supplied seed. {}", rationale(row, answer)?)
            } else {
                rationale(row, answer)?
            };
            json!({"action":"collect","job_id":identifier,"job":job,"rationale":reason.chars().take(4000).collect::<String>(),"evidence_ids":references,"anchors":citations.iter().map(|c|c.anchor.clone()).collect::<Vec<_>>(),"depth":depth})
        } else {
            let mut job = Job {
                command: optional_string(row, "command")?
                    .ok_or_else(|| anyhow!("A new recommendation requires a collector command"))?
                    .to_owned(),
                target: Some(
                    optional_string(row, "target")?
                        .ok_or_else(|| {
                            anyhow!("A new recommendation requires an exact cited target")
                        })?
                        .to_owned(),
                ),
                options: options(row)?,
            };
            let anchor_values = anchors(row)?;
            ensure!(
                !anchor_values.is_empty(),
                "A new recommendation requires preserved evidence and exact source anchors"
            );
            let citations = verify_anchors(
                w,
                &anchor_values,
                &report_map,
                &aliases,
                &mut verified_blobs,
            )?;
            validate_job_policy(&mut job, session, &citations, true, false)?;
            ensure!(
                citations.iter().any(|c| grounds(&job, &c.literals)),
                "Proposed target is not an exact cited literal or supported derivation; source instructions are not collection authority"
            );
            let mut depth = 1;
            for citation in &citations {
                for parent in &jobs {
                    if parent["run_id"] == citation.anchor["run_id"] {
                        depth = depth.max(parent["depth"].as_u64().unwrap_or(0) as usize + 1);
                    }
                }
            }
            ensure!(
                depth <= session["options"]["max_depth"].as_u64().unwrap_or(0) as usize,
                "New recommendation exceeds the discovery depth limit"
            );
            ensure!(
                !jobs.iter().any(|existing| same_job(existing, &job)),
                "Proposed collection already exists in this discovery queue; select its pending job ID or choose another target"
            );
            let references = citations
                .iter()
                .map(|c| text(&c.anchor, "evidence_id").to_owned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect::<Vec<_>>();
            w.references(references.clone(), true)?;
            json!({"action":"collect","job_id":null,"job":job,"rationale":rationale(row,answer)?,"evidence_ids":references,"anchors":citations.iter().map(|c|c.anchor.clone()).collect::<Vec<_>>(),"depth":depth})
        };
        let key = if let Some(id) = chosen["job_id"].as_str() {
            format!("existing:{id}")
        } else {
            format!("proposed:{}", chosen["job"])
        };
        if let Some(previous) = choices.get_mut(&key) {
            merge_citations(previous, &chosen)?;
        } else {
            choices.insert(key, chosen);
        }
        ensure!(
            choices.len() <= 1,
            "NVIDIA returned multiple distinct next jobs; recommendation is ambiguous"
        );
    }
    choices
        .into_values()
        .next()
        .ok_or_else(|| anyhow!("NVIDIA returned no usable recommendation"))
}

fn optional_string<'a>(value: &'a Value, key: &str) -> Result<Option<&'a str>> {
    match value.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) if !s.is_empty() => Ok(Some(s)),
        _ => bail!("Recommendation field {key} must be a nonempty string"),
    }
}
fn rationale(row: &Value, answer: &Value) -> Result<String> {
    let value = row["rationale"]
        .as_str()
        .or(row["reason"].as_str())
        .or(answer["response"].as_str())
        .unwrap_or("")
        .trim();
    ensure!(
        !value.is_empty() && value.len() <= 16000,
        "Recommendation requires a bounded public rationale"
    );
    Ok(value
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(4000)
        .collect())
}
fn options(row: &Value) -> Result<Value> {
    let value = if let Some(value) = row.get("options").filter(|v| !v.is_null()) {
        value.clone()
    } else if let Some(value) = row.get("options_json").filter(|v| !v.is_null()) {
        value.clone()
    } else {
        json!({})
    };
    let value = if let Some(s) = value.as_str() {
        serde_json::from_str(s)
            .map_err(|_| anyhow!("Collector options_json must be a valid JSON object"))?
    } else {
        value
    };
    ensure!(
        value.is_object() && value.to_string().len() <= 20000,
        "Collector options must be a bounded JSON object"
    );
    Ok(value)
}
fn anchors(row: &Value) -> Result<Vec<Value>> {
    if let Some(value) = row.get("anchors").filter(|v| !v.is_null()) {
        let value = if let Some(s) = value.as_str() {
            serde_json::from_str::<Value>(s)?
        } else {
            value.clone()
        };
        let anchors = value
            .as_array()
            .ok_or_else(|| anyhow!("Recommendation anchors must be a JSON array"))?;
        ensure!(
            anchors.len() <= 50,
            "Recommendation has too many source anchors"
        );
        Ok(anchors.clone())
    } else {
        Ok(vec![
            json!({"evidence_id":row["evidence_id"],"run_id":row["run_id"],"report_sha256":row["report_sha256"],"json_pointer":row["json_pointer"],"source_ids":row["source_ids"]}),
        ])
    }
}
fn job_from_record(record: &Value) -> Result<Job> {
    Ok(Job {
        command: text(record, "command").to_owned(),
        target: record["target"].as_str().map(str::to_owned),
        options: record.get("options").cloned().unwrap_or_else(|| json!({})),
    })
}
fn same_job(record: &Value, job: &Job) -> bool {
    text(record, "command") == job.command
        && record["target"].as_str() == job.target.as_deref()
        && record["options"] == job.options
}
fn is_seed(session: &Value, job: &Job) -> bool {
    let command = match text(session, "kind") {
        "url" => "http",
        other => other,
    };
    job.command == command && job.target.as_deref() == Some(text(session, "seed"))
}
fn merge_citations(previous: &mut Value, next: &Value) -> Result<()> {
    for key in ["anchors", "evidence_ids"] {
        let target = previous[key].as_array_mut().unwrap();
        for item in next[key].as_array().into_iter().flatten() {
            if !target.contains(item) {
                ensure!(
                    target.len() < 50,
                    "Recommendation selects too many distinct source anchors; narrow the SQL result"
                );
                target.push(item.clone());
            }
        }
    }
    previous["depth"] = json!(
        previous["depth"]
            .as_u64()
            .unwrap_or(0)
            .max(next["depth"].as_u64().unwrap_or(0))
    );
    Ok(())
}

fn verify_anchors(
    w: &Workbench,
    input: &[Value],
    reports: &BTreeMap<&str, &Value>,
    aliases: &BTreeMap<&str, &str>,
    verified: &mut BTreeSet<String>,
) -> Result<Vec<Citation>> {
    ensure!(
        !input.is_empty() && input.len() <= 50,
        "Supply 1–50 exact source anchors"
    );
    let mut output = vec![];
    for anchor in input {
        let run = optional_string(anchor, "run_id")?
            .ok_or_else(|| anyhow!("Source anchor requires run_id"))?;
        let evidence = optional_string(anchor, "evidence_id")?
            .ok_or_else(|| anyhow!("Source anchor requires evidence_id"))?;
        let hash = optional_string(anchor, "report_sha256")?
            .ok_or_else(|| anyhow!("Source anchor requires report_sha256"))?;
        let pointer = optional_string(anchor, "json_pointer")?
            .ok_or_else(|| anyhow!("Source anchor requires json_pointer"))?;
        ensure!(
            pointer.starts_with('/') && pointer.len() <= 1024 && pointer.split('/').count() <= 64,
            "Source anchor must name a bounded exact JSON pointer"
        );
        let report = reports
            .get(run)
            .ok_or_else(|| anyhow!("Cited run is not in the preserved case selection"))?;
        ensure!(
            !text(report, "command").starts_with("analysis.")
                && text(&report["data"], "kind") != "model_assessment"
                && text(report, "kind") != "model_assessment"
                && !report["sources"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|source| text(source, "kind") == "model_assessment"),
            "Model assessments cannot provide independent source grounding for another recommendation"
        );
        ensure!(
            matches!(text(report, "status"), "ok" | "partial" | "not_found"),
            "Failed or blocked provider results cannot ground a new collection recommendation"
        );
        let expected = aliases
            .get(run)
            .map(|s| s.to_string())
            .unwrap_or_else(|| format!("evidence_{run}"));
        ensure!(
            evidence == expected,
            "Source evidence ID does not belong to the cited run"
        );
        ensure!(
            hash.len() == 64 && hash == w.store.report_hash(run)?,
            "Source anchor report hash is missing or does not match the preserved report"
        );
        let value = report.pointer(pointer).ok_or_else(|| {
            anyhow!("Source anchor JSON pointer does not resolve in the preserved report")
        })?;
        let report_sources = report["sources"].as_array().cloned().unwrap_or_default();
        let allowed = report_sources
            .iter()
            .filter_map(|s| s["source_id"].as_str())
            .collect::<BTreeSet<_>>();
        let supplied = strings(&anchor["source_ids"]);
        ensure!(
            supplied.iter().all(|s| allowed.contains(s.as_str())),
            "Source anchor cites an unknown source ID"
        );
        let mut source_ids = strings(&value["source_ids"]);
        if source_ids.is_empty()
            && let Some((parent, _)) = pointer.rsplit_once('/')
        {
            source_ids = strings(&report.pointer(parent).unwrap_or(&Value::Null)["source_ids"]);
        }
        if source_ids.is_empty() {
            source_ids = if supplied.is_empty() {
                allowed.iter().map(|s| s.to_string()).collect()
            } else {
                supplied
            };
        }
        ensure!(
            source_ids.iter().all(|s| allowed.contains(s.as_str())),
            "Preserved structured value refers to an unknown source ID"
        );
        let mut source_hashes = vec![];
        for source in &report_sources {
            if !source_ids.iter().any(|s| source["source_id"] == *s) {
                continue;
            }
            if let Some(hash) = source["sha256"].as_str() {
                ensure!(
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()),
                    "Cited source hash is malformed"
                );
                if !(source["kind"] == "local_file" && source["raw_copied"] == false)
                    && verified.insert(hash.to_owned())
                {
                    let path = w.store.path.join("blobs").join(hash);
                    let metadata = fs::symlink_metadata(&path)?;
                    ensure!(
                        metadata.file_type().is_file() && metadata.len() <= 32 * 1024 * 1024,
                        "Cited source bytes are missing or not a regular bounded artifact"
                    );
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        ensure!(
                            metadata.nlink() == 1,
                            "Cited source artifact cannot be hard linked"
                        );
                    }
                    ensure!(
                        digest(&fs::read(path)?) == hash,
                        "Cited source artifact hash does not match its preserved bytes"
                    );
                }
                source_hashes.push(json!({"source_id":source["source_id"],"sha256":hash,"hash_scope":source["hash_scope"]}));
            }
        }
        output.push(Citation{anchor:json!({"evidence_id":evidence,"run_id":run,"report_sha256":hash,"json_pointer":pointer,"source_ids":source_ids,"source_hashes":source_hashes,"scope":"structured_collector_report","interpretation":"Collection candidate only; no identity or ownership attribution"}),literals:literals(report,pointer,value)});
    }
    Ok(output)
}

fn literals(report: &Value, pointer: &str, value: &Value) -> Vec<Literal> {
    let mut output = vec![];
    let mut add = |value: &Value, kind: &str| {
        if let Some(value) = value.as_str().filter(|s| !s.is_empty() && s.len() <= 8192) {
            output.push(Literal {
                value: value.into(),
                kind: kind.into(),
            });
        }
    };
    if value.is_string() {
        let kind = pointer
            .rsplit_once('/')
            .and_then(|(p, _)| report.pointer(p))
            .map(|p| text(p, "type"))
            .unwrap_or("");
        add(value, kind);
    } else if value.is_object() {
        add(&value["value"], text(value, "type"));
        for key in ["target", "to"] {
            add(&value[key]["value"], text(&value[key], "type"));
        }
        for key in ["url", "profile_url", "original_url", "replay_url"] {
            add(&value[key], "url");
        }
    }
    output
}

fn static_url(target: &str) -> Result<Url> {
    collectors::public_url(target)
}
fn literal_host(literal: &Literal) -> Option<String> {
    if let Ok(url) = static_url(&literal.value) {
        return url
            .host_str()
            .map(|s| s.trim_matches(['[', ']']).to_ascii_lowercase());
    }
    if let Some((_, domain)) = literal.value.rsplit_once('@')
        && let Ok(domain) = collectors::domain_name(domain)
    {
        return Some(domain);
    }
    collectors::domain_name(&literal.value).ok()
}
fn grounds(job: &Job, literals: &[Literal]) -> bool {
    let target = job.target.as_deref().unwrap_or("");
    literals.iter().any(|literal| match job.command.as_str() {
        "http" | "profile" | "archive" => static_url(target)
            .ok()
            .zip(static_url(&literal.value).ok())
            .is_some_and(|(a, b)| a == b),
        "telegram-profile" | "telegram-messages" | "telegram-post" | "bellingcat-telegram" => {
            static_url(target)
                .ok()
                .zip(static_url(&literal.value).ok())
                .is_some_and(|(a, b)| a == b)
                || matches!(literal.kind.as_str(), "username" | "handle")
                    && literal.value.strip_prefix("telegram:")
                        == Some(target.trim_start_matches('@'))
        }
        "dns" | "rdap" | "ct" | "tls" | "domain" => {
            literal.value.eq_ignore_ascii_case(target)
                || literal_host(literal).is_some_and(|h| {
                    h.eq_ignore_ascii_case(target.trim_end_matches('.').trim_matches(['[', ']']))
                })
        }
        "email" => literal.value == target,
        "username" => {
            matches!(literal.kind.as_str(), "username" | "handle")
                && (literal.value == target
                    || literal
                        .value
                        .split_once(':')
                        .is_some_and(|(_, value)| value == target.trim_start_matches('@')))
        }
        "search" => literal.value.len() >= 3 && target.contains(&literal.value),
        _ => false,
    })
}

fn provider(target: &str) -> Option<&'static str> {
    let url = Url::parse(target).ok()?;
    let host = url.host_str()?.trim_start_matches("www.");
    Some(match host {
        "github.com" | "api.github.com" => "github",
        "gitlab.com" => "gitlab",
        "codeberg.org" => "codeberg",
        "dev.to" => "devto",
        "lichess.org" => "lichess",
        "chess.com" | "api.chess.com" => "chess",
        "huggingface.co" => "huggingface",
        "news.ycombinator.com" | "hacker-news.firebaseio.com" => "hackernews",
        "reddit.com" => "reddit",
        "bsky.app" | "public.api.bsky.app" => "bluesky",
        "steamcommunity.com" => "steam",
        "youtube.com" | "m.youtube.com" => "youtube",
        "t.me" | "telegram.me" => "telegram",
        "instagram.com" => "instagram",
        "x.com" | "twitter.com" => "x",
        "tiktok.com" => "tiktok",
        "threads.net" => "threads",
        "pinterest.com" => "pinterest",
        "facebook.com" | "m.facebook.com" => "facebook",
        "twitch.tv" => "twitch",
        "medium.com" => "medium",
        "linkedin.com" => "linkedin",
        "keybase.io" => "keybase",
        _ => return None,
    })
}
fn in_scope(host: &str, domains: &[String]) -> bool {
    domains.is_empty()
        || domains.iter().any(|d| {
            host.eq_ignore_ascii_case(d)
                || host
                    .to_ascii_lowercase()
                    .ends_with(&format!(".{}", d.to_ascii_lowercase()))
        })
}

fn validate_job_policy(
    job: &mut Job,
    session: &Value,
    citations: &[Citation],
    new: bool,
    seed: bool,
) -> Result<()> {
    ensure!(
        PUBLIC.contains(&job.command.as_str()),
        "NVIDIA may recommend only supported public-source collectors; local files and arbitrary operations are excluded"
    );
    collectors::validate_job(job)?;
    let target = job
        .target
        .as_deref()
        .ok_or_else(|| anyhow!("Recommendation requires a target"))?;
    let sites = strings(&session["options"]["sites"]);
    let domains = strings(&session["options"]["domains"]);
    match job.command.as_str() {
        "username" => {
            if new {
                job.options["include_candidates"] = false.into();
                if !sites.is_empty() && job.options.get("sites").is_none() {
                    job.options["sites"] = json!(sites);
                }
            }
            let requested = strings(&job.options["sites"]);
            ensure!(
                sites.is_empty()
                    || !requested.is_empty() && requested.iter().all(|s| sites.contains(s)),
                "Username recommendation widens the original provider allowlist"
            );
            ensure!(
                target.len() <= 255
                    && !target.chars().any(|c| c.is_control()
                        || c.is_whitespace()
                        || matches!(c, '/' | '\\' | '?' | '#' | '%')),
                "Invalid public username target"
            );
        }
        "search" => {
            ensure!(
                session["options"]["web_search"] == true,
                "Web search was not enabled for this discovery"
            );
            ensure!(
                target.len() <= 512 && !target.chars().any(char::is_control),
                "Search recommendations must be bounded public queries"
            );
            if new {
                job.options["limit"] = json!(job.options["limit"].as_u64().unwrap_or(10).min(20));
            }
            if !seed && !domains.is_empty() {
                ensure!(
                    citations
                        .iter()
                        .flat_map(|c| &c.literals)
                        .any(|l| literal_host(l).is_some_and(|h| in_scope(&h, &domains))),
                    "Search recommendation has no cited target inside the domain scope"
                );
            }
        }
        "http" | "profile" | "archive" => {
            let url = static_url(target)?;
            ensure!(
                in_scope(url.host_str().unwrap_or(""), &domains),
                "Recommendation is outside the domain scope"
            );
            if !sites.is_empty()
                && let Some(p) = provider(target)
            {
                ensure!(
                    sites.iter().any(|s| s == p),
                    "Recommendation widens the original provider allowlist"
                );
            }
            if new {
                job.target = Some(url.to_string());
            }
        }
        "telegram-profile" | "telegram-messages" | "telegram-post" | "bellingcat-telegram" => {
            if !sites.is_empty() {
                ensure!(
                    sites.iter().any(|s| s == "telegram"),
                    "Telegram is outside the original provider allowlist"
                );
            }
            let host = if target.contains("://") {
                let url = static_url(target)?;
                ensure!(
                    url.scheme() == "https"
                        && ["t.me", "telegram.me", "www.t.me", "www.telegram.me"]
                            .contains(&url.host_str().unwrap_or(""))
                        && url.port().is_none(),
                    "Telegram recommendations require exact public HTTPS t.me targets"
                );
                let mut parts = url
                    .path_segments()
                    .unwrap()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>();
                if parts.first() == Some(&"s") {
                    parts.remove(0);
                }
                ensure!(
                    (1..=2).contains(&parts.len())
                        && regex::Regex::new(r"^[A-Za-z][A-Za-z0-9_]{3,31}$")?.is_match(parts[0])
                        && ![
                            "share",
                            "proxy",
                            "socks",
                            "login",
                            "joinchat",
                            "addstickers",
                            "addemoji",
                            "iv"
                        ]
                        .contains(&parts[0].to_ascii_lowercase().as_str()),
                    "Unsupported or non-public Telegram target"
                );
                if parts.len() == 2 {
                    ensure!(
                        parts[1]
                            .parse::<u64>()
                            .is_ok_and(|n| (1..=2147483647).contains(&n)),
                        "Telegram post ID is invalid"
                    );
                }
                if matches!(
                    job.command.as_str(),
                    "telegram-post" | "bellingcat-telegram"
                ) {
                    ensure!(parts.len() == 2, "An exact Telegram post URL is required");
                }
                url.host_str().unwrap().to_owned()
            } else {
                ensure!(
                    matches!(
                        job.command.as_str(),
                        "telegram-profile" | "telegram-messages"
                    ) && regex::Regex::new(r"^@?[A-Za-z][A-Za-z0-9_]{3,31}$")?.is_match(target),
                    "Use an exact Telegram public handle or post URL"
                );
                "t.me".into()
            };
            ensure!(
                in_scope(&host, &domains),
                "Telegram target is outside the domain scope"
            );
            if job.command == "bellingcat-telegram" {
                if new {
                    job.options["download_media"] = false.into();
                } else {
                    ensure!(
                        job.options["download_media"] != true,
                        "NVIDIA cannot select a media-downloading pending job"
                    );
                }
            }
            if job.command == "telegram-messages" {
                ensure!(
                    job.options.get("query").is_none(),
                    "NVIDIA cannot introduce an uncited Telegram text filter"
                );
                if new {
                    job.options["limit"] =
                        json!(job.options["limit"].as_u64().unwrap_or(20).min(20));
                }
            }
        }
        "email" => {
            let (local, domain) = target
                .rsplit_once('@')
                .ok_or_else(|| anyhow!("Email recommendation requires an exact cited address"))?;
            ensure!(
                !local.is_empty() && local.len() <= 64 && target.len() <= 254,
                "Email target is invalid"
            );
            let host = collectors::domain_name(domain)?;
            ensure!(
                in_scope(&host, &domains),
                "Email domain is outside the discovery scope"
            );
        }
        "dns" | "rdap" | "ct" | "tls" | "domain" => {
            let host = if let Ok(ip) = target.parse::<IpAddr>() {
                static_url(&format!(
                    "https://{}/",
                    if ip.is_ipv6() {
                        format!("[{ip}]")
                    } else {
                        ip.to_string()
                    }
                ))?;
                ip.to_string()
            } else if job.command == "rdap"
                && (target.to_ascii_uppercase().starts_with("AS")
                    || target.chars().all(|c| c.is_ascii_digit()))
            {
                ensure!(
                    domains.is_empty(),
                    "AS-number collection is outside the configured domain scope"
                );
                target.to_owned()
            } else {
                collectors::domain_name(target)?
            };
            ensure!(
                in_scope(&host, &domains),
                "Recommendation is outside the domain scope"
            );
        }
        _ => unreachable!(),
    }
    if job.command == "tls" {
        let port = job.options["port"].as_u64().unwrap_or(443);
        if port != 443 {
            ensure!(
                citations
                    .iter()
                    .flat_map(|c| &c.literals)
                    .filter_map(|l| static_url(&l.value).ok())
                    .any(|u| u.port() == Some(port as u16)
                        && u.host_str().is_some_and(
                            |h| h.eq_ignore_ascii_case(job.target.as_deref().unwrap_or(""))
                        )),
                "Nonstandard TLS ports require the same explicit port in a cited URL"
            );
        }
        if new {
            job.options["port"] = port.into();
        }
    }
    if job.command == "archive" {
        if new {
            job.options["prefix"] = false.into();
            job.options["limit"] = json!(job.options["limit"].as_u64().unwrap_or(20).min(20));
        } else {
            ensure!(
                job.options["prefix"] != true && job.options["limit"].as_u64().unwrap_or(20) <= 20,
                "NVIDIA archive jobs must be exact-URL queries limited to 20 captures"
            );
        }
    }
    collectors::validate_job(job)?;
    Ok(())
}
