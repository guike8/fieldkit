//! Newline-delimited real MCP subprocess transport. No shell, retry or unbounded readline.
use crate::common::{MAX_BYTES, Output, Tools};
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use std::{
    io::{BufReader, Read, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, SyncSender},
    thread,
    time::{Duration, Instant},
};

type WriteJob = (Vec<u8>, SyncSender<std::io::Result<()>>);
pub struct Client {
    child: Child,
    write: Option<SyncSender<WriteJob>>,
    read: Receiver<Result<Vec<u8>>>,
    sequence: u64,
    timeout: Duration,
    log: Option<Output>,
}
fn read_line(reader: &mut impl Read) -> Result<Vec<u8>> {
    let mut line = Vec::new();
    let mut byte = [0];
    loop {
        if reader.read(&mut byte)? == 0 {
            bail!("MCP server closed before a complete response");
        }
        if byte[0] == b'\n' {
            return Ok(line);
        }
        ensure!(
            (line.len() as u64) < MAX_BYTES,
            "MCP response exceeds byte bound"
        );
        line.push(byte[0]);
    }
}
impl Client {
    pub fn new(command: Vec<String>, log: Option<Output>) -> Result<Self> {
        Self::with_timeout(command, log, Duration::from_secs(90))
    }
    pub fn with_timeout(
        command: Vec<String>,
        log: Option<Output>,
        timeout: Duration,
    ) -> Result<Self> {
        ensure!(
            !command.is_empty() && !command[0].is_empty(),
            "Supply an explicit nonempty JSON argv array"
        );
        let mut child = Command::new(&command[0])
            .args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut input = child.stdin.take().context("Missing MCP stdin")?;
        let mut output = BufReader::new(child.stdout.take().context("Missing MCP stdout")?);
        let (write_tx, write_rx) = mpsc::sync_channel::<WriteJob>(1);
        thread::spawn(move || {
            while let Ok((bytes, done)) = write_rx.recv() {
                let result = input.write_all(&bytes).and_then(|_| input.flush());
                let failed = result.is_err();
                let _ = done.send(result);
                if failed {
                    break;
                }
            }
        });
        let (read_tx, read_rx) = mpsc::sync_channel(1);
        thread::spawn(move || {
            loop {
                let result = read_line(&mut output);
                let failed = result.is_err();
                if read_tx.send(result).is_err() || failed {
                    break;
                }
            }
        });
        let mut client = Self {
            child,
            write: Some(write_tx),
            read: read_rx,
            sequence: 0,
            timeout,
            log,
        };
        client.rpc("initialize",json!({"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"fieldkit-historical-replay","version":"2-rust"}}))?;
        Ok(client)
    }
    pub fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.sequence += 1;
        let request = json!({"jsonrpc":"2.0","id":self.sequence,"method":method,"params":params});
        let deadline = Instant::now() + self.timeout;
        let mut bytes = serde_json::to_vec(&request)?;
        ensure!(
            bytes.len() as u64 <= MAX_BYTES,
            "MCP request exceeds byte bound"
        );
        bytes.push(b'\n');
        let (done_tx, done_rx) = mpsc::sync_channel(1);
        self.write
            .as_ref()
            .context("Closed MCP transport")?
            .try_send((bytes, done_tx))
            .context("MCP write queue unavailable; do not retry")?;
        done_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("MCP write deadline reached; do not retry collection")??;
        let line = self
            .read
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .context("MCP response deadline reached; do not retry collection")??;
        let response: Value = serde_json::from_slice(&line).context("Invalid MCP response JSON")?;
        if let Some(log) = &mut self.log {
            log.line(&json!({"request":request,"response":response}))?;
        }
        ensure!(
            response["jsonrpc"] == "2.0"
                && response["id"] == self.sequence
                && response.get("error").is_none(),
            "Mismatched or failed MCP response: {response}"
        );
        response
            .get("result")
            .cloned()
            .context("MCP response has no result")
    }
    pub fn close(mut self) -> Result<()> {
        self.write.take();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.child.try_wait()? {
                ensure!(status.success(), "MCP server exited {status}");
                return Ok(());
            }
            if Instant::now() >= deadline {
                self.child.kill()?;
                bail!("MCP server did not close before deadline");
            }
            thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Tools for Client {
    fn tool(&mut self, name: &str, arguments: Value) -> Result<Value> {
        let result = self.rpc("tools/call", json!({"name":name,"arguments":arguments}))?;
        ensure!(result["isError"] != true, "MCP tool failed: {result}");
        let value = result
            .get("structuredContent")
            .context("Missing structured MCP content")?;
        if value["command"]
            .as_str()
            .is_some_and(|s| s.starts_with("case."))
        {
            value.get("data").cloned().context("Missing case tool data")
        } else {
            Ok(value.clone())
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.write.take();
        let _ = self.child.kill();
        let _ = self.child.try_wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounded_complete_lines_only() {
        assert_eq!(read_line(&mut &b"{}\nrest"[..]).unwrap(), b"{}");
        assert!(read_line(&mut &b"partial"[..]).is_err());
        let long = vec![b'x'; MAX_BYTES as usize + 1];
        assert!(read_line(&mut long.as_slice()).is_err());
    }
}
