use crate::{BASE_URL, MAX_HTTP, MAX_MESSAGE, MODEL, agent::Chat, scope::ToolBackend};
use anyhow::{Result, anyhow, bail, ensure};
use reqwest::{blocking::Client, redirect::Policy};
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    os::fd::{AsRawFd, RawFd},
    path::{Path, PathBuf},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

pub struct ChatClient {
    client: Client,
    url: url::Url,
    model: String,
    key: String,
    max_tokens: u64,
}
impl ChatClient {
    pub fn new(
        base: &str,
        model: &str,
        key: &str,
        timeout: Duration,
        max_tokens: u64,
    ) -> Result<Self> {
        let mut url = url::Url::parse(base).map_err(|_| anyhow!("Invalid API base URL"))?;
        let loopback = matches!(
            url.host_str(),
            Some("localhost" | "127.0.0.1" | "[::1]" | "::1")
        );
        ensure!(
            (url.scheme() == "https" || (url.scheme() == "http" && loopback))
                && url.host_str().is_some()
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none(),
            "Use an HTTPS API base URL (HTTP only for loopback); no URL credentials/query"
        );
        ensure!(
            !model.is_empty() && !key.is_empty() && !timeout.is_zero() && max_tokens > 0,
            "Model, API key, positive token limit and finite timeout are required"
        );
        url.set_path(&format!(
            "{}/chat/completions",
            url.path().trim_end_matches('/')
        ));
        // No retries, redirects or ambient proxy credentials. Never expose error URLs/bodies.
        let client = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .timeout(timeout)
            .build()
            .map_err(|_| anyhow!("Provider client setup failed"))?;
        Ok(Self {
            client,
            url,
            model: model.into(),
            key: key.into(),
            max_tokens,
        })
    }
    pub fn default_for(key: &str) -> Result<Self> {
        Self::new(BASE_URL, MODEL, key, Duration::from_secs(90), 2048)
    }
}
impl Chat for ChatClient {
    fn complete(&mut self, messages: &[Value], tools: &[Value]) -> Result<Value> {
        let mut payload = json!({"model":self.model,"messages":messages,"max_tokens":self.max_tokens,
            "temperature":0.0,"stream":false});
        if !tools.is_empty() {
            payload["tools"] = json!(tools);
            payload["tool_choice"] = json!("auto");
            payload["parallel_tool_calls"] = json!(false);
        }
        let response = self
            .client
            .post(self.url.clone())
            .bearer_auth(&self.key)
            .json(&payload)
            .send()
            .map_err(|_| anyhow!("Provider transport failure; request not retried"))?;
        let status = response.status();
        ensure!(
            status.is_success(),
            "Provider HTTP {}; request not retried",
            status.as_u16()
        );
        let mut bytes = Vec::new();
        response
            .take(MAX_HTTP + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| anyhow!("Provider transport failure; request not retried"))?;
        ensure!(
            bytes.len() as u64 <= MAX_HTTP,
            "Provider response exceeded 8 MiB"
        );
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow!("Provider JSON failure; request not retried"))?;
        ensure!(value.is_object(), "Provider returned a non-object response");
        Ok(value)
    }
}

pub struct McpClient {
    child: Child,
    stdin: ChildStdin,
    stdout: ChildStdout,
    buffer: Vec<u8>,
    timeout: Duration,
    counter: u64,
    pub initialization: Value,
    pub tools: Vec<Value>,
}
fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    })
}
fn nonblocking(fd: RawFd) -> Result<()> {
    // SAFETY: owned live child pipe descriptor; fcntl does not retain pointers.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    ensure!(
        flags >= 0 && unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } >= 0,
        "Could not configure MCP pipe"
    );
    Ok(())
}
fn ready(fd: RawFd, events: i16, deadline: Instant) -> Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        ensure!(
            !remaining.is_zero(),
            "MCP timeout; action outcome may be unknown. No retry performed"
        );
        let mut poll = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let ms = remaining
            .as_millis()
            .saturating_add(1)
            .min(i32::MAX as u128) as i32;
        // SAFETY: pointer references one live initialized pollfd for this call only.
        let result = unsafe { libc::poll(&mut poll, 1, ms) };
        if result > 0 {
            return Ok(());
        }
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            bail!("MCP pipe failed; action outcome may be unknown");
        }
    }
}
impl McpClient {
    pub fn connect(
        binary: &Path,
        workspace: &Path,
        case: &Path,
        timeout: Duration,
        key_env: &str,
    ) -> Result<Self> {
        ensure!(
            !timeout.is_zero() && Instant::now().checked_add(timeout).is_some(),
            "MCP timeout must be positive and bounded"
        );
        let mut command = Command::new(absolute(binary)?);
        command
            .args(["--case"])
            .arg(absolute(case)?)
            .arg("mcp")
            .arg("--workspace")
            .arg(absolute(workspace)?);
        for (name, _) in std::env::vars_os() {
            let upper = name.to_string_lossy().to_uppercase();
            if name == key_env
                || upper.ends_with("API_KEY")
                || upper.ends_with("ACCESS_TOKEN")
                || upper.ends_with("AUTH_TOKEN")
                || upper.ends_with("SECRET_KEY")
            {
                command.env_remove(name);
            }
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|_| anyhow!("Could not start native MCP process"))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let mut client = Self {
            child,
            stdin,
            stdout,
            buffer: vec![],
            timeout,
            counter: 0,
            initialization: Value::Null,
            tools: vec![],
        };
        nonblocking(client.stdin.as_raw_fd())?;
        nonblocking(client.stdout.as_raw_fd())?;
        client.initialization = client.request(
            "initialize",
            &json!({"protocolVersion":"2025-06-18","capabilities":{},
            "clientInfo":{"name":"fieldkit-inkling","version":"0.1.0"}}),
        )?;
        client.notify("notifications/initialized", &json!({}))?;
        let tools = client.request("tools/list", &json!({}))?;
        client.tools = tools["tools"]
            .as_array()
            .ok_or_else(|| anyhow!("Invalid MCP tool list"))?
            .clone();
        Ok(client)
    }
    fn send(&mut self, value: &Value, deadline: Instant) -> Result<()> {
        let mut data = serde_json::to_vec(value)?;
        data.push(b'\n');
        ensure!(data.len() <= MAX_MESSAGE, "MCP request exceeds 1 MiB");
        let mut written = 0;
        while written < data.len() {
            ready(self.stdin.as_raw_fd(), libc::POLLOUT, deadline)?;
            match self.stdin.write(&data[written..]) {
                Ok(0) => bail!("MCP input closed; action outcome may be unknown"),
                Ok(n) => written += n,
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => bail!("MCP input closed; action outcome may be unknown"),
            }
        }
        Ok(())
    }
    pub fn notify(&mut self, method: &str, params: &Value) -> Result<()> {
        self.send(
            &json!({"jsonrpc":"2.0","method":method,"params":params}),
            Instant::now() + self.timeout,
        )
    }
    pub fn request(&mut self, method: &str, params: &Value) -> Result<Value> {
        self.counter += 1;
        let id = self.counter;
        let deadline = Instant::now() + self.timeout;
        self.send(
            &json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}),
            deadline,
        )?;
        loop {
            if let Some(end) = self.buffer.iter().position(|b| *b == b'\n') {
                let line: Vec<_> = self.buffer.drain(..=end).collect();
                let response: Value =
                    serde_json::from_slice(&line).map_err(|_| anyhow!("Invalid MCP JSON"))?;
                ensure!(
                    response.is_object()
                        && response["id"].as_u64() == Some(id)
                        && response["jsonrpc"] == "2.0",
                    "Unexpected MCP response id/protocol"
                );
                ensure!(
                    response.get("error").is_none(),
                    "MCP rejected request; inspect local state before resuming"
                );
                ensure!(
                    response["result"].is_object(),
                    "MCP response missing result"
                );
                return Ok(response["result"].clone());
            }
            ready(self.stdout.as_raw_fd(), libc::POLLIN, deadline)?;
            let mut chunk = [0u8; 65536];
            match self.stdout.read(&mut chunk) {
                Ok(0) => bail!("MCP exited; action outcome may be unknown"),
                Ok(n) => {
                    self.buffer.extend_from_slice(&chunk[..n]);
                    ensure!(
                        self.buffer.len() <= MAX_MESSAGE,
                        "MCP response exceeds 1 MiB"
                    );
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(_) => bail!("MCP pipe failed; action outcome may be unknown"),
            }
        }
    }
}
impl ToolBackend for McpClient {
    fn tools(&self) -> Vec<Value> {
        self.tools.clone()
    }
    fn call(&mut self, name: &str, args: &Value) -> Result<Value> {
        self.request("tools/call", &json!({"name":name,"arguments":args}))
    }
}
impl Drop for McpClient {
    fn drop(&mut self) {
        // No IO worker or blocking pipe join: inherited pipes cannot extend the deadline.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
