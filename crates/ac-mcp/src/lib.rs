//! MCP stdio server (specs/13-mcp-server.md): newline-delimited JSON-RPC 2.0 on stdin and
//! stdout, with the tools in [`tools`]. Logs go to stderr and the log file, never stdout.

mod tools;

use std::io::{BufRead, Write};
use std::path::PathBuf;

use ac_core::Home;
use serde_json::{Value, json};

/// Protocol versions this server speaks, newest first.
const VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

const INSTRUCTIONS: &str = "aftercast: post-processed live coding streams. Start with \
list_sessions and session_overview, read index.md in slices with read_index, verify with \
get_transcript (granularity word) and get_frame, then validate_suggestions and \
write_suggestions. Times are master (rec_t) seconds or HH:MM:SS. Redacted ranges come back \
as placeholders and must never be used in suggestions.";

pub struct Server {
    home: Home,
    /// Loaded on every call, so edits apply without a restart.
    config: PathBuf,
}

impl Server {
    pub fn new(home: Home, config: PathBuf) -> Self {
        Self { home, config }
    }

    /// Serves until stdin closes.
    pub fn serve(&self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle(&line) {
                writeln!(output, "{reply}")?;
                output.flush()?;
            }
        }
        Ok(())
    }

    /// One message in, at most one reply out (notifications get none).
    pub fn handle(&self, line: &str) -> Option<Value> {
        let msg: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => return Some(error(Value::Null, -32700, &format!("parse error: {e}"))),
        };
        let id = msg.get("id").cloned()?;
        let method = msg["method"].as_str().unwrap_or_default();
        let params = &msg["params"];
        let result = match method {
            "initialize" => {
                let asked = params["protocolVersion"].as_str().unwrap_or_default();
                let version = VERSIONS
                    .iter()
                    .find(|v| **v == asked)
                    .unwrap_or(&VERSIONS[0]);
                json!({
                    "protocolVersion": version,
                    "capabilities": { "tools": {} },
                    "serverInfo": { "name": "aftercast", "version": env!("CARGO_PKG_VERSION") },
                    "instructions": INSTRUCTIONS,
                })
            }
            "ping" => json!({}),
            "tools/list" => json!({ "tools": tools::list() }),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or_default();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match tools::call(&self.home, &self.config, name, &args) {
                    Ok(content) => json!({ "content": content }),
                    Err(e) => {
                        json!({ "content": [{ "type": "text", "text": e }], "isError": true })
                    }
                }
            }
            _ => return Some(error(id, -32601, &format!("method not found: {method}"))),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }
}

fn error(id: Value, code: i32, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}
