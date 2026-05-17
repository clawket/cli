//! MCP cross-client compatibility harness.
//!
//! Spawns `clawket mcp` as a subprocess, drives it over stdio with the
//! JSON-RPC protocol that real MCP clients (Claude Code, MCP Inspector,
//! Cursor) use. Each test asserts a specific contract:
//!   - server initializes and lists exactly the v3.0 read-only RAG tool set
//!   - every tool advertises a description + an inputSchema
//!   - daemon-missing tool calls return a structured error (not a panic)
//!
//! These tests do NOT require clawketd. They route every HTTP call to a
//! socket that doesn't exist (`CLAWKET_SOCKET` → /tmp/nonexistent…),
//! which lets us prove daemon-down failure modes are surfaced cleanly.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const EXPECTED_TOOLS: &[&str] = &[
    "clawket_search_knowledge",
    "clawket_search_tasks",
    "clawket_find_similar_tasks",
    "clawket_get_task_context",
    "clawket_get_recent_decisions",
];

struct McpProcess {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
}

impl McpProcess {
    fn spawn() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let bin = env!("CARGO_BIN_EXE_clawket");
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Path must NOT exist; we just need a sentinel that hyper-util's UDS
        // connector will fail to dial. std::env::temp_dir() is per-OS sane.
        let dead_socket = std::env::temp_dir().join(format!(
            "clawket-mcp-compat-{nonce}-{n}-nonexistent.sock"
        ));
        // Force every daemon call to fail fast so we can verify graceful errors
        // without needing a live daemon.
        let mut child = Command::new(bin)
            .arg("mcp")
            .env("CLAWKET_SOCKET", &dead_socket)
            .env("CLAWKET_DISABLE_AUTO_START", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn clawket mcp");
        let stdout = child.stdout.take().expect("stdout");
        Self {
            child,
            reader: BufReader::new(stdout),
        }
    }

    fn send(&mut self, msg: &Value) {
        let line = serde_json::to_string(msg).unwrap();
        let stdin = self.child.stdin.as_mut().expect("stdin");
        stdin.write_all(line.as_bytes()).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    fn recv(&mut self, want_id: i64, timeout: Duration) -> Value {
        let deadline = Instant::now() + timeout;
        loop {
            if Instant::now() > deadline {
                panic!("timeout waiting for id={want_id}");
            }
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) => panic!("EOF before id={want_id}"),
                Ok(_) => {}
                Err(e) => panic!("read error: {e}"),
            }
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(&line) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if v.get("id").and_then(|i| i.as_i64()) == Some(want_id) {
                return v;
            }
        }
    }

    fn initialize(&mut self) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name": "compat-test", "version": "0"}
            }
        }));
        let _ = self.recv(1, Duration::from_secs(5));
        self.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        // Drain stderr for diagnostics if a test panicked; harmless on success.
        if let Some(mut err) = self.child.stderr.take() {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            if !s.is_empty() && std::thread::panicking() {
                eprintln!("--- mcp stderr ---\n{s}\n------------------");
            }
        }
    }
}

#[test]
fn lists_all_v3_rag_tools() {
    let mut p = McpProcess::spawn();
    p.initialize();
    p.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let resp = p.recv(2, Duration::from_secs(5));
    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools array")
        .clone();
    assert_eq!(
        tools.len(),
        EXPECTED_TOOLS.len(),
        "expected {} tools, got {}: {:?}",
        EXPECTED_TOOLS.len(),
        tools.len(),
        tools
            .iter()
            .map(|t| t["name"].as_str().unwrap_or(""))
            .collect::<Vec<_>>()
    );
    let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
    for want in EXPECTED_TOOLS {
        assert!(names.contains(want), "missing tool: {want}");
    }
}

#[test]
fn every_tool_has_description_and_input_schema() {
    let mut p = McpProcess::spawn();
    p.initialize();
    p.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let resp = p.recv(2, Duration::from_secs(5));
    for t in resp["result"]["tools"].as_array().unwrap() {
        let name = t["name"].as_str().unwrap_or("");
        let desc = t["description"].as_str().unwrap_or("");
        assert!(
            !desc.trim().is_empty(),
            "tool {name} has empty description — clients show this to the LLM"
        );
        // rmcp serializes `inputSchema` (camelCase) per MCP spec.
        let schema = t.get("inputSchema").expect("inputSchema present");
        assert!(
            schema.get("type").is_some() || schema.get("$ref").is_some(),
            "tool {name} inputSchema lacks type/ref: {schema}"
        );
    }
}

#[test]
fn server_info_advertises_clawket_implementation() {
    let mut p = McpProcess::spawn();
    p.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "compat-test", "version": "0"}
        }
    }));
    let resp = p.recv(1, Duration::from_secs(5));
    let info = &resp["result"]["serverInfo"];
    assert_eq!(info["name"], "clawket", "server name should be 'clawket'");
    assert!(
        info["version"].is_string(),
        "version should be set from CARGO_PKG_VERSION: {info}"
    );
    let instructions = resp["result"]["instructions"]
        .as_str()
        .unwrap_or("")
        .to_string();
    for tool in EXPECTED_TOOLS {
        assert!(
            instructions.contains(tool),
            "instructions should mention {tool}: {instructions}"
        );
    }
}

#[test]
fn daemon_missing_returns_error_not_panic() {
    let mut p = McpProcess::spawn();
    p.initialize();
    // search_knowledge only needs `query`, so it's the cheapest call to verify
    // that the tool surfaces a daemon-down error cleanly rather than crashing
    // the MCP process.
    p.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "clawket_search_knowledge",
            "arguments": {"query": "anything"}
        }
    }));
    let resp = p.recv(2, Duration::from_secs(5));
    let result = &resp["result"];
    assert_eq!(
        result["isError"], true,
        "daemon-missing call should be flagged as error: {result}"
    );
    let body = result["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        body.contains("error") || body.contains("DAEMON_ERROR"),
        "error body should be structured JSON with 'error' key: {body}"
    );
}

#[test]
fn invalid_input_returns_validation_error() {
    let mut p = McpProcess::spawn();
    p.initialize();
    // find_similar_tasks requires task_id OR query — neither given.
    p.send(&json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "clawket_find_similar_tasks",
            "arguments": {}
        }
    }));
    let resp = p.recv(2, Duration::from_secs(5));
    let body = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        body.contains("INVALID_INPUT"),
        "should surface INVALID_INPUT: {body}"
    );
}
