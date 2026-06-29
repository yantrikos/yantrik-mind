//! mcp — a Model Context Protocol (stdio) client. THE FORCE MULTIPLIER: one integration unlocks the
//! whole MCP-server ecosystem (Gmail, Notion, Slack, Maps, Spotify, GitHub, filesystem, databases…)
//! instead of hand-writing each native. We spawn each configured server as a subprocess, run the
//! JSON-RPC `initialize` handshake, discover its tools, and call them.
//!
//! TRANSPORT: stdio, newline-delimited JSON-RPC 2.0 (each message one line, no embedded newlines).
//!
//! SAFETY: a server is configured EXPLICITLY by the user (with their own creds), so a configured
//! server is trusted transport. Within a server, every tool is classified read-only vs mutating
//! (server `readOnlyHint`/`destructiveHint` annotation, else a conservative name heuristic). The
//! CALLER (conversation layer) runs read-only tools freely and routes mutating tools through the
//! harm-gate — there is NO un-gated write path. Tool OUTPUT is untrusted third-party data; the
//! caller wraps it as reference-data-not-instructions (prompt-injection surface).

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One configured MCP server: how to launch it.
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

impl McpServerConfig {
    /// Parse the de-facto `mcpServers` config shape (as used by Claude Desktop and other clients):
    /// `{ "mcpServers": { "<name>": { "command", "args": [..], "env": {..} } } }`. Also accepts the
    /// key `servers`, or the server-map at the top level directly.
    pub fn from_json(v: &Value) -> Vec<Self> {
        let map = v.get("mcpServers").or_else(|| v.get("servers")).unwrap_or(v);
        let obj = match map.as_object() {
            Some(o) => o,
            None => return vec![],
        };
        let mut out = Vec::new();
        for (name, cfg) in obj {
            // Skip an explicitly-disabled entry.
            if cfg.get("disabled").and_then(|x| x.as_bool()) == Some(true) {
                continue;
            }
            let command = cfg.get("command").and_then(|x| x.as_str()).unwrap_or("").to_string();
            if command.is_empty() {
                continue;
            }
            let args = cfg
                .get("args")
                .and_then(|x| x.as_array())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                .unwrap_or_default();
            let env = cfg
                .get("env")
                .and_then(|x| x.as_object())
                .map(|o| o.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
                .unwrap_or_default();
            out.push(Self { name: name.clone(), command, args, env });
        }
        out
    }
}

/// A tool exposed by an MCP server.
#[derive(Clone, Debug)]
pub struct McpTool {
    pub server: String,
    pub name: String, // the bare name on the server
    pub description: String,
    pub read_only: bool,
    pub input_schema: Value,
}

impl McpTool {
    /// The collision-free id the agent loop selects by: `mcp.<server>.<tool>`.
    pub fn qualified(&self) -> String {
        format!("mcp.{}.{}", self.server, self.name)
    }
}

/// Read-only if the server annotates it so; otherwise a conservative verb heuristic (when unknown,
/// treat as mutating so it must clear the harm-gate).
fn classify_read_only(tool: &Value, name: &str) -> bool {
    if let Some(b) = tool.get("annotations").and_then(|a| a.get("readOnlyHint")).and_then(|x| x.as_bool()) {
        return b;
    }
    if tool.get("annotations").and_then(|a| a.get("destructiveHint")).and_then(|x| x.as_bool()) == Some(true) {
        return false;
    }
    let n = name.to_lowercase();
    const READ: [&str; 12] =
        ["get", "list", "search", "read", "fetch", "query", "find", "lookup", "describe", "show", "check", "retrieve"];
    READ.iter().any(|p| n.starts_with(p) || n.contains(&format!("_{p}")))
}

/// Flatten an MCP `tools/call` result (`{ content: [{type:"text", text}, ...], isError? }`) to text.
fn render_tool_result(r: &Value) -> String {
    let is_err = r.get("isError").and_then(|x| x.as_bool()).unwrap_or(false);
    let mut out = String::new();
    if let Some(arr) = r.get("content").and_then(|x| x.as_array()) {
        for c in arr {
            match c.get("type").and_then(|x| x.as_str()) {
                Some("text") => {
                    if let Some(t) = c.get("text").and_then(|x| x.as_str()) {
                        out.push_str(t);
                        out.push('\n');
                    }
                }
                Some(other) => out.push_str(&format!("[{other} content]\n")),
                None => {}
            }
        }
    }
    let out = out.trim().to_string();
    let out = if out.is_empty() { "(no content)".to_string() } else { out };
    if is_err {
        format!("(tool error) {out}")
    } else {
        out
    }
}

/// A live connection to one MCP server (owns the subprocess + its stdio). All I/O is blocking.
struct Conn {
    name: String,
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
    tools: Vec<McpTool>,
}

impl Conn {
    fn connect(cfg: &McpServerConfig, timeout: Duration) -> anyhow::Result<Self> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        for (k, v) in &cfg.env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("spawn '{}': {e}", cfg.command))?;
        let stdin = child.stdin.take().ok_or_else(|| anyhow::anyhow!("no stdin"))?;
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
        let reader = BufReader::new(stdout);
        let mut c = Self { name: cfg.name.clone(), child, stdin, reader, next_id: 0, tools: vec![] };
        c.handshake(timeout)?;
        c.tools = c.list_tools(timeout)?;
        Ok(c)
    }

    fn send(&mut self, msg: &Value) -> anyhow::Result<()> {
        let line = serde_json::to_string(msg)?;
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()?;
        Ok(())
    }

    /// Read line-delimited JSON-RPC until the response with matching `id` arrives, skipping any
    /// interleaved notifications / unrelated messages.
    fn read_reply(&mut self, id: i64) -> anyhow::Result<Value> {
        loop {
            let mut line = String::new();
            let n = self.reader.read_line(&mut line)?;
            if n == 0 {
                anyhow::bail!("{}: server closed the connection", self.name);
            }
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue, // a non-JSON log line on stdout — ignore
            };
            if v.get("id").and_then(|x| x.as_i64()) == Some(id) {
                if let Some(err) = v.get("error") {
                    anyhow::bail!("{}: {}", self.name, err.get("message").and_then(|m| m.as_str()).unwrap_or("error"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            // else: a notification or an unrelated id — keep reading
        }
    }

    fn request(&mut self, method: &str, params: Value, _timeout: Duration) -> anyhow::Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        self.read_reply(id)
    }

    fn notify(&mut self, method: &str, params: Value) -> anyhow::Result<()> {
        self.send(&json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    fn handshake(&mut self, timeout: Duration) -> anyhow::Result<()> {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {"name":"yantrik-mind","version":"1.0"}
            }),
            timeout,
        )?;
        self.notify("notifications/initialized", json!({}))?;
        Ok(())
    }

    fn list_tools(&mut self, timeout: Duration) -> anyhow::Result<Vec<McpTool>> {
        let r = self.request("tools/list", json!({}), timeout)?;
        let arr = r.get("tools").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        let server = self.name.clone();
        Ok(arr
            .iter()
            .map(|t| {
                let name = t.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let description = t.get("description").and_then(|x| x.as_str()).unwrap_or("").to_string();
                let input_schema = t.get("inputSchema").cloned().unwrap_or(json!({}));
                let read_only = classify_read_only(t, &name);
                McpTool { server: server.clone(), name, description, read_only, input_schema }
            })
            .filter(|t| !t.name.is_empty())
            .collect())
    }

    fn call(&mut self, tool: &str, args: &Value, timeout: Duration) -> anyhow::Result<String> {
        let r = self.request("tools/call", json!({"name": tool, "arguments": args}), timeout)?;
        Ok(render_tool_result(&r))
    }

    fn shutdown(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Manages all configured MCP servers + the aggregate tool index. Connections are established by
/// `connect_all` (blocking, slow — a cold `npx` start downloads the package), so the caller runs it
/// on a background thread; the catalog reflects whatever has connected so far. Calls are blocking
/// (`call_blocking`, run via `spawn_blocking`).
pub struct McpHub {
    servers: Mutex<HashMap<String, Arc<Mutex<Conn>>>>,
    tools: Mutex<Vec<McpTool>>,
    timeout: Duration,
}

impl Default for McpHub {
    fn default() -> Self {
        Self::new()
    }
}

impl McpHub {
    pub fn new() -> Self {
        Self { servers: Mutex::new(HashMap::new()), tools: Mutex::new(vec![]), timeout: Duration::from_secs(45) }
    }

    /// Connect to every configured server. Failures are logged + skipped — one broken/slow server
    /// never sinks the rest. Blocking: call from a background thread.
    pub fn connect_all(&self, configs: &[McpServerConfig]) {
        for cfg in configs {
            match Conn::connect(cfg, self.timeout) {
                Ok(conn) => {
                    let n = conn.tools.len();
                    self.tools.lock().unwrap().extend(conn.tools.iter().cloned());
                    self.servers.lock().unwrap().insert(cfg.name.clone(), Arc::new(Mutex::new(conn)));
                    eprintln!("[mcp] connected '{}' ({n} tools)", cfg.name);
                }
                Err(e) => eprintln!("[mcp] '{}' failed: {e}", cfg.name),
            }
        }
    }

    /// Every discovered tool across all connected servers.
    pub fn tools(&self) -> Vec<McpTool> {
        self.tools.lock().unwrap().clone()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.lock().unwrap().is_empty()
    }

    /// A compact catalog of the connected MCP tools for the agent prompt.
    pub fn catalog(&self) -> String {
        let tools = self.tools.lock().unwrap();
        if tools.is_empty() {
            return String::new();
        }
        let mut s = String::from(
            "\nCONNECTED INTEGRATIONS (MCP — call by the EXACT id; read-only run instantly, writes need the user's ok):",
        );
        for t in tools.iter() {
            let lock = if t.read_only { "" } else { " [write — gated]" };
            let desc = t.description.lines().next().unwrap_or("").chars().take(100).collect::<String>();
            s.push_str(&format!("\n- {} — {desc}{lock}", t.qualified()));
        }
        s
    }

    pub fn lookup(&self, qualified: &str) -> Option<McpTool> {
        self.tools.lock().unwrap().iter().find(|t| t.qualified() == qualified).cloned()
    }

    /// Call a tool by its qualified id (`mcp.<server>.<tool>`). Blocking — run inside `spawn_blocking`.
    pub fn call_blocking(&self, qualified: &str, args: &Value) -> anyhow::Result<String> {
        let t = self.lookup(qualified).ok_or_else(|| anyhow::anyhow!("no such integration tool"))?;
        let conn = self
            .servers
            .lock()
            .unwrap()
            .get(&t.server)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("integration '{}' not connected", t.server))?;
        let mut c = conn.lock().unwrap();
        c.call(&t.name, args, self.timeout)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mcpservers_config() {
        let v = json!({
            "mcpServers": {
                "github": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-github"], "env": { "GITHUB_TOKEN": "x" } },
                "off": { "command": "npx", "args": [], "disabled": true },
                "nocmd": { "args": ["whatever"] }
            }
        });
        let cfgs = McpServerConfig::from_json(&v);
        assert_eq!(cfgs.len(), 1, "skips disabled + command-less entries");
        assert_eq!(cfgs[0].name, "github");
        assert_eq!(cfgs[0].command, "npx");
        assert_eq!(cfgs[0].args, vec!["-y", "@modelcontextprotocol/server-github"]);
        assert_eq!(cfgs[0].env, vec![("GITHUB_TOKEN".to_string(), "x".to_string())]);
    }

    #[test]
    fn classifies_read_only_by_hint_then_heuristic() {
        // explicit annotation wins
        assert!(classify_read_only(&json!({"annotations":{"readOnlyHint":true}}), "send_message"));
        assert!(!classify_read_only(&json!({"annotations":{"readOnlyHint":false}}), "list_things"));
        assert!(!classify_read_only(&json!({"annotations":{"destructiveHint":true}}), "get_thing"));
        // heuristic fallback
        assert!(classify_read_only(&json!({}), "search_repositories"));
        assert!(classify_read_only(&json!({}), "get_file_contents"));
        assert!(classify_read_only(&json!({}), "notion_query_database"));
        assert!(!classify_read_only(&json!({}), "create_issue"));
        assert!(!classify_read_only(&json!({}), "send_email"));
        assert!(!classify_read_only(&json!({}), "delete_page"));
    }

    #[test]
    fn renders_tool_result_text_and_errors() {
        let ok = json!({"content":[{"type":"text","text":"hello"},{"type":"text","text":"world"}]});
        assert_eq!(render_tool_result(&ok), "hello\nworld");
        let err = json!({"isError":true,"content":[{"type":"text","text":"boom"}]});
        assert_eq!(render_tool_result(&err), "(tool error) boom");
        let img = json!({"content":[{"type":"image","data":"..."}]});
        assert_eq!(render_tool_result(&img), "[image content]");
        assert_eq!(render_tool_result(&json!({"content":[]})), "(no content)");
    }

    #[test]
    fn qualified_id_is_collision_free() {
        let t = McpTool { server: "github".into(), name: "create_issue".into(), description: String::new(), read_only: false, input_schema: json!({}) };
        assert_eq!(t.qualified(), "mcp.github.create_issue");
    }
}
