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

pub mod store;
pub use store::{RecipeStore, RunRecord};

use async_trait::async_trait;
use mind_inference::InferencePool;
use mind_types::{
    ActionDecision, ActionIntent, ActionRequest, ActionRuntime, Capability, Event, EventBody,
    EventSource, RiskLevel, TurnContext,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use yantrik_ml::{ChatMessage, GenerationConfig};

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// A throwaway TurnContext for the harm-gate (it inspects the intent, not the context).
fn dummy_ctx(req: &ActionRequest) -> TurnContext {
    TurnContext::new(
        Event {
            id: req.id.clone(),
            trace_id: req.id.clone(),
            source: EventSource::SelfReflection,
            body: EventBody::plain("recipe action"),
            ts: req.created_ms,
        },
        req.created_ms,
    )
}

// ── Step model (lifted) ───────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RecipeStep {
    /// Direct tool call — no LLM. Result stored under `store_as`.
    Tool { tool_name: String, args: Value, store_as: String, #[serde(default)] on_error: ErrorAction },
    /// LLM over resolved context, result stored under `store_as`.
    Think { prompt: String, store_as: String, #[serde(default)] on_error: ErrorAction },
    /// LLM synthesis with per-claim citations from `source_vars`. Stores CitedOutput JSON.
    ThinkCited { prompt: String, store_as: String, source_vars: Vec<String>, #[serde(default)] on_error: ErrorAction },
    /// Deterministic: strip uncited claims from a CitedOutput, keep the grounded ones.
    Validate { input_var: String, store_as: String },
    /// Format a (validated) value for presentation.
    Render { input_var: String, store_as: String, #[serde(default)] format: RenderFormat },
    /// Jump to `target_step` if the condition holds (pure Rust, no LLM).
    JumpIf { condition: Condition, target_step: usize },
    /// Emit a message to the user (supports {{var}}).
    Notify { message: String },
    /// PAUSE and ask the user a question; their next message is bound to `store_as` and the recipe
    /// resumes from the next step. Requires a store (persistence) so the pause survives across turns.
    AskUser { question: String, store_as: String },
    /// An OUTWARD action (e.g. send an email). Fields are {{var}}-resolved, then the action rides the
    /// harm-gate + ActionRuntime: Execute runs it; RequireConfirmation pauses the recipe for a yes;
    /// Deny fails it. Non-idempotent — never blind-rerun on recovery.
    Act { kind: String, target: String, summary: String, payload: String },
}

impl RecipeStep {
    fn on_error(&self) -> ErrorAction {
        match self {
            RecipeStep::Tool { on_error, .. }
            | RecipeStep::Think { on_error, .. }
            | RecipeStep::ThinkCited { on_error, .. } => on_error.clone(),
            _ => ErrorAction::Fail,
        }
    }

    /// Idempotent steps are safe to re-run on crash recovery; an `Act` is NOT.
    pub fn is_idempotent(&self) -> bool {
        !matches!(self, RecipeStep::Act { .. })
    }
}

/// What to do when a step fails — lifted from the original engine. `Replan` is the adaptive one:
/// the LLM diagnoses the failure and rewrites the remaining steps.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub enum ErrorAction {
    /// Abort the recipe (default).
    #[default]
    Fail,
    /// Skip this step and continue.
    Skip,
    /// Retry this step up to `max` times.
    Retry { max: u8 },
    /// Jump to another step index.
    JumpTo { step: usize },
    /// Ask the LLM to diagnose the failure and replace the remaining steps.
    Replan,
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
    /// Adaptations made on failure: ("<failed step>", "<error>", "<what changed>").
    pub failure_learnings: Vec<(String, String, String)>,
    /// An outward action awaiting confirmation — the recipe paused here until the user says yes.
    pub pending_action: Option<ActionRequest>,
    /// A clarifying question the recipe paused on — answer via `resume_with_answer(run_id, ..)`.
    pub pending_question: Option<PendingQuestion>,
    pub vars: HashMap<String, Value>,
}

/// A recipe paused on an `AskUser` step, awaiting the user's free-form answer.
#[derive(Debug, Clone)]
pub struct PendingQuestion {
    pub run_id: String,
    pub question: String,
}

enum StepResult {
    Continue,
    JumpTo(usize),
    Notify(String),
    Failed(String),
    /// An Act step needs confirmation — pause the recipe and surface the proposed action.
    Pending(ActionRequest),
    /// An AskUser step — pause the recipe and surface the question.
    Ask(String),
}

/// How a step failure was resolved by its `ErrorAction`.
enum ErrorResolution {
    /// Move past the failed step.
    Skip,
    /// Re-run the current step (Retry, or Replan that replaced steps in place).
    RetryHere,
    /// Jump to a step index.
    JumpTo(usize),
    /// Give up — the recipe fails.
    Abort,
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
    /// Outward-action runtime — required for `Act` steps (harm-gate + confirmation).
    runtime: Option<Arc<dyn ActionRuntime>>,
    /// Durable run state — when set, runs are persisted per step and recoverable on restart.
    store: Option<Arc<RecipeStore>>,
}

impl RecipeEngine {
    pub fn new(inference: InferencePool, host: Arc<dyn RecipeHost>, persona: impl Into<String>) -> Self {
        Self { inference, host, persona: persona.into(), runtime: None, store: None }
    }

    /// Enable `Act` steps by giving the engine the harm-gated action runtime.
    pub fn with_runtime(mut self, runtime: Arc<dyn ActionRuntime>) -> Self {
        self.runtime = Some(runtime);
        self
    }

    /// Persist runs (durability + crash recovery).
    pub fn with_store(mut self, store: Arc<RecipeStore>) -> Self {
        self.store = Some(store);
        self
    }

    pub async fn run(&self, recipe: &Recipe) -> RunOutcome {
        let id = format!("{}-{}", recipe.id, now_ms());
        self.run_from(&id, &recipe.name, recipe.steps.clone(), 0, HashMap::new()).await
    }

    /// Recover runs left mid-flight by a crash. Idempotent steps are re-run from where they stopped;
    /// a non-idempotent step (an Act/send) is failed-visibly, never blind-replayed (no double-send).
    pub async fn resume_incomplete(&self) -> usize {
        let store = match &self.store {
            Some(s) => s.clone(),
            None => return 0,
        };
        let mut resumed = 0;
        for rec in store.resumable() {
            match rec.steps.get(rec.current_step) {
                Some(step) if !step.is_idempotent() => {
                    store.set_status(&rec.id, "failed", Some("interrupted at a non-idempotent step; not retried"), now_ms());
                }
                _ => {
                    self.run_from(&rec.id, &rec.name, rec.steps, rec.current_step, rec.vars).await;
                    resumed += 1;
                }
            }
        }
        resumed
    }

    /// Resume a recipe that paused on an `AskUser` step, binding the user's answer + continuing.
    pub async fn resume_with_answer(&self, run_id: &str, answer: &str) -> RunOutcome {
        let empty = || RunOutcome {
            ok: false,
            error: Some("no such paused recipe".into()),
            notifications: vec![],
            failure_learnings: vec![],
            pending_action: None,
            pending_question: None,
            vars: HashMap::new(),
        };
        let store = match &self.store {
            Some(s) => s.clone(),
            None => return empty(),
        };
        let rec = match store.load(run_id) {
            Some(r) => r,
            None => return empty(),
        };
        let mut vars = rec.vars;
        // Bind the answer to the AskUser step's store_as, then continue past it.
        if let Some(RecipeStep::AskUser { store_as, .. }) = rec.steps.get(rec.current_step) {
            vars.insert(store_as.clone(), Value::String(answer.to_string()));
        }
        self.run_from(&rec.id, &rec.name, rec.steps.clone(), rec.current_step + 1, vars).await
    }

    async fn run_from(
        &self,
        id: &str,
        name: &str,
        mut steps: Vec<RecipeStep>,
        start: usize,
        mut vars: HashMap<String, Value>,
    ) -> RunOutcome {
        let mut notifications = Vec::new();
        let mut failure_learnings = Vec::new();
        let mut i = start;
        let mut guard = 0usize;
        let persist = |status: &str, step: usize, steps: &[RecipeStep], vars: &HashMap<String, Value>, error: Option<&str>| {
            if let Some(s) = &self.store {
                let _ = s.save(
                    &RunRecord {
                        id: id.to_string(),
                        name: name.to_string(),
                        status: status.to_string(),
                        current_step: step,
                        steps: steps.to_vec(),
                        vars: vars.clone(),
                        error: error.map(|e| e.to_string()),
                    },
                    now_ms(),
                );
            }
        };
        persist("running", i, &steps, &vars, None);
        while i < steps.len() {
            guard += 1;
            if guard > 1000 {
                persist("failed", i, &steps, &vars, Some("step budget exceeded"));
                return RunOutcome { ok: false, error: Some("step budget exceeded".into()), notifications, failure_learnings, pending_action: None, pending_question: None, vars };
            }
            let step = steps[i].clone();
            match self.execute_step(&step, &mut vars).await {
                StepResult::Continue => i += 1,
                StepResult::JumpTo(t) => i = t,
                StepResult::Notify(m) => {
                    notifications.push(m);
                    i += 1;
                }
                StepResult::Pending(req) => {
                    // Pause here: the action needs the user's confirmation before it runs.
                    persist("waiting", i, &steps, &vars, None);
                    return RunOutcome { ok: true, error: None, notifications, failure_learnings, pending_action: Some(req), pending_question: None, vars };
                }
                StepResult::Ask(question) => {
                    // Pause here: wait for the user's free-form answer (resume_with_answer binds it).
                    persist("waiting", i, &steps, &vars, None);
                    let pq = PendingQuestion { run_id: id.to_string(), question };
                    return RunOutcome { ok: true, error: None, notifications, failure_learnings, pending_action: None, pending_question: Some(pq), vars };
                }
                StepResult::Failed(e) => {
                    match self.handle_error(i, &e, &step.on_error(), &mut vars, &mut steps, &mut failure_learnings).await {
                        ErrorResolution::Skip => i += 1,
                        ErrorResolution::RetryHere => { /* re-run steps[i] */ }
                        ErrorResolution::JumpTo(t) => i = t,
                        ErrorResolution::Abort => {
                            persist("failed", i, &steps, &vars, Some(&e));
                            return RunOutcome { ok: false, error: Some(e), notifications, failure_learnings, pending_action: None, pending_question: None, vars };
                        }
                    }
                }
            }
            // Record progress so a crash here resumes from the right place.
            persist("running", i, &steps, &vars, None);
        }
        persist("done", i, &steps, &vars, None);
        RunOutcome { ok: true, error: None, notifications, failure_learnings, pending_action: None, pending_question: None, vars }
    }

    /// Resolve a step failure per its `ErrorAction`. `Replan` asks the LLM to rewrite the tail.
    async fn handle_error(
        &self,
        i: usize,
        error: &str,
        on_error: &ErrorAction,
        vars: &mut HashMap<String, Value>,
        steps: &mut Vec<RecipeStep>,
        learnings: &mut Vec<(String, String, String)>,
    ) -> ErrorResolution {
        match on_error {
            ErrorAction::Fail => ErrorResolution::Abort,
            ErrorAction::Skip => ErrorResolution::Skip,
            ErrorAction::JumpTo { step } => ErrorResolution::JumpTo(*step),
            ErrorAction::Retry { max } => {
                let key = format!("_retry_{i}");
                let n = vars.get(&key).and_then(|v| v.as_u64()).unwrap_or(0);
                if n < *max as u64 {
                    vars.insert(key, Value::from(n + 1));
                    ErrorResolution::RetryHere
                } else {
                    ErrorResolution::Abort
                }
            }
            ErrorAction::Replan => {
                match self.replan(i, error, steps).await {
                    Some(new_steps) => {
                        let n = new_steps.len();
                        // Replace the failed step + the rest of the tail with the LLM's plan.
                        steps.truncate(i);
                        steps.extend(new_steps);
                        learnings.push((format!("step {i}"), error.to_string(), format!("replanned with {n} new step(s)")));
                        ErrorResolution::RetryHere
                    }
                    None => ErrorResolution::Abort,
                }
            }
        }
    }

    /// The adaptive bit: the LLM diagnoses the failure and returns replacement steps as JSON.
    async fn replan(&self, i: usize, error: &str, steps: &[RecipeStep]) -> Option<Vec<RecipeStep>> {
        let remaining: Vec<String> = steps.iter().skip(i).filter_map(|s| serde_json::to_string(s).ok()).collect();
        let prompt = format!(
            "A recipe step failed.\nFailed step index: {i}\nError: {error}\nRemaining steps (JSON): {}\n\n\
             Diagnose the failure and return FIXED replacement steps as a JSON array of RecipeStep \
             (same shape as the remaining steps). If unrecoverable, return [].",
            remaining.join(", ")
        );
        let messages = vec![
            ChatMessage::system("You are a recipe debugger. Output ONLY a JSON array of replacement steps."),
            ChatMessage::user(&prompt),
        ];
        let resp = self.inference.chat(messages, GenerationConfig::default()).await.ok()?;
        let arr = extract_json_array(&resp.text);
        match serde_json::from_str::<Vec<RecipeStep>>(&arr) {
            Ok(new_steps) if !new_steps.is_empty() => Some(new_steps),
            _ => None,
        }
    }

    async fn execute_step(&self, step: &RecipeStep, vars: &mut HashMap<String, Value>) -> StepResult {
        match step {
            RecipeStep::Tool { tool_name, args, store_as, .. } => {
                match self.host.call_tool(tool_name, args).await {
                    Ok(out) => {
                        vars.insert(store_as.clone(), Value::String(out));
                        StepResult::Continue
                    }
                    Err(e) => StepResult::Failed(format!("tool '{tool_name}' failed: {e}")),
                }
            }
            RecipeStep::Think { prompt, store_as, .. } => {
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
            RecipeStep::ThinkCited { prompt, store_as, source_vars, .. } => {
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
            RecipeStep::AskUser { question, .. } => StepResult::Ask(resolve_vars(question, vars)),
            RecipeStep::Act { kind, target, summary, payload } => {
                let runtime = match &self.runtime {
                    Some(r) => r,
                    None => return StepResult::Failed("no action runtime configured for Act step".into()),
                };
                let intent = ActionIntent {
                    kind: kind.clone(),
                    target: resolve_vars(target, vars),
                    summary: resolve_vars(summary, vars),
                    payload: Some(resolve_vars(payload, vars)),
                    capabilities: vec![Capability::SendMessage],
                    risk: RiskLevel::Medium,
                    reversible: false,
                };
                let req = ActionRequest {
                    id: format!("rcp-{}", now_ms()),
                    actor: "recipe".into(),
                    intent,
                    justification: "recipe action step".into(),
                    created_ms: now_ms(),
                };
                let ctx = dummy_ctx(&req);
                match runtime.decide(&req, &ctx).await {
                    ActionDecision::Deny { reason } => StepResult::Failed(format!("harm-gate denied: {reason}")),
                    ActionDecision::RequireConfirmation { .. } => StepResult::Pending(req),
                    ActionDecision::Execute => match runtime.execute(req).await {
                        Ok(r) if r.ok => StepResult::Continue,
                        Ok(r) => StepResult::Failed(r.output),
                        Err(e) => StepResult::Failed(e.to_string()),
                    },
                }
            }
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

/// Extract the first [...] JSON array from an LLM response (lenient).
fn extract_json_array(text: &str) -> String {
    if let (Some(start), Some(end)) = (text.find('['), text.rfind(']')) {
        if end > start {
            return text[start..=end].to_string();
        }
    }
    "[]".to_string()
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
            // Source reads degrade gracefully: if one is unreadable, skip it and brief on the rest.
            RecipeStep::Tool { tool_name: "inbox".into(), args: serde_json::json!({"limit": 10}), store_as: "inbox".into(), on_error: ErrorAction::Skip },
            RecipeStep::Tool { tool_name: "github".into(), args: serde_json::json!({"limit": 15}), store_as: "github".into(), on_error: ErrorAction::Skip },
            RecipeStep::Tool { tool_name: "due_tasks".into(), args: serde_json::json!({}), store_as: "tasks".into(), on_error: ErrorAction::Skip },
            RecipeStep::ThinkCited {
                prompt: "Compose a terse morning briefing. Lead with what needs attention; group by source.".into(),
                store_as: "cited".into(),
                source_vars: vec!["inbox".into(), "github".into(), "tasks".into()],
                on_error: ErrorAction::Fail,
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
                "broken" => anyhow::bail!("simulated tool failure"),
                _ => "(none)".into(),
            })
        }
    }

    fn engine(llm_text: &str) -> RecipeEngine {
        let scripted = Arc::new(ScriptedLLM::new(llm_text));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        RecipeEngine::new(pool, Arc::new(ScriptedHost), "You are JARVIS.")
    }

    use mind_types::{ActionDecision, ActionReceipt, ActionRequest};
    use std::sync::Mutex;

    struct FakeRuntime {
        decision: ActionDecision,
        executed: Arc<Mutex<u32>>,
    }
    #[async_trait]
    impl ActionRuntime for FakeRuntime {
        async fn decide(&self, _req: &ActionRequest, _ctx: &mind_types::TurnContext) -> ActionDecision {
            self.decision.clone()
        }
        async fn execute(&self, req: ActionRequest) -> mind_types::Result<ActionReceipt> {
            *self.executed.lock().unwrap() += 1;
            Ok(ActionReceipt { request_id: req.id, ok: true, output: "sent".into(), idempotency_key: "k".into() })
        }
    }

    fn act_recipe() -> Recipe {
        Recipe {
            id: "act".into(),
            name: "act".into(),
            steps: vec![RecipeStep::Act {
                kind: "send_email".into(),
                target: "a@b.com".into(),
                summary: "say hi".into(),
                payload: "hello".into(),
            }],
        }
    }

    fn engine_with_runtime(decision: ActionDecision) -> (RecipeEngine, Arc<Mutex<u32>>) {
        let scripted = Arc::new(ScriptedLLM::new("unused"));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        let executed = Arc::new(Mutex::new(0));
        let rt: Arc<dyn ActionRuntime> = Arc::new(FakeRuntime { decision, executed: executed.clone() });
        let eng = RecipeEngine::new(pool, Arc::new(ScriptedHost), "JARVIS").with_runtime(rt);
        (eng, executed)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn act_step_requiring_confirmation_pauses_with_pending() {
        let (eng, executed) = engine_with_runtime(ActionDecision::RequireConfirmation { reason: "outward".into() });
        let out = eng.run(&act_recipe()).await;
        assert!(out.ok && out.pending_action.is_some(), "should pause for confirmation");
        assert_eq!(*executed.lock().unwrap(), 0, "must NOT execute before confirmation");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn act_step_execute_runs_the_action() {
        let (eng, executed) = engine_with_runtime(ActionDecision::Execute);
        let out = eng.run(&act_recipe()).await;
        assert!(out.ok && out.pending_action.is_none());
        assert_eq!(*executed.lock().unwrap(), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn act_step_denied_fails_the_recipe() {
        let (eng, executed) = engine_with_runtime(ActionDecision::Deny { reason: "nope".into() });
        let out = eng.run(&act_recipe()).await;
        assert!(!out.ok);
        assert_eq!(*executed.lock().unwrap(), 0);
    }

    fn temp_db(tag: &str) -> String {
        std::env::temp_dir()
            .join(format!("mind_recipes_{tag}_{}.db", now_ms()))
            .to_string_lossy()
            .to_string()
    }

    fn plain_engine_with_store(store: Arc<RecipeStore>) -> RecipeEngine {
        let scripted = Arc::new(ScriptedLLM::new("unused"));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        RecipeEngine::new(pool, Arc::new(ScriptedHost), "JARVIS").with_store(store)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn recovery_fails_visibly_on_interrupted_act() {
        let store = Arc::new(RecipeStore::open(&temp_db("act")).unwrap());
        // Simulate a crash mid-Act (non-idempotent) — status left 'running' at that step.
        store
            .save(
                &RunRecord {
                    id: "r1".into(),
                    name: "send".into(),
                    status: "running".into(),
                    current_step: 0,
                    steps: vec![RecipeStep::Act { kind: "send_email".into(), target: "a@b".into(), summary: "s".into(), payload: "p".into() }],
                    vars: HashMap::new(),
                    error: None,
                },
                now_ms(),
            )
            .unwrap();
        let resumed = plain_engine_with_store(store.clone()).resume_incomplete().await;
        assert_eq!(resumed, 0, "a non-idempotent send must NOT be blind-replayed");
        assert!(store.resumable().is_empty(), "it should be marked failed, not left running");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn askuser_pauses_then_resumes_with_answer() {
        let store = Arc::new(RecipeStore::open(&temp_db("ask")).unwrap());
        let eng = plain_engine_with_store(store.clone());
        let recipe = Recipe {
            id: "ask".into(),
            name: "ask".into(),
            steps: vec![
                RecipeStep::AskUser { question: "What's your favorite color?".into(), store_as: "color".into() },
                RecipeStep::Notify { message: "Got it: {{color}}".into() },
            ],
        };
        let out = eng.run(&recipe).await;
        let pq = out.pending_question.expect("should pause on AskUser");
        assert!(pq.question.contains("favorite color"));
        assert!(out.notifications.is_empty(), "must pause BEFORE the Notify");

        let resumed = eng.resume_with_answer(&pq.run_id, "teal").await;
        assert!(resumed.ok, "{:?}", resumed.error);
        assert_eq!(resumed.notifications, vec!["Got it: teal".to_string()], "answer bound + recipe continued");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn recovery_reruns_idempotent_step() {
        let store = Arc::new(RecipeStore::open(&temp_db("idem")).unwrap());
        store
            .save(
                &RunRecord {
                    id: "r2".into(),
                    name: "notify".into(),
                    status: "running".into(),
                    current_step: 0,
                    steps: vec![RecipeStep::Notify { message: "hi".into() }],
                    vars: HashMap::new(),
                    error: None,
                },
                now_ms(),
            )
            .unwrap();
        let resumed = plain_engine_with_store(store.clone()).resume_incomplete().await;
        assert_eq!(resumed, 1, "an idempotent step is safe to re-run");
        assert!(store.resumable().is_empty(), "it should complete (done), not stay running");
    }

    #[test]
    fn act_is_not_idempotent() {
        let act = RecipeStep::Act { kind: "send_email".into(), target: "x".into(), summary: "y".into(), payload: "z".into() };
        assert!(!act.is_idempotent());
        assert!(RecipeStep::Notify { message: "x".into() }.is_idempotent());
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
    async fn replan_recovers_a_failed_step() {
        // A Tool step fails; on_error=Replan asks the LLM, which returns replacement steps; the
        // recipe adapts and completes instead of aborting.
        let replacement = r#"Here you go: [{"Notify":{"message":"recovered via replan"}}]"#;
        let recipe = Recipe {
            id: "t".into(),
            name: "t".into(),
            steps: vec![
                RecipeStep::Tool {
                    tool_name: "broken".into(),
                    args: serde_json::json!({}),
                    store_as: "x".into(),
                    on_error: ErrorAction::Replan,
                },
                RecipeStep::Notify { message: "this original step gets replaced".into() },
            ],
        };
        let out = engine(replacement).run(&recipe).await;
        assert!(out.ok, "recipe should recover: {:?}", out.error);
        assert_eq!(out.notifications, vec!["recovered via replan".to_string()]);
        assert_eq!(out.failure_learnings.len(), 1, "the adaptation should be recorded");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn skip_on_error_degrades_gracefully() {
        // A failing source with on_error=Skip is skipped, not fatal.
        let recipe = Recipe {
            id: "t".into(),
            name: "t".into(),
            steps: vec![
                RecipeStep::Tool { tool_name: "broken".into(), args: serde_json::json!({}), store_as: "x".into(), on_error: ErrorAction::Skip },
                RecipeStep::Notify { message: "still here".into() },
            ],
        };
        let out = engine("unused").run(&recipe).await;
        assert!(out.ok);
        assert_eq!(out.notifications, vec!["still here".to_string()]);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn tool_steps_populate_sources() {
        let llm = r#"{"claims":[{"text":"x","sources":["inbox"],"confidence":"low"}]}"#;
        let out = engine(llm).run(&morning_briefing()).await;
        assert!(out.vars.get("inbox").and_then(|v| v.as_str()).unwrap().contains("boss@acme.com"));
        assert!(out.vars.get("github").and_then(|v| v.as_str()).unwrap().contains("PR #8"));
    }
}
