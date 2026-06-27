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
use serde::Deserialize;
use yantrik_ml::{ChatMessage, GenerationConfig};

#[derive(Debug, Clone)]
pub struct AgentResult {
    pub task: String,
    pub answer: String,
    pub steps: usize,
    /// A short trace of tool calls made (for transparency/audit).
    pub trace: Vec<String>,
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
    /// Tools this sub-agent may call (read-only in v1).
    tools: Vec<String>,
    max_steps: usize,
}

impl SubAgent {
    pub fn new(
        inference: InferencePool,
        host: Arc<dyn RecipeHost>,
        persona: impl Into<String>,
        tools: Vec<String>,
        max_steps: usize,
    ) -> Self {
        Self { inference, host, persona: persona.into(), tools, max_steps: max_steps.max(1) }
    }

    /// Run the ReAct loop for one task.
    pub async fn run(&self, task: &str) -> AgentResult {
        let mut observations = String::new();
        let mut trace: Vec<String> = Vec::new();
        let tool_list = self.tools.join(", ");

        for step in 0..self.max_steps {
            let prompt = format!(
                "Task: {task}\n\
                 Read-only tools you may call: [{tool_list}]\n\
                 Observations so far:\n{obs}\n\n\
                 Decide the next action. Respond with STRICT JSON and nothing else:\n\
                 {{\"action\":\"call_tool\"|\"finish\",\"tool\":\"<name>\",\"args\":{{}},\"answer\":\"...\"}}\n\
                 Call a tool to gather what you still need. When you have enough, action=finish with a \
                 concise answer grounded ONLY in the observations — never invent. Prefer to finish early.",
                obs = if observations.is_empty() { "(none yet)" } else { &observations },
            );
            let messages = vec![
                ChatMessage::system(&self.persona),
                ChatMessage::system("You are a focused research sub-agent. Output ONLY the decision JSON."),
                ChatMessage::user(&prompt),
            ];
            let text = match self.inference.chat(messages, GenerationConfig::default()).await {
                Ok(r) => r.text,
                Err(e) => return AgentResult { task: task.into(), answer: format!("(sub-agent inference error: {e})"), steps: step, trace },
            };
            let decision = parse_decision(&text);

            if decision.action == "finish" || (decision.action.is_empty() && !decision.answer.is_empty()) {
                return AgentResult { task: task.into(), answer: decision.answer, steps: step + 1, trace };
            }
            // call_tool
            let tool = decision.tool.trim().to_string();
            if tool.is_empty() {
                return AgentResult { task: task.into(), answer: decision.answer, steps: step + 1, trace };
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
        AgentResult { task: task.into(), answer, steps: self.max_steps, trace }
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
