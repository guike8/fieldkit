//! Real HTTP mock gateway tests; no NVIDIA credentials or internet access.
use fieldkit::nvidia::{NvidiaClient, NvidiaConfig};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

const TOKEN: &str = "fieldkit-test-api-token";
#[derive(Clone)]
struct Reply {
    code: u16,
    media: &'static str,
    parts: Vec<(u64, Vec<u8>)>,
}
impl Reply {
    fn json(code: u16, value: Value) -> Self {
        Self {
            code,
            media: "application/json",
            parts: vec![(0, serde_json::to_vec(&value).unwrap())],
        }
    }
    fn sse(value: &str) -> Self {
        Self {
            code: 200,
            media: "text/event-stream",
            parts: value
                .as_bytes()
                .chunks(7)
                .map(|c| (0, c.to_vec()))
                .collect(),
        }
    }
}
#[derive(Debug)]
struct Seen {
    path: String,
    headers: Vec<(String, String)>,
    body: Value,
}
struct Gateway {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    join: Option<JoinHandle<()>>,
}
impl Gateway {
    fn new(replies: Vec<Reply>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(vec![]));
        let observed = seen.clone();
        let join = thread::spawn(move || {
            for reply in replies {
                let (stream, _) = listener.accept().unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                let mut headers = vec![];
                let mut length = 0;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    if let Some((key, value)) = line.trim().split_once(':') {
                        if key.eq_ignore_ascii_case("content-length") {
                            length = value.trim().parse::<usize>().unwrap();
                        }
                        headers.push((key.to_ascii_lowercase(), value.trim().to_owned()));
                    }
                }
                let mut bytes = vec![0u8; length];
                reader.read_exact(&mut bytes).unwrap();
                let body = if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                };
                observed.lock().unwrap().push(Seen {
                    path,
                    headers,
                    body,
                });
                write_reply(stream, reply);
            }
        });
        Self {
            url,
            seen,
            join: Some(join),
        }
    }
    fn client(&self, timeout: u64) -> NvidiaClient {
        NvidiaClient::new(
            NvidiaConfig::new(&self.url, TOKEN, Some("fieldkit".into()), timeout).unwrap(),
        )
        .unwrap()
    }
    fn finish(mut self) -> Vec<Seen> {
        self.join.take().unwrap().join().unwrap();
        Arc::try_unwrap(self.seen).unwrap().into_inner().unwrap()
    }
}
fn write_reply(mut stream: TcpStream, reply: Reply) {
    let size = reply
        .parts
        .iter()
        .map(|(_, bytes)| bytes.len())
        .sum::<usize>();
    let header = format!(
        "HTTP/1.1 {} Mock\r\nContent-Type: {}\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n",
        reply.code, reply.media
    );
    if stream.write_all(header.as_bytes()).is_err() {
        return;
    }
    let _ = stream.flush();
    for (delay, bytes) in reply.parts {
        if delay > 0 {
            thread::sleep(Duration::from_millis(delay));
        }
        if stream.write_all(&bytes).is_err() {
            break;
        }
        let _ = stream.flush();
    }
}
fn answer() -> String {
    "data: {\"type\":\"step\",\n\rdata: \"node\":\"candidates_retrieval\",\"phase\":\"end\",\"thought\":\"PRIVATE THOUGHT\"}\n\ndata: {\"type\":\"sql\",\"sql\":\"SELECT job_id FROM candidate_jobs\"}\n\ndata: {\"type\":\"result\",\"answer\":{\"response\":\"Inspect the cited profile next\",\"sql_code\":\"SELECT job_id FROM candidate_jobs\",\"sql_response_from_db\":[\"[{\\\"job_id\\\":\\\"job_1\\\"}]\"],\"thoughts\":\"PRIVATE THOUGHT\"}}\n\ndata: [DONE]\n\n".to_string().replace("\n\rdata:","\ndata:")
}

#[test]
fn calls_actual_chat_gateway_with_auth_and_retains_only_public_result() {
    let server = Gateway::new(vec![Reply::sse(&answer())]);
    let progress = Mutex::new(vec![]);
    let response=server.client(10).ask("Which candidate job has evidence?",&json!({"allowed_tables":["candidate_jobs"],"constraints":"Return supported job IDs only"}),&AtomicBool::new(false),|step|progress.lock().unwrap().push(step)).unwrap();
    assert_eq!(response["provider"], "nvidia_auto_ontology");
    assert_eq!(response["sql_response_from_db"][0]["job_id"], "job_1");
    assert_eq!(response["request_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(response["response_sha256"].as_str().unwrap().len(), 64);
    assert!(!response.to_string().contains("PRIVATE THOUGHT"));
    assert!(!format!("{:?}", progress.lock().unwrap()).contains("PRIVATE THOUGHT"));
    let requests = server.finish();
    assert_eq!(requests[0].path, "/api/chat/completions");
    assert!(
        requests[0]
            .headers
            .iter()
            .any(|(k, v)| k == "x-api-key" && v == TOKEN)
    );
    assert_eq!(requests[0].body["target_db"], "fieldkit");
    assert_eq!(requests[0].body["prediction"], false);
    assert!(requests[0].body["evidence"].is_string());
    assert!(requests[0].body.get("conversation_id").is_none());
}

#[test]
fn long_silent_model_gap_does_not_become_a_read_timeout() {
    let mut reply = Reply::sse("");
    reply.parts = vec![
        (
            0,
            b"data: {\"type\":\"step\",\"node\":\"sql_generation\",\"phase\":\"start\"}\n\n"
                .to_vec(),
        ),
        (2300, answer().into_bytes()),
    ];
    let server = Gateway::new(vec![reply]);
    let started = Instant::now();
    let result = server
        .client(8)
        .ask("Rank jobs", &json!({}), &AtomicBool::new(false), |_| {})
        .unwrap();
    assert!(started.elapsed() >= Duration::from_millis(2300));
    assert_eq!(result["sql_response_from_db"][0]["job_id"], "job_1");
    server.finish();
}

#[test]
fn cancellation_during_silent_stream_returns_promptly_without_partial_answer() {
    let mut reply = Reply::sse("");
    reply.parts = vec![
        (
            0,
            b"data: {\"type\":\"step\",\"node\":\"sql_generation\",\"phase\":\"start\"}\n\n"
                .to_vec(),
        ),
        (800, answer().into_bytes()),
    ];
    let server = Gateway::new(vec![reply]);
    let stop = Arc::new(AtomicBool::new(false));
    let signal = stop.clone();
    let trigger = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        signal.store(true, Ordering::Relaxed);
    });
    let started = Instant::now();
    let error = server
        .client(10)
        .ask("Rank jobs", &json!({}), &stop, |_| {})
        .unwrap_err()
        .to_string();
    assert!(error.contains("cancelled"));
    assert!(started.elapsed() < Duration::from_millis(700));
    trigger.join().unwrap();
    server.finish();
}

#[test]
fn deadline_and_incomplete_or_errored_streams_never_return_results() {
    for stream in [
        "data: {\"type\":\"result\",\"answer\":{\"response\":\"partial\"}}\n\n",
        "data: {\"type\":\"error\",\"message\":\"fieldkit-test-api-token PRIVATE THOUGHT\"}\n\ndata: [DONE]\n\n",
        "data: [DONE]\n\n",
        "data: {broken json}\n\ndata: [DONE]\n\n",
    ] {
        let server = Gateway::new(vec![Reply::sse(stream)]);
        let error = server
            .client(5)
            .ask("Rank jobs", &json!({}), &AtomicBool::new(false), |_| {})
            .unwrap_err()
            .to_string();
        assert!(!error.contains(TOKEN));
        assert!(!error.contains("PRIVATE THOUGHT"));
        server.finish();
    }
    let mut reply = Reply::sse("");
    reply.parts = vec![(1300, answer().into_bytes())];
    let server = Gateway::new(vec![reply]);
    let started = Instant::now();
    assert!(
        server
            .client(1)
            .ask("Rank jobs", &json!({}), &AtomicBool::new(false), |_| {})
            .is_err()
    );
    assert!(started.elapsed() < Duration::from_millis(1250));
    server.finish();
}

#[test]
fn actionable_http_errors_never_echo_server_secrets_or_follow_redirects() {
    for status in [302, 401, 403, 409, 422, 429, 503] {
        let server = Gateway::new(vec![Reply::json(
            status,
            json!({"error":TOKEN,"thoughts":"PRIVATE THOUGHT"}),
        )]);
        let error = server
            .client(5)
            .ask("Rank jobs", &json!({}), &AtomicBool::new(false), |_| {})
            .unwrap_err()
            .to_string();
        assert!(error.contains(&format!("HTTP {status}")));
        assert!(!error.contains(TOKEN));
        assert!(!error.contains("PRIVATE THOUGHT"));
        assert_eq!(server.finish().len(), 1);
    }
}

#[test]
fn readiness_checks_live_gateway_compilation_connections_and_catalog() {
    let server = Gateway::new(vec![
        Reply::json(200, json!({"status":"ok","extra_secret":TOKEN})),
        Reply::json(
            200,
            json!({"calculated":true,"running":false,"last_success_at":"2026-11-11T10:00:00Z","last_failure_at":null}),
        ),
        Reply::json(200, json!({"count":1})),
        Reply::json(200, json!({"data":[{"name":"fieldkit"}]})),
    ]);
    let status = server.client(5).status().unwrap();
    assert_eq!(status["ready"], true);
    assert_eq!(status["can_execute_sql"], true);
    assert!(!status.to_string().contains(TOKEN));
    assert_eq!(
        server
            .finish()
            .iter()
            .map(|s| s.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/api/health",
            "/api/semantic-compilation/status",
            "/api/connections",
            "/api/datasources/dbs"
        ]
    );
}

#[test]
fn forbidden_admin_probes_are_unverified_and_missing_compilation_blocks() {
    let server = Gateway::new(vec![
        Reply::json(200, json!({"status":"ok"})),
        Reply::json(
            200,
            json!({"calculated":true,"running":false,"last_success_at":"2026-11-11T10:00:00Z","last_failure_at":null}),
        ),
        Reply::json(403, json!({})),
        Reply::json(200, json!({"data":[{"name":"fieldkit"}]})),
    ]);
    let status = server.client(5).status().unwrap();
    assert_eq!(status["ready"], true);
    assert_eq!(status["can_execute_sql"], Value::Null);
    assert_eq!(status["unverified"].as_array().unwrap().len(), 1);
    server.finish();
    let server = Gateway::new(vec![
        Reply::json(200, json!({"status":"ok"})),
        Reply::json(200, json!({"calculated":false,"running":true})),
        Reply::json(200, json!({"count":1})),
        Reply::json(200, json!({"data":[{"name":"fieldkit"}]})),
    ]);
    let status = server.client(5).status().unwrap();
    assert_eq!(status["ready"], false);
    assert!(
        status["blockers"][0]
            .as_str()
            .unwrap()
            .contains("compiling")
    );
    server.finish();
}

#[test]
fn result_redaction_covers_echoed_tokens_and_nested_thought_fields() {
    let stream = format!(
        "data: {{\"type\":\"result\",\"answer\":{{\"response\":\"{TOKEN}\",\"sql_code\":\"SELECT '{TOKEN}'\",\"thoughts\":\"hidden\",\"sql_response_from_db\":[{{\"id\":1,\"x-api-key\":\"{TOKEN}\",\"nested\":{{\"thoughts\":\"hidden\"}}}}]}}}}\n\ndata: [DONE]\n\n"
    );
    let server = Gateway::new(vec![Reply::sse(&stream)]);
    let response = server
        .client(5)
        .ask("Rank jobs", &json!({}), &AtomicBool::new(false), |_| {})
        .unwrap();
    assert!(!response.to_string().contains(TOKEN));
    assert!(!response.to_string().contains("hidden"));
    assert_eq!(response["response"], "[REDACTED]");
    server.finish();
}

#[test]
fn environment_sqlite_connection_is_configured_even_without_managed_rows() {
    let server = Gateway::new(vec![
        Reply::json(200, json!({"status":"ok"})),
        Reply::json(
            200,
            json!({"calculated":true,"running":false,"last_success_at":"2026-11-11T10:00:00Z","last_failure_at":null}),
        ),
        Reply::json(200, json!({"count":0})),
        Reply::json(200, json!(true)),
        Reply::json(200, json!({"data":[{"name":"fieldkit"}]})),
    ]);
    let status = server.client(5).status().unwrap();
    assert_eq!(status["ready"], true);
    assert_eq!(status["configured_connection"], true);
    assert_eq!(status["environment_connection_source"], true);
    assert_eq!(status["can_execute_sql"], Value::Null);
    assert!(
        status["unverified"][0]
            .as_str()
            .unwrap()
            .contains("CONNECTION_STRINGS")
    );
    assert_eq!(
        server
            .finish()
            .iter()
            .map(|s| s.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "/api/health",
            "/api/semantic-compilation/status",
            "/api/connections",
            "/api/connections/source",
            "/api/datasources/dbs"
        ]
    );
}

#[test]
fn partial_or_failed_semantic_compilation_never_becomes_ready() {
    for (semantic, expected) in [
        (
            json!({"calculated":true,"running":true,"last_success_at":"2026-11-11T10:00:00Z","last_failure_at":null}),
            "still compiling",
        ),
        (
            json!({"calculated":true,"running":false,"last_success_at":null,"last_failure_at":null}),
            "last_success_at",
        ),
        (
            json!({"calculated":true,"running":false}),
            "last_success_at",
        ),
        (
            json!({"calculated":true,"running":false,"last_success_at":"not-a-timestamp","last_failure_at":null}),
            "last_success_at",
        ),
        (
            json!({"calculated":true,"running":false,"last_success_at":"2026-11-11T10:00:00Z","last_failure_at":"2026-11-11T10:01:00Z"}),
            "failed",
        ),
        (
            json!({"calculated":true,"running":false,"last_success_at":"2026-11-11T10:00:00Z","last_failure_at":"unverifiable"}),
            "failed",
        ),
    ] {
        let server = Gateway::new(vec![
            Reply::json(200, json!({"status":"ok"})),
            Reply::json(200, semantic.clone()),
            Reply::json(200, json!({"count":1})),
            Reply::json(200, json!({"data":[{"name":"fieldkit"}]})),
        ]);
        let status = server.client(5).status().unwrap();
        assert_eq!(status["ready"], false, "{semantic}");
        assert_eq!(status["full_compilation_complete"], false);
        assert!(
            status["blockers"]
                .as_array()
                .unwrap()
                .iter()
                .any(|b| b.as_str().unwrap().contains(expected)),
            "{status}"
        );
        assert_eq!(
            status["semantic_layer"]["last_success_at"],
            semantic["last_success_at"]
        );
        assert_eq!(
            status["semantic_layer"]["last_failure_at"],
            semantic["last_failure_at"]
        );
        server.finish();
    }
}

#[test]
fn completed_success_newer_than_old_failure_remains_ready() {
    let semantic = json!({"calculated":true,"running":false,"last_success_at":"2026-11-11T10:00:00Z","last_failure_at":"2026-11-11T09:59:00+00:00"});
    let server = Gateway::new(vec![
        Reply::json(200, json!({"status":"ok"})),
        Reply::json(200, semantic.clone()),
        Reply::json(200, json!({"count":1})),
        Reply::json(200, json!({"data":[{"name":"fieldkit"}]})),
    ]);
    let status = server.client(5).status().unwrap();
    assert_eq!(status["ready"], true);
    assert_eq!(status["full_compilation_complete"], true);
    assert_eq!(
        status["semantic_layer"]["last_success_at"],
        semantic["last_success_at"]
    );
    server.finish();
}
