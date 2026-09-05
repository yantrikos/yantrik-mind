//! E.CODERNIM1: an Anthropic-compatible `/v1/messages` on the control port, translated to an
//! OpenAI-compatible upstream (NVIDIA NIM by default).
//!
//! The coder is the Claude Code CLI, which speaks the Anthropic Messages API — content blocks,
//! `tool_use` / `tool_result`, SSE events. NIM speaks OpenAI chat completions. No configuration
//! bridges that, so the mind does: the CLI is pointed at the mind's own loopback port with the
//! console token, and the mind forwards with the NIM key, which the CLI never sees.
//!
//! Translation is PURE (`to_openai`, `from_openai`, `StreamTranslator`) and tested on recorded
//! shapes; the transport is the thin part at the bottom.

use serde_json::{json, Value};
use std::io::Write;

/// Where the OpenAI-compatible upstream lives. Overridable so containment can point it at a proxy.
pub fn upstream_base() -> String {
    std::env::var("YM_NIM_BASE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://integrate.api.nvidia.com/v1".to_string())
}

/// The model the CLI's own names (`claude-haiku-…`, `claude-sonnet-…`) map to. A request that names
/// a NIM model (`vendor/model`) keeps it.
pub fn default_model() -> String {
    std::env::var("YM_CODER_MODEL")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_MODEL.to_string())
}

pub const DEFAULT_MODEL: &str = "deepseek-ai/deepseek-v4-pro-0813";

/// Providers reject `max_tokens` above the model's ceiling with a 400; the CLI asks for large
/// values routinely. Clamped, never refused.
fn max_tokens_ceiling() -> u64 {
    std::env::var("YM_NIM_MAX_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|n| *n > 0)
        .unwrap_or(16_384)
}

fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter_map(|b| match b.get("type").and_then(Value::as_str) {
                Some("text") => b.get("text").and_then(Value::as_str).map(str::to_string),
                Some("image") => Some("[image omitted: this model has no vision]".to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn resolve_model(requested: Option<&str>, default: &str) -> String {
    match requested {
        Some(m) if m.contains('/') => m.to_string(),
        _ => default.to_string(),
    }
}

/// Anthropic Messages request → OpenAI chat-completions body.
pub fn to_openai(req: &Value, default_model: &str) -> Result<Value, String> {
    let mut messages: Vec<Value> = Vec::new();
    let system = req.get("system").map(text_of).unwrap_or_default();
    if !system.trim().is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    let Some(turns) = req.get("messages").and_then(Value::as_array) else {
        return Err("messages: required array".into());
    };
    for turn in turns {
        let role = turn.get("role").and_then(Value::as_str).unwrap_or("user");
        let content = turn.get("content").cloned().unwrap_or(Value::Null);
        match (role, &content) {
            ("assistant", Value::Array(blocks)) => {
                let mut text = String::new();
                let mut tool_calls = Vec::new();
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                if !text.is_empty() {
                                    text.push('\n');
                                }
                                text.push_str(t);
                            }
                        }
                        Some("tool_use") => tool_calls.push(json!({
                            "id": b.get("id").and_then(Value::as_str).unwrap_or(""),
                            "type": "function",
                            "function": {
                                "name": b.get("name").and_then(Value::as_str).unwrap_or(""),
                                "arguments": b.get("input").map(|i| i.to_string()).unwrap_or_else(|| "{}".into()),
                            }
                        })),
                        // thinking / redacted_thinking: the model's own, never replayed upstream.
                        _ => {}
                    }
                }
                let mut m = json!({"role": "assistant"});
                m["content"] = if text.is_empty() { Value::Null } else { Value::String(text) };
                if !tool_calls.is_empty() {
                    m["tool_calls"] = Value::Array(tool_calls);
                }
                messages.push(m);
            }
            ("user", Value::Array(blocks)) => {
                // tool_result blocks become `tool` messages, which must directly follow the
                // assistant's tool_calls; any text in the same turn follows them as a user message.
                let mut text_parts = Vec::new();
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("tool_result") => {
                            let body = b.get("content").map(text_of).unwrap_or_default();
                            let body = if b.get("is_error").and_then(Value::as_bool).unwrap_or(false) {
                                format!("[tool error] {body}")
                            } else {
                                body
                            };
                            messages.push(json!({
                                "role": "tool",
                                "tool_call_id": b.get("tool_use_id").and_then(Value::as_str).unwrap_or(""),
                                "content": body,
                            }));
                        }
                        Some("text") => {
                            if let Some(t) = b.get("text").and_then(Value::as_str) {
                                text_parts.push(t.to_string());
                            }
                        }
                        Some("image") => text_parts.push("[image omitted: this model has no vision]".into()),
                        _ => {}
                    }
                }
                if !text_parts.is_empty() {
                    messages.push(json!({"role": "user", "content": text_parts.join("\n")}));
                }
            }
            (r, Value::String(s)) => messages.push(json!({"role": r, "content": s})),
            (r, other) => messages.push(json!({"role": r, "content": text_of(other)})),
        }
    }
    let mut body = json!({
        "model": resolve_model(req.get("model").and_then(Value::as_str), default_model),
        "messages": messages,
        "max_tokens": req.get("max_tokens").and_then(Value::as_u64).unwrap_or(4096).min(max_tokens_ceiling()),
        "stream": req.get("stream").and_then(Value::as_bool).unwrap_or(false),
    });
    for k in ["temperature", "top_p"] {
        if let Some(v) = req.get(k) {
            body[k] = v.clone();
        }
    }
    if let Some(stops) = req.get("stop_sequences").filter(|s| s.as_array().is_some_and(|a| !a.is_empty())) {
        body["stop"] = stops.clone();
    }
    if let Some(tools) = req.get("tools").and_then(Value::as_array).filter(|t| !t.is_empty()) {
        let fns: Vec<Value> = tools
            .iter()
            .filter_map(|t| {
                let name = t.get("name").and_then(Value::as_str)?;
                Some(json!({
                    "type": "function",
                    "function": {
                        "name": name,
                        "description": t.get("description").and_then(Value::as_str).unwrap_or(""),
                        "parameters": t.get("input_schema").cloned()
                            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                    }
                }))
            })
            .collect();
        if !fns.is_empty() {
            body["tools"] = Value::Array(fns);
            if let Some(tc) = req.get("tool_choice") {
                body["tool_choice"] = match tc.get("type").and_then(Value::as_str) {
                    Some("any") => json!("required"),
                    Some("none") => json!("none"),
                    Some("tool") => json!({"type": "function", "function": {"name": tc.get("name").and_then(Value::as_str).unwrap_or("")}}),
                    _ => json!("auto"),
                };
            }
        }
    }
    if body["stream"].as_bool() == Some(true) {
        body["stream_options"] = json!({"include_usage": true});
    }
    Ok(body)
}

fn stop_reason_of(finish: Option<&str>) -> &'static str {
    match finish {
        Some("tool_calls") | Some("function_call") => "tool_use",
        Some("length") => "max_tokens",
        _ => "end_turn",
    }
}

fn parse_args(args: &str) -> Value {
    serde_json::from_str::<Value>(args)
        .ok()
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// OpenAI non-stream response → Anthropic message.
pub fn from_openai(resp: &Value, model: &str) -> Value {
    let choice = &resp["choices"][0];
    let msg = &choice["message"];
    let mut content = Vec::new();
    if let Some(t) = msg.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
        content.push(json!({"type": "text", "text": t}));
    }
    let mut had_tools = false;
    if let Some(calls) = msg.get("tool_calls").and_then(Value::as_array) {
        for (i, c) in calls.iter().enumerate() {
            had_tools = true;
            content.push(json!({
                "type": "tool_use",
                "id": c.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("call_{i}")),
                "name": c["function"]["name"].as_str().unwrap_or(""),
                "input": parse_args(c["function"]["arguments"].as_str().unwrap_or("{}")),
            }));
        }
    }
    if content.is_empty() {
        content.push(json!({"type": "text", "text": ""}));
    }
    let finish = choice.get("finish_reason").and_then(Value::as_str);
    // A tool call is a tool call whatever the upstream calls the finish: NIM's models answered
    // `finish_reason: "stop"` beside `tool_calls` on the first live run, and "end_turn" there
    // means the CLI never runs the tool. Only a cut-off generation overrides it.
    let stop = if had_tools && finish != Some("length") { "tool_use" } else { stop_reason_of(finish) };
    json!({
        "id": resp.get("id").and_then(Value::as_str).unwrap_or("msg_gateway"),
        "type": "message",
        "role": "assistant",
        "model": model,
        "content": content,
        "stop_reason": stop,
        "stop_sequence": null,
        "usage": {
            "input_tokens": resp["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            "output_tokens": resp["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        }
    })
}

/// The Anthropic error envelope, status kept in the message so the CLI's own words still carry it.
pub fn anthropic_error(status: u16, message: &str) -> Value {
    let kind = match status {
        401 | 403 => "authentication_error",
        429 => "rate_limit_error",
        400..=499 => "invalid_request_error",
        _ => "api_error",
    };
    json!({"type": "error", "error": {"type": kind, "message": format!("{status}: {message}")}})
}

/// A rough count for `/v1/messages/count_tokens`: the CLI uses it for budgeting, not billing.
pub fn count_tokens(req: &Value) -> Value {
    let chars = req.get("system").map(|s| text_of(s).len()).unwrap_or(0)
        + req
            .get("messages")
            .and_then(Value::as_array)
            .map(|ms| ms.iter().map(|m| m.to_string().len()).sum())
            .unwrap_or(0)
        + req.get("tools").map(|t| t.to_string().len()).unwrap_or(0);
    json!({"input_tokens": (chars / 4).max(1)})
}

/// OpenAI SSE chunks → Anthropic SSE events, one open content block at a time.
#[derive(Default)]
pub struct StreamTranslator {
    started: bool,
    next_index: usize,
    /// (anthropic block index) of the open text block, if any.
    open_text: Option<usize>,
    /// openai tool index → anthropic block index, for the currently open tool block.
    open_tool: Option<(usize, usize)>,
    stop_reason: Option<&'static str>,
    output_tokens: u64,
    input_tokens: u64,
    approx_chars: usize,
    model: String,
    id: String,
}

impl StreamTranslator {
    pub fn new(model: &str) -> Self {
        Self { model: model.to_string(), id: "msg_gateway".into(), ..Default::default() }
    }

    fn start(&mut self, out: &mut Vec<(String, Value)>) {
        if !self.started {
            self.started = true;
            out.push(("message_start".into(), json!({"type": "message_start", "message": {
                "id": self.id, "type": "message", "role": "assistant", "model": self.model,
                "content": [], "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}}})));
        }
    }

    fn close_open(&mut self, out: &mut Vec<(String, Value)>) {
        if let Some(i) = self.open_text.take() {
            out.push(("content_block_stop".into(), json!({"type": "content_block_stop", "index": i})));
        }
        if let Some((_, i)) = self.open_tool.take() {
            out.push(("content_block_stop".into(), json!({"type": "content_block_stop", "index": i})));
        }
    }

    /// Feed one SSE line from the upstream. Returns the Anthropic events it produces.
    pub fn feed(&mut self, line: &str) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        let Some(data) = line.strip_prefix("data:").map(str::trim) else { return out };
        if data == "[DONE]" {
            return out;
        }
        let Ok(chunk) = serde_json::from_str::<Value>(data) else { return out };
        if let Some(id) = chunk.get("id").and_then(Value::as_str) {
            if !self.started {
                self.id = id.to_string();
            }
        }
        self.start(&mut out);
        if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
            if let Some(n) = u["completion_tokens"].as_u64() {
                self.output_tokens = n;
            }
            if let Some(n) = u["prompt_tokens"].as_u64() {
                self.input_tokens = n;
            }
        }
        let choice = &chunk["choices"][0];
        let delta = &choice["delta"];
        // reasoning_content is the model thinking aloud (Kimi K3 does); it is not an answer block.
        if let Some(t) = delta.get("content").and_then(Value::as_str).filter(|t| !t.is_empty()) {
            if self.open_tool.is_some() {
                self.close_open(&mut out);
            }
            let i = match self.open_text {
                Some(i) => i,
                None => {
                    let i = self.next_index;
                    self.next_index += 1;
                    self.open_text = Some(i);
                    out.push(("content_block_start".into(), json!({"type": "content_block_start", "index": i, "content_block": {"type": "text", "text": ""}})));
                    i
                }
            };
            self.approx_chars += t.len();
            out.push(("content_block_delta".into(), json!({"type": "content_block_delta", "index": i, "delta": {"type": "text_delta", "text": t}})));
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for c in calls {
                let oi = c.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                let is_new = c.get("id").and_then(Value::as_str).is_some_and(|s| !s.is_empty())
                    || self.open_tool.map(|(o, _)| o != oi).unwrap_or(true);
                if is_new && self.open_tool.map(|(o, _)| o != oi).unwrap_or(true) {
                    self.close_open(&mut out);
                    let i = self.next_index;
                    self.next_index += 1;
                    self.open_tool = Some((oi, i));
                    out.push(("content_block_start".into(), json!({"type": "content_block_start", "index": i, "content_block": {
                        "type": "tool_use",
                        "id": c.get("id").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| format!("call_{oi}")),
                        "name": c["function"]["name"].as_str().unwrap_or(""),
                        "input": {}}})));
                }
                if let Some((_, i)) = self.open_tool {
                    if let Some(a) = c["function"]["arguments"].as_str().filter(|a| !a.is_empty()) {
                        self.approx_chars += a.len();
                        out.push(("content_block_delta".into(), json!({"type": "content_block_delta", "index": i, "delta": {"type": "input_json_delta", "partial_json": a}})));
                    }
                }
            }
        }
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.stop_reason = Some(if self.open_tool.is_some() && f != "length" { "tool_use" } else { stop_reason_of(Some(f)) });
        }
        out
    }

    /// The upstream is done: close what is open, say why, and end the message.
    pub fn finish(&mut self) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        self.start(&mut out);
        let saw_tool = self.open_tool.is_some();
        self.close_open(&mut out);
        let stop = self.stop_reason.unwrap_or(if saw_tool { "tool_use" } else { "end_turn" });
        let output = if self.output_tokens > 0 { self.output_tokens } else { (self.approx_chars / 4) as u64 };
        out.push(("message_delta".into(), json!({"type": "message_delta", "delta": {"stop_reason": stop, "stop_sequence": null}, "usage": {"output_tokens": output}})));
        out.push(("message_stop".into(), json!({"type": "message_stop"})));
        out
    }
}

pub fn sse_frame(event: &str, data: &Value) -> String {
    format!("event: {event}\ndata: {data}\n\n")
}

// ── transport ────────────────────────────────────────────────────────────────────────────────────

fn write_json(w: &mut impl Write, status: &str, body: &Value) {
    let b = body.to_string();
    let _ = w.write_all(
        format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", b.len()).as_bytes(),
    );
    let _ = w.write_all(b.as_bytes());
    let _ = w.flush();
}

fn write_chunk(w: &mut impl Write, s: &str) {
    let _ = w.write_all(format!("{:x}\r\n{s}\r\n", s.len()).as_bytes());
    let _ = w.flush();
}

fn upstream_key() -> Option<String> {
    std::env::var("NVIDIA_API_KEY").ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// `POST /v1/messages/count_tokens`.
pub fn handle_count_tokens(w: &mut impl Write, body: &str) {
    match serde_json::from_str::<Value>(body) {
        Ok(req) => write_json(w, "200 OK", &count_tokens(&req)),
        Err(e) => write_json(w, "400 Bad Request", &anthropic_error(400, &format!("bad json: {e}"))),
    }
}

/// `POST /v1/messages`: translate, forward with the upstream key, translate back — streamed when
/// the CLI asked for a stream.
pub fn handle_messages(w: &mut impl Write, body: &str) {
    let req: Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return write_json(w, "400 Bad Request", &anthropic_error(400, &format!("bad json: {e}"))),
    };
    let Some(key) = upstream_key() else {
        return write_json(w, "503 Service Unavailable", &anthropic_error(503, "NVIDIA_API_KEY is not set on this mind; the coder lane has no upstream"));
    };
    let model = default_model();
    let oa = match to_openai(&req, &model) {
        Ok(b) => b,
        Err(e) => return write_json(w, "400 Bad Request", &anthropic_error(400, &e)),
    };
    let used_model = oa["model"].as_str().unwrap_or(&model).to_string();
    let streaming = oa["stream"].as_bool() == Some(true);
    let url = format!("{}/chat/completions", upstream_base());
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(20))
        .timeout_read(std::time::Duration::from_secs(600))
        .build();
    let resp = agent
        .post(&url)
        .set("Authorization", &format!("Bearer {key}"))
        .set("Content-Type", "application/json")
        .set("Accept", if streaming { "text/event-stream" } else { "application/json" })
        .send_string(&oa.to_string());
    let resp = match resp {
        Ok(r) => r,
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let text: String = text.chars().take(600).collect();
            eprintln!("[gateway] upstream {code} for {used_model}: {}", text.replace('\n', " "));
            let status = format!("{code} {}", match code { 401 => "Unauthorized", 403 => "Forbidden", 429 => "Too Many Requests", 400 => "Bad Request", 404 => "Not Found", _ => "Bad Gateway" });
            return write_json(w, &status, &anthropic_error(code, &text));
        }
        Err(e) => {
            eprintln!("[gateway] upstream unreachable: {e}");
            return write_json(w, "502 Bad Gateway", &anthropic_error(502, &format!("upstream unreachable: {e}")));
        }
    };
    if !streaming {
        let text = resp.into_string().unwrap_or_default();
        return match serde_json::from_str::<Value>(&text) {
            Ok(v) => {
                let out = from_openai(&v, &used_model);
                eprintln!("[gateway] {used_model} non-stream: stop={} (upstream finish={}) in={} out={}", out["stop_reason"], v["choices"][0]["finish_reason"], out["usage"]["input_tokens"], out["usage"]["output_tokens"]);
                write_json(w, "200 OK", &out)
            }
            Err(e) => write_json(w, "502 Bad Gateway", &anthropic_error(502, &format!("upstream sent no json: {e}"))),
        };
    }
    let _ = w.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nTransfer-Encoding: chunked\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n");
    let mut tr = StreamTranslator::new(&used_model);
    let reader = std::io::BufReader::new(resp.into_reader());
    use std::io::BufRead;
    for line in reader.lines() {
        let Ok(line) = line else { break };
        for (ev, data) in tr.feed(&line) {
            write_chunk(w, &sse_frame(&ev, &data));
        }
    }
    let tail = tr.finish();
    let stop = tail.iter().find(|(e, _)| e == "message_delta").map(|(_, d)| d["delta"]["stop_reason"].to_string()).unwrap_or_default();
    for (ev, data) in tail {
        write_chunk(w, &sse_frame(&ev, &data));
    }
    eprintln!("[gateway] {used_model} stream: stop={stop} blocks={}", tr.next_index);
    let _ = w.write_all(b"0\r\n\r\n");
    let _ = w.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The shape Claude Code sends on its second turn: system, tools, a prior tool_use and its
    /// tool_result, then the model's next move.
    fn recorded_request() -> Value {
        json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 32000,
            "stream": false,
            "system": [{"type": "text", "text": "You are Claude Code.", "cache_control": {"type": "ephemeral"}}],
            "tools": [{"name": "Read", "description": "Read a file", "input_schema": {"type": "object", "properties": {"file_path": {"type": "string"}}, "required": ["file_path"]}}],
            "messages": [
                {"role": "user", "content": "Write hello.py"},
                {"role": "assistant", "content": [{"type": "text", "text": "Let me look."}, {"type": "tool_use", "id": "toolu_01", "name": "Read", "input": {"file_path": "/w/hello.py"}}]},
                {"role": "user", "content": [{"type": "tool_result", "tool_use_id": "toolu_01", "content": "print('hi')"}, {"type": "text", "text": "continue"}]}
            ]
        })
    }

    #[test]
    fn a_recorded_claude_code_request_translates_field_by_field() {
        let oa = to_openai(&recorded_request(), "minimaxai/minimax-m3").unwrap();
        assert_eq!(oa["model"], "minimaxai/minimax-m3", "the CLI's own model name maps to the configured one");
        assert_eq!(oa["max_tokens"], 16384, "clamped to the ceiling");
        let m = oa["messages"].as_array().unwrap();
        assert_eq!(m[0]["role"], "system");
        assert_eq!(m[0]["content"], "You are Claude Code.");
        assert_eq!(m[1], json!({"role": "user", "content": "Write hello.py"}));
        assert_eq!(m[2]["role"], "assistant");
        assert_eq!(m[2]["content"], "Let me look.");
        assert_eq!(m[2]["tool_calls"][0]["id"], "toolu_01");
        assert_eq!(m[2]["tool_calls"][0]["function"]["name"], "Read");
        assert_eq!(m[2]["tool_calls"][0]["function"]["arguments"], "{\"file_path\":\"/w/hello.py\"}");
        assert_eq!(m[3], json!({"role": "tool", "tool_call_id": "toolu_01", "content": "print('hi')"}), "a tool_result is a tool message carrying the call id");
        assert_eq!(m[4], json!({"role": "user", "content": "continue"}));
        assert_eq!(oa["tools"][0]["type"], "function");
        assert_eq!(oa["tools"][0]["function"]["name"], "Read");
        assert_eq!(oa["tools"][0]["function"]["parameters"]["required"][0], "file_path");
        assert!(oa.get("stream_options").is_none(), "no usage option unless streaming");
    }

    #[test]
    fn a_nim_model_name_is_kept_and_tool_choice_maps() {
        let mut r = recorded_request();
        r["model"] = json!("deepseek-ai/deepseek-v4-pro-0813");
        r["tool_choice"] = json!({"type": "any"});
        r["stream"] = json!(true);
        let oa = to_openai(&r, "minimaxai/minimax-m3").unwrap();
        assert_eq!(oa["model"], "deepseek-ai/deepseek-v4-pro-0813");
        assert_eq!(oa["tool_choice"], "required");
        assert_eq!(oa["stream_options"]["include_usage"], true);
        r["tool_choice"] = json!({"type": "tool", "name": "Read"});
        assert_eq!(to_openai(&r, "x/y").unwrap()["tool_choice"]["function"]["name"], "Read");
    }

    #[test]
    fn a_nim_reply_with_a_tool_call_becomes_tool_use_and_stops_for_it() {
        let resp = json!({"id": "chatcmpl-1", "choices": [{"index": 0, "message": {"role": "assistant", "content": null,
            "tool_calls": [{"id": "call_a", "type": "function", "function": {"name": "read_file", "arguments": "{\"path\":\"notes.txt\"}"}}]},
            "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 200, "completion_tokens": 31}});
        let a = from_openai(&resp, "minimaxai/minimax-m3");
        assert_eq!(a["stop_reason"], "tool_use", "the agentic loop turns on this word");
        assert_eq!(a["content"][0]["type"], "tool_use");
        assert_eq!(a["content"][0]["id"], "call_a");
        assert_eq!(a["content"][0]["input"]["path"], "notes.txt");
        assert_eq!(a["usage"]["input_tokens"], 200);
        // The shape NIM actually sent on the first live run: a tool call with finish "stop".
        let mut stop_beside_tools = resp.clone();
        stop_beside_tools["choices"][0]["finish_reason"] = json!("stop");
        assert_eq!(from_openai(&stop_beside_tools, "m")["stop_reason"], "tool_use", "a tool call is a tool call whatever the finish word");
        let plain = json!({"choices": [{"message": {"content": "done"}, "finish_reason": "stop"}]});
        let a = from_openai(&plain, "m");
        assert_eq!(a["stop_reason"], "end_turn");
        assert_eq!(a["content"][0]["text"], "done");
        let cut = json!({"choices": [{"message": {"content": "x"}, "finish_reason": "length"}]});
        assert_eq!(from_openai(&cut, "m")["stop_reason"], "max_tokens");
    }

    /// A recorded NIM stream: Kimi-style reasoning deltas first, then text, then a tool call in
    /// fragments, then usage. The Anthropic event order is fixed and the arguments reassemble.
    #[test]
    fn a_recorded_nim_stream_yields_the_anthropic_event_sequence() {
        let lines = [
            r#"data: {"id":"chatcmpl-9","choices":[{"index":0,"delta":{"role":"assistant","reasoning_content":"Think"},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-9","choices":[{"index":0,"delta":{"reasoning_content":"ing"},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-9","choices":[{"index":0,"delta":{"content":"I will "},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-9","choices":[{"index":0,"delta":{"content":"read it."},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-9","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_z","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-9","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-9","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"notes.txt\"}"}}]},"finish_reason":null}]}"#,
            r#"data: {"id":"chatcmpl-9","choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":120,"completion_tokens":40}}"#,
            "data: [DONE]",
        ];
        let mut tr = StreamTranslator::new("moonshotai/kimi-k3");
        let mut events: Vec<(String, Value)> = Vec::new();
        for l in lines {
            events.extend(tr.feed(l));
        }
        events.extend(tr.finish());
        let kinds: Vec<&str> = events.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(kinds, [
            "message_start",
            "content_block_start", "content_block_delta", "content_block_delta", "content_block_stop",
            "content_block_start", "content_block_delta", "content_block_delta", "content_block_stop",
            "message_delta", "message_stop",
        ], "{kinds:?}");
        assert_eq!(events[0].1["message"]["id"], "chatcmpl-9");
        assert_eq!(events[1].1["content_block"]["type"], "text");
        assert_eq!(events[5].1["content_block"], json!({"type": "tool_use", "id": "call_z", "name": "read_file", "input": {}}));
        let args: String = events[6..8].iter().map(|(_, d)| d["delta"]["partial_json"].as_str().unwrap()).collect();
        assert_eq!(args, "{\"path\":\"notes.txt\"}", "arguments reassemble across fragments");
        assert_eq!(events[9].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(events[9].1["usage"]["output_tokens"], 40);
        assert!(!events.iter().any(|(_, d)| d.to_string().contains("Thinking")), "reasoning is swallowed");
    }

    #[test]
    fn a_stream_that_ends_without_a_finish_reason_still_closes_cleanly() {
        let mut tr = StreamTranslator::new("m");
        let mut ev = tr.feed(r#"data: {"choices":[{"index":0,"delta":{"content":"hi"}}]}"#);
        ev.extend(tr.finish());
        let kinds: Vec<&str> = ev.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(kinds, ["message_start", "content_block_start", "content_block_delta", "content_block_stop", "message_delta", "message_stop"]);
        assert_eq!(ev[4].1["delta"]["stop_reason"], "end_turn");
        assert_eq!(sse_frame("message_stop", &json!({"type": "message_stop"})), "event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n");
    }

    #[test]
    fn errors_keep_the_status_in_the_message_for_the_clis_own_words() {
        let e = anthropic_error(429, "Too many requests");
        assert_eq!(e["error"]["type"], "rate_limit_error");
        assert!(e["error"]["message"].as_str().unwrap().starts_with("429:"));
        assert_eq!(anthropic_error(401, "x")["error"]["type"], "authentication_error");
        assert_eq!(anthropic_error(502, "x")["error"]["type"], "api_error");
        assert!(mind_tools::coder::provider_refusal(&format!("API Error: {}", anthropic_error(401, "bad key")["error"]["message"].as_str().unwrap())).is_some());
    }

    #[test]
    fn count_tokens_is_positive_and_grows_with_the_request() {
        let small = count_tokens(&json!({"messages": [{"role": "user", "content": "hi"}]}))["input_tokens"].as_u64().unwrap();
        let big = count_tokens(&recorded_request())["input_tokens"].as_u64().unwrap();
        assert!(small >= 1 && big > small);
    }

    /// Live, gated: one round trip to NIM through the translation, non-stream and stream.
    #[test]
    fn live_round_trip_through_nim() {
        if std::env::var("YM_NIM_LIVE").ok().as_deref() != Some("1") {
            eprintln!("skipped: YM_NIM_LIVE!=1");
            return;
        }
        let req = json!({"model": "claude-haiku", "max_tokens": 200, "temperature": 0,
            "tools": [{"name": "read_file", "description": "Read a file", "input_schema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}}],
            "messages": [{"role": "user", "content": "Read notes.txt and tell me its first line. Use the tool."}]});
        for stream in [false, true] {
            let mut r = req.clone();
            r["stream"] = json!(stream);
            let mut buf: Vec<u8> = Vec::new();
            handle_messages(&mut buf, &r.to_string());
            let text = String::from_utf8_lossy(&buf).to_string();
            assert!(text.starts_with("HTTP/1.1 200"), "{}", text.chars().take(300).collect::<String>());
            assert!(text.contains("tool_use"), "stream={stream}: {}", text.chars().take(800).collect::<String>());
            assert!(text.contains("notes.txt"), "stream={stream}");
        }
    }
}
