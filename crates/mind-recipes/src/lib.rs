//! mind-recipes — the recipe engine, lifted-and-adapted from yantrik-companion's proven design
//! (recipe.rs + recipe_executor.rs) onto the mind's clean seams. A Recipe is an ordered list of
//! typed steps run as a small state machine; the engine is decoupled from any god-object via the
//! `RecipeHost` seam (Tool steps) + an injected `InferencePool` (Think/ThinkCited).
//!
//! The standout, carried over verbatim in spirit: `ThinkCited` → `Validate` — LLM synthesis with
//! per-claim citations, then a DETERMINISTIC pass that strips uncited claims. That's anti-
//! confabulation built into the orchestration, which is exactly the mind's core principle.
//!
//! v1 is in-memory (vars in a HashMap). SQLite persistence + resumability (WaitFor/AskUser, the
//! `RecipeStore` from the original) + triggers are the clearly-additive next lift.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use mind_inference::InferencePool;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use yantrik_ml::{ChatMessage, GenerationConfig};

// ── Step model (lifted) ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecipeStep {
    /// Direct tool call — no LLM. Result stored under `store_as`.
    Tool { tool_name: String, args: Value, store_as: String },
    /// LLM over resolved context, result stored under `store_as`.
    Think { prompt: String, store_as: String },
    /// LLM synthesis with per-claim citations from `source_vars`. Stores CitedOutput JSON.
    ThinkCited { prompt: String, store_as: String, source_vars: Vec<String> },
    /// Deterministic: strip uncited claims from a CitedOutput, keep the grounded ones.
    Validate { input_var: String, store_as: String },
    /// Format a (validated) value for presentation.
    Render { input_var: String, store_as: String, #[serde(default)] format: RenderFormat },
    /// Jump to `target_step` if the condition holds (pure Rust, no LLM).
    JumpIf { condition: Condition, target_step: usize },
    /// Emit a message to the user (supports {{var}}).
    Notify { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub enum RenderFormat {
    #[default]
    Summary,
    Table,
    Cards,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op")]
pub enum Condition {
    VarExists { var: String },
    VarEmpty { var: String },
    VarContains { var: String, substring: String },
    Not { inner: Box<Condition> },
}

impl Condition {
    pub fn evaluate(&self, vars: &HashMap<String, Value>) -> bool {
        match self {
            Self::VarExists { var } => vars.contains_key(var),
            Self::VarEmpty { var } => vars.get(var).map_or(true, |v| {
                v.is_null()
                    || v.as_str().map_or(false, |s| s.is_empty())
                    || v.as_array().map_or(false, |a| a.is_empty())
            }),
            Self::VarContains { var, substring } => vars
                .get(var)
                .and_then(|v| v.as_str())
                .map_or(false, |s| s.contains(substring.as_str())),
            Self::Not { inner } => !inner.evaluate(vars),
        }
    }
}

// ── Citation types (lifted) ─────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CitedClaim {
    pub text: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub confidence: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CitedOutput {
    #[serde(default)]
    pub claims: Vec<CitedClaim>,
}

impl CitedClaim {
    /// A claim is grounded if it cites at least one source and isn't flagged uncited.
    fn is_grounded(&self) -> bool {
        !self.sources.is_empty() && self.confidence.to_lowercase() != "uncited"
    }
}

// ── Recipe + outcome ──────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Recipe {
    pub id: String,
    pub name: String,
    pub steps: Vec<RecipeStep>,
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    pub ok: bool,
    pub error: Option<String>,
    /// Messages the recipe chose to surface to the user (from Notify steps), in order.
    pub notifications: Vec<String>,
    pub vars: HashMap<String, Value>,
}

enum StepResult {
    Continue,
    JumpTo(usize),
    Notify(String),
    Failed(String),
}

/// Substitute {{var}} occurrences with the string form of recipe vars.
pub fn resolve_vars(template: &str, vars: &HashMap<String, Value>) -> String {
    let mut out = template.to_string();
    for (k, v) in vars {
        let needle = format!("{{{{{k}}}}}");
        if out.contains(&needle) {
            let s = v.as_str().map(|s| s.to_string()).unwrap_or_else(|| v.to_string());
            out = out.replace(&needle, &s);
        }
    }
    out
}

// ── The engine ────────────────────────────────────────────────────────────────────────────────

/// The Tool-step seam: the mind wires its read capabilities (and, later, gated act steps) here.
#[async_trait]
pub trait RecipeHost: Send + Sync {
    async fn call_tool(&self, tool: &str, args: &Value) -> anyhow::Result<String>;
}

pub struct RecipeEngine {
    inference: InferencePool,
    host: Arc<dyn RecipeHost>,
    persona: String,
}

impl RecipeEngine {
    pub fn new(inference: InferencePool, host: Arc<dyn RecipeHost>, persona: impl Into<String>) -> Self {
        Self { inference, host, persona: persona.into() }
    }

    pub async fn run(&self, recipe: &Recipe) -> RunOutcome {
        let mut vars: HashMap<String, Value> = HashMap::new();
        let mut notifications = Vec::new();
        let mut i = 0usize;
        let mut guard = 0usize;
        while i < recipe.steps.len() {
            guard += 1;
            if guard > 1000 {
                return RunOutcome { ok: false, error: Some("step budget exceeded".into()), notifications, vars };
            }
            match self.execute_step(&recipe.steps[i], &mut vars).await {
                StepResult::Continue => i += 1,
                StepResult::JumpTo(t) => i = t,
                StepResult::Notify(m) => {
                    notifications.push(m);
                    i += 1;
                }
                StepResult::Failed(e) => {
                    return RunOutcome { ok: false, error: Some(e), notifications, vars };
                }
            }
        }
        RunOutcome { ok: true, error: None, notifications, vars }
    }

    async fn execute_step(&self, step: &RecipeStep, vars: &mut HashMap<String, Value>) -> StepResult {
        match step {
            RecipeStep::Tool { tool_name, args, store_as } => {
                match self.host.call_tool(tool_name, args).await {
                    Ok(out) => {
                        vars.insert(store_as.clone(), Value::String(out));
                        StepResult::Continue
                    }
                    Err(e) => StepResult::Failed(format!("tool '{tool_name}' failed: {e}")),
                }
            }
            RecipeStep::Think { prompt, store_as } => {
                let resolved = resolve_vars(prompt, vars);
                let messages = vec![
                    ChatMessage::system(&self.persona),
                    ChatMessage::system(
                        "Answer based ONLY on the provided data. Never invent facts. If data is missing, say so.",
                    ),
                    ChatMessage::user(&resolved),
                ];
                match self.inference.chat(messages, GenerationConfig::default()).await {
                    Ok(r) => {
                        vars.insert(store_as.clone(), Value::String(r.text));
                        StepResult::Continue
                    }
                    Err(e) => StepResult::Failed(format!("LLM error: {e}")),
                }
            }
            RecipeStep::ThinkCited { prompt, store_as, source_vars } => {
                let resolved = resolve_vars(prompt, vars);
                let mut sources = String::new();
                for name in source_vars {
                    let content = vars.get(name).and_then(|v| v.as_str()).unwrap_or("(no data)");
                    sources.push_str(&format!("\n[source: {name}]\n{content}\n"));
                }
                let messages = vec![
                    ChatMessage::system(&self.persona),
                    ChatMessage::system(
                        "Synthesize ONLY from the sources below. Output STRICT JSON: \
                         {\"claims\":[{\"text\":\"...\",\"sources\":[\"<source name>\"],\"confidence\":\"high|medium|low\"}]}. \
                         Every claim MUST cite >=1 source name. If something isn't supported by a source, OMIT it. \
                         Do not output anything except the JSON.",
                    ),
                    ChatMessage::user(&format!("{resolved}\n\nSOURCES:{sources}")),
                ];
                match self.inference.chat(messages, GenerationConfig::default()).await {
                    Ok(r) => {
                        vars.insert(store_as.clone(), Value::String(r.text));
                        StepResult::Continue
                    }
                    Err(e) => StepResult::Failed(format!("LLM error: {e}")),
                }
            }
            RecipeStep::Validate { input_var, store_as } => {
                let raw = vars.get(input_var).and_then(|v| v.as_str()).unwrap_or("");
                let cited = parse_cited(raw);
                let kept: Vec<&CitedClaim> = cited.claims.iter().filter(|c| c.is_grounded()).collect();
                let dropped = cited.claims.len() - kept.len();
                // Store a structured, cleaned result: only grounded claims survive.
                let cleaned = CitedOutput { claims: kept.into_iter().cloned().collect() };
                let json = serde_json::to_value(&cleaned).unwrap_or(Value::Null);
                vars.insert(store_as.clone(), json);
                vars.insert(format!("{store_as}__dropped"), Value::from(dropped as u64));
                StepResult::Continue
            }
            RecipeStep::Render { input_var, store_as, format } => {
                let cited = vars
                    .get(input_var)
                    .and_then(|v| serde_json::from_value::<CitedOutput>(v.clone()).ok())
                    .unwrap_or_default();
                let text = render(&cited, format);
                vars.insert(store_as.clone(), Value::String(text));
                StepResult::Continue
            }
            RecipeStep::JumpIf { condition, target_step } => {
                if condition.evaluate(vars) {
                    StepResult::JumpTo(*target_step)
                } else {
                    StepResult::Continue
                }
            }
            RecipeStep::Notify { message } => StepResult::Notify(resolve_vars(message, vars)),
        }
    }
}

/// Lenient parse of an LLM's cited-output (extract the first {...} object).
fn parse_cited(raw: &str) -> CitedOutput {
    if let Ok(o) = serde_json::from_str::<CitedOutput>(raw) {
        return o;
    }
    if let (Some(start), Some(end)) = (raw.find('{'), raw.rfind('}')) {
        if end > start {
            if let Ok(o) = serde_json::from_str::<CitedOutput>(&raw[start..=end]) {
                return o;
            }
        }
    }
    CitedOutput::default()
}

fn render(cited: &CitedOutput, format: &RenderFormat) -> String {
    if cited.claims.is_empty() {
        return "(nothing grounded to report)".to_string();
    }
    match format {
        RenderFormat::Summary => cited.claims.iter().map(|c| format!("- {}", c.text)).collect::<Vec<_>>().join("\n"),
        RenderFormat::Cards => cited
            .claims
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{}. {} [{}]", i + 1, c.text, c.sources.join(",")))
            .collect::<Vec<_>>()
            .join("\n"),
        RenderFormat::Table => cited
            .claims
            .iter()
            .map(|c| format!("| {} | {} |", c.text, c.sources.join(", ")))
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

// ── Built-in recipes ──────────────────────────────────────────────────────────────────────────

/// The morning briefing as a declarative recipe: read inbox + github + due tasks, synthesize with
/// citations, strip anything uncited, render, surface. The host maps the tool names to capabilities.
pub fn morning_briefing() -> Recipe {
    Recipe {
        id: "builtin_morning_briefing".into(),
        name: "Morning Briefing".into(),
        steps: vec![
            RecipeStep::Tool { tool_name: "inbox".into(), args: serde_json::json!({"limit": 10}), store_as: "inbox".into() },
            RecipeStep::Tool { tool_name: "github".into(), args: serde_json::json!({"limit": 15}), store_as: "github".into() },
            RecipeStep::Tool { tool_name: "due_tasks".into(), args: serde_json::json!({}), store_as: "tasks".into() },
            RecipeStep::ThinkCited {
                prompt: "Compose a terse morning briefing. Lead with what needs attention; group by source.".into(),
                store_as: "cited".into(),
                source_vars: vec!["inbox".into(), "github".into(), "tasks".into()],
            },
            RecipeStep::Validate { input_var: "cited".into(), store_as: "valid".into() },
            RecipeStep::Render { input_var: "valid".into(), store_as: "briefing".into(), format: RenderFormat::Summary },
            RecipeStep::Notify { message: "{{briefing}}".into() },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mind_inference::ScriptedLLM;
    use yantrik_ml::LLMBackend;

    struct ScriptedHost;
    #[async_trait]
    impl RecipeHost for ScriptedHost {
        async fn call_tool(&self, tool: &str, _args: &Value) -> anyhow::Result<String> {
            Ok(match tool {
                "inbox" => "INBOX: 2 messages from boss@acme.com".into(),
                "github" => "GITHUB: PR #8 review_requested".into(),
                _ => "(none)".into(),
            })
        }
    }

    fn engine(llm_text: &str) -> RecipeEngine {
        let scripted = Arc::new(ScriptedLLM::new(llm_text));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        RecipeEngine::new(pool, Arc::new(ScriptedHost), "You are JARVIS.")
    }

    #[test]
    fn resolve_vars_substitutes() {
        let mut v = HashMap::new();
        v.insert("name".to_string(), Value::String("world".into()));
        assert_eq!(resolve_vars("hi {{name}}", &v), "hi world");
    }

    #[test]
    fn validate_strips_uncited_claims() {
        let raw = r#"{"claims":[
            {"text":"2 emails need attention","sources":["inbox"],"confidence":"high"},
            {"text":"a fabricated fact","sources":[],"confidence":"uncited"}
        ]}"#;
        let parsed = parse_cited(raw);
        let kept: Vec<_> = parsed.claims.iter().filter(|c| c.is_grounded()).collect();
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].text, "2 emails need attention");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn briefing_recipe_runs_and_drops_uncited() {
        // The LLM (scripted) returns one grounded + one uncited claim; Validate must drop the latter.
        let llm = r#"{"claims":[
            {"text":"2 emails from boss need a reply","sources":["inbox"],"confidence":"high"},
            {"text":"the stock market will crash tomorrow","sources":[],"confidence":"uncited"}
        ]}"#;
        let out = engine(llm).run(&morning_briefing()).await;
        assert!(out.ok, "recipe should complete: {:?}", out.error);
        assert_eq!(out.notifications.len(), 1);
        let brief = &out.notifications[0];
        assert!(brief.contains("2 emails from boss"), "grounded claim must survive: {brief}");
        assert!(!brief.contains("stock market"), "uncited claim must be stripped: {brief}");
        assert_eq!(out.vars.get("valid__dropped").and_then(|v| v.as_u64()), Some(1));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tool_steps_populate_sources() {
        let llm = r#"{"claims":[{"text":"x","sources":["inbox"],"confidence":"low"}]}"#;
        let out = engine(llm).run(&morning_briefing()).await;
        assert!(out.vars.get("inbox").and_then(|v| v.as_str()).unwrap().contains("boss@acme.com"));
        assert!(out.vars.get("github").and_then(|v| v.as_str()).unwrap().contains("PR #8"));
    }
}
