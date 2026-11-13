use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
};

pub const MAX_BYTES: u64 = 8 * 1024 * 1024;
pub type Records = BTreeMap<String, Value>;
pub type Bodies = BTreeMap<String, Vec<u8>>;
pub trait Tools {
    fn tool(&mut self, name: &str, arguments: Value) -> Result<Value>;
}
pub fn text(v: &Value) -> Result<&str> {
    v.as_str().context("expected string")
}
pub fn array(v: &Value) -> Result<&Vec<Value>> {
    v.as_array().context("expected array")
}
pub fn hash(raw: &[u8]) -> String {
    format!("{:x}", Sha256::digest(raw))
}
pub fn observation_hash(value: &Value) -> Result<String> {
    Ok(hash(&serde_json::to_vec(value)?))
}
pub fn canonical_digest(digest: &str) -> bool {
    digest.len() == 64
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn bounded_read(path: &Path) -> Result<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink() && meta.len() <= MAX_BYTES,
        "Expected bounded regular retained file"
    );
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 <= MAX_BYTES, "File exceeds bound");
    Ok(bytes)
}
pub fn read_json(path: &Path) -> Result<Value> {
    Ok(serde_json::from_slice(&bounded_read(path)?)?)
}
pub fn blob(case: &Path, digest: &str) -> Result<Vec<u8>> {
    ensure!(canonical_digest(digest), "Expected canonical SHA-256");
    let raw = bounded_read(&case.join("blobs").join(digest))?;
    ensure!(hash(&raw) == digest, "Original source hash mismatch");
    Ok(raw)
}
/// A private, exclusively created output, held open across checkpoints.
pub struct Output(File);
impl Output {
    pub fn new(path: &Path) -> Result<Self> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(Self(options.open(path).with_context(|| {
            format!("Create new private output {}", path.display())
        })?))
    }
    pub fn checkpoint(&mut self, value: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec_pretty(value)?;
        bytes.push(b'\n');
        self.0.seek(SeekFrom::Start(0))?;
        self.0.write_all(&bytes)?;
        self.0.set_len(bytes.len() as u64)?;
        self.0.sync_all()?;
        Ok(())
    }
    pub fn line(&mut self, value: &Value) -> Result<()> {
        serde_json::to_writer(&mut self.0, value)?;
        self.0.write_all(b"\n")?;
        self.0.flush()?;
        Ok(())
    }
}
pub fn original_source(report: &Value, case: &Path) -> Result<(Value, Vec<u8>)> {
    let digest = text(&report["data"]["body_sha256"])?;
    let ids = array(&report["data"]["source_ids"])?;
    let matches: Vec<_> = array(&report["sources"])?
        .iter()
        .filter(|s| s["sha256"] == digest && ids.contains(&s["source_id"]))
        .collect();
    ensure!(
        matches.len() == 1,
        "Final response must bind exactly one retained source"
    );
    Ok((matches[0].clone(), blob(case, digest)?))
}
pub fn pending_only(state: &Value, reviews: u64) -> bool {
    state["summary"] == json!([])
        && reviews == 0
        && state["findings"].as_array().is_some_and(|fs| {
            fs.iter().all(|f| {
                f["status"] == "pending_review"
                    && f["source_evidence"] == false
                    && f["origin"] == "analytical_proposal"
                    && f.get("review").is_none()
            })
        })
}
pub fn pointer_value<'a>(value: &'a Value, pointer: &str) -> Result<&'a Value> {
    ensure!(
        pointer.starts_with('/') && !pointer.is_empty(),
        "Expected non-root JSON pointer"
    );
    // Reject malformed escapes, rather than silently treating them as key bytes.
    for token in pointer.split('/').skip(1) {
        let mut chars = token.chars();
        while let Some(c) = chars.next() {
            if c == '~' {
                ensure!(
                    matches!(chars.next(), Some('0' | '1')),
                    "Invalid JSON pointer escape"
                );
            }
        }
    }
    value
        .pointer(pointer)
        .context("JSON pointer does not resolve")
}
#[derive(Default)]
pub struct Stored {
    pub hashes: BTreeMap<String, String>,
    pub reports: Records,
    pub reviews: u64,
}
/// Independently check every stored report and source with SQLite read-only URI mode.
pub fn stored(case: &Path) -> Result<Stored> {
    let path = case.canonicalize()?.join("case.sqlite3");
    let meta = std::fs::symlink_metadata(&path)?;
    ensure!(
        meta.is_file() && !meta.file_type().is_symlink(),
        "Expected regular case database"
    );
    let mut uri =
        url::Url::from_file_path(path).map_err(|_| anyhow::anyhow!("Invalid database path"))?;
    uri.set_query(Some("mode=ro"));
    let db = rusqlite::Connection::open_with_flags(
        uri.as_str(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    db.busy_timeout(std::time::Duration::from_secs(2))?;
    let mut result = Stored::default();
    let mut query = db.prepare("SELECT run_id,result_json,report_sha256 FROM runs")?;
    let mut rows = query.query([])?;
    while let Some(row) = rows.next()? {
        let run: String = row.get(0)?;
        let raw: String = row.get(1)?;
        let digest: String = row.get(2)?;
        ensure!(
            raw.len() as u64 <= MAX_BYTES && hash(raw.as_bytes()) == digest,
            "Independent stored report hash mismatch"
        );
        let report: Value = serde_json::from_str(&raw)?;
        ensure!(report["run_id"] == run, "Stored report run mismatch");
        for source in array(&report["sources"])? {
            blob(case, text(&source["sha256"])?)?;
        }
        result.hashes.insert(run.clone(), digest);
        result.reports.insert(run, report);
    }
    result.reviews = u64::try_from(db.query_row(
        "SELECT COUNT(*) FROM wb_items WHERE kind='investigation_reviews'",
        [],
        |r| r.get::<_, i64>(0),
    )?)?;
    Ok(result)
}
pub fn bind_stored(reports: &Records, stored: &Stored) -> Result<()> {
    for report in reports.values() {
        let run = text(&report["run_id"])?;
        ensure!(
            stored.reports.get(run) == Some(report),
            "MCP report differs from independently retained report"
        );
    }
    Ok(())
}
pub fn citations(
    state: &Value,
    receipt: &Value,
    reports: &Records,
    bodies: &Bodies,
    hashes: &BTreeMap<String, String>,
) -> Result<usize> {
    let entries = array(&receipt["inputs"])?;
    let mut count = 0;
    for finding in array(&state["findings"])? {
        let citations = array(&finding["citations"])?;
        ensure!((1..=20).contains(&citations.len()), "finding_has_citations");
        for citation in citations {
            let entry = entries
                .iter()
                .find(|e| e["step_id"] == citation["step_id"])
                .context("Citation references unknown step")?;
            let key = text(&entry["id"])?;
            let report = reports.get(key).context("Missing cited report")?;
            let raw = bodies.get(key).context("Missing cited bytes")?;
            let run = text(&entry["run_id"])?;
            ensure!(
                citation["run_id"] == run
                    && citation["report_sha256"]
                        == *hashes.get(run).context("Missing stored hash")?
                    && citation["sources"] == report["sources"],
                "citation_report_and_source_binding"
            );
            let source = citation["source_id"]
                .as_str()
                .map(|id| -> Result<&Value> {
                    if entry.get("source_id").is_some() {
                        ensure!(
                            entry["source_id"] == id,
                            "Citation differs from receipt source"
                        );
                    }
                    let source = array(&report["sources"])?
                        .iter()
                        .find(|s| s["source_id"] == id && s["sha256"] == hash(raw))
                        .context("Citation source mismatch")?;
                    ensure!(
                        source["kind"] == "http"
                            && source["http_status"]
                                .as_u64()
                                .is_some_and(|s| (200..300).contains(&s)),
                        "Citation requires successful HTTP source"
                    );
                    Ok(source)
                })
                .transpose()?;
            let observed = if citation.get("byte_start").is_some()
                || citation.get("byte_end").is_some()
            {
                let source = source.context("Byte citation requires original source")?;
                let media = text(&source["media_type"])?
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                ensure!(
                    ["text/html", "application/xhtml+xml", "text/plain"].contains(&media.as_str()),
                    "Byte citation requires text source"
                );
                let start = citation["byte_start"]
                    .as_u64()
                    .context("Invalid byte start")?;
                let end = citation["byte_end"].as_u64().context("Invalid byte end")?;
                ensure!(
                    start < end
                        && end <= raw.len() as u64
                        && end - start <= 16384
                        && citation.get("pointer").is_none(),
                    "citation_exact_original_range"
                );
                ensure!(
                    citation["scope"]
                        == "http_original_text_byte_range_not_execution_or_attribution",
                    "citation_text_scope"
                );
                Value::String(std::str::from_utf8(&raw[start as usize..end as usize])?.to_owned())
            } else if let Some(source) = source {
                let media = text(&source["media_type"])?
                    .split(';')
                    .next()
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                ensure!(
                    media == "application/json"
                        || (media.starts_with("application/") && media.ends_with("+json")),
                    "JSON citation requires JSON source"
                );
                ensure!(
                    citation["scope"] == "external_json_observation_not_identity_attribution",
                    "citation_json_scope"
                );
                pointer_value(&serde_json::from_slice(raw)?, text(&citation["pointer"])?)?.clone()
            } else {
                ensure!(
                    entry.get("source_id").is_none(),
                    "Source-backed event-stream citation requires source_id"
                );
                let pointer = text(&citation["pointer"])?;
                let scope = if pointer == "/data/tracking_identifiers"
                    || pointer.starts_with("/data/tracking_identifiers/")
                {
                    "http_static_tracking_extraction_not_execution_or_attribution"
                } else if pointer == "/data/json_urls" || pointer.starts_with("/data/json_urls/") {
                    "http_json_url_observation_not_fetch_or_attribution"
                } else {
                    "http_response_metadata_not_page_content"
                };
                ensure!(
                    report["command"] == "http" && citation["scope"] == scope,
                    "citation_report_observation_scope"
                );
                pointer_value(report, pointer)?.clone()
            };
            ensure!(
                observation_hash(&observed)? == text(&citation["observation_sha256"])?,
                "citation_original_observation_hash"
            );
            count += 1;
        }
    }
    Ok(count)
}
pub fn unique_records(entries: &Value) -> Result<BTreeMap<String, Value>> {
    let mut result = BTreeMap::new();
    for entry in array(entries)? {
        let id = text(&entry["id"])?;
        if result.insert(id.to_owned(), entry.clone()).is_some() {
            bail!("Duplicate receipt record");
        }
    }
    Ok(result)
}
