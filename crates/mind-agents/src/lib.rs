//! mind-agents — bounded, gated sub-agents the mind can dispatch.
//!
//! A `SubAgent` runs a ReAct loop (think → call a read tool → observe → … → finish) over a SUBSET
//! of the mind's tools, bounded by a step budget. It reuses `mind_recipes::RecipeHost` as the tool
//! seam and the `InferencePool` for thinking. Safety by construction:
//!  - v1 sub-agents get READ tools only (no Act) — so they can't cause outward effects. (When act is
//!    added, it rides the same harm-gate + ActionRuntime as everything else.)
//!  - a hard step budget prevents runaway loops; a tool not in the allow-list is refused.
//!  - the sub-agent's answer is UNTRUSTED to the caller (wrap it) — it may include tool/web content.
//!
//! `fan_out` runs many sub-agent tasks concurrently; real parallelism comes from the InferencePool's
//! blocking pool (permits>1 for API backends).

pub mod bus;
pub mod cognition;
pub mod compile;
pub mod nba;
pub mod procedure;

pub use bus::{signature, Bus};
pub use cognition::{Cognition, Outcome, Step};
pub use compile::{compile, Compilation, Origin};
pub use nba::{Action, Verb, Why};
pub use procedure::{Procedure, ProcedureKind};

use std::sync::Arc;

use futures::future::join_all;
use mind_inference::InferencePool;
use mind_recipes::RecipeHost;
use mind_types::{
    ActionDecision, ActionIntent, ActionRequest, ActionRuntime, Capability, Event, EventBody,
    EventSource, RiskLevel, TurnContext,
};
use serde::Deserialize;
use yantrik_ml::{ChatMessage, GenerationConfig};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn dummy_ctx(req: &ActionRequest) -> TurnContext {
    TurnContext::new(
        Event {
            id: req.id.clone(),
            trace_id: req.id.clone(),
            source: EventSource::SelfReflection,
            body: EventBody::plain("sub-agent action"),
            ts: req.created_ms,
        },
        req.created_ms,
    )
}

#[derive(Debug, Clone)]
pub struct AgentResult {
    pub task: String,
    pub answer: String,
    pub steps: usize,
    /// A short trace of tool calls made (for transparency/audit).
    pub trace: Vec<String>,
    /// Outward actions the agent PROPOSED that need the human's confirmation — it cannot self-approve
    /// (the harm-gate's confirmation requirement is inviolable even for sub-agents).
    pub pending_actions: Vec<ActionRequest>,
    /// Source URLs the agent searched/fetched — for citations.
    pub sources: Vec<String>,
    /// Why this run produced no deliverable, when it produced none.
    ///
    /// A synthesis or inference failure used to be FORMATTED INTO `answer` — so every caller saw a
    /// non-empty string and had no way to tell an API error from a report. The job board banked
    /// `(sub-agent synthesis error: …)` as a finished deliverable and credited the skill with a
    /// success. A failure has to be a field: prose can be mistaken for content, a `Some` cannot
    /// (E.SK5).
    pub error: Option<String>,
}

impl AgentResult {
    /// Did this run actually produce something? The ONE place callers should ask.
    ///
    /// Guessing from `!answer.is_empty()` is what put an error message on the board under a tick.
    pub fn ok(&self) -> bool {
        self.error.is_none() && !self.answer.trim().is_empty()
    }
}

/// Pull http(s) URLs out of text (for collecting research sources).
fn extract_urls(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for marker in ["https://", "http://"] {
        let mut rest = text;
        while let Some(i) = rest.find(marker) {
            let tail = &rest[i..];
            let end = tail.find(|c: char| c.is_whitespace() || matches!(c, '"' | '<' | '>' | ')' | ']'))
                .unwrap_or(tail.len());
            let url = tail[..end].trim_end_matches(['.', ',', ';']).to_string();
            if url.len() > marker.len() {
                out.push(url);
            }
            rest = &tail[end..];
        }
    }
    out
}

/// `null` where a string was expected reads as absent, not as a type error.
///
/// The schema we hand the model is `{"action":…,"tool":"<name>","args":{},"answer":…}`, and the
/// natural thing to write when finishing is `"tool": null` — there is no tool. Serde rejected that
/// outright ("invalid type: null, expected a string"), so a perfectly well-formed finish decision
/// failed the typed parse and fell through to the lenient recovery path. Observed live 2026-08-11.
fn null_as_default<'de, D, T>(d: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(d)?.unwrap_or_default())
}

/// The LLM's decision each step.
#[derive(Debug, Deserialize, Default)]
struct Decision {
    #[serde(default, deserialize_with = "null_as_default")]
    action: String, // "call_tool" | "finish"
    #[serde(default, deserialize_with = "null_as_default")]
    tool: String,
    #[serde(default, deserialize_with = "null_as_default")]
    args: serde_json::Value,
    #[serde(default, deserialize_with = "null_as_default")]
    answer: String,
}

/// Model text that is meant to be PROSE, with any control envelope peeled off.
///
/// The budget-exhausted synthesis call at the end of `run` used its reply verbatim. A small model
/// that has spent the whole task emitting decision JSON keeps doing so on the last call too, so the
/// sub-agent's "answer" became the literal string
/// `{"action": "finish", "tool": null, "answer": "I cannot provide…\n\nNext Action: …"}` — which
/// reached the cockpit and was displayed to the user, `\n` escapes and all. The escapes are the tell:
/// real parsing would have turned them into newlines, so nothing had parsed it.
///
/// Everywhere a model's text is treated as prose, it goes through here.
pub fn plain_prose(raw: &str) -> String {
    let t = raw.rsplit("</think>").next().unwrap_or(raw).trim();
    // Peel at most twice: an envelope inside an envelope happens, three deep does not.
    let mut cur = t.to_string();
    for _ in 0..2 {
        let inner = unwrap_envelope(&cur).or_else(|| salvage_answer(&cur));
        let Some(inner) = inner else { break };
        cur = inner;
    }
    cur
}

/// Extract the `answer` value from an envelope that is NOT valid JSON.
///
/// This is the case that matters, and the first fix missed it. The models emit answers containing
/// UNESCAPED double quotes — `definitive "best" list`, `(e.g., "List the top 3 sources")` — which makes
/// the envelope malformed, so `serde_json` correctly refuses it and a JSON-based peel can never fire.
/// The raw envelope then sailed through to the user's screen a second time, after a deploy that was
/// supposed to have fixed it.
///
/// So this works on the text: find the `answer` key, take its quoted value, and undo the escapes by
/// hand. It is deliberately narrow — the text must look like an envelope (leading brace, an `answer`
/// key) before a single character is touched.
fn salvage_answer(s: &str) -> Option<String> {
    let t = s.trim();
    if !t.starts_with('{') {
        return None;
    }
    let key = t.find("\"answer\"")?;
    let colon = t[key..].find(':')? + key;
    let open = t[colon..].find('"')? + colon + 1;

    // Candidate ends: every quote not preceded by a backslash. The right one is the LAST candidate
    // that closes the object — anything earlier is an unescaped quote inside the prose.
    let bytes = t.as_bytes();
    let mut end = None;
    for (i, _) in t[open..].char_indices().map(|(i, c)| (i + open, c)).filter(|(_, c)| *c == '"') {
        if i > 0 && bytes[i - 1] == b'\\' {
            continue;
        }
        let rest = t[i + 1..].trim_start();
        // `"}`, `"} `, or `", "next_key"` — a real delimiter, not prose punctuation.
        if rest.starts_with('}') || rest.starts_with(',') {
            end = Some(i);
        }
    }
    let end = end?;
    if end <= open {
        return None;
    }
    let out = unescape(&t[open..end]);
    let out = out.trim();
    if out.is_empty() {
        None
    } else {
        Some(out.to_string())
    }
}

/// Undo JSON string escapes by hand, for text that never parsed.
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('/') => out.push('/'),
            // An unknown escape keeps both characters rather than silently eating one.
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// One layer: if the text is (or contains) an object carrying an `answer`/`text`/`response` string,
/// return that string. None when there is no envelope to peel — which must leave the text untouched,
/// or ordinary prose that merely mentions braces would be mangled.
fn unwrap_envelope(s: &str) -> Option<String> {
    let (a, b) = (s.find('{')?, s.rfind('}')?);
    if b <= a {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&s[a..=b]).ok()?;
    let inner = ["answer", "text", "response", "final", "content"]
        .iter()
        .find_map(|k| v.get(*k).and_then(|x| x.as_str()))?;
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_string())
}

pub struct SubAgent {
    inference: InferencePool,
    host: Arc<dyn RecipeHost>,
    persona: String,
    /// Read tools this sub-agent may call.
    tools: Vec<String>,
    max_steps: usize,
    /// Optional harm-gated runtime + the tool names that are OUTWARD actions (gated).
    runtime: Option<Arc<dyn ActionRuntime>>,
    act_tools: Vec<String>,
}

impl SubAgent {
    pub fn new(
        inference: InferencePool,
        host: Arc<dyn RecipeHost>,
        persona: impl Into<String>,
        tools: Vec<String>,
        max_steps: usize,
    ) -> Self {
        Self {
            inference,
            host,
            persona: persona.into(),
            tools,
            max_steps: max_steps.max(1),
            runtime: None,
            act_tools: Vec::new(),
        }
    }

    /// Make the sub-agent act-capable: `act_tools` (e.g. ["send_email"]) route through the harm-gate.
    /// The agent can PROPOSE these; it can never self-confirm an action that needs confirmation.
    pub fn with_actions(mut self, runtime: Arc<dyn ActionRuntime>, act_tools: Vec<String>) -> Self {
        self.runtime = Some(runtime);
        for t in &act_tools {
            if !self.tools.contains(t) {
                self.tools.push(t.clone());
            }
        }
        self.act_tools = act_tools;
        self
    }

    /// Run the ReAct loop for one task.
    pub async fn run(&self, task: &str) -> AgentResult {
        let mut observations = String::new();
        let mut trace: Vec<String> = Vec::new();
        let mut pending_actions: Vec<ActionRequest> = Vec::new();
        let mut sources: Vec<String> = Vec::new();
        let tool_list = self.tools.join(", ");
        let act_note = if self.act_tools.is_empty() {
            String::new()
        } else {
            format!(
                " Action tools (OUTWARD, need the user's confirmation — propose with args like \
                 {{\"target\":\"...\",\"summary\":\"...\",\"payload\":\"...\"}}): [{}].",
                self.act_tools.join(", ")
            )
        };

        for step in 0..self.max_steps {
            let prompt = format!(
                "Task: {task}\n\
                 Tools you may call: [{tool_list}].{act_note}\n\
                 Observations so far:\n{obs}\n\n\
                 Decide the next action. Respond with STRICT JSON and nothing else:\n\
                 {{\"action\":\"call_tool\"|\"finish\",\"tool\":\"<name>\",\"args\":{{}},\"answer\":\"...\"}}\n\
                 Call a tool to gather what you still need. When you have enough, action=finish with a \
                 concise answer grounded ONLY in the observations — never invent. Prefer to finish early.\n\
                 ESCAPE the answer properly: every \" inside it must be \\\", every newline \\n. \
                 Unescaped quotes make the whole decision unreadable.\n\
                 This is a BACKGROUND JOB, not a conversation. Nobody is waiting to reply, so `answer` \
                 must never ask the user to clarify, choose between options, or specify anything — that \
                 delivers nothing. Give the most useful grounded result the observations support. If they \
                 fall short, state plainly WHAT IS MISSING; that is a finding, not a question.",
                obs = if observations.is_empty() { "(none yet)" } else { &observations },
            );
            let messages = vec![
                ChatMessage::system(&self.persona),
                ChatMessage::system("You are a focused sub-agent. Output ONLY the decision JSON."),
                ChatMessage::user(&prompt),
            ];
            // PRIVATE-GROUNDED: a sub-agent's task + accumulated trace carry whatever the parent
            // turn was about — on a companion that is the household's own context. Fail closed.
            let text = match self.inference.chat_grounded(messages, GenerationConfig::default()).await {
                Ok(r) => r.text,
                Err(e) => {
                    return AgentResult {
                        task: task.into(),
                        answer: format!("(sub-agent inference error: {e})"),
                        steps: step,
                        trace,
                        pending_actions,
                        sources: sources.clone(),
                        error: Some(e.to_string()),
                    }
                }
            };
            let decision = parse_decision(&text);

            if decision.action == "finish" || (decision.action.is_empty() && !decision.answer.is_empty()) {
                let answer = plain_prose(&decision.answer);
                return AgentResult { task: task.into(), answer, steps: step + 1, trace, pending_actions, sources: sources.clone(), error: None };
            }
            // call_tool
            let tool = decision.tool.trim().to_string();
            if tool.is_empty() {
                let answer = plain_prose(&decision.answer);
                return AgentResult { task: task.into(), answer, steps: step + 1, trace, pending_actions, sources: sources.clone(), error: None };
            }
            // OUTWARD action tool → through the harm-gate. The agent can never self-confirm.
            if self.act_tools.iter().any(|t| t == &tool) {
                if let Some(rt) = &self.runtime {
                    let intent = ActionIntent {
                        kind: tool.clone(),
                        target: decision.args.get("target").or_else(|| decision.args.get("to")).and_then(|v| v.as_str()).unwrap_or("").to_string(),
                        summary: decision.args.get("summary").or_else(|| decision.args.get("subject")).and_then(|v| v.as_str()).unwrap_or("(sub-agent action)").to_string(),
                        payload: Some(decision.args.get("payload").or_else(|| decision.args.get("body")).and_then(|v| v.as_str()).unwrap_or("").to_string()),
                        capabilities: vec![Capability::SendMessage],
                        risk: RiskLevel::Medium,
                        reversible: false,
                    };
                    let req = ActionRequest { id: format!("sa-{}", now_ms()), actor: "sub-agent".into(), intent, justification: format!("sub-agent task: {task}"), created_ms: now_ms() };
                    let obs = match rt.decide(&req, &dummy_ctx(&req)).await {
                        ActionDecision::Execute => match rt.execute(req).await {
                            Ok(r) if r.ok => format!("done: {}", r.output),
                            Ok(r) => format!("failed: {}", r.output),
                            Err(e) => format!("failed: {e}"),
                        },
                        ActionDecision::RequireConfirmation { .. } => {
                            pending_actions.push(req);
                            "PROPOSED — needs the user's confirmation; NOT executed".to_string()
                        }
                        ActionDecision::Deny { reason } => format!("BLOCKED by harm-gate: {reason}"),
                    };
                    trace.push(format!("{tool}: {obs}"));
                    observations.push_str(&format!("[{tool}] => {obs}\n"));
                    continue;
                }
            }
            if !self.tools.iter().any(|t| t == &tool) {
                observations.push_str(&format!("[{tool}] REFUSED: not in the allowed tool set\n"));
                trace.push(format!("{tool}: refused (not allowed)"));
                continue;
            }
            let obs = match self.host.call_tool(&tool, &decision.args).await {
                Ok(o) => o,
                Err(e) => format!("error: {e}"),
            };
            // Collect sources for citations: an explicit fetch url + any urls in the observation.
            if tool == "fetch" {
                if let Some(u) = decision.args.get("url").and_then(|v| v.as_str()) {
                    if !sources.iter().any(|s| s == u) {
                        sources.push(u.to_string());
                    }
                }
            }
            for u in extract_urls(&obs) {
                if !sources.iter().any(|s| s == &u) {
                    sources.push(u);
                }
            }
            let short: String = obs.chars().take(120).collect();
            trace.push(format!("{tool}: {short}"));
            observations.push_str(&format!("[{tool}] => {obs}\n"));
        }

        // Budget exhausted — synthesize a best-effort answer from observations (no new tools).
        //
        // Two things this prompt has to say that the first version did not. It must ask for PROSE:
        // the model has spent every prior call emitting decision JSON and will happily emit one more
        // (see `plain_prose`). And it must say NO ONE IS LISTENING — a background job that answers
        // "please clarify what you'd like" has produced nothing, because the person who asked is not
        // in a conversation with it. Observed live: a stock-research job spent 11 steps, fetched six
        // sources, then asked the user to clarify.
        let messages = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::system(
                "You are writing the FINAL DELIVERABLE of a background job. Plain prose — no JSON, no \
                 envelope, no `action` field. This is not a conversation: the person who asked is not \
                 here to answer a question, so never end by asking them to clarify or choose. Give \
                 them the most useful grounded result the observations support. If the observations \
                 genuinely do not answer the task, say exactly WHAT IS MISSING and what you would \
                 need to get it — that is a finding, not a question.",
            ),
            ChatMessage::user(&format!(
                "Task: {task}\nObservations:\n{observations}\n\nWrite the deliverable. Ground every \
                 claim in the observations above; never invent."
            )),
        ];
        // The answer stays human-readable on failure — the sources below it are still worth seeing,
        // and a caller that only displays text should keep displaying something. What changes is
        // that the failure is ALSO a field, so a caller that has to DECIDE can tell (E.SK5).
        // PRIVATE-GROUNDED, like the decision loop 120 lines above — and for a stronger reason.
        // The loop's prompt carries the task and the trace; THIS one carries the task AND every
        // observation gathered along the way, so it is strictly more household context. It was a
        // bare `chat()`, which takes the cloud lane silently and never touches PRIVACY_ESCALATED,
        // so the escalation did not appear on the dashboard either. The privacy guard did not catch
        // it because the call was WRAPPED across lines and the scan matched one line at a time
        // (E.SEC2).
        let (answer, error) = match self.inference.chat_grounded(messages, GenerationConfig::default()).await {
            Ok(r) => (plain_prose(&r.text), None),
            Err(e) => (format!("(sub-agent synthesis error: {e})"), Some(e.to_string())),
        };
        AgentResult { task: task.into(), answer, steps: self.max_steps, trace, pending_actions, sources: sources.clone(), error }
    }

    /// Run several tasks concurrently (parallelism via the InferencePool's blocking pool).
    pub async fn fan_out(&self, tasks: Vec<String>) -> Vec<AgentResult> {
        join_all(tasks.iter().map(|t| self.run(t))).await
    }
}

/// Lenient parse of the decision JSON (extract the first {...}).
fn parse_decision(raw: &str) -> Decision {
    // A think:true reasoner emits a <think>…</think> preamble — often containing braces — before the
    // JSON. Strip it first, else the {…} span extraction grabs the wrong span, the typed parse fails,
    // and the whole raw blob leaks to the user as the "answer" (observed live: the MoE reasoner's
    // {"action":"finish","answer":…} dumped verbatim into chat). Mirrors the main loop's </think> handling.
    let raw = raw.rsplit("</think>").next().unwrap_or(raw).trim();
    if let Ok(d) = serde_json::from_str::<Decision>(raw) {
        return d;
    }
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}')) {
        if e > s {
            let span = &raw[s..=e];
            if let Ok(d) = serde_json::from_str::<Decision>(span) {
                return d;
            }
            // Partly-malformed object (truncated / stray field): pull what we can out of a lenient
            // Value so we NEVER dump raw JSON. An extractable answer or tool wins over the raw fallback.
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(span) {
                let answer = v.get("answer").and_then(|x| x.as_str()).unwrap_or_default().to_string();
                let tool = v.get("tool").and_then(|x| x.as_str()).unwrap_or_default().to_string();
                if !answer.is_empty() || !tool.is_empty() {
                    let action = v
                        .get("action")
                        .and_then(|x| x.as_str())
                        .unwrap_or(if tool.is_empty() { "finish" } else { "call_tool" })
                        .to_string();
                    return Decision { action, tool, args: v.get("args").cloned().unwrap_or_default(), answer };
                }
            }
        }
    }
    // No usable JSON → treat the whole (think-stripped) text as a finished answer (graceful).
    Decision { action: "finish".into(), answer: raw.trim().to_string(), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    #[test]
    fn parse_decision_strips_think_and_never_leaks_raw_json() {
        // A think:true reasoner's <think> preamble (with braces) must not derail extraction.
        let raw = "<think>Plan: {weather, things}. I'll finish now.</think>\n\
                   {\"action\":\"finish\",\"answer\":\"Branson is warm in July.\"}";
        let d = parse_decision(raw);
        assert_eq!(d.action, "finish");
        assert_eq!(d.answer, "Branson is warm in July.");
        // A stray extra field still yields the clean answer, never the raw JSON blob.
        let d2 = parse_decision("{\"action\":\"finish\",\"answer\":\"hi\",\"note\":\"x\"}");
        assert_eq!(d2.answer, "hi");
        assert!(!d2.answer.contains('{'));
    }
    use yantrik_ml::{LLMBackend, LLMResponse};

    /// An LLM that returns a fixed sequence of responses (for multi-step ReAct tests).
    struct SeqLLM {
        responses: Mutex<VecDeque<String>>,
    }
    impl SeqLLM {
        fn new(seq: Vec<&str>) -> Self {
            Self { responses: Mutex::new(seq.into_iter().map(|s| s.to_string()).collect()) }
        }
    }
    impl LLMBackend for SeqLLM {
        fn chat(
            &self,
            _messages: &[ChatMessage],
            _config: &GenerationConfig,
            _tools: Option<&[serde_json::Value]>,
        ) -> anyhow::Result<LLMResponse> {
            let text = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| "{\"action\":\"finish\",\"answer\":\"done\"}".into());
            Ok(LLMResponse {
                thinking: String::new(),
                text,
                prompt_tokens: 0,
                completion_tokens: 0,
                tool_calls: vec![],
                api_tool_calls: vec![],
                stop_reason: "stop".into(),
            })
        }
        fn chat_streaming(
            &self,
            messages: &[ChatMessage],
            config: &GenerationConfig,
            tools: Option<&[serde_json::Value]>,
            on_token: &mut dyn FnMut(&str),
        ) -> anyhow::Result<LLMResponse> {
            let r = self.chat(messages, config, tools)?;
            on_token(&r.text);
            Ok(r)
        }
        fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
            Ok(text.split_whitespace().count())
        }
        fn backend_name(&self) -> &str {
            "seq"
        }
    }

    struct FakeHost;
    #[async_trait::async_trait]
    impl RecipeHost for FakeHost {
        async fn call_tool(&self, tool: &str, _args: &serde_json::Value) -> anyhow::Result<String> {
            match tool {
                "recall" => Ok("user prefers terse replies".into()),
                "inbox" => Ok("2 unread from boss".into()),
                _ => anyhow::bail!("unknown tool"),
            }
        }
    }

    fn agent(seq: Vec<&str>, tools: Vec<&str>, max: usize) -> SubAgent {
        let pool = InferencePool::new(Arc::new(SeqLLM::new(seq)) as Arc<dyn LLMBackend>, 1);
        SubAgent::new(pool, Arc::new(FakeHost), "JARVIS", tools.into_iter().map(|s| s.into()).collect(), max)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn react_loop_calls_a_tool_then_finishes() {
        let seq = vec![
            r#"{"action":"call_tool","tool":"recall","args":{"query":"prefs"}}"#,
            r#"{"action":"finish","answer":"You prefer terse replies."}"#,
        ];
        let r = agent(seq, vec!["recall", "inbox"], 5).run("what do I prefer?").await;
        assert_eq!(r.answer, "You prefer terse replies.");
        assert_eq!(r.steps, 2);
        assert_eq!(r.trace.len(), 1, "one tool call");
        assert!(r.trace[0].contains("recall"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn disallowed_tool_is_refused() {
        let seq = vec![
            r#"{"action":"call_tool","tool":"exec","args":{}}"#,
            r#"{"action":"finish","answer":"can't use that"}"#,
        ];
        let r = agent(seq, vec!["recall"], 5).run("do something").await;
        assert!(r.trace.iter().any(|t| t.contains("refused")), "exec must be refused: {:?}", r.trace);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn budget_is_bounded() {
        // Always asks to call a tool → never finishes → must stop at max_steps and synthesize.
        let seq = vec![r#"{"action":"call_tool","tool":"recall","args":{}}"#; 50];
        let r = agent(seq, vec!["recall"], 3).run("loop forever?").await;
        assert_eq!(r.steps, 3, "must stop at the step budget");
    }

    struct ConfirmRuntime {
        executed: Arc<Mutex<u32>>,
    }
    #[async_trait::async_trait]
    impl ActionRuntime for ConfirmRuntime {
        async fn decide(&self, _req: &ActionRequest, _ctx: &TurnContext) -> ActionDecision {
            ActionDecision::RequireConfirmation { reason: "outward".into() }
        }
        async fn execute(&self, req: ActionRequest) -> mind_types::Result<mind_types::ActionReceipt> {
            *self.executed.lock().unwrap() += 1;
            Ok(mind_types::ActionReceipt { request_id: req.id, ok: true, output: "sent".into(), idempotency_key: "k".into() })
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn act_capable_agent_proposes_but_never_self_confirms() {
        let seq = vec![
            r#"{"action":"call_tool","tool":"send_email","args":{"target":"a@b.com","summary":"hi","payload":"hello"}}"#,
            r#"{"action":"finish","answer":"I drafted an email for your approval."}"#,
        ];
        let pool = InferencePool::new(Arc::new(SeqLLM::new(seq)) as Arc<dyn LLMBackend>, 1);
        let executed = Arc::new(Mutex::new(0));
        let rt: Arc<dyn ActionRuntime> = Arc::new(ConfirmRuntime { executed: executed.clone() });
        let agent = SubAgent::new(pool, Arc::new(FakeHost), "JARVIS", vec![], 5)
            .with_actions(rt, vec!["send_email".into()]);
        let r = agent.run("email a@b.com that the deploy is live").await;
        assert_eq!(r.pending_actions.len(), 1, "the action must be PROPOSED, not executed");
        assert_eq!(r.pending_actions[0].intent.target, "a@b.com");
        assert_eq!(*executed.lock().unwrap(), 0, "a sub-agent can NEVER self-confirm an outward action");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn fan_out_runs_all_tasks() {
        // Each task finishes immediately (the default when the seq is exhausted).
        let pool = InferencePool::new(Arc::new(SeqLLM::new(vec![])) as Arc<dyn LLMBackend>, 4);
        let a = SubAgent::new(pool, Arc::new(FakeHost), "JARVIS", vec!["recall".into()], 2);
        let out = a.fan_out(vec!["q1".into(), "q2".into(), "q3".into()]).await;
        assert_eq!(out.len(), 3);
        assert_eq!(out[0].task, "q1");
    }

    // ── THE ENVELOPE LEAK ────────────────────────────────────────────────────────────────────────
    //
    // Reported by the user from the cockpit on 2026-08-11: a delegated "Stock finder" job displayed
    // its own control JSON as the answer, `\n` escapes intact. The escapes were the diagnosis — real
    // parsing turns them into newlines, so nothing had parsed it. The string below is the live one.

    const LIVE_LEAK: &str = r#"{"action": "finish", "tool": null, "answer": "I cannot provide specific stock trading recommendations for today.\n\nThe search results provide tools and lists from various sources.\n\nNext Action: To proceed, please clarify if you want me to summarize the *types* of actionable data (e.g., \"List the top 3 sources\")."}"#;

    #[test]
    fn tool_null_does_not_break_the_typed_parse() {
        // `"tool": null` is the natural thing a model writes when it is finishing rather than calling
        // a tool, and serde rejected it outright: "invalid type: null, expected a string".
        //
        // This asserts on the TYPED deserialize, not on `parse_decision`. Going through
        // `parse_decision` would pass either way, because its lenient recovery path rescues the answer
        // — so that version of this test could not fail and did not cover the fix at all. The typed
        // parse succeeding is the actual property: it keeps the common case off the recovery path.
        let d: Decision = serde_json::from_str(LIVE_LEAK).expect("null must deserialize as absent");
        assert_eq!(d.action, "finish");
        assert_eq!(d.tool, "");
        assert!(d.answer.starts_with("I cannot provide"), "answer was not extracted: {:?}", d.answer);
        // And the whole-decision path still agrees.
        assert_eq!(parse_decision(LIVE_LEAK).action, "finish");
    }

    #[test]
    fn the_control_envelope_never_reaches_the_reader() {
        let out = plain_prose(LIVE_LEAK);
        assert!(!out.contains("\"action\""), "the envelope is still there: {out}");
        assert!(!out.contains("finish"), "the envelope is still there: {out}");
        // And the escapes must have become REAL newlines — that is what proves it parsed. The needle
        // is a backslash followed by 'n', built from chars so no amount of escaping confusion can turn
        // it into an actual newline (which is what an earlier version of this line accidentally
        // asserted, making the test fail against correct output).
        let escaped_nl: String = ['\\', 'n'].iter().collect();
        assert!(!out.contains(&escaped_nl), "escaped newlines survived, so nothing parsed: {out:?}");
        assert!(out.contains('\n'), "the paragraph breaks were lost: {out:?}");
        assert!(out.starts_with("I cannot provide"));
    }

    // THE SECOND LEAK. After the first fix was deployed, the same envelope reached the screen again.
    // The reason is visible in the payload: the answers carry UNESCAPED double quotes — `definitive
    // "best" list`, `(e.g., "List the top 3 sources")` — so the envelope is not valid JSON, serde_json
    // correctly refuses it, and no JSON-based peel can ever fire. Both strings below are verbatim from
    // the live job board, which is the point: I wrote the first fix against a well-formed sample I had
    // typed myself, and the real payloads were not well-formed.

    const MALFORMED_A: &str = concat!(
        r#"{"action": "finish", "tool": null, "answer": "I cannot provide specific stock trading "#,
        r#"recommendations for today.\n\n**Next Action:** please clarify if you want me to summarize "#,
        r#"the *types* of actionable data (e.g., "List the top 3 sources that focus on movers"), or "#,
        r#"if you have a specific sector in mind."}"#
    );

    const MALFORMED_B: &str = concat!(
        r#"{"action":"finish","answer":"The search results provide several *categories* of stock "#,
        r#"recommendations but do not give a single, definitive "best" list for today without knowing "#,
        r#"your risk tolerance.\n\n**Recommendation:** Specify a timeframe and I can narrow it down."}"#
    );

    #[test]
    fn a_malformed_envelope_is_salvaged_by_text() {
        let escaped_nl: String = ['\\', 'n'].iter().collect();
        for (name, raw) in [("A", MALFORMED_A), ("B", MALFORMED_B)] {
            // Confirm the premise first: these really are invalid JSON, so no JSON path can help.
            assert!(
                serde_json::from_str::<serde_json::Value>(raw).is_err(),
                "sample {name} was supposed to be malformed JSON — if it parses, this test is not \
                 exercising the salvage path at all"
            );
            let out = plain_prose(raw);
            assert!(!out.contains("\"action\""), "sample {name} still carries the envelope: {out}");
            assert!(!out.starts_with('{'), "sample {name} still starts with a brace: {out}");
            assert!(!out.contains(&escaped_nl), "sample {name} kept escaped newlines: {out:?}");
            assert!(out.contains('\n'), "sample {name} lost its paragraph breaks: {out:?}");
            assert!(out.contains('"'), "sample {name} lost the quoted text inside the prose: {out:?}");
        }
        assert!(plain_prose(MALFORMED_A).starts_with("I cannot provide"));
        assert!(plain_prose(MALFORMED_B).starts_with("The search results provide"));
        assert!(plain_prose(MALFORMED_B).ends_with("narrow it down."));
    }

    #[test]
    fn the_salvage_refuses_anything_that_is_not_an_envelope() {
        // It rewrites text by hand, so it must be certain before touching a character.
        assert_eq!(salvage_answer("The \"answer\" is 42."), None, "no leading brace");
        assert_eq!(salvage_answer("{\"tool\":\"now\",\"args\":{}}"), None, "no answer key");
        assert_eq!(salvage_answer("{\"answer\":\"\"}"), None, "an empty answer is not a salvage");
        assert_eq!(salvage_answer(""), None);
    }

    #[test]
    fn unescape_keeps_an_unknown_escape_intact() {
        // Eating the backslash of an escape we do not know would silently corrupt a Windows path or a
        // regex in the prose.
        assert_eq!(unescape("C:\\\\Users\\\\sync"), "C:\\Users\\sync");
        assert_eq!(unescape("a\\qb"), "a\\qb");
        assert_eq!(unescape("trailing\\"), "trailing\\");
    }

    #[test]
    fn prose_that_merely_mentions_braces_is_left_alone() {
        // The peel must be conservative. Real prose about JSON must survive untouched, or fixing the
        // leak would corrupt every reply that discusses code.
        let p = "Set {\"retries\": 3} in the config and restart.";
        assert_eq!(plain_prose(p), p);
        let q = "No braces here at all.";
        assert_eq!(plain_prose(q), q);
    }

    #[test]
    fn a_think_preamble_is_stripped_before_peeling() {
        let out = plain_prose("<think>weighing it up {maybe}</think>\n{\"answer\":\"Six sources agree.\"}");
        assert_eq!(out, "Six sources agree.");
    }

    #[test]
    fn a_doubly_wrapped_answer_is_peeled() {
        let out = plain_prose(r#"{"answer":"{\"text\":\"Done.\"}"}"#);
        assert_eq!(out, "Done.");
    }

    #[tokio::test]
    async fn an_exhausted_budget_delivers_prose_not_json() {
        // Every step emits a tool call so the budget runs out, and the final synthesis call answers
        // with an envelope — exactly the live shape. The sub-agent's answer must be the prose.
        let pool = mind_inference::InferencePool::new(
            Arc::new(SeqLLM::new(vec![
                r#"{"action":"call_tool","tool":"recall","args":{"query":"a"}}"#,
                r#"{"action":"call_tool","tool":"recall","args":{"query":"b"}}"#,
                LIVE_LEAK,
            ])) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let a = SubAgent::new(pool, Arc::new(FakeHost), "JARVIS", vec!["recall".into()], 2);
        let r = a.run("find the best stocks to trade today").await;
        assert!(!r.answer.contains("\"action\""), "the envelope reached the caller: {}", r.answer);
        assert!(r.answer.starts_with("I cannot provide"), "got: {}", r.answer);
    }
}

/// E.SK5 — a failure must be a field, not prose.
#[cfg(test)]
mod sk5 {
    use super::*;

    fn result(answer: &str, error: Option<&str>) -> AgentResult {
        AgentResult {
            task: "t".into(),
            answer: answer.into(),
            steps: 1,
            trace: vec![],
            pending_actions: vec![],
            sources: vec![],
            error: error.map(String::from),
        }
    }

    #[test]
    fn a_synthesis_error_is_not_a_deliverable() {
        // THE LIVE REPORT: a test-market run on NVDA searched six pages, failed at synthesis, and
        // returned `(sub-agent synthesis error: OpenAI-compatible API request failed)`. Callers
        // decided success with `!answer.trim().is_empty()` — and that string is not empty — so an
        // API error went on the job board under a green tick and was credited to the skill.
        let failed = result("(sub-agent synthesis error: OpenAI-compatible API request failed)", Some("OpenAI-compatible API request failed"));
        assert!(!failed.ok(), "an error message is not an answer");
        assert!(!failed.answer.trim().is_empty(), "and it is NOT empty — which is why the old guess failed");

        let inference_failed = result("(sub-agent inference error: timeout)", Some("timeout"));
        assert!(!inference_failed.ok(), "the loop's own failure path too");

        // The control, so `ok()` is not simply false.
        assert!(result("WMT closed at $105.38, down 1.04%.", None).ok(), "a real deliverable is ok");
        // And an empty answer with no error is still not a deliverable.
        assert!(!result("   ", None).ok(), "blank is not an answer");
    }
}
