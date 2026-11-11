//! Client for the actual NVIDIA Auto Ontology public gateway.
//!
//! Calls `/api/chat/completions`, not an inference/NIM endpoint. The `evidence`
//! argument is a trusted Fieldkit schema/constraint object: callers must never
//! place untrusted source text in the upstream API's authoritative evidence slot.
//! Source data belongs in the separately configured read-only case projection.
//! Streaming thoughts are discarded; only node/phase progress is retained.

use crate::model::digest;
use anyhow::{Result, anyhow, bail, ensure};
use reqwest::{
    blocking::Client,
    header::{HeaderMap, HeaderValue},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fmt,
    io::Read,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use url::Url;

const MAX_REQUEST_BYTES: usize = 256 * 1024;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_STEPS: usize = 512;
const MAX_ROWS: usize = 500;
const POLL_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct NvidiaConfig {
    pub url: String,
    pub api_token: String,
    pub target_db: Option<String>,
    pub timeout_secs: u64,
}

impl fmt::Debug for NvidiaConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NvidiaConfig")
            .field("url", &self.url.replace(&self.api_token, "[REDACTED]"))
            .field("api_token", &"[REDACTED]")
            .field(
                "target_db",
                &self
                    .target_db
                    .as_ref()
                    .map(|s| s.replace(&self.api_token, "[REDACTED]")),
            )
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl NvidiaConfig {
    pub fn new(
        url: impl Into<String>,
        api_token: impl Into<String>,
        target_db: Option<String>,
        timeout_secs: u64,
    ) -> Result<Self> {
        let url = url.into();
        let api_token = api_token.into();
        ensure!(
            url.len() <= 4096 && url.trim() == url && !url.chars().any(char::is_control),
            "AUTO_ONTOLOGY_API_URL must be a bounded absolute gateway URL"
        );
        let parsed = Url::parse(&url).map_err(|_| {
            anyhow!("AUTO_ONTOLOGY_API_URL must be an absolute HTTP(S) gateway URL")
        })?;
        ensure!(
            parsed.username().is_empty()
                && parsed.password().is_none()
                && parsed.query().is_none()
                && parsed.fragment().is_none(),
            "Auto Ontology gateway URL cannot contain credentials, query parameters, or fragments"
        );
        let host = parsed
            .host_str()
            .ok_or_else(|| anyhow!("Auto Ontology gateway URL requires a host"))?;
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        ensure!(
            parsed.scheme() == "https" || (parsed.scheme() == "http" && loopback),
            "Remote Auto Ontology gateways require HTTPS; HTTP is allowed only on localhost or a loopback IP"
        );
        ensure!(
            !api_token.is_empty()
                && api_token.len() <= 8192
                && api_token
                    .chars()
                    .all(|c| c.is_ascii() && !c.is_ascii_whitespace() && !c.is_control()),
            "AUTO_ONTOLOGY_API_TOKEN must be a nonempty API token without whitespace"
        );
        ensure!(
            !url.contains(&api_token)
                && target_db.as_ref().is_none_or(|db| !db.contains(&api_token)),
            "Keep Auto Ontology credentials only in AUTO_ONTOLOGY_API_TOKEN, not the gateway URL or database name"
        );
        ensure!(
            (1..=900).contains(&timeout_secs),
            "AUTO_ONTOLOGY_TIMEOUT must be between 1 and 900 seconds"
        );
        if let Some(db) = &target_db {
            ensure!(
                !db.trim().is_empty() && db.len() <= 255 && !db.chars().any(char::is_control),
                "AUTO_ONTOLOGY_TARGET_DB must be a bounded database name"
            );
        }
        Ok(Self {
            url: parsed.as_str().trim_end_matches('/').into(),
            api_token,
            target_db,
            timeout_secs,
        })
    }

    /// Read explicit environment variables or the Fieldkit integration's local
    /// key/value file. No shell syntax or interpolation is ever evaluated.
    pub fn from_env() -> Result<Option<Self>> {
        let url = env("AUTO_ONTOLOGY_API_URL")?;
        let token = env("AUTO_ONTOLOGY_API_TOKEN")?;
        if url.is_none() && token.is_none() {
            let Some((_, values)) = file_config()? else {
                return Ok(None);
            };
            let url = values.get("AUTO_ONTOLOGY_API_URL").ok_or_else(|| {
                anyhow!("Auto Ontology configuration file requires AUTO_ONTOLOGY_API_URL")
            })?;
            let token = values.get("AUTO_ONTOLOGY_API_TOKEN").ok_or_else(|| {
                anyhow!("Auto Ontology configuration file requires AUTO_ONTOLOGY_API_TOKEN")
            })?;
            let timeout = values
                .get("AUTO_ONTOLOGY_TIMEOUT")
                .map(|s| {
                    s.parse::<u64>().map_err(|_| {
                        anyhow!("AUTO_ONTOLOGY_TIMEOUT must be an integer from 1 to 900")
                    })
                })
                .transpose()?
                .unwrap_or(120);
            return Self::new(
                url,
                token,
                values.get("AUTO_ONTOLOGY_TARGET_DB").cloned(),
                timeout,
            )
            .map(Some);
        }
        let url = url.ok_or_else(|| {
            anyhow!("Set AUTO_ONTOLOGY_API_URL to the Auto Ontology Next.js gateway")
        })?;
        let token = token
            .ok_or_else(|| anyhow!("Set AUTO_ONTOLOGY_API_TOKEN to an Auto Ontology API token"))?;
        let timeout = env("AUTO_ONTOLOGY_TIMEOUT")?
            .map(|s| {
                s.parse::<u64>()
                    .map_err(|_| anyhow!("AUTO_ONTOLOGY_TIMEOUT must be an integer from 1 to 900"))
            })
            .transpose()?
            .unwrap_or(120);
        Self::new(url, token, env("AUTO_ONTOLOGY_TARGET_DB")?, timeout).map(Some)
    }
}

fn env(name: &str) -> Result<Option<String>> {
    match std::env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(std::env::VarError::NotPresent) => Ok(None),
        Err(_) => bail!("{name} must contain valid UTF-8"),
    }
}

fn file_config() -> Result<Option<(PathBuf, BTreeMap<String, String>)>> {
    let explicit = env("FIELDKIT_AUTO_ONTOLOGY_ENV")?;
    let path = explicit
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("integrations/auto-ontology/fieldkit.env"));
    if !path.exists() {
        ensure!(
            explicit.is_none(),
            "FIELDKIT_AUTO_ONTOLOGY_ENV does not name an existing configuration file"
        );
        return Ok(None);
    }
    let info = std::fs::symlink_metadata(&path)
        .map_err(|_| anyhow!("Could not inspect Auto Ontology configuration file"))?;
    ensure!(
        info.file_type().is_file() && info.len() <= 65536,
        "Auto Ontology configuration must be a regular file of at most 64 KiB, without symlinks"
    );
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(&path)
        .map_err(|_| anyhow!("Could not open Auto Ontology configuration file"))?;
    let opened = file
        .metadata()
        .map_err(|_| anyhow!("Could not inspect opened Auto Ontology configuration file"))?;
    ensure!(
        opened.is_file() && opened.len() <= 65536,
        "Auto Ontology configuration exceeds its regular-file limit"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        ensure!(
            info.dev() == opened.dev() && info.ino() == opened.ino(),
            "Auto Ontology configuration changed while opening"
        );
    }
    let mut body = String::new();
    file.take(65537)
        .read_to_string(&mut body)
        .map_err(|_| anyhow!("Auto Ontology configuration must be bounded UTF-8 text"))?;
    ensure!(
        body.len() <= 65536,
        "Auto Ontology configuration exceeds 64 KiB"
    );
    Ok(Some((path, parse_config(&body)?)))
}

fn parse_config(body: &str) -> Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let(key,value)=line.split_once('=').ok_or_else(||anyhow!("Auto Ontology configuration requires plain KEY=value lines; shell syntax is not evaluated"))?;
        let key = key.trim();
        if ![
            "AUTO_ONTOLOGY_API_URL",
            "AUTO_ONTOLOGY_API_TOKEN",
            "AUTO_ONTOLOGY_TARGET_DB",
            "AUTO_ONTOLOGY_TIMEOUT",
            "FIELDKIT_AUTO_ONTOLOGY_PROJECTION",
        ]
        .contains(&key)
        {
            continue;
        }
        let mut value = value.trim();
        if value.starts_with(['\'', '"']) {
            let quote = value.as_bytes()[0];
            ensure!(
                value.len() >= 2 && value.as_bytes().last() == Some(&quote),
                "Auto Ontology configuration contains an unmatched quote"
            );
            value = &value[1..value.len() - 1];
        }
        ensure!(
            !value.contains('\0') && !output.contains_key(key),
            "Auto Ontology configuration contains a duplicate key or NUL byte"
        );
        output.insert(key.to_owned(), value.to_owned());
    }
    Ok(output)
}

/// The read-only SQLite projection is separate from the authoritative case.
/// Relative paths in an explicit configuration file resolve beside that file.
pub fn projection_path() -> Result<PathBuf> {
    if let Some(path) = env("FIELDKIT_AUTO_ONTOLOGY_PROJECTION")? {
        ensure!(
            !path.trim().is_empty() && !path.contains('\0'),
            "FIELDKIT_AUTO_ONTOLOGY_PROJECTION must be a nonempty path"
        );
        return Ok(PathBuf::from(path));
    }
    if let Some((file, values)) = file_config()?
        && let Some(path) = values.get("FIELDKIT_AUTO_ONTOLOGY_PROJECTION")
    {
        ensure!(
            !path.trim().is_empty(),
            "Configured Auto Ontology projection path must not be empty"
        );
        let path = PathBuf::from(path);
        return Ok(if path.is_absolute() {
            path
        } else {
            file.parent().unwrap_or_else(|| Path::new(".")).join(path)
        });
    }
    Ok(PathBuf::from(
        "integrations/auto-ontology/data/fieldkit.sqlite",
    ))
}

#[derive(Clone)]
pub struct NvidiaClient {
    config: NvidiaConfig,
    client: Client,
    async_client: reqwest::Client,
}

impl fmt::Debug for NvidiaClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NvidiaClient")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl NvidiaClient {
    pub fn new(config: NvidiaConfig) -> Result<Self> {
        // Revalidate public struct fields so constructing the struct directly
        // cannot bypass the credential/transport checks.
        let config = NvidiaConfig::new(
            config.url,
            config.api_token,
            config.target_db,
            config.timeout_secs,
        )?;
        let mut headers = HeaderMap::new();
        let mut token = HeaderValue::from_str(&config.api_token)
            .map_err(|_| anyhow!("Auto Ontology API token is not a valid header value"))?;
        token.set_sensitive(true);
        headers.insert("x-api-key", token);
        let client = Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .default_headers(headers)
            .connect_timeout(POLL_TIMEOUT)
            .timeout(POLL_TIMEOUT.min(Duration::from_secs(config.timeout_secs)))
            .user_agent(concat!(
                "Fieldkit/",
                env!("CARGO_PKG_VERSION"),
                " Auto-Ontology integration"
            ))
            .build()
            .map_err(|_| anyhow!("Could not initialize Auto Ontology gateway client"))?;
        let mut headers = HeaderMap::new();
        let mut token = HeaderValue::from_str(&config.api_token)
            .map_err(|_| anyhow!("Auto Ontology API token is not a valid header value"))?;
        token.set_sensitive(true);
        headers.insert("x-api-key", token);
        let async_client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .pool_max_idle_per_host(0)
            .default_headers(headers)
            .connect_timeout(Duration::from_secs(10).min(Duration::from_secs(config.timeout_secs)))
            .timeout(Duration::from_secs(config.timeout_secs))
            .user_agent(concat!(
                "Fieldkit/",
                env!("CARGO_PKG_VERSION"),
                " Auto-Ontology integration"
            ))
            .build()
            .map_err(|_| anyhow!("Could not initialize Auto Ontology streaming client"))?;
        Ok(Self {
            config,
            client,
            async_client,
        })
    }
    pub fn from_env() -> Result<Option<Self>> {
        NvidiaConfig::from_env()?.map(Self::new).transpose()
    }
    pub fn config(&self) -> &NvidiaConfig {
        &self.config
    }
    fn endpoint(&self, path: &str) -> String {
        format!("{}{}", self.config.url, path)
    }

    /// Probe the real gateway and the prerequisites used by NVIDIA's own
    /// readiness tool. A forbidden admin-only connections probe is unverified,
    /// whereas a forbidden chat/compilation probe prevents asking questions.
    pub fn status(&self) -> Result<Value> {
        let mut blockers = vec![];
        let mut unverified = vec![];
        let health = self.probe("/api/health")?;
        let health_status = health
            .value
            .as_ref()
            .and_then(|v| v["status"].as_str())
            .map(|v| safe_text(v, 40, &self.config.api_token))
            .unwrap_or_else(|| "unknown".into());
        let healthy = health.code == Some(200) && health_status == "ok";
        if !healthy {
            blockers.push("Auto Ontology gateway health is not ready; check its application and Postgres services".to_owned());
        }
        let semantic = self.probe("/api/semantic-compilation/status")?;
        let calculated = semantic
            .value
            .as_ref()
            .and_then(|v| v["calculated"].as_bool());
        let running = semantic.value.as_ref().and_then(|v| v["running"].as_bool());
        let last_success = semantic
            .value
            .as_ref()
            .and_then(|v| v["last_success_at"].as_str());
        let last_failure = semantic
            .value
            .as_ref()
            .and_then(|v| v["last_failure_at"].as_str());
        let succeeded = last_success.and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok());
        let failed = last_failure.and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok());
        let bad_failure_marker = semantic.value.as_ref().is_some_and(|v| {
            v.get("last_failure_at")
                .is_some_and(|value| !value.is_null())
                && failed.is_none()
        });
        let failure_after_success = failed
            .as_ref()
            .is_some_and(|failure| succeeded.as_ref().is_none_or(|success| failure >= success));
        let full_compilation_complete = calculated == Some(true)
            && running == Some(false)
            && succeeded.is_some()
            && !failure_after_success
            && !bad_failure_marker;
        if semantic.code == Some(403) {
            blockers.push("This API token lacks chat permission; grant chat/use on the Auto Ontology deployment".into());
        } else if running == Some(true) {
            blockers.push("Auto Ontology is still compiling its semantic layer. Existing terms can make calculated=true before the pass is complete; wait for the full compilation success marker".into());
        } else if running != Some(false) {
            blockers.push("Auto Ontology compilation state could not be verified; wait for a completed, idle semantic compilation before asking questions".into());
        } else if calculated != Some(true) {
            blockers.push("Auto Ontology's semantic layer is not compiled; ingest the case projection and compile it before asking questions".into());
        } else if succeeded.is_none() {
            blockers.push("Auto Ontology has no valid last_success_at marker for a completed full semantic compilation. Partial terms or a manually imported glossary are insufficient; finish compilation before asking questions".into());
        } else if failure_after_success || bad_failure_marker {
            blockers.push("Auto Ontology reports a failed or unverifiable compilation after its last success. Complete a successful full rebuild before asking questions".into());
        }
        let connections = self.probe("/api/connections")?;
        let mut can_execute = connections
            .value
            .as_ref()
            .and_then(|v| v["count"].as_u64())
            .map(|n| n > 0);
        let mut configured_connection = can_execute;
        let mut environment_source = None;
        if connections.code == Some(403) {
            unverified.push("Token cannot inspect database connections (often admin-only); SQL connectivity is unverified".to_owned());
        } else if can_execute == Some(false) {
            let source = self.probe("/api/connections/source")?;
            environment_source = source.value.as_ref().and_then(Value::as_bool);
            if environment_source == Some(true) {
                configured_connection = Some(true);
                can_execute = None;
                unverified.push("The database connection is configured through CONNECTION_STRINGS; live SQL execution remains unverified until the first successful gateway query".into());
            } else {
                blockers.push("No configured Auto Ontology database connection could be verified; configure the read-only Fieldkit projection through CONNECTION_STRINGS or managed connections".into());
            }
        } else if can_execute.is_none() {
            blockers.push("Auto Ontology database connection configuration could not be checked; inspect the configured read-only Fieldkit projection connection".into());
        }
        let catalog = self.probe("/api/datasources/dbs")?;
        let databases = catalog
            .value
            .as_ref()
            .and_then(|v| v["data"].as_array())
            .map(|rows| {
                rows.iter()
                    .take(500)
                    .filter_map(|v| v["name"].as_str())
                    .map(|s| safe_text(s, 255, &self.config.api_token))
                    .collect::<Vec<_>>()
            });
        let catalog_present = databases.as_ref().map(|d| !d.is_empty());
        if catalog.code == Some(403) {
            unverified.push(
                "Token cannot inspect the ingested catalog; target database coverage is unverified"
                    .to_owned(),
            );
        } else if catalog_present != Some(true) {
            blockers.push("Auto Ontology has no verified ingested catalog; ingest the Fieldkit projection before asking questions".into());
        }
        if let (Some(target), Some(dbs)) = (&self.config.target_db, &databases)
            && !dbs.contains(target)
        {
            blockers.push("AUTO_ONTOLOGY_TARGET_DB is not present in the ingested catalog; ingest that projection or correct the configured database name".into());
        }
        let mut output = json!({"provider":"nvidia_auto_ontology","endpoint":self.config.url,"ready":blockers.is_empty(),"health":{"http_status":health.code,"status":health_status},"semantic_layer":{"http_status":semantic.code,"calculated":calculated,"running":running,"last_success_at":last_success,"last_failure_at":last_failure,"full_compilation_complete":full_compilation_complete},"semantic_layer_built":full_compilation_complete,"full_compilation_complete":full_compilation_complete,"can_execute_sql":can_execute,"configured_connection":configured_connection,"environment_connection_source":environment_source,"catalog_present":catalog_present,"databases":databases.unwrap_or_default(),"target_db":self.config.target_db,"blockers":blockers,"unverified":unverified});
        redact(&mut output, &self.config.api_token);
        Ok(output)
    }

    fn probe(&self, path: &str) -> Result<Probe> {
        let response = match self
            .client
            .get(self.endpoint(path))
            .header("Accept", "application/json")
            .send()
        {
            Ok(response) => response,
            Err(_) => {
                return Ok(Probe {
                    code: None,
                    value: None,
                });
            }
        };
        let code = response.status().as_u16();
        if code == 401 {
            return Err(http_error(code));
        }
        if code != 200 {
            return Ok(Probe {
                code: Some(code),
                value: None,
            });
        }
        let mut response = response;
        let started = Instant::now();
        let mut bytes = vec![];
        let mut chunk = [0u8; 8192];
        loop {
            if started.elapsed() >= Duration::from_secs(self.config.timeout_secs.min(10)) {
                return Ok(Probe {
                    code: Some(code),
                    value: None,
                });
            }
            match response.read(&mut chunk) {
                Ok(0) => break,
                Ok(size) => {
                    if bytes.len() + size > MAX_RESPONSE_BYTES {
                        return Ok(Probe {
                            code: Some(code),
                            value: None,
                        });
                    }
                    bytes.extend_from_slice(&chunk[..size]);
                }
                Err(_) => {
                    return Ok(Probe {
                        code: Some(code),
                        value: None,
                    });
                }
            }
        }
        Ok(Probe {
            code: Some(code),
            value: serde_json::from_slice::<Value>(&bytes).ok(),
        })
    }

    /// Ask the gateway's compiled semantic layer. `evidence` must contain only
    /// trusted Fieldkit schema and execution constraints, never raw source text.
    /// The returned rows are observations from Auto Ontology; the caller still
    /// validates candidate IDs against local evidence before applying a plan.
    pub fn ask(
        &self,
        question: &str,
        evidence: &Value,
        stop: &AtomicBool,
        progress: impl Fn(Value),
    ) -> Result<Value> {
        ensure!(
            tokio::runtime::Handle::try_current().is_err(),
            "Call the synchronous Auto Ontology connector from a blocking worker thread"
        );
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| anyhow!("Could not initialize Auto Ontology streaming runtime"))?;
        runtime.block_on(self.ask_stream(question, evidence, stop, progress))
    }

    async fn ask_stream(
        &self,
        question: &str,
        evidence: &Value,
        stop: &AtomicBool,
        progress: impl Fn(Value),
    ) -> Result<Value> {
        ensure!(
            !question.trim().is_empty() && question.len() <= 16000 && !question.contains('\0'),
            "Auto Ontology question must contain 1–16000 characters"
        );
        ensure!(
            evidence.is_object(),
            "Auto Ontology evidence argument must be a trusted Fieldkit schema/constraints object"
        );
        cancelled(stop)?;
        let mut body = json!({"question":question,"evidence":serde_json::to_string(evidence)?,"prediction":false});
        if let Some(db) = &self.config.target_db {
            body["target_db"] = db.clone().into();
        }
        let body = serde_json::to_vec(&body)?;
        ensure!(
            body.len() <= MAX_REQUEST_BYTES,
            "Auto Ontology request exceeds the 256 KiB limit"
        );
        let endpoint = self.endpoint("/api/chat/completions");
        let started = Instant::now();
        let mut response = poll_http(
            self.async_client
                .post(&endpoint)
                .header("Accept", "text/event-stream")
                .header("Content-Type", "application/json")
                .body(body.clone())
                .send(),
            stop,
            started,
            self.config.timeout_secs,
        )
        .await?;
        if response.status().as_u16() != 200 {
            return Err(http_error(response.status().as_u16()));
        }
        let media = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim();
        ensure!(
            media.eq_ignore_ascii_case("text/event-stream"),
            "Auto Ontology chat did not return its documented text/event-stream response; use the public Next.js gateway"
        );
        if let Some(size) = response.content_length() {
            ensure!(
                size <= MAX_RESPONSE_BYTES as u64,
                "Auto Ontology response exceeds the 4 MiB limit"
            );
        }
        let mut parser = SseParser::new(&self.config.api_token);
        let mut received = vec![];
        loop {
            cancelled(stop)?;
            ensure!(
                started.elapsed() < Duration::from_secs(self.config.timeout_secs),
                "Auto Ontology question exceeded AUTO_ONTOLOGY_TIMEOUT; ask a narrower question or increase the configured deadline"
            );
            match poll_http(response.chunk(), stop, started, self.config.timeout_secs).await? {
                None => {
                    parser.finish(&progress)?;
                    break;
                }
                Some(bytes) => {
                    ensure!(
                        received.len() + bytes.len() <= MAX_RESPONSE_BYTES,
                        "Auto Ontology response exceeds the 4 MiB limit"
                    );
                    received.extend_from_slice(&bytes);
                    parser.feed(&bytes, &progress)?;
                    if parser.done {
                        break;
                    }
                }
            }
        }
        cancelled(stop)?;
        ensure!(
            parser.done,
            "Auto Ontology SSE stream ended without [DONE]; partial answers are not applied"
        );
        let answer=parser.answer.ok_or_else(||anyhow!("Auto Ontology completed without a result; check the compiled semantic layer and target database connection"))?;
        let raw_rows = answer.get("sql_response_from_db").unwrap_or(&Value::Null);
        let (rows, row_count) = normalise_rows(raw_rows)?;
        let response_text = answer["response"].as_str().unwrap_or("");
        let sql = answer["sql_code"].as_str().unwrap_or("");
        let mut result = json!({"provider":"nvidia_auto_ontology","endpoint":endpoint,"response":response_text,"sql_code":sql,"sql_response_from_db":rows,"row_count":row_count,"rows_truncated":row_count>MAX_ROWS,"steps":parser.steps,"request_sha256":digest(&body),"response_sha256":digest(&received)});
        redact(&mut result, &self.config.api_token);
        Ok(result)
    }
}

struct Probe {
    code: Option<u16>,
    value: Option<Value>,
}
fn http_error(code: u16) -> anyhow::Error {
    let action = match code {
        301..=399 => {
            "Gateway redirected the request; redirects are disabled to protect the API token. Configure the final public gateway origin directly"
        }
        401 => {
            "API token was rejected. Set AUTO_ONTOLOGY_API_TOKEN to a valid Auto Ontology API token"
        }
        403 => {
            "API token lacks permission. Grant chat/use (and catalog read permissions where needed) on the Auto Ontology deployment"
        }
        409 => {
            "Semantic layer is not compiled or a conversation is already busy. Complete ingestion/semantic compilation and retry after the active run finishes"
        }
        422 => {
            "Gateway rejected the documented chat request schema. Check the deployed Auto Ontology API version and target database configuration"
        }
        429 => "Gateway rate limit reached. Wait before retrying; no automatic retry was performed",
        500..=599 => {
            "Gateway or backend service is unavailable. Check the Auto Ontology application, ingestion worker, database, and model service logs"
        }
        _ => "Gateway rejected the request. Check service configuration and API compatibility",
    };
    anyhow!("NVIDIA Auto Ontology HTTP {code}: {action}")
}
fn cancelled(stop: &AtomicBool) -> Result<()> {
    ensure!(
        !stop.load(Ordering::Relaxed),
        "Auto Ontology request cancelled; partial answers are not applied"
    );
    Ok(())
}
async fn poll_http<T>(
    future: impl std::future::Future<Output = std::result::Result<T, reqwest::Error>>,
    stop: &AtomicBool,
    started: Instant,
    timeout_secs: u64,
) -> Result<T> {
    let mut future = Box::pin(future);
    loop {
        cancelled(stop)?;
        let remaining = Duration::from_secs(timeout_secs).saturating_sub(started.elapsed());
        ensure!(
            !remaining.is_zero(),
            "Auto Ontology request exceeded AUTO_ONTOLOGY_TIMEOUT; no partial result was applied"
        );
        match tokio::time::timeout(Duration::from_millis(100).min(remaining), future.as_mut()).await
        {
            Ok(Ok(value)) => return Ok(value),
            Ok(Err(_)) => bail!(
                "Auto Ontology gateway connection failed or timed out; check service health and AUTO_ONTOLOGY_API_URL. No partial result was applied"
            ),
            Err(_) => continue,
        }
    }
}
fn safe_text(value: &str, max: usize, token: &str) -> String {
    value
        .replace(token, "[REDACTED]")
        .chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .take(max)
        .collect()
}
fn redact(value: &mut Value, token: &str) {
    match value {
        Value::String(s) => *s = s.replace(token, "[REDACTED]"),
        Value::Array(a) => {
            for item in a {
                redact(item, token);
            }
        }
        Value::Object(object) => {
            object.retain(|key, _| {
                !matches!(
                    key.to_ascii_lowercase().as_str(),
                    "thought" | "thoughts" | "authorization" | "x-api-key" | "api_token"
                )
            });
            for item in object.values_mut() {
                redact(item, token);
            }
        }
        _ => {}
    }
}

fn normalise_rows(raw: &Value) -> Result<(Vec<Value>, usize)> {
    let decoded = if let Some(items) = raw.as_array().filter(|a| a.len() == 1 && a[0].is_string()) {
        Some(
            serde_json::from_str::<Value>(items[0].as_str().unwrap()).map_err(|_| {
                anyhow!("Auto Ontology database rows used an unsupported JSON-records format")
            })?,
        )
    } else {
        None
    };
    let raw = decoded.as_ref().unwrap_or(raw);
    let values = if raw.is_null() {
        vec![]
    } else if let Some(array) = raw.as_array() {
        ensure!(
            array.iter().all(Value::is_object),
            "Auto Ontology database rows must be JSON record objects"
        );
        array.clone()
    } else if raw.is_object() {
        vec![raw.clone()]
    } else {
        bail!("Auto Ontology database rows used an unsupported response shape");
    };
    let count = values.len();
    Ok((values.into_iter().take(MAX_ROWS).collect(), count))
}

struct SseParser<'a> {
    pending: Vec<u8>,
    event_data: String,
    steps: Vec<Value>,
    answer: Option<Value>,
    done: bool,
    token: &'a str,
}
impl<'a> SseParser<'a> {
    fn new(token: &'a str) -> Self {
        Self {
            pending: vec![],
            event_data: String::new(),
            steps: vec![],
            answer: None,
            done: false,
            token,
        }
    }
    fn feed(&mut self, bytes: &[u8], progress: &impl Fn(Value)) -> Result<()> {
        self.pending.extend_from_slice(bytes);
        while let Some(end) = self.pending.iter().position(|b| *b == b'\n') {
            ensure!(
                end <= MAX_LINE_BYTES,
                "Auto Ontology SSE line exceeds the bounded line limit"
            );
            let line = self.pending.drain(..=end).collect::<Vec<_>>();
            let line = &line[..line.len() - 1];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            let line = std::str::from_utf8(line)
                .map_err(|_| anyhow!("Auto Ontology SSE contains invalid UTF-8"))?;
            self.line(line.trim_start_matches('\u{feff}'), progress)?;
            if self.done {
                break;
            }
        }
        ensure!(
            self.pending.len() <= MAX_LINE_BYTES,
            "Auto Ontology SSE line exceeds the bounded line limit"
        );
        Ok(())
    }
    fn line(&mut self, line: &str, progress: &impl Fn(Value)) -> Result<()> {
        if line.is_empty() {
            return self.event(progress);
        }
        if line.starts_with(':') {
            return Ok(());
        }
        if let Some(data) = line.strip_prefix("data:") {
            let data = data.strip_prefix(' ').unwrap_or(data);
            ensure!(
                self.event_data.len() + data.len() < MAX_EVENT_BYTES,
                "Auto Ontology SSE event exceeds the bounded event limit"
            );
            if !self.event_data.is_empty() {
                self.event_data.push('\n');
            }
            self.event_data.push_str(data);
        }
        Ok(())
    }
    fn finish(&mut self, progress: &impl Fn(Value)) -> Result<()> {
        if !self.pending.is_empty() {
            let bytes = std::mem::take(&mut self.pending);
            let line = std::str::from_utf8(&bytes)
                .map_err(|_| anyhow!("Auto Ontology SSE contains invalid UTF-8"))?;
            self.line(line.trim_end_matches('\r'), progress)?;
        }
        self.event(progress)
    }
    fn event(&mut self, progress: &impl Fn(Value)) -> Result<()> {
        if self.event_data.is_empty() {
            return Ok(());
        }
        let data = std::mem::take(&mut self.event_data);
        if data.trim() == "[DONE]" {
            self.done = true;
            return Ok(());
        }
        let event: Value = serde_json::from_str(&data).map_err(|_| {
            anyhow!("Auto Ontology SSE contained malformed event JSON; no result was applied")
        })?;
        ensure!(
            event.is_object(),
            "Auto Ontology SSE event must be a JSON object"
        );
        match event["type"].as_str() {
            Some("step") => {
                let node = event["node"].as_str().unwrap_or("working");
                let node = node
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ':'))
                    .take(100)
                    .collect::<String>();
                let phase = match event["phase"].as_str() {
                    Some("start") => "start",
                    Some("end") => "end",
                    _ => "progress",
                };
                let summary =
                    json!({"type":"step","node":safe_text(&node,100,self.token),"phase":phase});
                if self.steps.len() < MAX_STEPS {
                    self.steps.push(summary.clone());
                }
                progress(summary);
            }
            Some("sql") => progress(json!({"type":"sql","phase":"validated"})),
            Some("result") => {
                ensure!(
                    self.answer.is_none(),
                    "Auto Ontology returned multiple final results; no result was applied"
                );
                let mut answer = event["answer"].clone();
                ensure!(
                    answer.is_object(),
                    "Auto Ontology result lacks its documented answer object"
                );
                if let Some(object) = answer.as_object_mut() {
                    object.remove("thoughts");
                    object.remove("thought");
                }
                redact(&mut answer, self.token);
                self.answer = Some(answer);
                progress(json!({"type":"result","phase":"received"}));
            }
            Some("error") => bail!(
                "Auto Ontology reported a streamed agent error; check the semantic layer, target connection, and gateway logs. No result was applied"
            ),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_gateway_transport_and_redacts_config_debug() {
        for url in [
            "http://example.org",
            "http://192.168.1.1:3000",
            "http://169.254.169.254",
            "https://user:pass@example.org",
            "https://example.org/?token=x",
            "https://example.org/#fragment",
            "file:///tmp/service",
        ] {
            assert!(NvidiaConfig::new(url, "secret", None, 120).is_err());
        }
        let config = NvidiaConfig::new("http://127.0.0.1:3000", "secret", None, 120).unwrap();
        assert!(!format!("{config:?}").contains("secret"));
        assert!(NvidiaConfig::new("https://example.org", "secret\nheader", None, 120).is_err());
    }
    #[test]
    fn fragmented_multiline_sse_preserves_public_result_and_discards_thoughts() {
        let bytes=b": ping\r\n\r\ndata: {\"type\":\"step\",\r\ndata: \"node\":\"extract\",\"phase\":\"end\",\"thought\":\"hidden chain\"}\r\n\r\ndata: {\"type\":\"result\",\"answer\":{\"response\":\"public answer\",\"thoughts\":\"hidden chain\",\"sql_code\":\"SELECT 1\",\"sql_response_from_db\":[{\"id\":1}]}}\n\ndata: [DONE]\n\n";
        let mut parser = SseParser::new("secret");
        for chunk in bytes.chunks(7) {
            parser.feed(chunk, &|_| {}).unwrap();
        }
        assert!(parser.done);
        assert_eq!(parser.answer.as_ref().unwrap()["response"], "public answer");
        let retained = json!({"answer":parser.answer,"steps":parser.steps}).to_string();
        assert!(!retained.contains("hidden chain"));
    }
    #[test]
    fn rows_support_upstream_json_records_wrapping() {
        let (rows, count) = normalise_rows(&json!(["[{\"job_id\":\"job_1\"}]"])).unwrap();
        assert_eq!(count, 1);
        assert_eq!(rows[0]["job_id"], "job_1");
        assert!(normalise_rows(&json!(["unparseable"])).is_err());
    }
    #[test]
    fn config_file_values_are_literals_and_duplicate_keys_fail() {
        let config=parse_config("# Local service\nAUTO_ONTOLOGY_API_URL=http://127.0.0.1:18760\nAUTO_ONTOLOGY_API_TOKEN='$(echo never-executed)'\nAUTO_ONTOLOGY_TARGET_DB=fieldkit\nFIELDKIT_AUTO_ONTOLOGY_PROJECTION=data/fieldkit.sqlite\n").unwrap();
        assert_eq!(config["AUTO_ONTOLOGY_API_TOKEN"], "$(echo never-executed)");
        assert_eq!(
            config["FIELDKIT_AUTO_ONTOLOGY_PROJECTION"],
            "data/fieldkit.sqlite"
        );
        assert!(
            parse_config("AUTO_ONTOLOGY_API_TOKEN=first\nAUTO_ONTOLOGY_API_TOKEN=second").is_err()
        );
        assert!(parse_config("AUTO_ONTOLOGY_API_TOKEN='unmatched").is_err());
    }
}
