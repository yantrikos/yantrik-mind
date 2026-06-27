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

/// The LLM's decision each step.
#[derive(Debug, Deserialize, Default)]
struct Decision {
    #[serde(default)]
    action: String, // "call_tool" | "finish"
    #[serde(default)]
    tool: String,
    #[serde(default)]
    args: serde_json::Value,
    #[serde(default)]
    answer: String,
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
                 concise answer grounded ONLY in the observations — never invent. Prefer to finish early.",
                obs = if observations.is_empty() { "(none yet)" } else { &observations },
            );
            let messages = vec![
                ChatMessage::system(&self.persona),
                ChatMessage::system("You are a focused sub-agent. Output ONLY the decision JSON."),
                ChatMessage::user(&prompt),
            ];
            let text = match self.inference.chat(messages, GenerationConfig::default()).await {
                Ok(r) => r.text,
                Err(e) => return AgentResult { task: task.into(), answer: format!("(sub-agent inference error: {e})"), steps: step, trace, pending_actions, sources: sources.clone() },
            };
            let decision = parse_decision(&text);

            if decision.action == "finish" || (decision.action.is_empty() && !decision.answer.is_empty()) {
                return AgentResult { task: task.into(), answer: decision.answer, steps: step + 1, trace, pending_actions, sources: sources.clone() };
            }
            // call_tool
            let tool = decision.tool.trim().to_string();
            if tool.is_empty() {
                return AgentResult { task: task.into(), answer: decision.answer, steps: step + 1, trace, pending_actions, sources: sources.clone() };
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
        let messages = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::user(&format!(
                "Answer this task from the observations below; if they're insufficient, say so plainly. \
                 Do not invent.\nTask: {task}\nObservations:\n{observations}"
            )),
        ];
        let answer = self
            .inference
            .chat(messages, GenerationConfig::default())
            .await
            .map(|r| r.text)
            .unwrap_or_else(|e| format!("(sub-agent synthesis error: {e})"));
        AgentResult { task: task.into(), answer, steps: self.max_steps, trace, pending_actions, sources: sources.clone() }
    }

    /// Run several tasks concurrently (parallelism via the InferencePool's blocking pool).
    pub async fn fan_out(&self, tasks: Vec<String>) -> Vec<AgentResult> {
        join_all(tasks.iter().map(|t| self.run(t))).await
    }
}

/// Lenient parse of the decision JSON (extract the first {...}).
fn parse_decision(raw: &str) -> Decision {
    if let Ok(d) = serde_json::from_str::<Decision>(raw) {
        return d;
    }
    if let (Some(s), Some(e)) = (raw.find('{'), raw.rfind('}')) {
        if e > s {
            if let Ok(d) = serde_json::from_str::<Decision>(&raw[s..=e]) {
                return d;
            }
        }
    }
    // No JSON at all → treat the whole text as a finished answer (graceful).
    Decision { action: "finish".into(), answer: raw.trim().to_string(), ..Default::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::Mutex;
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
}
