use fieldkit_agent::{Chat, ChatClient, MAX_HTTP, MAX_MESSAGE, MODEL, McpClient};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    path::Path,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

// Shutdown is tied to completion of the tested client call, not a short wall-clock
// window that can expire while macOS starts a TLS client under parallel test load.
struct HttpMock {
    stop: mpsc::Sender<()>,
    worker: Option<thread::JoinHandle<usize>>,
}
impl HttpMock {
    fn finish(mut self) -> usize {
        let _ = self.stop.send(());
        self.worker.take().unwrap().join().unwrap()
    }
}
impl Drop for HttpMock {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn http(status: u16, body: Vec<u8>) -> (String, mpsc::Receiver<(String, Value)>, HttpMock) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    listener.set_nonblocking(true).unwrap();
    let (tx, rx) = mpsc::channel();
    let (stop, stopped) = mpsc::channel();
    let worker = thread::spawn(move || {
        let mut count = 0;
        while matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty)) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    // Accepted sockets inherit O_NONBLOCK on macOS, unlike Linux.
                    // This fixture's read_exact/write_all calls require blocking I/O.
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    stream
                        .set_write_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut request = vec![];
                    let mut one = [0u8; 1];
                    while !request.ends_with(b"\r\n\r\n") {
                        stream.read_exact(&mut one).unwrap();
                        request.push(one[0]);
                    }
                    let headers = String::from_utf8(request).unwrap();
                    let length = headers
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|s| s.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    let mut bytes = vec![0; length];
                    stream.read_exact(&mut bytes).unwrap();
                    tx.send((headers, serde_json::from_slice(&bytes).unwrap()))
                        .unwrap();
                    count += 1;
                    let response = format!(
                        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nLocation: http://{address}/unexpected\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    stream.write_all(response.as_bytes()).unwrap();
                    let body_result = stream.write_all(&body);
                    assert!(
                        body_result.is_ok() || !(200..300).contains(&status),
                        "mock HTTP body write failed: {body_result:?}"
                    );
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2))
                }
                Err(e) => panic!("{e}"),
            }
        }
        count
    });
    (
        format!("http://{address}/v1"),
        rx,
        HttpMock {
            stop,
            worker: Some(worker),
        },
    )
}
#[test]
fn openai_request_contract() {
    let body = fieldkit_agent::benchmark::scripted_reply(json!("Synthetic answer"), json!([]));
    let (base, rx, worker) = http(200, body.to_string().into_bytes());
    let mut client = ChatClient::new(
        &base,
        MODEL,
        "synthetic-test-key",
        Duration::from_secs(2),
        2048,
    )
    .unwrap();
    let reply = client
        .complete(
            &[json!({"role":"user","content":"Synthetic"})],
            &[json!({"type":"function"})],
        )
        .unwrap();
    let (headers, payload) = rx.recv().unwrap();
    assert!(headers.starts_with("POST /v1/chat/completions "));
    assert!(
        headers
            .to_lowercase()
            .contains("authorization: bearer synthetic-test-key")
    );
    assert_eq!(payload["model"], MODEL);
    assert_eq!(payload["parallel_tool_calls"], false);
    assert_eq!(payload["stream"], false);
    assert_eq!(payload["max_tokens"], 2048);
    assert_eq!(payload["temperature"], 0.0);
    assert_eq!(payload["tool_choice"], "auto");
    assert_eq!(
        reply["choices"][0]["message"]["content"],
        "Synthetic answer"
    );
    assert_eq!(worker.finish(), 1);
}
#[test]
fn provider_errors_redirects_and_response_limits_never_retry_or_echo_secrets() {
    for (status, body, expected) in [
        (429, b"provider-body-private-secret".to_vec(), "HTTP 429"),
        (307, b"redirect".to_vec(), "HTTP 307"),
        (200, b"{invalid-secret".to_vec(), "JSON failure"),
        (200, b"[]".to_vec(), "non-object"),
        (200, vec![b'x'; MAX_HTTP as usize + 1], "exceeded 8 MiB"),
    ] {
        let (base, _rx, worker) = http(status, body);
        let mut client =
            ChatClient::new(&base, MODEL, "secret", Duration::from_secs(5), 2048).unwrap();
        let error = client.complete(&[], &[]).unwrap_err().to_string();
        assert!(
            error.contains(expected),
            "HTTP {status}, expected {expected}: {error}"
        );
        assert!(!error.contains("private-secret"));
        assert!(!error.contains("invalid-secret"));
        assert_eq!(worker.finish(), 1);
    }
}
#[test]
fn base_url_and_client_limits_fail_closed() {
    for base in [
        "http://public.example/v1",
        "https://user:pass@example.org/v1",
        "https://example.org/v1?key=secret",
        "https://example.org/#fragment",
        "file:///tmp/key",
    ] {
        assert!(ChatClient::new(base, MODEL, "secret", Duration::from_secs(1), 2048).is_err());
    }
    assert!(
        ChatClient::new(
            "https://example.org/v1",
            MODEL,
            "",
            Duration::from_secs(1),
            2048
        )
        .is_err()
    );
    assert!(ChatClient::new("https://example.org/v1", MODEL, "key", Duration::ZERO, 2048).is_err());
}
fn script(path: &Path, source: &str) {
    std::fs::write(path, format!("#!/bin/sh\n{source}\n")).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
}
fn connect(path: &Path, timeout: Duration) -> anyhow::Result<McpClient> {
    McpClient::connect(
        path,
        path.parent().unwrap(),
        &path.parent().unwrap().join("case"),
        timeout,
        "SYNTHETIC_MODEL_KEY",
    )
}
fn error(path: &Path, source: &str, expected: &str) {
    script(path, source);
    let start = Instant::now();
    // Only the deliberate no-response timeout probe needs a subsecond deadline.
    // Protocol tests must allow macOS to launch and parse the disposable shell stub.
    let timeout = if expected.starts_with("timeout") {
        Duration::from_millis(100)
    } else {
        Duration::from_secs(3)
    };
    let e = match connect(path, timeout) {
        Ok(_) => panic!("must fail"),
        Err(e) => e,
    };
    assert!(e.to_string().contains(expected), "{e}");
    assert!(start.elapsed() < timeout + Duration::from_secs(2));
}
#[test]
fn mcp_initialization_timeout_wrong_id_bad_json_eof_and_oversize() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fake-mcp");
    error(
        &p,
        "exec sleep 10",
        "timeout; action outcome may be unknown",
    );
    error(
        &p,
        "read line; printf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}'",
        "response id",
    );
    error(
        &p,
        "read line; printf '%s\\n' '{invalid'",
        "Invalid MCP JSON",
    );
    error(&p, "exit 0", "MCP exited");
    error(
        &p,
        &format!("read line; printf '%s' '{}'", "x".repeat(MAX_MESSAGE + 1)),
        "exceeds 1 MiB",
    );
}
const HANDSHAKE: &str = "read line\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}'\nread line\nread line\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}'\n";
#[test]
fn mcp_blocked_writes_share_deadline_and_cleanup_reaps_child() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fake-mcp");
    let pid_file = d.path().join("pid");
    script(
        &p,
        &format!(
            "printf '%s' \"$$\" > '{}'\n{HANDSHAKE}exec sleep 10",
            pid_file.display()
        ),
    );
    let mut c = connect(&p, Duration::from_secs(3)).unwrap();
    let start = Instant::now();
    let pid = std::fs::read_to_string(&pid_file)
        .unwrap()
        .parse::<libc::pid_t>()
        .unwrap();
    assert!(
        c.request("large", &json!({"payload":"x".repeat(512*1024)}))
            .unwrap_err()
            .to_string()
            .contains("timeout")
    );
    drop(c);
    assert!(start.elapsed() < Duration::from_secs(5));
    // SAFETY: signal 0 probes existence only; this PID was recorded by our disposable child.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
#[test]
fn mcp_request_limit_and_strict_protocol() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fake-mcp");
    script(&p, &format!("{HANDSHAKE}exec sleep 10"));
    let mut c = connect(&p, Duration::from_secs(3)).unwrap();
    assert!(
        c.request("large", &json!({"p":"x".repeat(MAX_MESSAGE)}))
            .unwrap_err()
            .to_string()
            .contains("request exceeds")
    );
    drop(c);
    for (reply, expected) in [
        (
            "{\"jsonrpc\":\"1.0\",\"id\":1,\"result\":{}}",
            "response id/protocol",
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"error\":{\"message\":\"secret\"}}",
            "rejected request",
        ),
        (
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":[]}",
            "missing result",
        ),
    ] {
        error(
            &p,
            &format!("read line; printf '%s\\n' '{reply}'"),
            expected,
        );
    }
}
#[test]
fn native_stderr_is_inherited_not_an_undrained_private_pipe() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fake-mcp");
    script(
        &p,
        &format!(
            "printf '%s' '{}' >&2\n{HANDSHAKE}exec sleep 10",
            "diagnostic".repeat(16384)
        ),
    );
    let start = Instant::now();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_fieldkit-agent"))
        .env("SYNTHETIC_MODEL_KEY", "test-only-key")
        .args(["Question", "--binary"])
        .arg(&p)
        .arg("--workspace")
        .arg(d.path())
        .arg("--case")
        .arg(d.path().join("case"))
        .args([
            "--mode",
            "person",
            "--seed",
            "sample",
            "--api-key-env",
            "SYNTHETIC_MODEL_KEY",
            "--base-url",
            "http://127.0.0.1:1/v1",
            "--timeout",
            "3",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(out.stderr.len() >= 16384 * 10);
    assert!(!String::from_utf8_lossy(&out.stderr).contains("test-only-key"));
    assert!(start.elapsed() < Duration::from_secs(5));
}
#[test]
fn selected_and_conventional_model_keys_not_inherited() {
    // A child test process carries credentials, avoiding unsafe mutation of global test environment.
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("fake-mcp");
    script(
        &p,
        "read line\nif [ -n \"${SYNTHETIC_MODEL_KEY+x}${OPENAI_API_KEY+x}${INKLING_API_KEY+x}\" ]; then exit 1; fi\nif [ -n \"${MCP_TEST_MARKER:-}\" ]; then printf '%s' clean > \"$MCP_TEST_MARKER\"; fi\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"key_present\":false}}'\nread line\nread line\nprintf '%s\\n' '{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}'\nexec sleep 10",
    );
    let marker = d.path().join("credentials-scrubbed");
    // CLI itself receives a key; the native subprocess must not inherit it.
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_fieldkit-agent"))
        .env("MCP_TEST_MARKER", &marker)
        .env("SYNTHETIC_MODEL_KEY", "test-only-key")
        .env("OPENAI_API_KEY", "test-only-key")
        .env("INKLING_API_KEY", "test-only-key")
        .args(["Question", "--binary"])
        .arg(&p)
        .arg("--workspace")
        .arg(d.path())
        .arg("--case")
        .arg(d.path().join("case"))
        .args([
            "--mode",
            "person",
            "--seed",
            "sample",
            "--api-key-env",
            "SYNTHETIC_MODEL_KEY",
            "--base-url",
            "http://127.0.0.1:1/v1",
            "--timeout",
            "3",
        ])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("test-only-key"));
    assert_eq!(std::fs::read_to_string(marker).unwrap(), "clean");
    // Independent startup proves this fixture returns key_present=false and tools=[].
    let c = connect(&p, Duration::from_secs(3)).unwrap();
    assert_eq!(c.initialization["key_present"], false);
    assert!(c.tools.is_empty());
}
