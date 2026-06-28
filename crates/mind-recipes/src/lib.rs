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
    /// PERSISTENT DELEGATION (time): sleep until an absolute epoch-ms, then continue. The run is
    /// persisted as `sleeping`; the tick (`resume_due`) wakes it when the time has passed.
    WaitUntil { until_ms: u64 },
    /// PERSISTENT DELEGATION (condition): poll a read tool every `poll_secs` until `condition` holds
    /// on its stored result (then continue), giving up at `expire_ms` (then fail). Each poll sleeps
    /// the run; the tick re-polls. read/monitor only — the doing is later, harm-gated, `Act` steps.
    WaitForCondition {
        tool_name: String,
        args: Value,
        store_as: String,
        condition: Condition,
        poll_secs: u64,
        expire_ms: u64,
    },
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
    /// The recipe is SLEEPING on a WaitUntil/WaitForCondition step until this epoch-ms; the tick
    /// (`resume_due`) wakes it. `None` for a non-sleeping outcome.
    pub sleeping_until: Option<u64>,
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
    /// A WaitUntil/WaitForCondition step — pause the recipe; wake at this epoch-ms (the tick resumes).
    Sleep(u64),
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
        self.run_with(recipe, HashMap::new()).await
    }

    /// Start a run with initial vars — how a DELEGATED run is created: seed `__effect_budget` (cap on
    /// outward actions) and any inputs before the first step. The intent hash is stamped from the
    /// recipe's `Act` steps on first run and re-validated on every later resume.
    pub async fn run_with(&self, recipe: &Recipe, vars: HashMap<String, Value>) -> RunOutcome {
        let id = format!("{}-{}", recipe.id, now_ms());
        self.run_from(&id, &recipe.name, recipe.steps.clone(), 0, vars).await
    }

    /// THE PLANNER — author a runnable recipe from a free-form goal. The LLM emits a JSON array of
    /// `RecipeStep` over a constrained menu + the read tools (same shape Replan already produces, so
    /// this reuses the proven authoring path). The planner only PROPOSES: outward `Act` steps are
    /// still harm-gated, confirmation-required, and effect-budget-capped when the recipe runs.
    /// Returns `None` if the model produced nothing parseable.
    pub async fn plan(&self, goal: &str, now_ms: u64) -> Option<Vec<RecipeStep>> {
        // A raw template (literal JSON braces + `{{var}}` placeholders) with simple text tokens we
        // substitute — avoids `format!` brace-escaping entirely.
        let template = r#"Turn the GOAL into a runnable recipe: a JSON array of RecipeStep (externally-tagged JSON).
Read tools available for Tool / WaitForCondition steps: inbox, github, web_search, fetch, recall, due_tasks.
Step types:
- {"Tool":{"tool_name":"web_search","args":{"query":"..."},"store_as":"hits"}}
- {"Tool":{"tool_name":"fetch","args":{"url":"https://..."},"store_as":"page"}}
- {"Think":{"prompt":"summarize {{hits}}","store_as":"answer"}}
- {"Notify":{"message":"{{answer}}"}}
- {"WaitForCondition":{"tool_name":"inbox","args":{"limit":10},"store_as":"inbox","condition":{"op":"VarContains","var":"inbox","substring":"keyword"},"poll_secs":120,"expire_ms":NOW_MS}}
- {"WaitUntil":{"until_ms":NOW_MS}}
- {"Act":{"kind":"send_email","target":"addr","summary":"subject","payload":"body"}}
RULES: prefer read -> Think -> Notify. Reference an earlier step's result by its store_as in double-brace placeholders (see Think/Notify). Use Act ONLY if the goal clearly wants an OUTWARD action; it will require the user's confirmation. End with a Notify that reports the result. Keep it under 6 steps. Current epoch ms = NOW_MS; for any time or expiry use that number plus an offset in ms. Output ONLY the JSON array — no prose, no code fences.
GOAL: GOAL_HERE"#;
        let prompt = template.replace("NOW_MS", &now_ms.to_string()).replace("GOAL_HERE", goal);
        let messages = vec![
            ChatMessage::system("You are JARVIS's task planner. Output ONLY a JSON array of RecipeStep."),
            ChatMessage::user(&prompt),
        ];
        // Reasoning models burn tokens on a preamble before the JSON, so give generous headroom.
        let cfg = GenerationConfig { max_tokens: 8000, ..GenerationConfig::default() };
        let resp = self.inference.chat(messages, cfg).await.ok()?;
        let arr = extract_recipe_json(&resp.text);
        match serde_json::from_str::<Vec<RecipeStep>>(&arr) {
            Ok(steps) if !steps.is_empty() => Some(steps),
            _ => None,
        }
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

    /// PERSISTENT-DELEGATION TICK: wake every sleeping run whose wake time has passed. Call this on
    /// the scheduler heartbeat. Before resuming, each run re-validates its authorized-intent hash —
    /// a changed set of `Act` steps parks it as `needs_confirmation` instead of executing (so a
    /// long-delegated task can't drift into doing something different from what was authorized).
    pub async fn resume_due(&self, now_ms: u64) -> Vec<RunOutcome> {
        let store = match &self.store {
            Some(s) => s.clone(),
            None => return Vec::new(),
        };
        let mut outcomes = Vec::new();
        for rec in store.due_sleeping(now_ms) {
            let stamped = rec.vars.get("__intent_hash").and_then(|v| v.as_i64());
            if stamped != Some(intent_hash(&rec.steps)) {
                store.set_status(
                    &rec.id,
                    "needs_confirmation",
                    Some("intent changed since delegation — awaiting re-confirmation"),
                    now_ms,
                );
                continue;
            }
            // WaitUntil's wait is satisfied by the due check → step past it. WaitForCondition re-polls.
            let resume_at = match rec.steps.get(rec.current_step) {
                Some(RecipeStep::WaitUntil { .. }) => rec.current_step + 1,
                _ => rec.current_step,
            };
            outcomes.push(self.run_from(&rec.id, &rec.name, rec.steps, resume_at, rec.vars).await);
        }
        outcomes
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
            sleeping_until: None,
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
        // Stamp the authorized-intent hash once; delegated runs re-validate it on each resume so a
        // mutated set of outward (Act) steps can never silently execute after a wait.
        if !vars.contains_key("__intent_hash") {
            vars.insert("__intent_hash".into(), Value::from(intent_hash(&steps)));
        }
        persist("running", i, &steps, &vars, None);
        while i < steps.len() {
            guard += 1;
            if guard > 1000 {
                persist("failed", i, &steps, &vars, Some("step budget exceeded"));
                return RunOutcome { ok: false, error: Some("step budget exceeded".into()), notifications, failure_learnings, pending_action: None, pending_question: None, sleeping_until: None, vars };
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
                    return RunOutcome { ok: true, error: None, notifications, failure_learnings, pending_action: Some(req), pending_question: None, sleeping_until: None, vars };
                }
                StepResult::Ask(question) => {
                    // Pause here: wait for the user's free-form answer (resume_with_answer binds it).
                    persist("waiting", i, &steps, &vars, None);
                    let pq = PendingQuestion { run_id: id.to_string(), question };
                    return RunOutcome { ok: true, error: None, notifications, failure_learnings, pending_action: None, pending_question: Some(pq), sleeping_until: None, vars };
                }
                StepResult::Sleep(wake_at) => {
                    // Persistent delegation: park the run until `wake_at`; the tick (`resume_due`)
                    // wakes it. The wait step re-evaluates on resume (WaitForCondition re-polls).
                    vars.insert("__wake_at".into(), Value::from(wake_at));
                    persist("sleeping", i, &steps, &vars, None);
                    return RunOutcome { ok: true, error: None, notifications, failure_learnings, pending_action: None, pending_question: None, sleeping_until: Some(wake_at), vars };
                }
                StepResult::Failed(e) => {
                    match self.handle_error(i, &e, &step.on_error(), &mut vars, &mut steps, &mut failure_learnings).await {
                        ErrorResolution::Skip => i += 1,
                        ErrorResolution::RetryHere => { /* re-run steps[i] */ }
                        ErrorResolution::JumpTo(t) => i = t,
                        ErrorResolution::Abort => {
                            persist("failed", i, &steps, &vars, Some(&e));
                            return RunOutcome { ok: false, error: Some(e), notifications, failure_learnings, pending_action: None, pending_question: None, sleeping_until: None, vars };
                        }
                    }
                }
            }
            // Record progress so a crash here resumes from the right place.
            persist("running", i, &steps, &vars, None);
        }
        persist("done", i, &steps, &vars, None);
        RunOutcome { ok: true, error: None, notifications, failure_learnings, pending_action: None, pending_question: None, sleeping_until: None, vars }
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
                // Effect-budget: a delegated run carries a cap on outward actions. Replan can't expand
                // it (the counter lives in vars, preserved across replans/resumes).
                if let Some(b) = vars.get("__effect_budget").and_then(|v| v.as_i64()) {
                    if b <= 0 {
                        return StepResult::Failed("effect budget exhausted".into());
                    }
                    vars.insert("__effect_budget".into(), Value::from(b - 1));
                }
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
            RecipeStep::WaitUntil { until_ms } => {
                if now_ms() >= *until_ms {
                    StepResult::Continue
                } else {
                    StepResult::Sleep(*until_ms)
                }
            }
            RecipeStep::WaitForCondition { tool_name, args, store_as, condition, poll_secs, expire_ms } => {
                if now_ms() >= *expire_ms {
                    return StepResult::Failed(format!("WaitForCondition expired before '{store_as}' held"));
                }
                match self.host.call_tool(tool_name, args).await {
                    Ok(out) => {
                        vars.insert(store_as.clone(), Value::String(out));
                    }
                    Err(e) => return StepResult::Failed(format!("monitor tool '{tool_name}' failed: {e}")),
                }
                if condition.evaluate(vars) {
                    StepResult::Continue
                } else {
                    let wake = now_ms().saturating_add(poll_secs.saturating_mul(1000));
                    StepResult::Sleep(wake.min(*expire_ms))
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

/// A stable hash of a run's authorized OUTWARD intent — the (kind,target,summary) of its `Act`
/// steps. FNV-1a (deterministic across processes, unlike the std hasher's per-process seed) so a
/// run can stamp it at creation and re-validate it after a restart. 0 when there are no Act steps.
fn intent_hash(steps: &[RecipeStep]) -> i64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325; // FNV offset basis
    let mut feed = |s: &str| {
        for b in s.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3); // FNV prime
        }
    };
    for s in steps {
        if let RecipeStep::Act { kind, target, summary, .. } = s {
            feed(kind);
            feed("\x1f");
            feed(target);
            feed("\x1f");
            feed(summary);
            feed("\x1e");
        }
    }
    h as i64
}

/// Robustly pull a JSON array out of a planner/LLM reply that may include a reasoning preamble
/// (`<think>…</think>`, which can itself contain `[`) and/or a ```json fence. Drops the think block,
/// prefers fenced content, then slices the first `[` to the last `]`.
fn extract_recipe_json(text: &str) -> String {
    let mut t = text;
    if let Some(idx) = t.rfind("</think>") {
        t = &t[idx + "</think>".len()..];
    }
    // Prefer the contents of the first fenced block, if any.
    let body = if let Some(start) = t.find("```") {
        let after = &t[start + 3..];
        let after = after.strip_prefix("json").or_else(|| after.strip_prefix("JSON")).unwrap_or(after);
        let after = after.trim_start_matches(['\n', '\r', ' ']);
        after.split("```").next().unwrap_or(after)
    } else {
        t
    };
    if let (Some(s), Some(e)) = (body.find('['), body.rfind(']')) {
        if e > s {
            return body[s..=e].to_string();
        }
    }
    "[]".to_string()
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

    // ── persistent delegation ──────────────────────────────────────────────────────────────────

    /// A condition tool whose answer flips from "pending" to "ready" — to drive WaitForCondition.
    struct FlipHost {
        ready: Arc<std::sync::atomic::AtomicBool>,
    }
    #[async_trait]
    impl RecipeHost for FlipHost {
        async fn call_tool(&self, _tool: &str, _args: &Value) -> anyhow::Result<String> {
            Ok(if self.ready.load(std::sync::atomic::Ordering::SeqCst) {
                "STATUS: ready".into()
            } else {
                "STATUS: pending".into()
            })
        }
    }

    /// WaitUntil parks the run until its time, then the tick (`resume_due`) wakes it and it continues.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wait_until_sleeps_then_resumes_on_tick() {
        let store = Arc::new(RecipeStore::open(&temp_db("waituntil")).unwrap());
        let eng = plain_engine_with_store(store.clone());
        let future = now_ms() + 60_000;
        let rec = Recipe {
            id: "wu".into(),
            name: "wu".into(),
            steps: vec![
                RecipeStep::WaitUntil { until_ms: future },
                RecipeStep::Notify { message: "awake".into() },
            ],
        };
        let out = eng.run(&rec).await;
        assert_eq!(out.sleeping_until, Some(future), "should sleep until the target time");
        assert!(out.notifications.is_empty(), "must not run past the wait yet");

        assert!(eng.resume_due(future - 1).await.is_empty(), "not due yet → no resume");
        let woke = eng.resume_due(future + 1).await;
        assert_eq!(woke.len(), 1, "due now → resumes exactly one run");
        assert!(woke[0].notifications.iter().any(|n| n == "awake"), "runs the step after the wait");
    }

    /// WaitForCondition re-polls each tick; stays asleep while false, continues once true.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn wait_for_condition_polls_until_true() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let store = Arc::new(RecipeStore::open(&temp_db("wfc")).unwrap());
        let ready = Arc::new(AtomicBool::new(false));
        let pool = InferencePool::new(Arc::new(ScriptedLLM::new("x")) as Arc<dyn LLMBackend>, 1);
        let eng = RecipeEngine::new(pool, Arc::new(FlipHost { ready: ready.clone() }), "JARVIS")
            .with_store(store.clone());
        let rec = Recipe {
            id: "wfc".into(),
            name: "wfc".into(),
            steps: vec![
                RecipeStep::WaitForCondition {
                    tool_name: "status".into(),
                    args: serde_json::json!({}),
                    store_as: "st".into(),
                    condition: Condition::VarContains { var: "st".into(), substring: "ready".into() },
                    poll_secs: 30,
                    expire_ms: now_ms() + 3_600_000,
                },
                RecipeStep::Notify { message: "condition met".into() },
            ],
        };
        let out = eng.run(&rec).await;
        assert!(out.sleeping_until.is_some(), "condition false → sleeps");
        assert!(out.notifications.is_empty());

        // Still false: a tick re-polls and sleeps again.
        let w1 = eng.resume_due(now_ms() + 10_000_000).await;
        assert_eq!(w1.len(), 1);
        assert!(w1[0].sleeping_until.is_some(), "still pending → sleeps again");

        // Flip true: the next tick re-polls and the run completes.
        ready.store(true, Ordering::SeqCst);
        let w2 = eng.resume_due(now_ms() + 20_000_000).await;
        assert_eq!(w2.len(), 1);
        assert!(w2[0].notifications.iter().any(|n| n == "condition met"), "condition true → runs the step");
    }

    /// A delegated run whose stored `Act` steps were altered after delegation must NOT execute on
    /// resume — it parks as `needs_confirmation` (intent-hash re-validation).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn intent_hash_mismatch_parks_for_confirmation() {
        let store = Arc::new(RecipeStore::open(&temp_db("intent")).unwrap());
        let executed = Arc::new(Mutex::new(0u32));
        let rt: Arc<dyn ActionRuntime> = Arc::new(FakeRuntime { decision: ActionDecision::Execute, executed: executed.clone() });
        let pool = InferencePool::new(Arc::new(ScriptedLLM::new("x")) as Arc<dyn LLMBackend>, 1);
        let eng = RecipeEngine::new(pool, Arc::new(ScriptedHost), "JARVIS").with_runtime(rt).with_store(store.clone());
        let future = now_ms() + 60_000;
        let rec = Recipe {
            id: "ih".into(),
            name: "ih".into(),
            steps: vec![
                RecipeStep::WaitUntil { until_ms: future },
                RecipeStep::Act { kind: "send_email".into(), target: "a@b.com".into(), summary: "hi".into(), payload: "p".into() },
            ],
        };
        let out = eng.run_with(&rec, HashMap::new()).await;
        assert!(out.sleeping_until.is_some());

        // Tamper the stored Act target, keeping status=sleeping + the original stamped intent hash.
        let mut r = store.due_sleeping(future + 1).into_iter().next().expect("one sleeping run");
        if let RecipeStep::Act { target, .. } = &mut r.steps[1] {
            *target = "attacker@evil.com".into();
        }
        store.save(&r, future).unwrap();

        let woke = eng.resume_due(future + 2).await;
        assert!(woke.is_empty(), "tampered run must not resume/execute");
        assert_eq!(store.load(&r.id).unwrap().status, "needs_confirmation");
        assert_eq!(*executed.lock().unwrap(), 0, "the altered action must never run");
    }

    /// Planner JSON extraction survives a reasoning preamble (with a stray `[`) and a ```json fence.
    #[test]
    fn extract_recipe_json_handles_think_and_fence() {
        let msg = "<think>I'll use [web_search] then notify the user</think>\n```json\n[{\"Notify\":{\"message\":\"hi\"}}]\n```";
        let arr = extract_recipe_json(msg);
        let steps: Vec<RecipeStep> = serde_json::from_str(&arr).expect("should parse despite think+fence");
        assert_eq!(steps.len(), 1);
    }

    /// The planner authors a JSON recipe from a goal (LLM scripted), and the recipe then runs.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn planner_authors_a_runnable_recipe() {
        let recipe_json = r#"[{"Tool":{"tool_name":"inbox","args":{"limit":5},"store_as":"inbox"}},{"Notify":{"message":"Inbox: {{inbox}}"}}]"#;
        let eng = engine(recipe_json);
        let steps = eng.plan("summarize my inbox", 1000).await.expect("planner should author steps");
        assert_eq!(steps.len(), 2, "should parse both authored steps");
        let rec = Recipe { id: "p".into(), name: "p".into(), steps };
        let out = eng.run(&rec).await;
        assert!(out.ok);
        assert!(out.notifications.iter().any(|n| n.contains("boss@acme.com")), "Notify renders the gathered inbox");
    }

    /// The effect budget caps outward actions across a delegated run; Replan/resume can't expand it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn effect_budget_caps_outward_actions() {
        let (eng, executed) = engine_with_runtime(ActionDecision::Execute);
        let two_acts = Recipe {
            id: "eb".into(),
            name: "eb".into(),
            steps: vec![
                RecipeStep::Act { kind: "send_email".into(), target: "a@b".into(), summary: "1".into(), payload: "p".into() },
                RecipeStep::Act { kind: "send_email".into(), target: "c@d".into(), summary: "2".into(), payload: "p".into() },
            ],
        };
        let mut vars = HashMap::new();
        vars.insert("__effect_budget".into(), Value::from(1i64));
        let out = eng.run_with(&two_acts, vars).await;
        assert!(!out.ok, "second action should be capped");
        assert_eq!(out.error.as_deref(), Some("effect budget exhausted"));
        assert_eq!(*executed.lock().unwrap(), 1, "exactly one action runs under a budget of 1");
    }
}
