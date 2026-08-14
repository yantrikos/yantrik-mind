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
    /// LLM over resolved context, result stored under `store_as`.
    ///
    /// `max_tokens` exists because the default (2048) is a REPLY budget, and a step that authors a
    /// document needs a document budget. A page-building chain silently produced 6.7 KB of HTML that
    /// stopped mid-tag — no `</body>`, no `</html>` — and the chain published the fragment and
    /// announced it was live. `None` keeps the old default for every step that just writes a summary.
    Think {
        prompt: String,
        store_as: String,
        #[serde(default)]
        on_error: ErrorAction,
        #[serde(default)]
        max_tokens: Option<usize>,
        /// Force the reasoning preamble off (or on) for this step.
        ///
        /// `None` leaves it to the backend, and on a THINKING model that means thinking is ON and the
        /// token budget is shared between the preamble and the answer. For a step that authors a
        /// DOCUMENT that is the wrong trade: the model spends its budget reasoning about the brief and
        /// runs out before the document is finished. Measured: the same prompt produced a complete
        /// 9-10k-character page with thinking off, and ~900 characters of non-document through the
        /// recipe path with it left to the default.
        #[serde(default)]
        think: Option<bool>,
    },
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
    /// RECURRING DELEGATION (cadence): sleep until the next occurrence of a local-time cadence; on
    /// wake, run the steps AFTER this one; when the recipe completes, LOOP back here and park for
    /// the following occurrence — the run never reaches `done` on its own, it is cancelled or it
    /// recurs. This is the primitive WaitForCondition cannot fake: "every Monday, gather sources,
    /// compose the report, file it" is a cadence, not a wait-until-match.
    ///
    /// `every`: "daily" | "weekly". `weekday`: 0=Monday..6=Sunday (weekly only). Times are the
    /// USER'S local clock via YM_TZ_OFFSET_MINUTES — fixed-offset arithmetic, so a run lands an
    /// hour shifted across a DST change until the offset env is updated; accepted and documented
    /// rather than dragging a tz database into this crate.
    Schedule {
        every: String,
        #[serde(default)]
        weekday: u8,
        hour: u8,
        #[serde(default)]
        minute: u8,
    },
}

/// Next occurrence of a cadence strictly after `now_ms`, in epoch ms. Pure arithmetic (epoch day 0
/// = Thursday ⇒ Monday-based weekday = (days + 3) % 7).
pub(crate) fn next_occurrence_ms(now_ms: u64, every: &str, weekday: u8, hour: u8, minute: u8, tz_offset_min: i64) -> u64 {
    const DAY: i64 = 86_400_000;
    let local_now = now_ms as i64 + tz_offset_min * 60_000;
    let today_start = local_now.div_euclid(DAY) * DAY;
    let in_day = (hour as i64) * 3_600_000 + (minute as i64) * 60_000;
    let mut candidate = today_start + in_day;
    if every == "weekly" {
        let today_wd = (local_now.div_euclid(DAY) + 3).rem_euclid(7) as u8; // 0 = Monday
        let ahead = ((weekday as i64) - (today_wd as i64)).rem_euclid(7);
        candidate = today_start + ahead * DAY + in_day;
        if candidate <= local_now {
            candidate += 7 * DAY;
        }
    } else if candidate <= local_now {
        candidate += DAY; // daily
    }
    (candidate - tz_offset_min * 60_000) as u64
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
    /// Flowing prose — no markers. For a CONVERSATIONAL answer, where the citation machinery is
    /// there to strip ungrounded claims, not to reformat the reply as a list.
    ///
    /// `Summary` is right for a briefing, which genuinely is a list of items. It is wrong for a
    /// chat turn, and the seam showed: the agent loop's compose step is told "compose FRESH in your
    /// own voice; never mirror the work log's list formatting", and then the re-grounding pass
    /// OVERWROTE that prose with `- {claim}` lines. Every factual answer came out as bullets, and a
    /// one-claim reply came out as a single bullet — the cockpit rendered a plain "hi" as "• hi",
    /// which is what made this visible.
    Prose,
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
/// `resolve_vars` over every string in a JSON value, recursively — for tool arguments.
///
/// Strings only. A `{{var}}` standing alone in a number or bool position is not a thing anyone
/// writes, and reinterpreting types here would make an arg's shape depend on its content.
pub fn resolve_args(args: &Value, vars: &HashMap<String, Value>) -> Value {
    match args {
        Value::String(s) => Value::String(resolve_vars(s, vars)),
        Value::Array(a) => Value::Array(a.iter().map(|v| resolve_args(v, vars)).collect()),
        Value::Object(o) => {
            Value::Object(o.iter().map(|(k, v)| (k.clone(), resolve_args(v, vars))).collect())
        }
        other => other.clone(),
    }
}

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
    /// ANTI-CONFABULATION answer (reusable inline): synthesize ONLY from `evidence` with per-claim
    /// citations (ThinkCited), then DETERMINISTICALLY strip any uncited claim (Validate) and render
    /// (Render). Returns the grounded text, or None if nothing in the evidence supports an answer (the
    /// caller should then say "I don't know" / fall back). This is the recipe engine's standout
    /// anti-hallucination — stronger than a prompt plea — exposed for the agent loop's factual answers.
    pub async fn cited_answer(&self, question: &str, evidence: &str) -> Option<String> {
        let messages = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::system(
                "Answer the user's question using ONLY the source below. Output STRICT JSON: \
                 {\"claims\":[{\"text\":\"...\",\"sources\":[\"evidence\"],\"confidence\":\"high|medium|low\"}]}. \
                 Every claim MUST cite \"evidence\". If the source doesn't support something, OMIT it. JSON only.",
            ),
            ChatMessage::user(&format!("QUESTION: {question}\n\nSOURCES:\n[source: evidence]\n{evidence}")),
        ];
        // Reasoning/compose step (grounded answer synthesis) → route to the strong reasoner model
        // (prefer_reasoner) but default think:FALSE. think:true generates thousands of thinking tokens
        // that hold the single GPU 60-90s/call and pile up a multi-minute queue — impractical for an
        // interactive companion; the 35B is strong enough to synthesize without it. YM_THINK_REASONING=on
        // re-enables thinking for anyone who wants the last quality point and can eat the latency.
        let cfg = GenerationConfig {
            think: mind_inference::think_for("reasoning", Some(false)),
            prefer_reasoner: true,
            ..GenerationConfig::default()
        };
        // PRIVATE-GROUNDED: `evidence` is the agent loop's WORK LOG — tool results verbatim, including
        // `recall` output (the household's stored beliefs). The loop reasons on the private lane and
        // then handed the SAME private text to an unscoped (cloud) call here, silently undoing its own
        // guarantee on every factual answer. Fails closed: None ⇒ the caller keeps its un-cited answer
        // (capability reduced, confidentiality preserved).
        let raw = self.inference.chat_grounded(messages, cfg).await.ok()?.text;
        let cited = parse_cited(&raw);
        let kept = CitedOutput { claims: cited.claims.into_iter().filter(|c| c.is_grounded()).collect() };
        if kept.claims.is_empty() {
            return None;
        }
        // PROSE, not Summary. This is a CHAT reply: the citation pass exists to strip ungrounded
        // claims, and reformatting the result as a bullet list was a side effect nobody asked for —
        // it silently undid the compose step's "never mirror the work log's list formatting" and
        // turned every factual answer into bullets.
        Some(render(&kept, &RenderFormat::Prose))
    }

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
- {"Schedule":{"every":"weekly","weekday":0,"hour":9,"minute":0}}  (recurring: steps AFTER this run at each occurrence, forever until cancelled; weekday 0=Monday. Use for "every day/week at ..." goals; put it FIRST.)
- {"Act":{"kind":"send_email","target":"addr","summary":"subject","payload":"body"}}
RULES: prefer read -> Think -> Notify. Reference an earlier step's result by its store_as in double-brace placeholders (see Think/Notify). Use Act ONLY if the goal clearly wants an OUTWARD action; it will require the user's confirmation. End with a Notify that reports the result. Keep it under 6 steps. Current epoch ms = NOW_MS; for any time or expiry use that number plus an offset in ms. Output ONLY the JSON array — no prose, no code fences.
GOAL: GOAL_HERE"#;
        let prompt = template.replace("NOW_MS", &now_ms.to_string()).replace("GOAL_HERE", goal);
        let messages = vec![
            ChatMessage::system("You are JARVIS's task planner. Output ONLY a JSON array of RecipeStep."),
            ChatMessage::user(&prompt),
        ];
        // Recipe planning IS reasoning → strong reasoner model (prefer_reasoner), default think:FALSE
        // (think:true's huge preamble holds the GPU and queues everything; the 35B plans fine without
        // it). Generous max_tokens headroom retained. YM_THINK_PLAN=on re-enables thinking.
        let cfg = GenerationConfig {
            max_tokens: 8000,
            think: mind_inference::think_for("plan", Some(false)),
            prefer_reasoner: true,
            ..GenerationConfig::default()
        };
        // PRIVATE-GROUNDED: the plan is generated from the caller's goal, which on a companion turn
        // is the user's own words about their life. Private lane first, fail closed.
        let resp = self.inference.chat_grounded(messages, cfg).await.ok()?;
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
            // WaitUntil's / Schedule's wait is satisfied by the due check → step past it (Schedule's
            // recurrence is re-armed at run COMPLETION, not here). WaitForCondition re-polls.
            let resume_at = match rec.steps.get(rec.current_step) {
                Some(RecipeStep::WaitUntil { .. }) | Some(RecipeStep::Schedule { .. }) => rec.current_step + 1,
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
        // A recipe with a Schedule step RECURS instead of finishing: loop back to the schedule and
        // park for the next occurrence. Retry counters are cleared so each occurrence gets a fresh
        // error budget; accumulated vars are kept (this occurrence's outputs ground the next one).
        if let Some(sched_idx) = steps.iter().position(|s| matches!(s, RecipeStep::Schedule { .. })) {
            if let Some(RecipeStep::Schedule { every, weekday, hour, minute }) = steps.get(sched_idx) {
                let off: i64 =
                    std::env::var("YM_TZ_OFFSET_MINUTES").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
                let wake = next_occurrence_ms(now_ms(), every, *weekday, *hour, *minute, off);
                vars.retain(|k, _| !k.starts_with("_retry_"));
                vars.insert("__wake_at".into(), Value::from(wake));
                persist("sleeping", sched_idx, &steps, &vars, None);
                return RunOutcome { ok: true, error: None, notifications, failure_learnings, pending_action: None, pending_question: None, sleeping_until: Some(wake), vars };
            }
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
        // PRIVATE-GROUNDED: replan sees the failing step's params + error, which carry whatever the
        // recipe was working on (often private). Private lane first, fail closed.
        let resp = self.inference.chat_grounded(messages, GenerationConfig::default()).await.ok()?;
        let arr = extract_json_array(&resp.text);
        match serde_json::from_str::<Vec<RecipeStep>>(&arr) {
            Ok(new_steps) if !new_steps.is_empty() => Some(new_steps),
            _ => None,
        }
    }

    async fn execute_step(&self, step: &RecipeStep, vars: &mut HashMap<String, Value>) -> StepResult {
        match step {
            RecipeStep::Tool { tool_name, args, store_as, .. } => {
                // Tool args are {{var}}-resolved like every other step's fields.
                //
                // They were NOT, and that quietly capped what a recipe could be: a tool could only
                // ever receive constants, so no step could feed its output into a tool. Every chain
                // therefore had to END at a Think/Notify — "research it, then PUBLISH it" was not
                // expressible. Notify and Think had resolution; Tool was the one that needed it to
                // make a chain actually do something.
                let resolved = resolve_args(args, vars);
                match self.host.call_tool(tool_name, &resolved).await {
                    Ok(out) => {
                        vars.insert(store_as.clone(), Value::String(out));
                        StepResult::Continue
                    }
                    Err(e) => StepResult::Failed(format!("tool '{tool_name}' failed: {e}")),
                }
            }
            RecipeStep::Think { prompt, store_as, max_tokens, think, .. } => {
                let resolved = resolve_vars(prompt, vars);
                let messages = vec![
                    ChatMessage::system(&self.persona),
                    ChatMessage::system(
                        "Answer based ONLY on the provided data. Never invent facts. If data is missing, say so.",
                    ),
                    ChatMessage::user(&resolved),
                ];
                let cfg = GenerationConfig {
                    max_tokens: max_tokens.unwrap_or(GenerationConfig::default().max_tokens),
                    think: *think,
                    ..GenerationConfig::default()
                };
                match self.inference.chat_grounded(messages, cfg).await {
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
                match self.inference.chat_grounded(messages, GenerationConfig::default()).await {
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
            RecipeStep::Schedule { every, weekday, hour, minute } => {
                // First encounter parks until the next occurrence; `resume_due` steps PAST this on
                // wake (like WaitUntil), and end-of-run loops back here for the following one.
                let off: i64 = std::env::var("YM_TZ_OFFSET_MINUTES").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
                StepResult::Sleep(next_occurrence_ms(now_ms(), every, *weekday, *hour, *minute, off))
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
        // The claims ARE sentences, so joining them reads as a paragraph. Terminal punctuation is
        // supplied where the model omitted it, because "A B C" run together is the one way this
        // renders worse than the bullets it replaces.
        RenderFormat::Prose => cited
            .claims
            .iter()
            .map(|c| {
                let t = c.text.trim();
                match t.chars().last() {
                    Some(last) if ".!?:;\"')".contains(last) => t.to_string(),
                    Some(_) => format!("{t}."),
                    None => String::new(),
                }
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
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

    pub(crate) struct ScriptedHost;
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

    /// A chat answer must come back as PROSE, not as a bullet list.
    ///
    /// `cited_answer` is the agent loop's anti-confabulation pass: it re-derives the answer as
    /// claims and deterministically drops any that cite nothing. That part is right. What was wrong
    /// is that it rendered the survivors with `RenderFormat::Summary` — `- {claim}` per line — and
    /// that output REPLACES the composed reply. So the compose step was told "compose fresh in your
    /// own voice; never mirror the work log's list formatting", produced prose, and had it
    /// overwritten with bullets on every factual turn.
    ///
    /// The single-claim case is how it surfaced: a plain "hi" came back as one bullet, and the
    /// cockpit dutifully rendered "• hi".
    #[tokio::test]
    async fn a_cited_chat_answer_reads_as_prose_not_bullets() {
        let e = engine(
            r#"{"claims":[{"text":"The deploy finished at 09:14","sources":["evidence"],"confidence":"high"},
                          {"text":"Two checks are still pending.","sources":["evidence"],"confidence":"medium"}]}"#,
        );
        let out = e.cited_answer("how did the deploy go?", "EVIDENCE: deploy ok 09:14; 2 checks pending").await.unwrap();

        assert!(!out.contains("- "), "a chat answer must not be rendered as a markdown list: {out}");
        assert!(!out.starts_with('-'), "no leading bullet marker: {out}");
        // Both grounded claims survive, joined as sentences with punctuation supplied where missing.
        assert_eq!(out, "The deploy finished at 09:14. Two checks are still pending.");

        // THE ONE-CLAIM CASE that put "• hi" on the screen — bare text, no marker at all.
        let e = engine(r#"{"claims":[{"text":"hi","sources":["evidence"],"confidence":"high"}]}"#);
        assert_eq!(e.cited_answer("hi", "EVIDENCE: greeting").await.unwrap(), "hi.");

        // Still fails CLOSED: nothing grounded means None, so the caller keeps its own answer
        // rather than showing "(nothing grounded to report)".
        let e = engine(r#"{"claims":[{"text":"invented","sources":[],"confidence":"uncited"}]}"#);
        assert!(e.cited_answer("q", "EVIDENCE: none").await.is_none(), "ungrounded claims must yield None");
    }

    /// `Summary` is still bullets — the briefing recipe genuinely IS a list of items, and the fix
    /// above must not have changed it.
    #[test]
    fn the_briefing_summary_format_is_still_a_list() {
        let cited = CitedOutput {
            claims: vec![
                CitedClaim { text: "Inbox: 2 from boss".into(), sources: vec!["evidence".into()], confidence: "high".into() },
                CitedClaim { text: "PR #8 needs review".into(), sources: vec!["evidence".into()], confidence: "high".into() },
            ],
        };
        assert_eq!(render(&cited, &RenderFormat::Summary), "- Inbox: 2 from boss\n- PR #8 needs review");
        assert_eq!(render(&cited, &RenderFormat::Prose), "Inbox: 2 from boss. PR #8 needs review.");
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

    /// PAUSE must hold an order without losing its place in the cadence.
    ///
    /// The mechanism is a status the waking tick does not select for, so the important properties to
    /// pin are: a paused order is never woken however far past its time the tick runs, and resuming
    /// restores the ORIGINAL wake time rather than restarting the clock. If resume reset the timer,
    /// pausing a Monday-09:00 order on Tuesday and resuming on Wednesday would silently move it to
    /// Wednesday forever.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn pause_holds_an_order_and_resume_keeps_its_original_time() {
        let store = Arc::new(RecipeStore::open(&temp_db("pause")).unwrap());
        let eng = plain_engine_with_store(store.clone());
        let future = now_ms() + 60_000;
        let rec = Recipe {
            id: "po".into(),
            name: "weekly report".into(),
            steps: vec![
                RecipeStep::WaitUntil { until_ms: future },
                RecipeStep::Notify { message: "fired".into() },
            ],
        };
        eng.run(&rec).await;
        assert_eq!(eng.list_sleeping().len(), 1);
        // The run id is `{recipe.id}-{timestamp}`, not the recipe id — read it from the store.
        let id = eng.list_sleeping()[0].0.clone();

        assert!(eng.pause_run(&id), "a sleeping order can be paused");
        assert!(eng.list_sleeping().is_empty(), "paused orders leave the sleeping list");
        assert_eq!(eng.list_paused().len(), 1, "and appear as paused");
        assert_eq!(eng.list_paused()[0].2, future, "its next time is preserved, not cleared");

        // The whole point: the tick must not fire it, even long past its time.
        assert!(
            eng.resume_due(future + 10_000_000).await.is_empty(),
            "a paused order must never be woken by the tick"
        );

        assert!(eng.resume_run(&id), "and it can be resumed");
        assert_eq!(eng.list_sleeping()[0].2, future, "resume restores the ORIGINAL time");
        let woke = eng.resume_due(future + 1).await;
        assert_eq!(woke.len(), 1, "once resumed and due, it fires");
        assert!(woke[0].notifications.iter().any(|n| n == "fired"));
    }

    /// RUN NOW fires an order early without touching its schedule, and refuses a paused one rather
    /// than un-pausing it behind the operator's back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn run_now_fires_early_and_refuses_a_paused_order() {
        let store = Arc::new(RecipeStore::open(&temp_db("runnow")).unwrap());
        let eng = plain_engine_with_store(store.clone());
        let far_future = now_ms() + 7 * 24 * 3600 * 1000;
        let rec = Recipe {
            id: "rn".into(),
            name: "weekly".into(),
            steps: vec![
                RecipeStep::WaitUntil { until_ms: far_future },
                RecipeStep::Notify { message: "ran".into() },
            ],
        };
        eng.run(&rec).await;
        let id = eng.list_sleeping()[0].0.clone();
        // Not due for a week, so a tick now does nothing.
        assert!(eng.resume_due(now_ms()).await.is_empty());

        assert!(eng.run_now(&id, now_ms()), "run_now makes it due");
        let woke = eng.resume_due(now_ms()).await;
        assert_eq!(woke.len(), 1, "the very next tick fires it");
        assert!(woke[0].notifications.iter().any(|n| n == "ran"));

        // A paused order is refused — making it due would require un-pausing it, and the pause
        // would then vanish when the run re-armed.
        let rec2 = Recipe {
            id: "rn2".into(),
            name: "held".into(),
            steps: vec![RecipeStep::WaitUntil { until_ms: far_future }, RecipeStep::Notify { message: "no".into() }],
        };
        eng.run(&rec2).await;
        let id2 = eng.list_sleeping()[0].0.clone();
        assert!(eng.pause_run(&id2));
        assert!(!eng.run_now(&id2, now_ms()), "run_now must refuse a paused order");
        assert_eq!(eng.run_status(&id2).as_deref(), Some("paused"), "and leave it paused");
    }

    /// A paused order must stay cancellable — otherwise pausing would trap it, since cancel used to
    /// look only at the sleeping list.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_paused_order_can_still_be_cancelled() {
        let store = Arc::new(RecipeStore::open(&temp_db("pcancel")).unwrap());
        let eng = plain_engine_with_store(store.clone());
        let rec = Recipe {
            id: "pc".into(),
            name: "held".into(),
            steps: vec![
                RecipeStep::WaitUntil { until_ms: now_ms() + 60_000 },
                RecipeStep::Notify { message: "x".into() },
            ],
        };
        eng.run(&rec).await;
        let id = eng.list_sleeping()[0].0.clone();
        assert!(eng.pause_run(&id));
        assert!(eng.cancel_run(&id), "a paused order must be cancellable");
        assert!(eng.list_paused().is_empty());
        assert!(eng.list_sleeping().is_empty());
        assert_eq!(eng.run_status(&id).as_deref(), Some("cancelled"));
    }

    /// Without a store, scheduling cannot survive a restart — so the engine must SAY it has none
    /// rather than accepting orders that will be silently lost.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn no_store_means_no_scheduling_and_it_says_so() {
        let scripted = Arc::new(ScriptedLLM::new("unused"));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        let eng = RecipeEngine::new(pool, Arc::new(ScriptedHost), "JARVIS");
        assert!(!eng.has_store());
        assert!(eng.list_sleeping().is_empty());
        assert!(eng.list_paused().is_empty());
        assert!(!eng.pause_run("anything"));
        assert!(!eng.resume_run("anything"));
        assert!(!eng.run_now("anything", now_ms()));
        assert_eq!(eng.run_status("anything"), None);
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

#[cfg(test)]
mod schedule_tests {
    use super::*;

    // 2026-08-10 is a Monday? Epoch day math: use a KNOWN anchor instead — 1970-01-05 was a Monday
    // (epoch day 4). All times UTC (offset 0) unless the test says otherwise.
    const DAY: u64 = 86_400_000;
    const MON_1970_01_05: u64 = 4 * DAY;

    #[test]
    fn weekly_lands_on_the_requested_weekday_and_time() {
        // From Wednesday 1970-01-07 noon, next Monday 09:00 is 1970-01-12 09:00.
        let wed_noon = MON_1970_01_05 + 2 * DAY + 12 * 3_600_000;
        let next = next_occurrence_ms(wed_noon, "weekly", 0, 9, 0, 0);
        assert_eq!(next, MON_1970_01_05 + 7 * DAY + 9 * 3_600_000);
    }

    #[test]
    fn same_day_before_the_hour_fires_today_after_it_fires_next_week() {
        let mon_8am = MON_1970_01_05 + 8 * 3_600_000;
        assert_eq!(next_occurrence_ms(mon_8am, "weekly", 0, 9, 0, 0), MON_1970_01_05 + 9 * 3_600_000, "an hour away is TODAY");
        let mon_10am = MON_1970_01_05 + 10 * 3_600_000;
        assert_eq!(next_occurrence_ms(mon_10am, "weekly", 0, 9, 0, 0), MON_1970_01_05 + 7 * DAY + 9 * 3_600_000, "already passed → next week");
    }

    #[test]
    fn daily_advances_one_day_when_past() {
        let noon = MON_1970_01_05 + 12 * 3_600_000;
        assert_eq!(next_occurrence_ms(noon, "daily", 0, 9, 0, 0), MON_1970_01_05 + DAY + 9 * 3_600_000);
    }

    /// Chicago (-300): "Monday 09:00 local" is Monday 14:00 UTC. The whole point of the offset —
    /// a cadence set on the user's clock must not fire on the server's.
    #[test]
    fn tz_offset_shifts_the_utc_instant_not_the_local_clock() {
        let sun_noon_utc = MON_1970_01_05 - DAY + 12 * 3_600_000;
        let next = next_occurrence_ms(sun_noon_utc, "weekly", 0, 9, 0, -300);
        assert_eq!(next, MON_1970_01_05 + 14 * 3_600_000);
    }

    #[test]
    fn schedule_is_idempotent_and_never_an_act() {
        let s = RecipeStep::Schedule { every: "weekly".into(), weekday: 0, hour: 9, minute: 0 };
        assert!(s.is_idempotent(), "re-arming a schedule on crash recovery is safe");
    }
}

#[cfg(test)]
mod schedule_loop_tests {
    use super::*;
    use mind_inference::ScriptedLLM;
    use std::collections::HashMap;
    use yantrik_ml::LLMBackend;

    /// THE recurrence contract, end to end: a Schedule-led recipe parks, wakes, runs its work,
    /// and — the part WaitUntil cannot do — parks AGAIN for the next occurrence instead of
    /// finishing. A standing order is never "done"; it recurs or it is cancelled.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_scheduled_recipe_recurs_instead_of_finishing() {
        let store = Arc::new(RecipeStore::open(":memory:").expect("store"));
        let scripted = Arc::new(ScriptedLLM::new("unused"));
        let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
        let eng = RecipeEngine::new(pool, Arc::new(super::tests::ScriptedHost), "JARVIS").with_store(store.clone());
        let rec = Recipe {
            id: "patrol".into(),
            name: "weekly patrol".into(),
            steps: vec![
                RecipeStep::Schedule { every: "weekly".into(), weekday: 0, hour: 9, minute: 0 },
                RecipeStep::Notify { message: "patrol ran".into() },
            ],
        };
        // 1. Starting the recipe parks it at the schedule step (sleeping, future wake).
        let out = eng.run_with(&rec, HashMap::new()).await;
        let first_wake = out.sleeping_until.expect("must park on the schedule");
        assert!(out.notifications.is_empty(), "work must NOT run before the first occurrence");

        // 2. The tick fires past the wake instant: the work runs…
        let outcomes = eng.resume_due(first_wake + 1).await;
        assert_eq!(outcomes.len(), 1, "one due run should wake");
        let o = &outcomes[0];
        assert!(o.notifications.iter().any(|n| n.contains("patrol ran")), "the occurrence does its work");

        // 3. …and the run is SLEEPING again — never "done". The re-park instant comes from the
        // REAL clock (production semantics: a box that slept through an occurrence fires at the
        // next natural instant rather than replaying a backlog), so under the test's simulated
        // tick it equals the same next-natural occurrence; the invariant is that it re-parks at a
        // valid future occurrence at all.
        let second_wake = o.sleeping_until.expect("a scheduled recipe re-parks after its work");
        assert!(second_wake >= first_wake, "re-park is never earlier than the schedule");
        assert_eq!((second_wake as i64 - first_wake as i64) % (7 * 86_400_000), 0, "re-park lands ON the weekly cadence grid");

        // 4. Nothing is due just before it; the same run wakes again after it — indefinitely.
        assert!(eng.resume_due(second_wake - 1).await.is_empty(), "not due early");
        let again = eng.resume_due(second_wake + 1).await;
        assert_eq!(again.len(), 1, "the standing order recurs");
        assert!(again[0].sleeping_until.is_some(), "and re-parks yet again");
    }
}

impl RecipeEngine {
    /// Every sleeping run: (id, name, wake_at_ms). The visibility half of standing orders — a
    /// scheduled run that exists only as a DB row is indistinguishable from one never registered.
    pub fn list_sleeping(&self) -> Vec<(String, String, u64)> {
        let Some(store) = &self.store else { return Vec::new() };
        store
            .due_sleeping(u64::MAX)
            .into_iter()
            .map(|r| {
                let wake = r.vars.get("__wake_at").and_then(|v| v.as_u64()).unwrap_or(0);
                (r.id, r.name, wake)
            })
            .collect()
    }

    /// Cancel a sleeping run by id. True if one was cancelled. Terminal status "cancelled" — the
    /// tick never wakes it again, and the row remains as the audit record of the order having
    /// existed.
    pub fn cancel_run(&self, id: &str) -> bool {
        let Some(store) = &self.store else { return false };
        // A PAUSED order is cancellable too — otherwise pausing something would trap it, since the
        // sleeping list no longer contains it.
        let exists = store
            .by_status("sleeping")
            .iter()
            .chain(store.by_status("paused").iter())
            .any(|r| r.id == id);
        if exists {
            store.set_status(id, "cancelled", Some("cancelled by operator"), 0);
        }
        exists
    }

    /// Every PAUSED standing order: (id, name, wake_at_ms).
    ///
    /// Pause is expressed as a status the waking tick does not select for, which is why it needs no
    /// changes to `resume_due`: `due_sleeping` queries `status='sleeping'`, so a paused row is
    /// simply never a candidate. Its `__wake_at` is left untouched, so resuming restores the
    /// original cadence rather than restarting the clock — pausing a Monday-09:00 order on Tuesday
    /// and resuming Wednesday must still fire the following Monday.
    pub fn list_paused(&self) -> Vec<(String, String, u64)> {
        let Some(store) = &self.store else { return Vec::new() };
        store
            .by_status("paused")
            .into_iter()
            .map(|r| {
                let wake = r.vars.get("__wake_at").and_then(|v| v.as_u64()).unwrap_or(0);
                (r.id, r.name, wake)
            })
            .collect()
    }

    /// Pause a sleeping standing order. True if one was paused.
    pub fn pause_run(&self, id: &str) -> bool {
        let Some(store) = &self.store else { return false };
        let exists = store.by_status("sleeping").iter().any(|r| r.id == id);
        if exists {
            store.set_status(id, "paused", Some("paused by operator"), 0);
        }
        exists
    }

    /// Resume a paused standing order at its original wake time. True if one was resumed.
    ///
    /// If that time has already passed while paused, the next tick fires it immediately — which is
    /// the honest behaviour: the order was due and is now un-paused. It does not silently skip to
    /// the following occurrence, because that would drop work the operator asked for.
    pub fn resume_run(&self, id: &str) -> bool {
        let Some(store) = &self.store else { return false };
        let exists = store.by_status("paused").iter().any(|r| r.id == id);
        if exists {
            store.set_status(id, "sleeping", None, 0);
        }
        exists
    }

    /// Fire a standing order NOW without disturbing its cadence.
    ///
    /// Implemented by moving `__wake_at` into the past so the next tick treats it as due. The steps
    /// and the `Schedule` step itself are untouched, so when the run completes it re-arms for its
    /// normal next occurrence — a manual run is an extra execution, not a reschedule.
    ///
    /// Deliberately does NOT execute inline: the tick owns run execution (it holds the recovery and
    /// intent-hash checks), and running the same steps from two places would be two code paths to
    /// keep honest instead of one.
    ///
    /// Only a SLEEPING order can be fired. A paused one is refused rather than quietly un-paused:
    /// making it due requires setting status back to `sleeping`, and the `Schedule` step re-arms as
    /// sleeping when the run completes, so the pause would vanish without anyone being told. Better
    /// to make the operator resume it deliberately.
    pub fn run_now(&self, id: &str, now_ms: u64) -> bool {
        let Some(store) = &self.store else { return false };
        let Some(mut rec) = store.by_status("sleeping").into_iter().find(|r| r.id == id) else {
            return false;
        };
        rec.vars.insert("__wake_at".to_string(), serde_json::json!(now_ms.saturating_sub(1)));
        store.save(&rec, now_ms).is_ok()
    }

    /// Is durable scheduling available? Without a store, a scheduled run cannot survive a restart,
    /// so an order registered here would be silently lost — callers surface this rather than
    /// offering scheduling that will not hold.
    pub fn has_store(&self) -> bool {
        self.store.is_some()
    }

    /// Is there a run with this id in any of these statuses? Lets a caller distinguish "no such
    /// order" from "that order is in the wrong state for this action".
    pub fn run_status(&self, id: &str) -> Option<String> {
        let store = self.store.as_ref()?;
        for st in ["sleeping", "paused", "running", "waiting", "needs_confirmation", "done", "failed", "cancelled"] {
            if store.by_status(st).iter().any(|r| r.id == id) {
                return Some(st.to_string());
            }
        }
        None
    }
}

#[cfg(test)]
mod chaining_tests {
    use super::*;
    use std::sync::Mutex;

    /// Records the args each tool call actually received.
    struct SpyHost {
        seen: Mutex<Vec<(String, Value)>>,
    }
    #[async_trait::async_trait]
    impl RecipeHost for SpyHost {
        async fn call_tool(&self, tool: &str, args: &Value) -> anyhow::Result<String> {
            self.seen.lock().unwrap().push((tool.to_string(), args.clone()));
            Ok(format!("{tool} ran"))
        }
    }

    #[test]
    fn resolve_args_reaches_nested_strings() {
        let mut vars = HashMap::new();
        vars.insert("page".to_string(), Value::String("<!doctype html>…".into()));
        vars.insert("who".to_string(), Value::String("Pranab".into()));
        let out = resolve_args(
            &serde_json::json!({"html": "{{page}}", "meta": {"author": "{{who}}"}, "tags": ["{{who}}"], "n": 3}),
            &vars,
        );
        assert_eq!(out["html"], "<!doctype html>…");
        assert_eq!(out["meta"]["author"], "Pranab");
        assert_eq!(out["tags"][0], "Pranab");
        assert_eq!(out["n"], 3, "non-strings are untouched");
    }

    #[tokio::test]
    async fn a_tool_step_receives_the_previous_step_s_output() {
        // THE CHAINING GAP. Tool args were passed to the host verbatim, so a tool could only ever be
        // given constants — no step could feed its result into one. Every chain had to end at a
        // Think/Notify, which is why "research it, then PUBLISH it" was not expressible and a
        // delegated "build me a site" could only ever come back as text.
        let host = Arc::new(SpyHost { seen: Mutex::new(Vec::new()) });
        let llm = Arc::new(mind_inference::ScriptedLLM::new("<!doctype html><title>Made</title>"));
        let pool = InferencePool::new(llm as Arc<dyn yantrik_ml::LLMBackend>, 1);
        let engine = RecipeEngine::new(pool, host.clone(), "JARVIS");
        let recipe = Recipe {
            id: "t".into(),
            name: "chain".into(),
            steps: vec![
                RecipeStep::Think { prompt: "write it".into(), store_as: "page".into(), on_error: ErrorAction::Fail, max_tokens: None, think: None },
                RecipeStep::Tool {
                    tool_name: "publish_page".into(),
                    args: serde_json::json!({"html": "{{page}}"}),
                    store_as: "url".into(),
                    on_error: ErrorAction::Fail,
                },
            ],
        };
        let out = engine.run_with(&recipe, HashMap::new()).await;
        assert!(out.ok, "chain failed: {:?}", out.error);
        let seen = host.seen.lock().unwrap();
        let (tool, args) = seen.first().expect("the tool step never ran");
        assert_eq!(tool, "publish_page");
        assert_eq!(
            args.get("html").and_then(|v| v.as_str()),
            Some("<!doctype html><title>Made</title>"),
            "the tool got the placeholder instead of the authored document — args are not resolved"
        );
    }
}
