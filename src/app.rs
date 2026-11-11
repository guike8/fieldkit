//! Loopback-only native HTTP application, sharing the Rust case engine.
use crate::{
    Collected, Workbench, api, collectors,
    mcp::{scoped_operation, scoped_path},
    model::{digest, id, now, text},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::Read,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    thread,
};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

const INDEX: &str = include_str!("../assets/index.html");
const SCRIPT: &str = include_str!("../assets/app.js");
const STYLES: &str = include_str!("../assets/styles.css");
const MAX_BODY: usize = 65536;
const MAX_ATTACHMENT: usize = 4 * 1024 * 1024;
type QueuedJob = (String, String, Value);

struct Application {
    model: Workbench,
    workspace: PathBuf,
    token: String,
    jobs: Mutex<BTreeMap<String, Value>>,
    queue: mpsc::SyncSender<QueuedJob>,
}

struct Reply {
    status: u16,
    media: &'static str,
    body: Vec<u8>,
    attachment: Option<String>,
}
impl Reply {
    fn json(value: Value, status: u16) -> Self {
        Self {
            status,
            media: "application/json; charset=utf-8",
            body: value.to_string().into_bytes(),
            attachment: None,
        }
    }
    fn bytes(body: Vec<u8>, media: &'static str) -> Self {
        Self {
            status: 200,
            media,
            body,
            attachment: None,
        }
    }
    fn error(message: &str, status: u16) -> Self {
        Self::json(
            json!({"ok":false,"error":{"code":"APP_REQUEST","message":message,"retryable":false}}),
            status,
        )
    }
}

fn header<'a>(request: &'a Request, name: &str) -> Option<&'a str> {
    request
        .headers()
        .iter()
        .find(|header| header.field.as_str().as_str().eq_ignore_ascii_case(name))
        .map(|header| header.value.as_str())
}
fn same_token(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |different, (a, b)| different | (a ^ b))
        == 0
}
fn guard(request: &Request, port: u16) -> Result<()> {
    let allowed = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    ensure!(
        request
            .headers()
            .iter()
            .filter(|header| header.field.equiv("Host"))
            .count()
            == 1,
        "Supply exactly one local Host header"
    );
    let host = header(request, "Host").unwrap_or("").to_ascii_lowercase();
    ensure!(
        allowed.contains(&host),
        "Unrecognized local application host"
    );
    let origins: Vec<_> = request
        .headers()
        .iter()
        .filter(|header| header.field.equiv("Origin"))
        .collect();
    ensure!(origins.len() <= 1, "Supply at most one Origin header");
    if let Some(origin) = origins.first() {
        ensure!(
            allowed
                .iter()
                .any(|host| origin.value.as_str() == format!("http://{host}")),
            "Cross-origin application requests are not supported"
        );
    }
    ensure!(
        !matches!(
            header(request, "Sec-Fetch-Site"),
            Some("cross-site" | "same-site")
        ),
        "Open the application directly on its local origin"
    );
    Ok(())
}

impl Application {
    fn new(model: Workbench, workspace: PathBuf) -> Result<Arc<Self>> {
        let workspace = workspace.canonicalize().context("Workspace must exist")?;
        ensure!(workspace.is_dir(), "Workspace must be a directory");
        scoped_path(&workspace, &model.store.path.to_string_lossy(), true)?;
        let (queue, receiver) = mpsc::sync_channel::<QueuedJob>(8);
        let application = Arc::new(Self {
            model,
            workspace,
            token: format!(
                "{}{}",
                uuid::Uuid::new_v4().simple(),
                uuid::Uuid::new_v4().simple()
            ),
            jobs: Mutex::new(BTreeMap::new()),
            queue,
        });
        let receiver = Arc::new(Mutex::new(receiver));
        for _ in 0..2 {
            let application = Arc::downgrade(&application);
            let receiver = Arc::clone(&receiver);
            thread::spawn(move || {
                loop {
                    let message = match receiver.lock() {
                        Ok(receiver) => receiver.recv(),
                        Err(_) => return,
                    };
                    let Ok((identifier, operation, payload)) = message else {
                        break;
                    };
                    let Some(app) = application.upgrade() else {
                        break;
                    };
                    if let Ok(mut jobs) = app.jobs.lock()
                        && let Some(job) = jobs.get_mut(&identifier)
                    {
                        job["status"] = json!("running");
                        job["started_at"] = json!(now());
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let payload =
                            scoped_operation(&app.workspace, &app.model, &operation, &payload)?;
                        api::apply(&app.model, &operation, &payload)
                    }));
                    if let Ok(mut jobs) = app.jobs.lock()
                        && let Some(job) = jobs.get_mut(&identifier)
                    {
                        job["completed_at"] = json!(now());
                        match result {
                            Ok(Ok(record)) => {
                                let status = text(&record, "status");
                                job["status"] = json!(if matches!(
                                    status,
                                    "error"
                                        | "failed"
                                        | "invalid"
                                        | "blocked"
                                        | "rate_limited"
                                        | "unavailable"
                                ) {
                                    "failed"
                                } else {
                                    "completed"
                                });
                                job["run_id"] =
                                    record.get("run_id").cloned().unwrap_or(Value::Null);
                                job["result_status"] =
                                    record.get("status").cloned().unwrap_or(Value::Null);
                                job["errors"] = record.get("errors").cloned().unwrap_or(json!([]));
                            }
                            Ok(Err(error)) => {
                                job["status"] = json!("failed");
                                job["error"] =
                                    json!({"code":"JOB_ERROR","message":format!("{error:#}")});
                            }
                            Err(_) => {
                                job["status"] = json!("failed");
                                job["error"] = json!({"code":"JOB_ERROR","message":"Collector job stopped unexpectedly; inspect and resume the saved case"});
                            }
                        }
                    }
                }
            });
        }
        Ok(application)
    }

    fn enqueue(&self, operation: &str, payload: Value) -> Result<Value> {
        let mut jobs = self
            .jobs
            .lock()
            .map_err(|_| anyhow::anyhow!("Job queue is unavailable"))?;
        ensure!(
            jobs.values()
                .filter(|job| matches!(text(job, "status"), "queued" | "running"))
                .count()
                < 8,
            "Wait for a running collector before adding another"
        );
        if jobs.len() >= 100 {
            let removable = jobs
                .iter()
                .find(|(_, job)| !matches!(text(job, "status"), "queued" | "running"))
                .map(|(id, _)| id.clone());
            if let Some(id) = removable {
                jobs.remove(&id);
            }
        }
        let identifier = id("job");
        let job = json!({"id":identifier,"operation":operation,"status":"queued","command":payload.get("command"),"target":payload.get("target"),"created_at":now()});
        jobs.insert(identifier.clone(), job);
        if self
            .queue
            .try_send((identifier.clone(), operation.into(), payload))
            .is_err()
        {
            jobs.remove(&identifier);
            bail!("Collector queue is full; try again after a result arrives");
        }
        drop(jobs);
        Ok(json!({"ok":true,"job_id":identifier,"state":self.model.snapshot()?}))
    }

    fn get(&self, path: &str, query: &str) -> Result<Reply> {
        match path {
            "/api/session" => Ok(Reply::json(
                json!({"csrf_token":self.token,"mode":"local","runtime":"rust"}),
                200,
            )),
            "/api/state" => Ok(Reply::json(self.model.snapshot()?, 200)),
            "/api/capabilities" => Ok(Reply::json(collectors::capabilities(), 200)),
            "/api/jobs" => Ok(Reply::json(
                json!({"jobs":self.jobs.lock().map_err(|_|anyhow::anyhow!("Job queue unavailable"))?.values().cloned().collect::<Vec<_>>()}),
                200,
            )),
            "/api/ontology" => Ok(Reply::json(self.model.ontology_snapshot()?, 200)),
            "/api/discovery" => Ok(Reply::json(
                json!({"sessions":self.model.discoveries()?,"frontier":self.model.store.list("discovery_jobs")?}),
                200,
            )),
            "/api/export" => {
                let format = url::form_urlencoded::parse(query.as_bytes())
                    .find(|(key, _)| key == "format")
                    .map(|(_, value)| value.into_owned())
                    .unwrap_or("json".into());
                ensure!(
                    matches!(format.as_str(), "json" | "markdown"),
                    "Export format must be json or markdown"
                );
                let mut reply = Reply::bytes(
                    self.model.export(&format)?.into_bytes(),
                    if format == "json" {
                        "application/json"
                    } else {
                        "text/markdown; charset=utf-8"
                    },
                );
                reply.attachment = Some(format!(
                    "fieldkit-case.{}",
                    if format == "json" { "json" } else { "md" }
                ));
                Ok(reply)
            }
            "/" | "/index.html" => Ok(Reply::bytes(
                INDEX.as_bytes().to_vec(),
                "text/html; charset=utf-8",
            )),
            "/app.js" => Ok(Reply::bytes(
                SCRIPT.as_bytes().to_vec(),
                "text/javascript; charset=utf-8",
            )),
            "/styles.css" => Ok(Reply::bytes(
                STYLES.as_bytes().to_vec(),
                "text/css; charset=utf-8",
            )),
            _ if path.starts_with("/api/artifacts/") => {
                let hash = path.trim_start_matches("/api/artifacts/");
                ensure!(
                    hash.len() == 64
                        && hash
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c)),
                    "Invalid artifact hash"
                );
                let path = self.model.store.path.join("blobs").join(hash);
                let metadata = fs::symlink_metadata(&path)?;
                ensure!(
                    metadata.is_file()
                        && !metadata.file_type().is_symlink()
                        && metadata.len() <= 32 * 1024 * 1024,
                    "Artifact is not a bounded regular file"
                );
                let mut options = fs::OpenOptions::new();
                options.read(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    #[cfg(target_os = "macos")]
                    options.custom_flags(0x100 | 0x4); // O_NOFOLLOW | O_NONBLOCK
                    #[cfg(not(target_os = "macos"))]
                    options.custom_flags(0x20000 | 0x800);
                }
                let file = options.open(&path)?;
                let opened = file.metadata()?;
                ensure!(
                    opened.is_file() && opened.len() <= 32 * 1024 * 1024,
                    "Artifact changed before reading"
                );
                #[cfg(unix)]
                {
                    use std::os::unix::fs::MetadataExt;
                    ensure!(
                        opened.nlink() == 1
                            && metadata.dev() == opened.dev()
                            && metadata.ino() == opened.ino(),
                        "Artifact changed or is hard linked"
                    );
                }
                let mut bytes = Vec::new();
                file.take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
                ensure!(
                    bytes.len() <= 32 * 1024 * 1024 && digest(&bytes) == hash,
                    "Artifact does not match its stored hash"
                );
                let mut reply = Reply::bytes(bytes, "application/octet-stream");
                reply.attachment = Some(format!("{hash}.bin"));
                Ok(reply)
            }
            _ => Ok(Reply::error("Route not found", 404)),
        }
    }

    fn post(&self, request: &mut Request, path: &str) -> Result<Reply> {
        ensure!(
            header(request, "Transfer-Encoding").is_none(),
            "Use a bounded Content-Length request body"
        );
        ensure!(
            header(request, "Content-Type").and_then(|v| v.split(';').next())
                == Some("application/json"),
            "Application requests must use JSON"
        );
        let limit = if path == "/api/attachments" {
            6 * 1024 * 1024
        } else {
            MAX_BODY
        };
        let length = header(request, "Content-Length")
            .context("Content-Length is required")?
            .parse::<usize>()?;
        ensure!(
            length > 0 && length <= limit,
            "Request exceeds the operation's size limit"
        );
        let mut bytes = Vec::with_capacity(length);
        request
            .as_reader()
            .take((length + 1) as u64)
            .read_to_end(&mut bytes)?;
        ensure!(bytes.len() == length, "Incomplete request body");
        let payload: Value =
            serde_json::from_slice(&bytes).context("Request must contain valid JSON")?;
        if path == "/api/attachments" {
            let record = self.import_attachment(&payload)?;
            return Ok(Reply::json(
                json!({"ok":true,"record":record,"state":self.model.snapshot()?}),
                200,
            ));
        }
        let operation = path
            .strip_prefix("/api/")
            .context("Unknown operation route")?;
        ensure!(
            api::OPERATIONS.iter().any(|(name, _)| *name == operation),
            "Unknown application operation"
        );
        let payload = scoped_operation(&self.workspace, &self.model, operation, &payload)?;
        if matches!(operation, "tools/run" | "flows/run" | "discovery/run") {
            return Ok(Reply::json(self.enqueue(operation, payload)?, 202));
        }
        let record = api::apply(&self.model, operation, &payload)?;
        Ok(Reply::json(
            json!({"ok":true,"record":record,"state":self.model.snapshot()?}),
            200,
        ))
    }

    fn handle(&self, request: &mut Request, port: u16) -> Reply {
        if let Err(error) = guard(request, port) {
            return Reply::error(&error.to_string(), 403);
        }
        if request.method() == &Method::Post
            && !same_token(
                header(request, "X-Fieldkit-Token").unwrap_or(""),
                &self.token,
            )
        {
            return Reply::error("Refresh the application before submitting a change", 403);
        }
        let url = request.url().to_owned();
        let (path, query) = url.split_once('?').unwrap_or((&url, ""));
        let result = match *request.method() {
            Method::Get => self.get(path, query),
            Method::Post => self.post(request, path),
            _ => Ok(Reply::error("Method not allowed", 405)),
        };
        result.unwrap_or_else(|error| Reply::error(&format!("{error:#}"), 400))
    }

    fn import_attachment(&self, payload: &Value) -> Result<Value> {
        api::validate(
            payload,
            &json!({"type":"object","required":["name","media_type","content_base64"],"additionalProperties":false,
            "properties":{"name":{"type":"string","minLength":1,"maxLength":255},"media_type":{"type":"string","minLength":1,"maxLength":100},"content_base64":{"type":"string","maxLength":6*1024*1024}}}),
        )?;
        let name = text(payload, "name")
            .replace('\\', "/")
            .rsplit('/')
            .next()
            .unwrap_or("")
            .to_owned();
        ensure!(
            !name.is_empty() && name != "." && name != ".." && !name.chars().any(char::is_control),
            "Invalid attachment filename"
        );
        let media = text(payload, "media_type");
        ensure!(
            !media.chars().any(char::is_control),
            "Invalid attachment media type"
        );
        let body = decode_base64(text(payload, "content_base64"))?;
        ensure!(
            body.len() <= MAX_ATTACHMENT,
            "Attachments are limited to 4 MiB"
        );
        let hash = digest(&body);
        let identifier = id("evidence");
        let run = id("run");
        let source = id("src");
        let time = now();
        let sources = json!([{"source_id":source,"name":name,"kind":"analyst_supplied","collection_method":"manual_import","sha256":hash,"bytes":body.len(),"media_type":media,"observed_at":time,"hash_scope":"source_bytes","untrusted_content":true}]);
        let record = json!({"id":identifier,"title":name,"kind":"attachment","content":format!("Imported attachment: {name}"),"filename":name,"source_url":"","status":"unreviewed","review_note":"","run_id":run,"source_ids":[source],"sources":sources,"source_count":1,"sha256":hash,"artifact_sha256":hash,"hash_scope":"source_bytes","collection_method":"analyst_supplied","bytes":body.len(),"media_type":media,"collected_at":time,"created_at":time});
        let report = json!({"schema_version":"1.0","tool":"fieldkit","version":env!("CARGO_PKG_VERSION"),"run_id":run,"command":"evidence.import","target":name,"status":"ok","data":{"evidence_id":identifier,"title":name,"filename":name},"sources":sources,"entities":[],"relationships":[],"errors":[],"timing":{"started_at":time,"completed_at":time},"untrusted_content":true});
        self.model.store.save_report(&Collected {
            report,
            blobs: BTreeMap::from([(hash, body)]),
        })?;
        self.model.store.put("evidence", &record)?;
        self.model.store.event(
            "evidence.added",
            "Attachment preserved",
            json!({"evidence_id":identifier,"sha256":record["sha256"]}),
        )?;
        Ok(record)
    }
}

fn decode_base64(value: &str) -> Result<Vec<u8>> {
    ensure!(
        value.len().is_multiple_of(4) && value.len() <= 6 * 1024 * 1024,
        "Invalid base64 attachment size"
    );
    let mut output = Vec::with_capacity(value.len() / 4 * 3);
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for (index, chunk) in value.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let mut numbers = [0u32; 4];
        let mut padding = 0;
        for (i, byte) in chunk.iter().enumerate() {
            if *byte == b'=' {
                padding += 1;
                ensure!(
                    i >= 2 && index == value.len() / 4 - 1,
                    "Invalid base64 padding"
                );
            } else {
                ensure!(padding == 0, "Invalid base64 padding");
                numbers[i] = alphabet
                    .iter()
                    .position(|candidate| candidate == byte)
                    .context("Invalid base64 character")? as u32;
            }
        }
        ensure!(
            padding <= 2
                && (padding != 2 || numbers[1] & 15 == 0)
                && (padding != 1 || numbers[2] & 3 == 0),
            "Invalid base64 padding bits"
        );
        let bits = (numbers[0] << 18) | (numbers[1] << 12) | (numbers[2] << 6) | numbers[3];
        output.push((bits >> 16) as u8);
        if padding < 2 {
            output.push((bits >> 8) as u8);
        }
        if padding == 0 {
            output.push(bits as u8);
        }
    }
    Ok(output)
}

fn respond(request: Request, reply: Reply) {
    let mut response = Response::from_data(reply.body).with_status_code(StatusCode(reply.status));
    for (name, value) in [
        ("Content-Type", reply.media),
        ("Cache-Control", "no-store"),
        ("X-Content-Type-Options", "nosniff"),
        ("Referrer-Policy", "no-referrer"),
        (
            "Content-Security-Policy",
            "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
        ),
    ] {
        if let Ok(header) = Header::from_bytes(name.as_bytes(), value.as_bytes()) {
            response.add_header(header);
        }
    }
    if let Some(name) = reply.attachment
        && let Ok(header) = Header::from_bytes(
            b"Content-Disposition",
            format!("attachment; filename=\"{name}\"").as_bytes(),
        )
    {
        response.add_header(header);
    }
    let _ = request.respond(response);
}

pub fn serve(model: Workbench, workspace: PathBuf, port: u16) -> Result<()> {
    ensure!(port > 0, "Application port must be between 1 and 65535");
    let app = Application::new(model, workspace)?;
    let server = Server::http((std::net::Ipv4Addr::LOCALHOST, port))
        .map_err(|error| anyhow::anyhow!("Could not bind local application: {error}"))?;
    eprintln!("Fieldkit Rust app: http://127.0.0.1:{port}/");
    let active = Arc::new(AtomicUsize::new(0));
    for mut request in server.incoming_requests() {
        if active.fetch_add(1, Ordering::Relaxed) >= 16 {
            active.fetch_sub(1, Ordering::Relaxed);
            respond(
                request,
                Reply::error("Application is busy; retry shortly", 503),
            );
            continue;
        }
        let app = Arc::clone(&app);
        let active = Arc::clone(&active);
        thread::spawn(move || {
            let reply = app.handle(&mut request, port);
            respond(request, reply);
            active.fetch_sub(1, Ordering::Relaxed);
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Config;
    fn app() -> (tempfile::TempDir, Arc<Application>) {
        let directory = tempfile::tempdir().unwrap();
        let model = Workbench::new(directory.path().join("case"), Config::default()).unwrap();
        let app = Application::new(model, directory.path().into()).unwrap();
        (directory, app)
    }
    #[test]
    fn strict_base64_accepts_binary_and_rejects_invalid_padding() {
        assert_eq!(decode_base64("AAECAw==").unwrap(), [0, 1, 2, 3]);
        assert!(decode_base64("").unwrap().is_empty());
        for bad in ["Zg=", "Zh==", "Zm9=", "====", "AA=A", "AA==AAAA", "Zm 8"] {
            assert!(decode_base64(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn native_app_serves_shared_state_and_ontology() {
        let (_directory, app) = app();
        for path in ["/api/state", "/api/ontology", "/api/discovery"] {
            let reply = app.get(path, "").unwrap();
            assert_eq!(reply.status, 200);
            assert!(
                serde_json::from_slice::<Value>(&reply.body)
                    .unwrap()
                    .is_object()
            );
        }
        assert!(
            String::from_utf8(app.get("/", "").unwrap().body)
                .unwrap()
                .contains("Investigation workspace")
        );
    }
    #[test]
    fn attachment_bytes_are_hashed_and_preserved() {
        let (_directory, app) = app();
        let record=app.import_attachment(&json!({"name":"capture.bin","media_type":"application/octet-stream","content_base64":"AAECAw=="})).unwrap();
        let hash = text(&record, "sha256");
        let reply = app.get(&format!("/api/artifacts/{hash}"), "").unwrap();
        assert_eq!(reply.body, [0, 1, 2, 3]);
        assert_eq!(app.model.store.verify().unwrap()["ok"], true);
    }
    #[test]
    fn source_bytes_cannot_escape_artifact_addressing() {
        let (_directory, app) = app();
        assert!(app.get("/api/artifacts/../../etc/passwd", "").is_err());
        assert!(app.get("/api/export", "format=html").is_err());
    }
    #[test]
    fn token_comparison_requires_exact_value() {
        assert!(same_token("abc", "abc"));
        assert!(!same_token("abc", "abd"));
        assert!(!same_token("abc", "ab"));
    }

    fn request(server: &Server, raw: &str) -> (std::net::TcpStream, Request) {
        use std::io::Write;
        let mut client =
            std::net::TcpStream::connect(server.server_addr().to_ip().unwrap()).unwrap();
        client.write_all(raw.as_bytes()).unwrap();
        let request = server
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .expect("HTTP request");
        (client, request)
    }

    #[test]
    fn host_origin_and_csrf_guards_apply_to_real_http_requests() {
        let (_directory, app) = app();
        let server = Server::http((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        for headers in [
            format!("Host: attacker.invalid:{port}"),
            format!("Host: 127.0.0.1:{port}\r\nOrigin: https://attacker.invalid"),
            format!("Host: 127.0.0.1:{port}\r\nSec-Fetch-Site: cross-site"),
        ] {
            let (_client, mut request) = request(
                &server,
                &format!("GET /api/session HTTP/1.1\r\n{headers}\r\n\r\n"),
            );
            assert_eq!(app.handle(&mut request, port).status, 403);
        }
        let (_client, mut good) = request(
            &server,
            &format!(
                "GET /api/session HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\n\r\n"
            ),
        );
        assert_eq!(app.handle(&mut good, port).status, 200);
        let body = "{\"title\":\"Changed through HTTP\"}";
        let (_client, mut denied) = request(
            &server,
            &format!(
                "POST /api/case HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        );
        assert_eq!(app.handle(&mut denied, port).status, 403);
        assert_ne!(
            app.model.store.meta().unwrap()["title"],
            "Changed through HTTP"
        );
        let (_client, mut allowed) = request(
            &server,
            &format!(
                "POST /api/case HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nX-Fieldkit-Token: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                app.token,
                body.len()
            ),
        );
        assert_eq!(app.handle(&mut allowed, port).status, 200);
        assert_eq!(
            app.model.store.meta().unwrap()["title"],
            "Changed through HTTP"
        );
    }

    #[test]
    fn case_must_be_inside_workspace_before_app_threads_or_listener_start() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let model = Workbench::new(outside.path().join("case"), Config::default()).unwrap();
        assert!(Application::new(model, workspace.path().into()).is_err());
    }
}
