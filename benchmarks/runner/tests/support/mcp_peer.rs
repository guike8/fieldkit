//! Offline subprocess fixture only: never collects, opens cases or executes downloaded data.
use serde_json::{Value, json};
use std::io::{BufRead, Write};
fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let request: Value = serde_json::from_str(&line.unwrap()).unwrap();
        if mode == "partial" {
            stdout.write_all(b"{\"jsonrpc\":").unwrap();
            stdout.flush().unwrap();
            std::thread::sleep(std::time::Duration::from_secs(5));
            return;
        }
        if mode == "oversized" {
            stdout.write_all(&vec![b'x'; 8 * 1024 * 1024 + 1]).unwrap();
            stdout.flush().unwrap();
            return;
        }
        if mode == "closed" {
            return;
        }
        if mode == "malformed" {
            writeln!(stdout, "not JSON").unwrap();
            stdout.flush().unwrap();
            return;
        }
        let id = if mode == "wrong-id" {
            json!(999)
        } else {
            request["id"].clone()
        };
        let mut response = json!({"jsonrpc":"2.0","id":id,"result":{}});
        if mode == "rpc-error" {
            response =
                json!({"jsonrpc":"2.0","id":id,"error":{"code":-1,"message":"synthetic failure"}});
        } else if request["method"] == "tools/call" {
            response["result"] = if mode == "tool-error" {
                json!({"isError":true})
            } else {
                json!({"structuredContent":{"command":"case.verify","data":{"ok":true}}})
            };
        }
        writeln!(stdout, "{response}").unwrap();
        stdout.flush().unwrap();
    }
}
