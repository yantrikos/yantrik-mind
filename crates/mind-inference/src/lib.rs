//! mind-inference — the async facade over yantrik-ml's synchronous, blocking backends.
//!
//! Spike B (Phase 0): prove the **bounded blocking pool**. `LLMBackend::chat` is synchronous and
//! blocking (local candle/llama.cpp backends are additionally `Mutex`-serialized); calling it
//! directly from an async task would block a tokio worker for the whole generation and starve the
//! executor. So every call goes through `spawn_blocking` behind a `Semaphore` (permits = 1 for a
//! local single-model backend, higher for API backends). This queue is also where latency/quality
//! fallback + cost governance will live (Phase 2).

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Semaphore;
use yantrik_ml::{ChatMessage, GenerationConfig, LLMBackend, LLMResponse, ToolCall};

/// NIGHT SHIFT privacy lanes. Every inference request declares what class of data rides in the
/// prompt; the facade routes or REFUSES based on where the backing provider runs. This is the wall
/// the charter builds first: family data must not silently transit cloud providers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrivacyScope {
    /// Family memories, names, photos-derived facts, sensitive household context. Only providers
    /// in `YM_PRIVATE_PROVIDERS` (owned hardware) may serve it; otherwise the call is REFUSED and
    /// the caller must fall back to deterministic rendering (scaffold/fill).
    Private,
    /// Semi-private operational data the owner has EXPLICITLY allowed for named cloud providers
    /// via `YM_HOUSEHOLD_PROVIDERS` (default: current providers — making today's implicit routing
    /// explicit and revocable). The unscoped `chat()` defaults here.
    Household,
    /// Public-web research, generic scaffolding, code — any configured provider.
    Public,
}

impl PrivacyScope {
    pub fn as_str(&self) -> &'static str {
        match self {
            PrivacyScope::Private => "private",
            PrivacyScope::Household => "household",
            PrivacyScope::Public => "public",
        }
    }
}

/// Collapse every `system` message into ONE leading system message.
///
/// Strict chat templates require the system message to be first, and to be singular. qwen3.8's
/// raises `Jinja Exception: System message must be at the beginning.` and the whole request comes
/// back 500; gemma's accepts the same list happily. The mind always builds several system blocks
/// (persona, pack rules, format note, agent instructions), so "which model" silently decided
/// whether the mind worked at all.
///
/// Order is preserved and the blocks are joined with a blank line, so nothing is lost or reordered
/// relative to the other system blocks. A system message that arrives AFTER a user turn is also
/// hoisted — such a block is late-added context, and every template that rejects mid-conversation
/// system turns would reject it anyway.
pub(crate) fn merge_system_messages(messages: Vec<ChatMessage>) -> Vec<ChatMessage> {
    if messages.iter().filter(|m| m.role == "system").count() < 2 {
        return messages;
    }
    let mut system = String::new();
    let mut rest: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    for m in messages {
        if m.role == "system" {
            if !m.content.trim().is_empty() {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(m.content.trim());
            }
        } else {
            rest.push(m);
        }
    }
    let mut out = Vec::with_capacity(rest.len() + 1);
    if !system.is_empty() {
        out.push(ChatMessage::system(&system));
    }
    out.extend(rest);
    out
}

/// Pure policy: may a pool labeled `provider` serve a request of `scope`, given the two CSV
/// allowlists? Pure so it's testable without env races.
pub fn scope_allows(
    scope: PrivacyScope,
    provider: &str,
    household_csv: &str,
    private_csv: &str,
) -> bool {
    let pl = provider.to_lowercase();
    let in_list = |csv: &str| {
        csv.split(',')
            .map(|x| x.trim().to_lowercase())
            .filter(|x| !x.is_empty())
            .any(|x| pl.contains(&x))
    };
    match scope {
        PrivacyScope::Public => true,
        PrivacyScope::Household => in_list(household_csv),
        // The private lane never falls back to the household list — owned hardware or refusal.
        PrivacyScope::Private => in_list(private_csv),
    }
}

/// Per-scope authorized-dispatch/refused counters — the audit trail `ym privacy` renders.
/// Process-lifetime. A permitted call is counted before backend execution, so this is deliberately
/// an attempt/exposure-risk measure, not evidence that a provider returned a usable response.
static PRIVACY_SERVED: [std::sync::atomic::AtomicU64; 3] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];
static PRIVACY_REFUSED: [std::sync::atomic::AtomicU64; 3] = [
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
    std::sync::atomic::AtomicU64::new(0),
];
/// Stable call-site identities for Household-lane calls. Unlike a timestamp that must be manually
/// correlated with unrelated journal lines, this answers "what used the lane?" directly. Keys are
/// supplied by code as `&'static str`, never derived from prompts, so private/user text cannot enter
/// the audit surface.
static HOUSEHOLD_CALLSITES: std::sync::Mutex<Option<HashMap<String, u64>>> =
    std::sync::Mutex::new(None);
/// Private-grounded turns that ESCALATED to the household (cloud) lane because no owned-hardware
/// provider was configured. This is the honest audit of the privacy gap: it should be 0 once
/// YM_PRIVATE_PROVIDERS names a local/on-device provider. A NON-zero value means private family
/// context reached a cloud provider — the Constitutional-Kernel invariant is not yet true here.
static PRIVACY_ESCALATED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Survival mode: true when all cloud providers in the chain have failed and the mind is
/// operating on its local-only fallback tier.
static SURVIVAL_MODE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// Timestamp of when survival mode was first activated (for "active Nm" reporting).
static SURVIVAL_SINCE: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

fn scope_idx(s: PrivacyScope) -> usize {
    match s {
        PrivacyScope::Private => 0,
        PrivacyScope::Household => 1,
        PrivacyScope::Public => 2,
    }
}

/// The audit report: lanes config + per-scope served/refused counts since start.
/// How many private-grounded turns have ESCALATED to the household (cloud) lane. Exposed so other
/// crates can ASSERT the privacy property structurally: a path carrying private context must route
/// through `chat_grounded`/`chat_scoped(Private)` — which touch this counter — and never through an
/// unscoped `chat()`, which silently takes the Household lane and never counts. A test that seeds a
/// cloud-only pool and watches this move is proving "the private lane was at least ATTEMPTED".
/// E.SEC18: the lane counters as numbers, for the security audit's JSON. Same atomics the text
/// report reads; the names carry the exposure semantics (dispatched, not served).
pub fn privacy_lane_counts() -> serde_json::Value {
    use std::sync::atomic::Ordering;
    serde_json::json!({
        "dispatched_exposure": {
            "private": PRIVACY_SERVED[0].load(Ordering::Relaxed),
            "household": PRIVACY_SERVED[1].load(Ordering::Relaxed),
            "public": PRIVACY_SERVED[2].load(Ordering::Relaxed),
        },
        "refused": {
            "private": PRIVACY_REFUSED[0].load(Ordering::Relaxed),
            "household": PRIVACY_REFUSED[1].load(Ordering::Relaxed),
            "public": PRIVACY_REFUSED[2].load(Ordering::Relaxed),
        },
        "private_grounded_escalated_to_cloud": privacy_escalated_count(),
    })
}

pub fn privacy_escalated_count() -> u64 {
    PRIVACY_ESCALATED.load(std::sync::atomic::Ordering::Relaxed)
}

/// E.OBS1: the process-wide lane observer — installed once by the conversation engine, fired
/// POST-SUCCESS from the pool wrapper with the scope and the label of the link that ACTUALLY
/// ANSWERED (not the pre-dispatch route). Content never rides this hook. One emit site means a UI
/// badge can never assert a lane no provider served — and it is deliberately NOT beside
/// `PRIVACY_SERVED`, which is the conservative pre-dispatch EXPOSURE count, a different fact.
static LANE_OBSERVER: std::sync::OnceLock<Box<dyn Fn(&str, &str) + Send + Sync>> =
    std::sync::OnceLock::new();

/// Install the lane observer. First install wins (process-wide, like the stats it rides beside);
/// a second install is a no-op rather than an error so tests and multi-engine setups stay simple.
pub fn set_lane_observer(observer: Box<dyn Fn(&str, &str) + Send + Sync>) {
    let _ = LANE_OBSERVER.set(observer);
}

thread_local! {
    /// E.OBS1c: which chain link actually ANSWERED the call running on this blocking thread.
    /// Set by ChainBackend at its success returns, taken by the pool wrapper inside the SAME
    /// spawn_blocking closure — one closure runs to completion on its thread, so concurrent calls
    /// (other closures) can never cross-label. A tokio task_local cannot do this job: the chat
    /// trait is synchronous and runs under spawn_blocking, where task-locals do not reach.
    static SERVING_LINK: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn note_serving_link(label: &str) {
    SERVING_LINK.with(|c| *c.borrow_mut() = Some(label.to_string()));
}

// ── L4-0: the spend ledger seam (inference-ledger-v1) ─────────────────────────────────────────
// One terminal record per LOGICAL request at the outer request seam — gate through completion,
// plain and streaming alike. Refused = zero backend attempts; failed = accepted, nothing served;
// served = a response. The pool does not know how rows are stored: a bound `InferenceLedger`
// (the engine's own recorder, never a process-wide observer) receives the typed record.

thread_local! {
    /// The request's attempt counter, installed on the blocking thread for the request's
    /// duration. It is an `Arc` the awaiting side also holds, so a backend panic (a JoinError)
    /// loses the thread but not the count.
    static ATTEMPTS: std::cell::RefCell<Option<Arc<std::sync::atomic::AtomicU32>>> =
        const { std::cell::RefCell::new(None) };
}
fn install_attempts(counter: Arc<std::sync::atomic::AtomicU32>) {
    ATTEMPTS.with(|a| *a.borrow_mut() = Some(counter));
}
/// Count ONE backend invocation at the observable boundary: one direct `LLMBackend::chat` on a
/// leaf backend (counted by the pool BEFORE the call, so a panic inside it is still one
/// attempt), or one link invocation inside a provider chain (its survival tier included).
/// Transport-internal retries a backend hides are NOT counted — the number is invocations the
/// pool can see, never a claim about outbound HTTP attempts. Outside a request it is a no-op.
pub fn note_attempt() {
    ATTEMPTS.with(|a| {
        if let Some(c) = a.borrow().as_ref() {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    });
}
/// A chain counts its own links; a leaf backend cannot, so the pool counts its invocation.
/// Chain-ness is read off the backend OBJECT, never off a configured label.
fn is_chain_backend(backend: &dyn LLMBackend) -> bool {
    backend.backend_name().starts_with("chain[")
}

tokio::task_local! {
    /// The loop opportunity a model call runs inside, set by the loop host around the act.
    static OPPORTUNITY: String;
}
/// Run `f` with the loop opportunity id every inference row inside it will carry. The id is
/// the loop ledger's own opportunity id, so a spend row joins its loop by identity — never by
/// timestamp.
pub async fn within_opportunity<F: std::future::Future>(id: String, f: F) -> F::Output {
    OPPORTUNITY.scope(id, f).await
}
/// The opportunity this task is running under, if any. Public because a caller that SPAWNS work
/// must carry it across by hand: task-locals are not inherited, and a spend row that loses its
/// opportunity joins no loop (E.PORT1-B, found in review).
pub fn current_opportunity() -> Option<String> {
    OPPORTUNITY.try_with(|s| s.clone()).ok()
}

/// How one logical request ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CallOutcome {
    /// The privacy gate refused it: zero backend attempts.
    Refused,
    /// Accepted, but no backend served it.
    Failed,
    /// A response came back.
    Served,
}
impl CallOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CallOutcome::Refused => "refused",
            CallOutcome::Failed => "failed",
            CallOutcome::Served => "served",
        }
    }
}
/// Where a token count came from. Only a backend contract that REPORTS counts may construct a
/// `TokenUsage`; today no backend does, so every row carries tokens absent. An estimate never
/// becomes a count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenProvenance {
    ProviderReported,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenUsage {
    pub prompt: u64,
    pub completion: u64,
    pub provenance: TokenProvenance,
}
/// One logical inference request, as the pool saw it. No prompt, no reply, no user datum: the
/// callsite is the static string the code authored.
#[derive(Clone, Debug)]
pub struct InferenceCall {
    pub callsite: &'static str,
    pub scope: PrivacyScope,
    /// The SELECTED route label (a provider or a chain) — never relabelled as a model.
    pub route: String,
    /// The link that answered, when the chain noted one (post-success fact).
    pub served_by: Option<String>,
    pub outcome: CallOutcome,
    /// Backend invocations at the observable boundary (see [`note_attempt`]).
    pub attempts: u32,
    pub latency_ms: u64,
    pub streaming: bool,
    /// The loop opportunity id this call ran inside, if the host set one.
    pub opportunity: Option<String>,
    pub tokens: Option<TokenUsage>,
}
/// The sink a pool records into — bound per pool by whoever owns the recorder.
pub trait InferenceLedger: Send + Sync {
    fn record_call(&self, call: InferenceCall);
}

fn take_serving_link() -> Option<String> {
    SERVING_LINK.with(|c| c.borrow_mut().take())
}

fn record_household_callsite(callsite: &'static str) {
    // A public caller can still supply an empty static string. Do not let that create a visually
    // blank dashboard row that looks attributed while naming nobody; fold missing identities into
    // the same explicit compatibility bucket as `chat()`.
    let callsite = if callsite.trim().is_empty() {
        "unattributed"
    } else {
        callsite
    };
    let mut guard = HOUSEHOLD_CALLSITES.lock().unwrap();
    let sites = guard.get_or_insert_with(HashMap::new);
    *sites.entry(callsite.to_string()).or_insert(0) += 1;
}

/// Household call sites observed since process start, most-used first. Exposed separately so an
/// audit UI can render structured rows instead of parsing the human report.
pub fn household_callsite_stats() -> Vec<(String, u64)> {
    let guard = HOUSEHOLD_CALLSITES.lock().unwrap();
    let mut rows: Vec<(String, u64)> = guard
        .as_ref()
        .map(|sites| {
            sites
                .iter()
                .map(|(site, count)| (site.clone(), *count))
                .collect()
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    rows
}

/// Name the condition behind a failed private-lane call, from the error text. Every branch
/// used to collapse into "the local lane is unreachable"; E.MSG1 found a 5 s call timeout reported
/// that way — true for the caller, wrong for the operator, who then checks the network instead of
/// the knob. A timeout names the knob and the seconds it waited.
pub fn lane_failure(detail: &str) -> (String, &'static str) {
    let d = detail.to_ascii_lowercase();
    if d.contains("429") {
        (
            "the local lane is RATE LIMITING (429 after retries)".to_string(),
            "reduce YM_INFER_PERMITS below the endpoint's slot count, or add slots",
        )
    } else if ["502", "503", "504"].iter().any(|c| d.contains(c)) {
        // Distinct from unreachable: the gateway answered, its worker did not.
        (
            "the local lane is FLAKY (gateway error after retries)".to_string(),
            "the endpoint is up but its backend is failing intermittently — check the model host",
        )
    } else if let Some(i) = d.find("timed out after ") {
        // E.MSG2: the client that timed out stated its own wait and model — relay it, never
        // append a second number from a model-less knob.
        let sentence: String = detail[i..].chars().take_while(|c| *c != '\n').collect();
        (
            format!("the local lane TIMED OUT — {sentence}"),
            "raise YM_LLM_TIMEOUT_MODELS for that model (or YM_LLM_TIMEOUT_S), lower the thinking level, or pick a faster model",
        )
    } else if d.contains("timeout") || d.contains("timed out") {
        (
            format!(
                "the local lane TIMED OUT (no answer within {} s — YM_LLM_TIMEOUT_S)",
                yantrik_ml::call_timeout::call_timeout().as_secs()
            ),
            "raise YM_LLM_TIMEOUT_S, lower the thinking level, or pick a faster model",
        )
    } else {
        (
            "the local lane is unreachable".to_string(),
            "check YM_LOCAL_OLLAMA_URL and the endpoint",
        )
    }
}

pub fn privacy_report(provider: &str) -> String {
    use std::sync::atomic::Ordering;
    let household =
        std::env::var("YM_HOUSEHOLD_PROVIDERS").unwrap_or_else(|_| DEFAULT_HOUSEHOLD.to_string());
    let private = std::env::var("YM_PRIVATE_PROVIDERS").unwrap_or_default();
    let household_sites = household_callsite_stats();
    let household_sites = if household_sites.is_empty() {
        "(none)".to_string()
    } else {
        household_sites
            .into_iter()
            .map(|(site, count)| format!("{site} {count}"))
            .collect::<Vec<_>>()
            .join(" · ")
    };
    format!(
        "PRIVACY LANES (charter wall — every LLM call declares a scope)\n\
         provider: {provider}\n\
         household allowlist (YM_HOUSEHOLD_PROVIDERS): {household}\n\
         private allowlist (YM_PRIVATE_PROVIDERS): {}\n\
         dispatched (exposure — a scope-authorized call was sent; NOT a usable answer) — private {} · household {} · public {}\n\
         refused — private {} · household {} · public {}\n\
         household dispatch sites — {}\n\
         private-grounded turns ESCALATED to cloud: {}  ← should be 0; a non-zero count means private context reached a cloud provider\n\
         Configure YM_PRIVATE_PROVIDERS with an owned/on-device provider to keep private-grounded turns home (escalations auto-drop to 0).",
        if private.is_empty() { "(none — private lane HARD-REFUSES; deterministic fallback only)" } else { private.as_str() },
        PRIVACY_SERVED[0].load(Ordering::Relaxed),
        PRIVACY_SERVED[1].load(Ordering::Relaxed),
        PRIVACY_SERVED[2].load(Ordering::Relaxed),
        PRIVACY_REFUSED[0].load(Ordering::Relaxed),
        PRIVACY_REFUSED[1].load(Ordering::Relaxed),
        PRIVACY_REFUSED[2].load(Ordering::Relaxed),
        household_sites,
        PRIVACY_ESCALATED.load(Ordering::Relaxed),
    )
}

/// `true` while all cloud providers are failing and the mind has fallen back to its local tier.
pub fn in_survival_mode() -> bool {
    SURVIVAL_MODE.load(std::sync::atomic::Ordering::Relaxed)
}

/// Degradation notice for the daily briefing: empty string when healthy, plaintext summary when in
/// survival mode (all cloud providers down, running on local inference only). Check this in proactive
/// schedulers and skip non-essential work when it is non-empty.
pub fn survival_status() -> String {
    if !SURVIVAL_MODE.load(std::sync::atomic::Ordering::Relaxed) {
        return String::new();
    }
    let mins = {
        let g = SURVIVAL_SINCE.lock().unwrap();
        g.as_ref().map_or(0, |t| t.elapsed().as_secs() / 60)
    };
    format!(
        "SURVIVAL MODE active ({mins}m): all cloud providers unavailable — running on local inference only. \
         Chat is answering via the local tier. Memory writes and notifications remain active. \
         Proactive briefings are paused until a cloud provider recovers."
    )
}

/// Default household allowlist = the providers the engine ships with today, so the wall's arrival
/// changes nothing until the owner edits the env. "scripted" keeps the test seam green.
pub const DEFAULT_HOUSEHOLD: &str = "minimax,nanogpt,ollama-cloud,claude-cli,scripted,chain";

/// Bounded async wrapper over a synchronous `LLMBackend`.
#[derive(Clone)]
pub struct InferencePool {
    backend: Arc<dyn LLMBackend>,
    sem: Arc<Semaphore>,
    /// Which provider(s) back this pool — e.g. "nanogpt -> minimax", "scripted". Drives the lanes.
    provider: Arc<str>,
    /// The dedicated PRIVATE lane (ARCH: local-owned inference). When set, a `PrivacyScope::Private`
    /// call is served ONLY by this backend — which MUST be constructed local-only (no cloud links)
    /// so a private turn CANNOT reach a third party by construction (sol redteam 019f8287). If it
    /// fails, the request FAILS CLOSED — it is never re-sent to the cloud/household backend, because
    /// an outage must reduce capability, never confidentiality. Set only from an owned endpoint.
    private: Option<(Arc<dyn LLMBackend>, Arc<str>)>,
    /// L4-0: the bound spend ledger. Shared by every clone and every role pool derived from this
    /// one, so binding the engine's recorder once binds the whole pool family.
    ledger: Arc<std::sync::RwLock<Option<Arc<dyn InferenceLedger>>>>,
}

impl InferencePool {
    /// `max_concurrency` = 1 for a local single-model backend (the Mutex makes more pointless and
    /// just queues); higher for API backends.
    pub fn new(backend: Arc<dyn LLMBackend>, max_concurrency: usize) -> Self {
        Self {
            backend,
            sem: Arc::new(Semaphore::new(max_concurrency.max(1))),
            provider: Arc::from("scripted"),
            private: None,
            ledger: Arc::new(std::sync::RwLock::new(None)),
        }
    }

    /// L4-0: bind the spend ledger this pool (and every pool sharing its slot) records into.
    pub fn bind_ledger(&self, ledger: Arc<dyn InferenceLedger>) {
        *self.ledger.write().unwrap_or_else(|e| e.into_inner()) = Some(ledger);
    }
    /// L4-0: a derived pool records into the same ledger as the pool it was derived from.
    pub fn share_ledger_slot(mut self, from: &InferencePool) -> Self {
        self.ledger = from.ledger.clone();
        self
    }
    fn record_call(&self, call: InferenceCall) {
        if let Some(l) = self
            .ledger
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            l.record_call(call);
        }
    }
    /// The route label the gate would select for `scope` — the same choice `gate_scope` makes,
    /// read without counting, so a refusal row can name what was refused.
    fn route_label(&self, scope: PrivacyScope) -> String {
        match (scope, &self.private) {
            (PrivacyScope::Private, Some((_, lbl))) => lbl.to_string(),
            _ => self.provider.to_string(),
        }
    }

    /// Name the provider(s) backing this pool — the privacy lanes route on it.
    pub fn with_provider(mut self, label: &str) -> Self {
        self.provider = Arc::from(label);
        self
    }

    /// Attach the dedicated LOCAL-ONLY private lane. `backend` MUST be a local/owned endpoint with
    /// no cloud fallback (the caller — `build_backend` — guarantees this by building it from the
    /// local URL only). A `Private` call is then served here and FAILS CLOSED on failure.
    pub fn with_private_backend(mut self, backend: Arc<dyn LLMBackend>, label: &str) -> Self {
        self.private = Some((backend, Arc::from(label)));
        self
    }

    /// True when a dedicated local-owned private lane is configured (private turns stay home + fail
    /// closed instead of escalating to cloud).
    pub fn has_private_lane(&self) -> bool {
        self.private.is_some()
    }

    /// The private lane, so another pool can be given the SAME one.
    ///
    /// Role pools are built from a provider spec and start with no private lane at all. That is not
    /// a policy — it is an omission, and it is invisible: the startup banner reports the DEFAULT
    /// pool's lane as active while every role pool quietly has none.
    pub fn private_lane(&self) -> Option<(Arc<dyn LLMBackend>, Arc<str>)> {
        self.private.clone()
    }

    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Unscoped chat = HOUSEHOLD lane (today's behavior, now explicit, audited, and revocable via
    /// YM_HOUSEHOLD_PROVIDERS). New code should call `chat_scoped` and say what it's carrying.
    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
    ) -> anyhow::Result<LLMResponse> {
        self.chat_household_attributed(messages, config, "unattributed")
            .await
    }

    /// Household chat with a stable, code-authored call-site identity. Use this instead of
    /// [`chat`](Self::chat) for every deliberate Household call so `ym privacy` can name the
    /// producer without reconstructing it from timestamps. `callsite` is static by type: prompts,
    /// names and other user data cannot accidentally become audit labels.
    pub async fn chat_household_attributed(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
        callsite: &'static str,
    ) -> anyhow::Result<LLMResponse> {
        self.chat_scoped_tools_attributed(
            messages,
            config,
            PrivacyScope::Household,
            Vec::new(),
            callsite,
        )
        .await
    }

    /// Scope-aware chat: routes or REFUSES per the privacy lanes. A refusal is an error the caller
    /// must handle by deterministic fallback — never by silently downgrading the scope.
    pub async fn chat_scoped(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
        scope: PrivacyScope,
    ) -> anyhow::Result<LLMResponse> {
        self.chat_scoped_tools(messages, config, scope, Vec::new())
            .await
    }

    /// Scope-aware chat WITH native function-calling: `tools` is the OpenAI-format schema list
    /// forwarded to the backend (which adapts it to Anthropic/Ollama). A tool-capable backend
    /// returns structured `tool_calls`; a backend that ignores the param degrades to free-text (the
    /// caller keeps its text-JSON fallback). An empty list is identical to plain `chat_scoped`.
    pub async fn chat_scoped_tools(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
        scope: PrivacyScope,
        tools: Vec<serde_json::Value>,
    ) -> anyhow::Result<LLMResponse> {
        self.chat_scoped_tools_attributed(messages, config, scope, tools, "chat_scoped_tools")
            .await
    }

    async fn chat_scoped_tools_attributed(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
        scope: PrivacyScope,
        tools: Vec<serde_json::Value>,
        callsite: &'static str,
    ) -> anyhow::Result<LLMResponse> {
        // ONE SYSTEM MESSAGE. Every caller here builds its prompt as several system blocks —
        // persona, then agent instructions, with pack rules and a format note INSERTED at index 1 —
        // and some chat templates refuse that outright. Diagnosed 2026-08-15 through a logging
        // proxy, after the turn had failed for an hour behind the words "Ollama API request failed":
        //
        //   HTTP 500 — Jinja Exception: System message must be at the beginning.
        //
        // qwen3.8's template raises on any system message that is not the first; gemma's tolerates
        // them, which is the entire reason this looked like a model-specific network fault and
        // survived every black-box probe (curl reproduced none of it, because curl sent one system
        // message). Merging is semantically free — the blocks are all system-level context in
        // order — and it makes the mind portable across templates instead of only working on the
        // ones that happen to be lenient.
        let messages = merge_system_messages(messages);
        // L4-0: one terminal row per logical request, whatever happens next.
        let call_t0 = std::time::Instant::now();
        let opportunity = current_opportunity();
        let (backend, selected_label) = match self.gate_scope(scope, callsite) {
            Ok(x) => x,
            Err(e) => {
                self.record_call(InferenceCall {
                    callsite,
                    scope,
                    route: self.route_label(scope),
                    served_by: None,
                    outcome: CallOutcome::Refused,
                    attempts: 0,
                    latency_ms: call_t0.elapsed().as_millis() as u64,
                    streaming: false,
                    opportunity,
                    tokens: None,
                });
                return Err(e);
            }
        };
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore never closed");
        let scope_for_lane = scope;
        let provider_for_lane = selected_label;
        // L4-0: the attempt counter outlives the blocking thread — a panic there still yields
        // a Failed row with the attempts made.
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_in = attempts.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let _permit = permit; // released when the blocking work finishes
            // E.OBS1c: clear any stale note left on this pooled blocking thread, so the label we
            // read afterwards can only have been written by THIS call's chain traversal.
            let _ = take_serving_link();
            install_attempts(attempts_in);
            let leaf = !is_chain_backend(backend.as_ref());
            let tools_ref = if tools.is_empty() { None } else { Some(tools.as_slice()) };
            let outcome: anyhow::Result<LLMResponse> = (|| {
            // BACKPRESSURE IS NOT AN OUTAGE.
            //
            // The endpoint answers 429 when more calls arrive than it has slots. That is the server
            // saying "wait", and it was being treated as a dead lane: for a PRIVATE turn the policy
            // is fail-closed, so one burst of load made the mind unable to think at all, and the log
            // said "local lane down" about an endpoint that answered 200 and completed a prompt
            // seconds later. Found live on 2026-08-25 with permits set to 6 against a 4-slot
            // cluster, so a burst did not merely risk this — it guaranteed it.
            //
            // Short bounded backoff: the wait is what the server asked for, and a private turn has
            // nowhere else to go. Anything that is not a rate limit fails immediately, because
            // retrying a real error just delays the truth.
            let mut wait_ms = 400;
            for attempt in 0..3 {
                // L4-0: a leaf backend's invocation IS the attempt, counted before the call so
                // a panic inside it is still one attempt; a chain counts its links itself.
                if leaf {
                    note_attempt();
                }
                match backend.chat(&messages, &config, tools_ref) {
                    Ok(r) => return Ok(r),
                    Err(e) => {
                        // TRANSIENT means the server is temporarily unable, not that the request is
                        // wrong. 429 is "wait"; 502/503/504 are a gateway or a worker hiccuping.
                        // Measured on the box: three identical completions in a row returned 200,
                        // 200, and then 502 {"error":"backend desktop error"} — the endpoint is
                        // flaky, not down. Retrying only the 429 left every such blip fatal, and
                        // because a private turn fails CLOSED by design it had nowhere to fall back
                        // to: one hiccup and the mind could not think.
                        let detail = format!("{e:#}");
                        let transient = ["429", "502", "503", "504"].iter().any(|c| detail.contains(c));
                        if !transient || attempt == 2 {
                            return Err(e);
                        }
                        eprintln!("[infer] transient model-endpoint error — backing off {wait_ms}ms (attempt {}): {detail}", attempt + 1);
                        std::thread::sleep(std::time::Duration::from_millis(wait_ms));
                        wait_ms *= 3;
                    }
                }
            }
            unreachable!("the loop returns on every path")
            })();
            // Captured on the SAME blocking thread the chain ran on — the only place the note is
            // visible, and the reason a task-local could not carry it.
            let served = take_serving_link();
            (outcome, served)
        })
        .await;
        let attempts = attempts.load(std::sync::atomic::Ordering::SeqCst);
        let latency_ms = call_t0.elapsed().as_millis() as u64;
        // L4-0: a panic on the blocking thread is a failed request, not a missing row.
        let (outcome, served) = match joined {
            Ok(x) => x,
            Err(join) => {
                self.record_call(InferenceCall {
                    callsite,
                    scope,
                    route: provider_for_lane.clone(),
                    served_by: None,
                    outcome: CallOutcome::Failed,
                    attempts,
                    latency_ms,
                    streaming: false,
                    opportunity,
                    tokens: None,
                });
                return Err(anyhow::anyhow!("inference task failed: {join}"));
            }
        };
        // L4-0: the terminal row. `served_by` follows E.OBS1c's rule below — a route is not a
        // server — and is absent on a failure.
        let served_by_label = served.clone().or_else(|| {
            (!provider_for_lane.starts_with("chain[")).then(|| provider_for_lane.clone())
        });
        let response = match outcome {
            Ok(r) => {
                self.record_call(InferenceCall {
                    callsite,
                    scope,
                    route: provider_for_lane.clone(),
                    served_by: served_by_label,
                    outcome: CallOutcome::Served,
                    attempts,
                    latency_ms,
                    streaming: false,
                    opportunity,
                    tokens: None,
                });
                // E.LOOP-J: what this generation actually managed, for the next one's budget.
                note_generation_rate(
                    &provider_for_lane,
                    r.completion_tokens as u64,
                    latency_ms,
                );
                r
            }
            Err(e) => {
                self.record_call(InferenceCall {
                    callsite,
                    scope,
                    route: provider_for_lane.clone(),
                    served_by: None,
                    outcome: CallOutcome::Failed,
                    attempts,
                    latency_ms,
                    streaming: false,
                    opportunity,
                    tokens: None,
                });
                return Err(e);
            }
        };
        // E.OBS1c: "served by" is a POST-SUCCESS fact. A chain notes the link that answered; a
        // single-provider backend never notes, and its configured label IS the server — but a
        // chain label ("chain[a -> b]") that somehow arrives un-noted must NOT be shown as if a
        // route were a server, so it emits nothing rather than a plausible lie.
        if let Some(observe) = LANE_OBSERVER.get() {
            let label = served.or_else(|| {
                (!provider_for_lane.starts_with("chain[")).then(|| provider_for_lane.clone())
            });
            if let Some(label) = label {
                observe(scope_for_lane.as_str(), &label);
            }
        }
        Ok(response)
    }

    /// The privacy gate, shared by the plain and STREAMING call paths so they can never drift:
    /// resolves which backend may serve this scope, refuses what the allowlists refuse, and records
    /// the authorized-DISPATCH (exposure) and refusal counts BEFORE the call — the answer-serving
    /// fact is emitted separately, post-success, by the pool wrapper.
    ///
    /// A PRIVATE call, when a dedicated local-owned lane exists, is served ONLY by that local-only
    /// backend — cloud is unreachable for it by construction (sol 019f8287: enforce at dispatch).
    /// Everything else (and Private when no local lane is configured) routes on the default backend.
    /// The explicit local-only lane is SANCTIONED BY CONSTRUCTION (built from the owned endpoint),
    /// which is stronger evidence than the env CSV ("a declaration, not evidence" — sol #5), so it
    /// bypasses the CSV allowlist; the CSV still gates the label-based (non-explicit) paths.
    fn gate_scope(
        &self,
        scope: PrivacyScope,
        callsite: &'static str,
    ) -> anyhow::Result<(Arc<dyn LLMBackend>, String)> {
        use std::sync::atomic::Ordering;
        let household = std::env::var("YM_HOUSEHOLD_PROVIDERS")
            .unwrap_or_else(|_| DEFAULT_HOUSEHOLD.to_string());
        let private = std::env::var("YM_PRIVATE_PROVIDERS").unwrap_or_default();
        let (backend, label, sanctioned) = match (scope, &self.private) {
            (PrivacyScope::Private, Some((be, lbl))) => (be.clone(), lbl.clone(), true),
            _ => (self.backend.clone(), self.provider.clone(), false),
        };
        if !sanctioned && !scope_allows(scope, &label, &household, &private) {
            PRIVACY_REFUSED[scope_idx(scope)].fetch_add(1, Ordering::Relaxed);
            eprintln!(
                "[privacy] REFUSED {} -> provider '{}' not in the {} allowlist",
                scope.as_str(),
                label,
                scope.as_str()
            );
            anyhow::bail!(
                "privacy: {}-scope request refused — provider '{}' is not allowlisted for this lane; use deterministic rendering (scaffold/fill) instead",
                scope.as_str(),
                label
            );
        }
        // Household is the ambiguous lane: Private is expected and Public is an explicit
        // declaration, but a Household count alone cannot distinguish an allowlisted public-facts
        // caller from a regression. Record the code-authored identity at the dispatch boundary,
        // where the lane decision becomes real. Journal timestamps remain useful chronology; this
        // tag supplies the missing attribution without relying on correlation.
        if matches!(scope, PrivacyScope::Household) {
            record_household_callsite(callsite);
            eprintln!("[privacy] household lane dispatch attempted via '{label}' at '{callsite}'");
        }
        // EXPOSURE, not service (E.OBS1c, Codex's split): this pre-dispatch count is the
        // conservative privacy record — a failed cloud request may still have TRANSMITTED the
        // prompt, so it must be counted here regardless of outcome. The UI's "served by" is a
        // different fact and is emitted post-success from the link that actually answered.
        PRIVACY_SERVED[scope_idx(scope)].fetch_add(1, Ordering::Relaxed);
        // The SELECTED label rides out with the backend (E.OBS1c review): a Private call served by
        // the private lane must fall back to the PRIVATE label, never to self.provider — which on
        // that path is the household backend's name and would be a privacy-misleading badge.
        Ok((backend, label.to_string()))
    }

    /// Household-scope chat that STREAMS tokens into `sink` as the model generates them, returning
    /// the same complete response the plain call would. Tool-less by design: several providers'
    /// streaming paths do not carry native tool_calls reliably, and the calls worth watching live —
    /// compose, synthesis — never use tools. The sink is unbounded because a slow UI must never
    /// backpressure a model turn; a dropped receiver makes sends into harmless no-ops.
    pub async fn chat_streaming_sink(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
        sink: tokio::sync::mpsc::UnboundedSender<String>,
        scope: PrivacyScope,
    ) -> anyhow::Result<LLMResponse> {
        // SCOPE IS A PARAMETER (E.SEC14). It was hardcoded `Household`, which meant the streaming
        // compose declared the same lane for a turn grounded in family memory as for one about the
        // weather. A lane belongs to the MATERIAL, not to the transport that happens to carry it.
        let messages = merge_system_messages(messages);
        // L4-0: the streaming path writes the same single terminal row as the plain path.
        let callsite: &'static str = "chat_streaming_sink";
        let call_t0 = std::time::Instant::now();
        let opportunity = current_opportunity();
        let (backend, selected_label) = match self.gate_scope(scope, callsite) {
            Ok(x) => x,
            Err(e) => {
                self.record_call(InferenceCall {
                    callsite,
                    scope,
                    route: self.route_label(scope),
                    served_by: None,
                    outcome: CallOutcome::Refused,
                    attempts: 0,
                    latency_ms: call_t0.elapsed().as_millis() as u64,
                    streaming: true,
                    opportunity,
                    tokens: None,
                });
                return Err(e);
            }
        };
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore never closed");
        let scope_for_lane = scope;
        let provider_for_lane = selected_label;
        let attempts = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let attempts_in = attempts.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let _ = take_serving_link();
            install_attempts(attempts_in);
            if !is_chain_backend(backend.as_ref()) {
                note_attempt();
            }
            let out = backend.chat_streaming(&messages, &config, None, &mut |tok| {
                let _ = sink.send(tok.to_string());
            });
            (out, take_serving_link())
        })
        .await;
        let attempts = attempts.load(std::sync::atomic::Ordering::SeqCst);
        let latency_ms = call_t0.elapsed().as_millis() as u64;
        let (outcome, served) = match joined {
            Ok(x) => x,
            Err(join) => {
                self.record_call(InferenceCall {
                    callsite,
                    scope,
                    route: provider_for_lane.clone(),
                    served_by: None,
                    outcome: CallOutcome::Failed,
                    attempts,
                    latency_ms,
                    streaming: true,
                    opportunity,
                    tokens: None,
                });
                return Err(anyhow::anyhow!("inference task failed: {join}"));
            }
        };
        let served_by_label = served.clone().or_else(|| {
            (!provider_for_lane.starts_with("chain[")).then(|| provider_for_lane.clone())
        });
        let response = match outcome {
            Ok(r) => {
                self.record_call(InferenceCall {
                    callsite,
                    scope,
                    route: provider_for_lane.clone(),
                    served_by: served_by_label,
                    outcome: CallOutcome::Served,
                    attempts,
                    latency_ms,
                    streaming: true,
                    opportunity,
                    tokens: None,
                });
                // E.LOOP-J: what this generation actually managed, for the next one's budget.
                note_generation_rate(
                    &provider_for_lane,
                    r.completion_tokens as u64,
                    latency_ms,
                );
                r
            }
            Err(e) => {
                self.record_call(InferenceCall {
                    callsite,
                    scope,
                    route: provider_for_lane.clone(),
                    served_by: None,
                    outcome: CallOutcome::Failed,
                    attempts,
                    latency_ms,
                    streaming: true,
                    opportunity,
                    tokens: None,
                });
                return Err(e);
            }
        };
        // Same post-success rule as the plain path (E.OBS1c): a route is not a server.
        if let Some(observe) = LANE_OBSERVER.get() {
            let label = served.or_else(|| {
                (!provider_for_lane.starts_with("chain[")).then(|| provider_for_lane.clone())
            });
            if let Some(label) = label {
                observe(scope_for_lane.as_str(), &label);
            }
        }
        Ok(response)
    }

    /// PRIVATE-GROUNDED inference (Constitutional-Kernel first rung, tier-agnostic): a turn that
    /// carries private personal context must PREFER the private lane (owned hardware / on-device).
    /// If a private provider is configured and serves it → the data stays home. If none is (the
    /// current default), the call ESCALATES to the household lane so the turn still works, but the
    /// escalation is COUNTED and logged — the privacy gap becomes visible and auto-closes the moment
    /// YM_PRIVATE_PROVIDERS names a local/on-device provider. Never breaks the turn.
    pub async fn chat_grounded(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
    ) -> anyhow::Result<LLMResponse> {
        self.chat_grounded_tools(messages, config, Vec::new()).await
    }

    /// Private-grounded chat WITH native function-calling — same private-lane-first / audited
    /// escalation policy as [`Self::chat_grounded`], but forwards the tool schema list so a tool-capable
    /// backend returns structured `tool_calls`. This is the agent loop's inference entry point.
    pub async fn chat_grounded_tools(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
        tools: Vec<serde_json::Value>,
    ) -> anyhow::Result<LLMResponse> {
        match self
            .chat_scoped_tools_attributed(
                messages.clone(),
                config.clone(),
                PrivacyScope::Private,
                tools.clone(),
                "private-grounded",
            )
            .await
        {
            Ok(r) => Ok(r), // served locally — private context stayed home
            Err(e) => {
                // FAIL CLOSED when a dedicated local private lane EXISTS but its backend failed
                // (outage / OOM / timeout): do NOT re-send the private prompt to a cloud/household
                // backend — an outage must reduce capability, never confidentiality (sol 019f8287).
                // The turn's caller degrades to deterministic rendering / an honest "unavailable".
                if self.private.is_some() {
                    PRIVACY_REFUSED[scope_idx(PrivacyScope::Private)]
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    // `{e:#}`, not `{e}`: this is an anyhow chain, and the OUTERMOST link is the
                    // generic context string ("Ollama API request failed") while the CAUSE — the
                    // transport error, the HTTP status, the TLS complaint — sits underneath it.
                    // Printing only the outer link cost hours on 2026-08-15: the log said the same
                    // seven words whether the endpoint was unreachable, the model name was wrong,
                    // or the body was rejected, so every diagnosis had to be done by black-box
                    // probing from outside. A fail-closed path is exactly where the reason must
                    // survive, because it is the path that ends the turn.
                    // Name the condition. "Local lane down" was printed about an endpoint that
                    // answered 200 and completed a prompt seconds later; the actual cause was a 429
                    // after the retries were exhausted, which is a load problem with a different
                    // fix (fewer permits, more slots) than an outage.
                    let detail = format!("{e:#}");
                    let (why, hint) = lane_failure(&detail);
                    eprintln!("[privacy] private lane FAILED — failing CLOSED (refusing cloud escalation of private context): {why}: {detail}");
                    return Err(anyhow::anyhow!(
                        "private inference unavailable — {why}; refusing to route private context to a cloud provider. Fix: {hint}"
                    ));
                }
                // No local private lane configured (the documented interim gap): escalate to the
                // household lane so the turn still works, but COUNT + log it — the gap is visible and
                // auto-closes the moment YM_LOCAL_OLLAMA_URL / YM_PRIVATE_PROVIDERS names a local lane.
                PRIVACY_ESCALATED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    // Say which condition actually failed. This line used to name YM_LOCAL_OLLAMA_URL
                    // unconditionally — including when that variable WAS set and the real gap was an
                    // empty YM_PRIVATE_PROVIDERS, or a role pool that never inherited a lane. A
                    // colleague spent hours on it and wrote the refusal up as a policy decision;
                    // a component that misreports why it failed sends everyone downstream to the
                    // wrong place.
                    "[privacy] private-grounded turn ESCALATED to household lane (provider '{}') — this pool has NO private lane. Local URL set: {}. Allowlist (YM_PRIVATE_PROVIDERS): {}. If both look right, this is a ROLE pool (YM_ROLE_*) that did not inherit the default's lane.",
                    self.provider,
                    if std::env::var("YM_LOCAL_OLLAMA_URL").map(|v| !v.trim().is_empty()).unwrap_or(false) { "yes" } else { "NO" },
                    match std::env::var("YM_PRIVATE_PROVIDERS") {
                        Ok(v) if !v.trim().is_empty() => v,
                        _ => "EMPTY".to_string(),
                    }
                );
                self.chat_scoped_tools_attributed(
                    messages,
                    config,
                    PrivacyScope::Household,
                    tools,
                    "private-grounded escalation",
                )
                .await
            }
        }
    }

    pub fn available_permits(&self) -> usize {
        self.sem.available_permits()
    }
}

/// A deterministic `LLMBackend` for tests across the whole system: it returns a canned reply and
/// records the last system prompt it saw, so orchestration (prompt grounding, routing) can be
/// asserted with zero real model. This is the injectable seam BUILD.md calls for.
pub struct ScriptedLLM {
    reply: String,
    /// What the backend claims about HOW the generation ended. Scriptable because "the API said it
    /// hit the token limit" is now a load-bearing fact — it is the only thing that can tell a
    /// truncated file from a model that ended without a trailing newline — and a stub that can
    /// only ever say "stop" cannot exercise the branch that acts on it.
    stop_reason: String,
    last_system: std::sync::Mutex<String>,
    last_user: std::sync::Mutex<String>,
    last_all: std::sync::Mutex<String>,
}

impl ScriptedLLM {
    pub fn new(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
            stop_reason: "stop".into(),
            last_system: std::sync::Mutex::new(String::new()),
            last_user: std::sync::Mutex::new(String::new()),
            last_all: std::sync::Mutex::new(String::new()),
        }
    }
    /// Script the stop reason: `"stop"`, `"length"`, `"eos"` or `"tool_calls"`.
    pub fn with_stop_reason(mut self, reason: impl Into<String>) -> Self {
        self.stop_reason = reason.into();
        self
    }

    /// The concatenated system-role content from the most recent call.
    pub fn last_system_prompt(&self) -> String {
        self.last_system.lock().unwrap().clone()
    }
    /// The most recent user-role content.
    pub fn last_user_prompt(&self) -> String {
        self.last_user.lock().unwrap().clone()
    }
    /// Everything the model saw last (all roles, "role: content" per line) — for grading what
    /// actually reached the model regardless of which role carried it.
    pub fn last_prompt(&self) -> String {
        self.last_all.lock().unwrap().clone()
    }
}

impl LLMBackend for ScriptedLLM {
    fn chat(
        &self,
        messages: &[ChatMessage],
        _config: &GenerationConfig,
        _tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        let sys = messages
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let usr = messages
            .iter()
            .filter(|m| m.role == "user")
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let all = messages
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");
        *self.last_system.lock().unwrap() = sys;
        *self.last_user.lock().unwrap() = usr;
        *self.last_all.lock().unwrap() = all;
        Ok(LLMResponse {
            thinking: String::new(),
            text: self.reply.clone(),
            prompt_tokens: 0,
            completion_tokens: 0,
            tool_calls: vec![],
            api_tool_calls: vec![],
            stop_reason: self.stop_reason.clone(),
        })
    }
    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
        _on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LLMResponse> {
        self.chat(messages, config, tools)
    }
    fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
        Ok(text.len() / 4)
    }
    fn backend_name(&self) -> &str {
        "scripted"
    }
}

/// A deterministic backend that returns a SCRIPTED SEQUENCE of replies, one per call, for exercising
/// MULTI-STEP control flow (the agentic loop) with no real model. Call 0 returns `replies[0]`, call 1
/// `replies[1]`, …; once exhausted it repeats the LAST reply (so a loop that keeps calling gets a
/// stable terminal response). Records every prompt it saw (per call) so an eval can grade what the
/// loop fed the model on each step. This is the enabling primitive for a deterministic agent-loop eval.
pub struct SequencedLLM {
    replies: Vec<String>,
    /// Optional NATIVE tool call scripted for each call — `native[i]`, when `Some`, is returned in
    /// `LLMResponse.tool_calls` (structured function-calling path) instead of relying on the text
    /// carrying a free-text JSON blob. Empty/short vec ⇒ no native call for that step. This is what
    /// lets a scenario exercise the native function-calling loop with no real model.
    native: Vec<Option<ToolCall>>,
    calls: std::sync::atomic::AtomicUsize,
    prompts: std::sync::Mutex<Vec<String>>,
    /// The `tools` param (the OpenAI-format schema list) seen on each call — so an eval can assert
    /// the loop actually PASSED tool schemas to the backend (the native-calling migration property).
    tools_seen: std::sync::Mutex<Vec<Vec<serde_json::Value>>>,
}

impl SequencedLLM {
    pub fn new(replies: Vec<impl Into<String>>) -> Self {
        Self {
            replies: replies.into_iter().map(Into::into).collect(),
            native: Vec::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            prompts: std::sync::Mutex::new(Vec::new()),
            tools_seen: std::sync::Mutex::new(Vec::new()),
        }
    }
    /// Script a NATIVE tool call for each step (parallel to `replies`): `Some((name, args))` makes
    /// call `i` return that structured tool call; `None` leaves it a text-only reply. Extra text in
    /// `replies[i]` still rides along (mirrors a model that emits both content and a tool call).
    pub fn with_native(mut self, native: Vec<Option<(&str, serde_json::Value)>>) -> Self {
        self.native = native
            .into_iter()
            .map(|o| {
                o.map(|(name, arguments)| ToolCall {
                    name: name.to_string(),
                    arguments,
                })
            })
            .collect();
        self
    }
    /// How many times the model was called (loop steps + compose).
    pub fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }
    /// Every prompt (all roles, "role: content") the model saw, in call order.
    pub fn prompts(&self) -> Vec<String> {
        self.prompts.lock().unwrap().clone()
    }
    /// The prompt seen on call `i` (0-based), or empty.
    pub fn prompt_at(&self, i: usize) -> String {
        self.prompts
            .lock()
            .unwrap()
            .get(i)
            .cloned()
            .unwrap_or_default()
    }
    /// The tool schemas passed on call `i` (0-based), or empty if none/out of range.
    pub fn tools_at(&self, i: usize) -> Vec<serde_json::Value> {
        self.tools_seen
            .lock()
            .unwrap()
            .get(i)
            .cloned()
            .unwrap_or_default()
    }
}

impl LLMBackend for SequencedLLM {
    fn chat(
        &self,
        messages: &[ChatMessage],
        _config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        let all = messages
            .iter()
            .map(|m| format!("{}: {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");
        self.prompts.lock().unwrap().push(all);
        self.tools_seen
            .lock()
            .unwrap()
            .push(tools.map(|t| t.to_vec()).unwrap_or_default());
        let i = self
            .calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let reply = self
            .replies
            .get(i)
            .or_else(|| self.replies.last())
            .cloned()
            .unwrap_or_default();
        let tool_calls = self
            .native
            .get(i)
            .cloned()
            .flatten()
            .into_iter()
            .collect::<Vec<_>>();
        let stop_reason = if tool_calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        }
        .to_string();
        Ok(LLMResponse {
            thinking: String::new(),
            text: reply,
            prompt_tokens: 0,
            completion_tokens: 0,
            tool_calls,
            api_tool_calls: vec![],
            stop_reason,
        })
    }
    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
        _on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LLMResponse> {
        self.chat(messages, config, tools)
    }
    fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
        Ok(text.len() / 4)
    }
    fn backend_name(&self) -> &str {
        "sequenced"
    }
}

/// A resilience chain over several `LLMBackend`s: try each in order; the first that returns a
/// non-empty success wins. An error OR an empty reply (some reasoning models emit nothing under a
/// tight token budget) falls over to the next link. For an always-on companion this means it keeps
/// answering when the primary provider rate-limits, errors, or returns nothing — the "many LLM
/// supports, just make them click" property. Links are built from whatever provider keys are present
/// (NanoGPT, Ollama Cloud, MiniMax, …), all OpenAI-compatible, so adding a provider is config-only.
/// How a ChainBackend picks which link to try FIRST each call. Every strategy still failover-
/// iterates the remaining links on error/empty, so a backup is always in play — the strategy only
/// sets the starting point / load distribution. Config: YM_BRAIN_STRATEGY (+ per-link weights).
#[derive(Clone, Debug)]
pub enum ChainStrategy {
    /// Fixed order: `links[0]` is primary, the rest are pure failover backups. (Default.)
    Failover,
    /// Rotate the starting link each call (even spread), then failover through the remainder.
    RoundRobin,
    /// Pick the starting link by weight share (e.g. 70/30), then failover. Weights map to link order.
    Weighted(Vec<u32>),
}

/// Deterministic weighted selection: cycles the counter through the summed weight window so that over
/// many calls each index gets its share (weights [70,30] → 70 of every 100 calls start at link 0).
/// Reproducible (no RNG) — important because scripts/tests must not depend on wall-clock randomness.
fn weighted_index(weights: &[u32], n: usize, counter: usize) -> usize {
    let total: u32 = weights.iter().take(n).sum();
    if total == 0 {
        return 0;
    }
    let pos = (counter as u32) % total;
    let mut acc = 0u32;
    for i in 0..n {
        acc += weights.get(i).copied().unwrap_or(0);
        if pos < acc {
            return i;
        }
    }
    0
}

/// The smallest reply budget worth giving a model running on OWNED HARDWARE.
///
/// Measured on the local pool, 2026-08-14, against `qwen3.6:35b-a3b-mtp-q4_K_M`:
///
/// ```text
/// num_predict=1     total=28.1s  load=0.30  prompt=0.12  eval=0.00
/// num_predict=20    total=15.0s  load=0.28  prompt=0.03  eval=0.06
/// num_predict=100   total=14.8s  load=0.29  prompt=0.03  eval=0.06
/// ```
///
/// Asking for a hundred tokens costs the SAME as asking for one. Load, prompt eval and generation
/// together account for under a second of it; the rest is a fixed per-call cost. On a local GPU the
/// bill is per CALL, not per token — so a tight cap buys nothing at all, while a truncated reply
/// costs the whole turn. We have already paid for that truncation twice: cut off mid-JSON it is an
/// unparseable tool call ("Sorry — I had trouble putting that together"), and cut off mid-`<think>`
/// it is an empty answer, because the reasoning consumed the budget the answer needed.
///
/// Cloud links are deliberately NOT touched — there, tokens are the invoice.
fn local_min_tokens() -> usize {
    std::env::var("YM_LOCAL_MIN_TOKENS")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(8192)
}

/// The tokens-per-second a generation is assumed to sustain, for sizing a budget against a clock.
///
/// Measured 2026-09-03 across three NIM models on real build legs: 15.5, 16.2 and 21.6 tok/s on
/// successful calls. The FLOOR is used, not the mean — sizing to an average guarantees the slow
/// half is cut, and being early costs a little headroom while being late costs the whole call.
const ASSUMED_TOKENS_PER_SECOND: usize = 15;

// ── E.LOOP-J: the OBSERVED generation rate ────────────────────────────────────────────────────
//
// The constant above is a guess frozen from three legs. Every served generation already knows the
// truth — `LLMResponse` carries `completion_tokens` and the pool times the call — so the budget
// can be sized from what this machine actually sees instead.
//
// Everything here is biased SLOW on purpose. Over-estimating the rate produces a budget too large
// to finish inside the provider's deadline, and the generation is cut: that is precisely the
// defect that cost reading 7, and it is silent at the point where it happens. Under-estimating
// only produces a smaller file set. So the window's FLOOR is used, never its mean — the same rule
// that turned 15.5 / 16.2 / 21.6 tok/s into 15 — and the tightest route is used rather than the
// matching one, because the budget is a safety limit and not a per-route optimisation.

/// Samples needed before an observation displaces the assumed constant. One fast call is noise.
const MIN_RATE_SAMPLES: usize = 5;

/// Generations smaller than this are not throughput measurements: at a few dozen tokens the round
/// trip and queueing dominate, so the implied rate says more about the network than the model.
const MIN_SAMPLE_TOKENS: u64 = 200;

/// Most recent samples kept per route. Enough to smooth a slow minute; short enough that a
/// provider that has genuinely slowed down is reflected quickly.
const RATE_WINDOW: usize = 16;

/// The band any observed rate is clamped into, so no single strange sample can size a budget.
const MIN_OBSERVED_RATE: usize = 3;
const MAX_OBSERVED_RATE: usize = 200;

static OBSERVED_RATES: std::sync::OnceLock<
    std::sync::Mutex<HashMap<String, std::collections::VecDeque<usize>>>,
> = std::sync::OnceLock::new();

fn observed_rates() -> &'static std::sync::Mutex<HashMap<String, std::collections::VecDeque<usize>>>
{
    OBSERVED_RATES.get_or_init(|| std::sync::Mutex::new(HashMap::new()))
}

/// Record one served generation's observed rate.
///
/// A sample is ADMITTED only when the provider actually reported a count and the generation was
/// big enough to measure. `completion_tokens` falls back to zero whenever a backend exposes no
/// usage object, so "not reported" and "zero tokens" arrive identical — and against a floor-based
/// estimate a single zero would read as infinitely slow and crush every later budget to the
/// minimum. Refusing the sample is the whole guard; there is nothing downstream to catch it.
pub fn note_generation_rate(route: &str, completion_tokens: u64, latency_ms: u64) {
    if completion_tokens < MIN_SAMPLE_TOKENS || latency_ms == 0 {
        return;
    }
    let rate = (completion_tokens * 1_000 / latency_ms) as usize;
    if rate == 0 {
        return;
    }
    if let Ok(mut m) = observed_rates().lock() {
        let w = m.entry(route.to_string()).or_default();
        w.push_back(rate.clamp(MIN_OBSERVED_RATE, MAX_OBSERVED_RATE));
        while w.len() > RATE_WINDOW {
            w.pop_front();
        }
    }
}

/// The slowest rate recently observed on any route, once enough samples exist to mean anything.
///
/// Deliberately not per-route: this sizes a safety limit, and the honest basis for a limit is the
/// slowest thing actually seen, not the one that happens to match a label.
pub fn observed_tokens_per_second() -> Option<usize> {
    let m = observed_rates().lock().ok()?;
    let mut best: Option<usize> = None;
    for w in m.values() {
        if w.len() < MIN_RATE_SAMPLES {
            continue;
        }
        if let Some(floor) = w.iter().copied().min() {
            best = Some(best.map_or(floor, |b: usize| b.min(floor)));
        }
    }
    best
}

/// Test-only: forget every observation, so one case cannot size another's budget.
#[doc(hidden)]
pub fn reset_observed_rates_for_test() {
    if let Ok(mut m) = observed_rates().lock() {
        m.clear();
    }
}

/// Fraction of the deadline a single generation may plan to occupy. The rest absorbs the model
/// being slower than the floor, queueing, and the round trip.
const DEADLINE_BUDGET_NUMERATOR: usize = 7;
const DEADLINE_BUDGET_DENOMINATOR: usize = 10;

/// Never propose less than this. Below it a file set cannot be authored at all, and failing loudly
/// on a deadline too short to work with beats emitting something guaranteed to be a fragment.
const MIN_AUTHORING_TOKENS: usize = 1_500;

/// A provider's declared per-request deadline in seconds, or `None` when none is declared.
///
/// `YM_PROVIDER_DEADLINE_S` is the TIGHTEST limit any configured provider imposes. It is deliberately
/// one value rather than one per provider: the budget is chosen when a recipe is built, before the
/// router has picked which link will serve the call, so a per-provider number could not be resolved
/// at the point it is needed. One conservative number is honest; a per-provider number resolved
/// against the wrong provider would not be.
pub fn provider_deadline_seconds() -> Option<usize> {
    std::env::var("YM_PROVIDER_DEADLINE_S")
        .ok()
        .and_then(|v| v.trim().parse::<usize>().ok())
        .filter(|n| *n > 0)
}

/// Clamp an authoring budget to what the provider's deadline can actually deliver.
///
/// `build_recipe` asked for 16,000 tokens in one generation. That constant was chosen when the only
/// model in use finished it in well under a minute. On 2026-09-03 NVIDIA NIM was measured cutting a
/// request at **~302 s** — four 504s across two different models landing in 302,155–302,180 ms, a
/// spread of 25 milliseconds — and at 15–22 tok/s, 16,000 tokens is 740–1,032 s. Every build that
/// used its budget was guaranteed to be cut, and the Mind then retried three times at 302 s each.
///
/// This CLAMPS rather than replaces: callers keep saying what the work needs, and the provider's
/// limit lowers it. With no deadline declared the request is returned untouched, so nothing changes
/// for a provider that has not told us anything — and on a model fast enough for the whole set, the
/// right split is no split at all, which is the shape reading 6 measured as the Mind's advantage.
pub fn authoring_budget(requested: usize) -> usize {
    match provider_deadline_seconds() {
        None => requested,
        Some(deadline) => {
            // What the machine has actually seen, when it has seen enough; otherwise the guess.
            let rate = observed_tokens_per_second().unwrap_or(ASSUMED_TOKENS_PER_SECOND);
            let affordable =
                deadline * rate * DEADLINE_BUDGET_NUMERATOR / DEADLINE_BUDGET_DENOMINATOR;
            requested.min(affordable.max(MIN_AUTHORING_TOKENS))
        }
    }
}

/// Below this, a caller is asking for ONE WORD or ONE LINE, not writing a reply budget — the
/// dispatch classifier that wants `yes` (12), a festival line (80). Those are exempt: raising them
/// would let a chatty model return a paragraph where the code expects a token, and truncation is
/// not a risk for a reply that was always meant to be three words. Everything at or above this is a
/// reply budget, where being cut off mid-structure is the failure that actually happens.
const DELIBERATE_BREVITY: usize = 128;

/// Raise a too-small budget when this link is owned hardware. `None` = leave the config alone.
///
/// Keyed on the `ollama-local:` label prefix, which is how local links are named at construction
/// and already how the privacy lane recognises owned hardware.
fn local_budget(label: &str, cfg: &GenerationConfig) -> Option<GenerationConfig> {
    if !label.to_lowercase().starts_with("ollama-local") {
        return None;
    }
    let floor = local_min_tokens();
    if cfg.max_tokens < DELIBERATE_BREVITY || cfg.max_tokens >= floor {
        return None;
    }
    Some(GenerationConfig {
        max_tokens: floor,
        ..cfg.clone()
    })
}

pub struct ChainBackend {
    links: Vec<Arc<dyn LLMBackend>>,
    labels: Vec<String>,
    name: String,
    /// Local survival-tier backend. Tried last, only when all `links` have failed. When it
    /// answers, survival mode activates globally; cleared automatically when a cloud link recovers.
    local: Option<(Arc<dyn LLMBackend>, String)>,
    /// First-link selection policy (failover / round-robin / weighted). Failover-iterates regardless.
    strategy: ChainStrategy,
    /// Rotation counter for RoundRobin / Weighted (deterministic + reproducible, no RNG).
    route: std::sync::atomic::AtomicUsize,
    /// Index of the strong "reasoner" link. A think:true (reasoning/compose, or an escalated retry)
    /// call is routed here FIRST, then failover — so a small dispatch model can own the fast path while
    /// multi-step reasoning goes to the capable model. None = strategy applies to think:true too.
    reasoner: Option<usize>,
}

impl ChainBackend {
    pub fn new(links: Vec<Arc<dyn LLMBackend>>) -> Self {
        let labels: Vec<String> = links.iter().map(|b| b.backend_name().to_string()).collect();
        Self::new_labeled(links, labels)
    }

    /// Provider-named links ("nanogpt", "minimax") — the stats record THESE, not the generic
    /// backend_name ("api"), so `ym providers` says who actually answered.
    pub fn new_labeled(links: Vec<Arc<dyn LLMBackend>>, labels: Vec<String>) -> Self {
        let name = format!("chain[{}]", labels.join(" -> "));
        Self {
            links,
            labels,
            name,
            local: None,
            strategy: ChainStrategy::Failover,
            route: std::sync::atomic::AtomicUsize::new(0),
            reasoner: None,
        }
    }

    /// Attach a local survival-tier backend (e.g. local Ollama). When all cloud links fail, this
    /// is tried last; on success it activates survival mode until a cloud link recovers.
    pub fn with_local_fallback(
        mut self,
        backend: Arc<dyn LLMBackend>,
        label: impl Into<String>,
    ) -> Self {
        self.local = Some((backend, label.into()));
        self
    }

    /// Set the first-link selection policy (failover / round-robin / weighted). Failover is always
    /// the safety net — every strategy tries the remaining links on error/empty.
    pub fn with_strategy(mut self, strategy: ChainStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    /// Designate the strong "reasoner" link (by index). A think:true call is routed there first, then
    /// failover — so a small dispatch model owns the fast path and multi-step reasoning gets the capable one.
    pub fn with_reasoner(mut self, idx: usize) -> Self {
        if idx < self.links.len() {
            self.reasoner = Some(idx);
        }
        self
    }

    /// The order to try links this call. A reasoning turn (`think == Some(true)`) with a designated
    /// reasoner starts there; otherwise the strategy sets the starting link / distribution. Then any
    /// hot (quota-burned) link is demoted to last. Always a full permutation, so failover covers all.
    fn routing_order(&self, think: Option<bool>, prefer_reasoner: bool) -> Vec<usize> {
        use std::sync::atomic::Ordering;
        let n = self.links.len();
        let mut order: Vec<usize> = (0..n).collect();
        if n <= 1 {
            return order;
        }
        let start = match self.reasoner {
            // Route to the reasoner for a think:true call OR an explicit prefer_reasoner (escalation) —
            // the latter gets the strong model WITHOUT think:true's GPU-hogging thinking preamble.
            Some(r) if think == Some(true) || prefer_reasoner => r,
            _ => match &self.strategy {
                ChainStrategy::Failover => 0,
                ChainStrategy::RoundRobin => self.route.fetch_add(1, Ordering::Relaxed) % n,
                ChainStrategy::Weighted(w) => {
                    weighted_index(w, n, self.route.fetch_add(1, Ordering::Relaxed))
                }
            },
        };
        if start > 0 {
            order = (0..n).map(|i| (start + i) % n).collect();
        }
        // Hot-link demotion (a nanogpt link near its weekly quota) — a STABLE partition preserves the
        // strategy order within the kept/demoted groups. Availability beats thrift; unknown never demotes.
        let demote_at: f64 = std::env::var("YM_DEMOTE_PCT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90.0);
        let hot = |i: &usize| -> bool {
            let l = self.labels.get(*i).map_or("", String::as_str);
            l.starts_with("nanogpt") && nanogpt_weekly_pct().is_some_and(|p| p >= demote_at)
        };
        let (cold, warm): (Vec<usize>, Vec<usize>) = order.iter().partition(|i| !hot(i));
        if !warm.is_empty() {
            eprintln!(
                "[chain] demoting hot link(s) to last: {:?}",
                warm.iter()
                    .map(|i| self.labels.get(*i).cloned().unwrap_or_default())
                    .collect::<Vec<_>>()
            );
            order = cold.into_iter().chain(warm).collect();
        }
        order
    }

    fn is_usable(r: &LLMResponse) -> bool {
        !r.text.trim().is_empty() || !r.tool_calls.is_empty() || !r.api_tool_calls.is_empty()
    }
}

/// Per-provider served/failed counters, recorded where the truth lives: the chain knows which
/// link actually answered each call and which failed over. Process-lifetime; `ym providers` reads.
static PROVIDER_STATS: std::sync::Mutex<Option<std::collections::HashMap<String, (u64, u64)>>> =
    std::sync::Mutex::new(None);

fn provider_record(name: &str, served: bool) {
    provider_record_usage(name, served, 0, 0);
}

/// Record one call: outcome + token usage. Persists a per-day rollup to provider_usage.json
/// (14-day window) so "how much this week" survives restarts — the LOCAL METER for providers
/// that expose no usage API (Ollama Cloud, MiniMax).
fn provider_record_usage(name: &str, served: bool, tokens_in: u64, tokens_out: u64) {
    {
        let mut g = PROVIDER_STATS.lock().unwrap();
        let m = g.get_or_insert_with(std::collections::HashMap::new);
        let e = m.entry(name.to_string()).or_insert((0, 0));
        if served {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }
    // Persistent daily rollup (best-effort; a failed write never blocks inference).
    let dir = std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into());
    let p = std::path::PathBuf::from(dir).join("provider_usage.json");
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut v: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|x| serde_json::from_str(&x).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let day = &mut v[&today];
    if day.is_null() {
        *day = serde_json::json!({});
    }
    let e = &mut day[name];
    if e.is_null() {
        *e = serde_json::json!({"in": 0, "out": 0, "served": 0, "failed": 0});
    }
    let bump = |e: &mut serde_json::Value, k: &str, n: u64| {
        e[k] = serde_json::json!(e[k].as_u64().unwrap_or(0) + n);
    };
    bump(e, "in", tokens_in);
    bump(e, "out", tokens_out);
    bump(e, if served { "served" } else { "failed" }, 1);
    // prune to 14 days
    if let Some(m) = v.as_object_mut() {
        if m.len() > 14 {
            let mut keys: Vec<String> = m.keys().cloned().collect();
            keys.sort();
            for old in keys.iter().take(m.len() - 14) {
                m.remove(old);
            }
        }
    }
    let tmp = p.with_extension("json.tmp");
    if std::fs::write(&tmp, v.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, &p);
    }
}

/// Cached NanoGPT weekly utilization (0-100). Probed at most every 30 min; None = unknown
/// (no key / probe failed) — unknown NEVER demotes. The chain uses this to route headroom-first.
fn nanogpt_weekly_pct() -> Option<f64> {
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, Option<f64>)>> =
        std::sync::Mutex::new(None);
    {
        let g = CACHE.lock().unwrap();
        if let Some((t, v)) = *g {
            if t.elapsed() < std::time::Duration::from_secs(1800) {
                return v;
            }
        }
    }
    let key = std::env::var("NANOGPT_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty());
    let v: Option<f64> = key.and_then(|key| {
        ureq::get("https://nano-gpt.com/api/subscription/v1/usage")
            .set("x-api-key", &key)
            .timeout(std::time::Duration::from_secs(8))
            .call()
            .ok()
            .and_then(|r| r.into_json::<serde_json::Value>().ok())
            .and_then(|j| {
                j.get("weeklyInputTokens")
                    .and_then(|w| w.get("percentUsed"))
                    .and_then(|x| x.as_f64())
                    .map(|p| p * 100.0)
            })
    });
    *CACHE.lock().unwrap() = Some((std::time::Instant::now(), v));
    v
}

/// Per-provider (today_in, today_out, week_in, week_out, week_served) from the persisted rollup —
/// the local meter `ym providers` renders. ISO week of today.
pub fn provider_usage_rollup() -> Vec<(String, u64, u64, u64, u64, u64)> {
    use chrono::Datelike;
    let dir = std::env::var("YM_STATE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind".into());
    let p = std::path::PathBuf::from(dir).join("provider_usage.json");
    let v: serde_json::Value = std::fs::read_to_string(&p)
        .ok()
        .and_then(|x| serde_json::from_str(&x).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    let today = chrono::Local::now();
    let today_s = today.format("%Y-%m-%d").to_string();
    let week = today.iso_week().week();
    let mut agg: std::collections::HashMap<String, (u64, u64, u64, u64, u64)> =
        std::collections::HashMap::new();
    if let Some(days) = v.as_object() {
        for (day, provs) in days {
            let in_week = chrono::NaiveDate::parse_from_str(day, "%Y-%m-%d")
                .map(|d| d.iso_week().week() == week && d.year() == today.year())
                .unwrap_or(false);
            let is_today = *day == today_s;
            if let Some(pm) = provs.as_object() {
                for (prov, e) in pm {
                    let g = |k: &str| e.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
                    let a = agg.entry(prov.clone()).or_insert((0, 0, 0, 0, 0));
                    if is_today {
                        a.0 += g("in");
                        a.1 += g("out");
                    }
                    if in_week {
                        a.2 += g("in");
                        a.3 += g("out");
                        a.4 += g("served");
                    }
                }
            }
        }
    }
    let mut out: Vec<(String, u64, u64, u64, u64, u64)> = agg
        .into_iter()
        .map(|(k, (a, b, c, d, e))| (k, a, b, c, d, e))
        .collect();
    out.sort_by(|a, b| b.3.cmp(&a.3));
    out
}

/// (provider, served, failed) sorted by served desc — who is ACTUALLY answering.
pub fn provider_stats() -> Vec<(String, u64, u64)> {
    let g = PROVIDER_STATS.lock().unwrap();
    let mut v: Vec<(String, u64, u64)> = g
        .as_ref()
        .map(|m| m.iter().map(|(k, (s, f))| (k.clone(), *s, *f)).collect())
        .unwrap_or_default();
    v.sort_by(|a, b| b.1.cmp(&a.1));
    v
}

impl LLMBackend for ChainBackend {
    fn chat(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        let mut last_err: Option<anyhow::Error> = None;
        // The strategy (failover / round-robin / weighted) sets which link is tried first + the load
        // distribution — except a think:true call is routed to the reasoner link first. A quota-burned
        // link is still demoted to last. Every order is a full permutation, so on error/empty we
        // failover through the rest — a backup is always in play.
        let order = self.routing_order(config.think, config.prefer_reasoner);
        for i in order {
            let be = &self.links[i];
            let label = self
                .labels
                .get(i)
                .map_or_else(|| be.backend_name(), String::as_str);
            // Owned hardware bills time, not tokens — give it room rather than truncating a tool
            // call or an answer to save a token that costs nothing. Cloud links pass through.
            let raised = local_budget(label, config);
            let config = raised.as_ref().unwrap_or(config);
            // L4-0: one link invocation is one attempt at the observable boundary.
            note_attempt();
            match be.chat(messages, config, tools) {
                Ok(r) if Self::is_usable(&r) => {
                    // Cloud answered: clear survival mode if it was active.
                    if self.local.is_some()
                        && SURVIVAL_MODE.swap(false, std::sync::atomic::Ordering::SeqCst)
                    {
                        *SURVIVAL_SINCE.lock().unwrap() = None;
                        eprintln!(
                            "[survival] cloud provider recovered ({label}) — exiting survival mode"
                        );
                    }
                    provider_record_usage(
                        label,
                        true,
                        r.prompt_tokens as u64,
                        r.completion_tokens as u64,
                    );
                    note_serving_link(label);
                    return Ok(r);
                }
                Ok(_) => {
                    provider_record(label, false);
                    eprintln!(
                        "[chain] {} returned empty — failing over",
                        be.backend_name()
                    );
                    last_err = Some(anyhow::anyhow!("empty response from {}", be.backend_name()));
                }
                Err(e) => {
                    provider_record(label, false);
                    eprintln!("[chain] {} failed ({e}) — failing over", be.backend_name());
                    last_err = Some(e);
                }
            }
        }
        // All cloud links exhausted — try the local survival tier.
        if let Some((local_be, local_label)) = &self.local {
            // The survival tier IS owned hardware, and it is reached when everything else has
            // already failed — the worst moment to also truncate the reply.
            let raised = local_budget(local_label, config);
            let config = raised.as_ref().unwrap_or(config);
            note_attempt();
            match local_be.chat(messages, config, tools) {
                Ok(r) if Self::is_usable(&r) => {
                    if !SURVIVAL_MODE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        *SURVIVAL_SINCE.lock().unwrap() = Some(std::time::Instant::now());
                        eprintln!("[survival] all cloud providers failed — activating local tier ({local_label})");
                    }
                    provider_record_usage(
                        local_label,
                        true,
                        r.prompt_tokens as u64,
                        r.completion_tokens as u64,
                    );
                    note_serving_link(local_label);
                    return Ok(r);
                }
                Ok(_) => {
                    provider_record(local_label, false);
                    eprintln!("[survival] local tier ({local_label}) returned empty");
                }
                Err(e) => {
                    provider_record(local_label, false);
                    eprintln!("[survival] local tier ({local_label}) also failed: {e}");
                }
            }
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("chain has no backends")))
    }

    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LLMResponse> {
        // The chain can't stream across a failover boundary cleanly, so it resolves the whole reply
        // (with fallover) then emits it once. The mind uses non-streaming `chat`, so this is a
        // correctness-preserving shim, not the hot path.
        let r = self.chat(messages, config, tools)?;
        on_token(&r.text);
        Ok(r)
    }

    fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
        match self.links.first() {
            Some(be) => be.count_tokens(text),
            None => Ok(text.len() / 4),
        }
    }

    fn backend_name(&self) -> &str {
        &self.name
    }

    // The same 4-of-6 gap the strict adapter had, in the backend that most needs the other two: a
    // chain is what runs during an outage. `is_degraded` reported false while the mind was in
    // survival mode, and `model_id` reported "unknown" for every pooled call. Reported by review as
    // the sibling of a defect it had already found.
    fn is_degraded(&self) -> bool {
        in_survival_mode() || self.links.iter().any(|l| l.is_degraded())
    }

    /// The chain is not one model, so this names the CHAIN — its ordered labels — rather than
    /// claiming a single id. "unknown" was worse: it is what a caller reads when nobody set one.
    fn model_id(&self) -> &str {
        &self.name
    }
}

// ── Provider catalog + per-function router ────────────────────────────────────────────────────
//
// "Configurable which function is done by which model/provider." Every provider is OpenAI-compatible,
// so a provider is just (base_url, key-env, default-model). A function ("role") is mapped to a
// provider:model via `YM_ROLE_<ROLE>`; unset roles use the default chain. This is the one place that
// knows provider endpoints — add a provider here and it's usable everywhere.

/// Resolve a "provider" or "provider:model" spec to an OpenAI-compat backend, reading the provider's
/// API key from env. `None` for an unknown provider or a missing/empty key.
fn configured_api_key(key_env: &str) -> Option<String> {
    std::env::var(key_env)
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty())
}

/// Split a `provider:model` spec into its halves, trimmed. A spec with no colon is a provider with
/// no model, which means "the provider's default".
pub fn split_spec(spec: &str) -> (&str, &str) {
    match spec.split_once(':') {
        Some((p, m)) => (p.trim(), m.trim()),
        None => (spec.trim(), ""),
    }
}

/// The provider table: `(base_url, key_env, default_model)` for a known provider, `None` otherwise.
///
/// This is the ONE place that knows provider endpoints. It was inline in `backend_from_spec`, which
/// meant a diagnostic that wanted to ask "what would this spec actually call" had to either copy the
/// table or build a backend it did not want. E.CFG1 needed exactly that question, so the table is
/// public and `backend_from_spec` reads it like everyone else.
pub fn provider_catalog(provider: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match provider {
        "nanogpt" => (
            "https://nano-gpt.com/api/v1",
            "NANOGPT_KEY",
            "deepseek/deepseek-v4-pro-cheaper",
        ),
        "ollama-cloud" | "ollama" => ("https://ollama.com/v1", "OLLAMA_CLOUD_KEY", "glm-4.7"),
        "minimax" => (
            "https://api.minimax.io/v1",
            "MINIMAX_API_KEY",
            "MiniMax-M2.7",
        ),
        // QwenCloud token-plan. NOTE the host: the public docs point at
        // dashscope-intl.aliyuncs.com, which REJECTS token-plan keys (sk-sp-…) — the working base is
        // the token-plan MaaS endpoint below. Benchmarked 2026-08-03 at 6/6 on brain_bench (this
        // mind's own tool-selection workload) vs 4-5/6 for every local pool member.
        // HOUSEHOLD LANE ONLY: it is a cloud provider, so it must never serve a private-grounded
        // turn — the private lane stays on owned hardware via YM_BRAIN_POOL.
        "qwencloud" | "qwen" => (
            "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
            "QWEN_API_KEY",
            "qwen3.8-max",
        ),
        "openrouter" => (
            "https://openrouter.ai/api/v1",
            "OPEN_ROUTER_KEY",
            "deepseek/deepseek-chat",
        ),
        // ── The FREE-TIER lanes (researched 2026-08-16). All OpenAI-compatible. These are for the
        // household/public lanes ONLY — a private-grounded turn stays on owned hardware regardless
        // (enforced at the call sites and canary-tested in privacy_tests below). Free tiers move;
        // the numbers live in the config schema descriptions, not here.
        // NVIDIA NIM: the deepest free catalog (100+ models, ~40 RPM free).
        "nim" | "nvidia" => (
            "https://integrate.api.nvidia.com/v1",
            "NVIDIA_API_KEY",
            "deepseek-ai/deepseek-v4",
        ),
        // Groq LPU: fastest turnaround per request on a free tier (~30 RPM / 1k RPD).
        "groq" => (
            "https://api.groq.com/openai/v1",
            "GROQ_API_KEY",
            "llama-3.3-70b-versatile",
        ),
        // Cerebras: highest free throughput (~1M tokens/day at ~2k tok/s).
        "cerebras" => (
            "https://api.cerebras.ai/v1",
            "CEREBRAS_API_KEY",
            "llama-3.3-70b",
        ),
        "grok" => ("https://api.x.ai/v1", "GROK_API_KEY", "grok-2-latest"),
        // Anthropic direct. Default Sonnet 5 (fast + cheap enough for an
        // always-on brain); swap the model to claude-opus-4-8 or claude-fable-5 (when it un-gates).
        "anthropic" => (
            "https://api.anthropic.com",
            "ANTHROPIC_API_KEY",
            "claude-sonnet-5",
        ),
        _ => return None,
    })
}

// ── E.PORT1: speak the documented protocol to providers that check it ────────────────────────
//
// `GenericOpenAIBackend` emits BOTH `think:false` and `reasoning_effort:"none"` whenever a call asks
// for thinking OFF. That pairing exists for one good reason: Ollama's OpenAI-compat endpoint ignores
// `think`, so on that path the non-standard value is the only thing that suppresses the preamble.
// But `"none"` is not in the OpenAI enum, which allows only
// low / medium / high, so a provider that validates the field answers 400 to EVERY call — measured
// live: five one-request probes, and the field is the only difference between 200 and 400.
//
// The backend that emits it lives in another repository. The fix here is a thin adapter: on the
// strict path a request for thinking OFF becomes "no preference", so neither field is sent.
//
// WHICH providers are strict is a decision, not a default. Review caught the first version getting
// it wrong by omission: the comment said "every provider reached through this table", and the table
// contains `ollama` and `ollama-cloud` — the very endpoints that IGNORE `think` and for which the
// non-standard value is the only thing that suppresses the preamble. Stripping it there would have
// applied a fix for "the provider REJECTS this field" to a provider that NEEDS it. So the exclusion
// is explicit and named.
//
// One honesty note, because review caught the first version of this comment claiming otherwise: the
// measured 10 s → 0.9 s improvement was taken on the LOCAL lane, which uses the native endpoint
// where `think` alone suffices. No measurement of `ollama.com/v1` exists. The exclusion rests on the
// endpoint family's behaviour, not on evidence from that host.
//
// A fifth review caught this passage claiming to be "the ONLY place in this file that should mention
// it" while the figure appeared in two more comments — a sentence about the file's contents that had
// stopped being true of the file. A sixth then caught the replacement ("scoped in all three places")
// being not quite true either: one of the three relies on its surrounding context rather than saying
// so, and a fourth comment mentions the same measurement's other half. The honest statement is the
// narrow one: this passage is where the retraction is argued, and no comment in this file may assert
// the figure as a property of the CLOUD lane.
//
// The LOCAL Ollama lane is a separate constructor (`local_backend_from_env`, and the brain pool)
// and is never wrapped at all.
//
// The price, stated plainly: on a strict provider the mind can no longer suppress a reasoning
// preamble, so such calls may be slower and spend tokens reasoning. That is what speaking the
// documented protocol costs; the alternative is a 400 every time.

/// Rewrites one field of a per-call config, then delegates everything.
struct StrictOpenAiBackend {
    inner: Arc<dyn LLMBackend>,
}

impl StrictOpenAiBackend {
    /// Thinking OFF is unrepresentable in the standard protocol, so it becomes no preference.
    /// Every other setting is passed through untouched.
    fn adapt(config: &GenerationConfig) -> GenerationConfig {
        let mut c = config.clone();
        if c.think == Some(false) {
            c.think = None;
        }
        c
    }
}

/// Providers whose endpoint needs the non-standard suppression field rather than rejecting it.
/// These are Ollama-compatible: they ignore `think` on the OpenAI-compat path, so removing
/// `reasoning_effort` would leave nothing suppressing the preamble and buy nothing in return.
fn provider_is_ollama_compatible(provider: &str) -> bool {
    matches!(provider, "ollama" | "ollama-cloud")
}

impl LLMBackend for StrictOpenAiBackend {
    fn chat(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        self.inner.chat(messages, &Self::adapt(config), tools)
    }

    fn chat_streaming(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<LLMResponse> {
        self.inner
            .chat_streaming(messages, &Self::adapt(config), tools, on_token)
    }

    fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
        self.inner.count_tokens(text)
    }

    fn backend_name(&self) -> &str {
        self.inner.backend_name()
    }

    // The trait has SIX methods and the first version implemented four, so `model_id` and
    // `is_degraded` fell through to the trait defaults — every strict cloud backend reported its
    // model as "unknown", and the test that claimed "identity is the inner backend's" could not see
    // it because the test double did not override `model_id` either. An adapter must be transparent
    // in every direction or it is a filter.
    fn is_degraded(&self) -> bool {
        self.inner.is_degraded()
    }

    fn model_id(&self) -> &str {
        self.inner.model_id()
    }
}

pub fn backend_from_spec(spec: &str) -> Option<Arc<dyn LLMBackend>> {
    let (provider, model) = split_spec(spec);
    let (base, key_env, default_model) = provider_catalog(provider)?;
    let key = configured_api_key(key_env)?;
    // Containment hook (E.CB2-N): `YM_PROVIDER_BASE_URL_<PROVIDER>` re-points one provider at a
    // local counting proxy that holds the real key; the process itself then only needs a
    // placeholder key. The provider name is the one written in the spec, upper-cased, '-' → '_'.
    let base = resolve_provider_base(provider, base, |name| std::env::var(name).ok());
    let base = base.as_str();
    let model = if model.is_empty() {
        default_model.to_string()
    } else {
        model.to_string()
    };
    if provider == "anthropic" {
        Some(Arc::new(yantrik_ml::AnthropicBackend::with_base_url(
            key, base, model,
        )) as Arc<dyn LLMBackend>)
    } else {
        // E.PORT1: wrap only the providers that VALIDATE the OpenAI enum. The Ollama-compatible
        // entries in this table need the non-standard value; stripping it there would remove the
        // only suppression they have, on a lane that never rejects it.
        let inner = Arc::new(yantrik_ml::GenericOpenAIBackend::for_provider(
            "openai",
            base,
            Some(key),
            model,
        )) as Arc<dyn LLMBackend>;
        if provider_is_ollama_compatible(provider) {
            Some(inner)
        } else {
            Some(Arc::new(StrictOpenAiBackend { inner }) as Arc<dyn LLMBackend>)
        }
    }
}

// ── E.CFG1: what the role router would actually call ─────────────────────────────────────────
//
// `Router::from_env` asks two questions of a spec — is the provider known, is its key present — and
// never asks whether the MODEL exists. A role set to a model its provider does not serve therefore
// builds a pool that reports itself as configured and fails only when something calls it. That is
// live on the staging box today, and it cost E.CB2-N a preflight round when a provider answered 410
// eight times for a model absent from its catalogue of 82. These types make the gap visible; they
// change no routing.

/// Why a spec would or would not produce a backend. `backend_from_spec` collapses the last two into
/// `None`, which is why its one message has to say "unknown provider or missing key" — two different
/// problems with two different fixes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecStatus {
    /// Provider known and its key present: this spec builds a backend.
    Configured,
    /// The provider is not in `provider_catalog`.
    UnknownProvider,
    /// The provider is known; `key_env` is unset or blank.
    MissingKey { key_env: &'static str },
}

/// One configured route: which variable set it, what it says, and what that resolves to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRoute {
    pub var: String,
    pub role: String,
    pub spec: String,
    pub provider: String,
    /// The model as written, or the provider's default when the spec named none.
    pub model: String,
    /// True when the spec named no model and the default was filled in.
    pub model_defaulted: bool,
    /// The base URL this route would actually call, `YM_PROVIDER_BASE_URL_<PROVIDER>` included.
    pub base_url: String,
    pub status: SpecStatus,
}

/// Classify one spec without building anything. Pure: the env lookup is injected.
pub fn classify_spec(
    spec: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> (String, String, bool, String, SpecStatus) {
    let (provider, model) = split_spec(spec);
    match provider_catalog(provider) {
        None => (
            provider.to_string(),
            model.to_string(),
            false,
            String::new(),
            SpecStatus::UnknownProvider,
        ),
        Some((base, key_env, default_model)) => {
            let defaulted = model.is_empty();
            let model = if defaulted { default_model } else { model }.to_string();
            let base_url = resolve_provider_base(provider, base, &lookup);
            let has_key = lookup(key_env)
                .map(|k| !k.trim().is_empty())
                .unwrap_or(false);
            let status = if has_key {
                SpecStatus::Configured
            } else {
                SpecStatus::MissingKey { key_env }
            };
            (provider.to_string(), model, defaulted, base_url, status)
        }
    }
}

/// Every explicitly configured route: the six `YM_ROLE_*` plus `YM_PRIMARY_BRAIN`. An unset variable
/// is absent from the plan — an unset role uses the default pool and makes no configuration claim.
/// Pure: env lookup injected, no network, no backends built.
pub fn role_plan(lookup: impl Fn(&str) -> Option<String> + Copy) -> Vec<RoleRoute> {
    let mut out = Vec::new();
    let mut push = |var: String, role: &str, spec: String| {
        let (provider, model, model_defaulted, base_url, status) = classify_spec(&spec, lookup);
        out.push(RoleRoute {
            var,
            role: role.to_string(),
            spec,
            provider,
            model,
            model_defaulted,
            base_url,
            status,
        });
    };
    for role in ["chat", "research", "util", "verify", "code", "consolidate"] {
        let var = format!("YM_ROLE_{}", role.to_uppercase());
        if let Some(spec) = lookup(&var)
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
        {
            push(var, role, spec);
        }
    }
    if let Some(spec) = primary_brain_spec(lookup) {
        push("YM_PRIMARY_BRAIN".to_string(), "primary-brain", spec);
    }
    out
}

/// A base URL with everything that can carry a secret removed: userinfo, query and fragment.
///
/// The rendered plan is console output and goes into reports. An operator override is free-form —
/// `https://user:token@host/v1`, or a signed `?key=…` — so printing it verbatim would turn a
/// diagnostic into a credential disclosure. The RUNTIME fetch still uses the real URL; only what is
/// shown is reduced to scheme, host, port and path.
pub fn redact_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s, r),
        None => ("", url),
    };
    // authority ends at the first '/', '?' or '#'
    let end = rest
        .find(|c| c == '/' || c == '?' || c == '#')
        .unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    // userinfo is everything before the LAST '@' in the authority
    let host = match authority.rsplit_once('@') {
        Some((_userinfo, h)) => h,
        None => authority,
    };
    let path = tail.split(['?', '#']).next().unwrap_or("").to_string();
    let host = if authority.contains('@') {
        format!("{host} (userinfo removed)")
    } else {
        host.to_string()
    };
    let query_note = if tail.contains('?') || tail.contains('#') {
        " (query removed)"
    } else {
        ""
    };
    if scheme.is_empty() {
        format!("{host}{path}{query_note}")
    } else {
        format!("{scheme}://{host}{path}{query_note}")
    }
}

/// Redact any URL-looking token inside a free-text reason.
///
/// `verify_models` takes the fetcher as a parameter, so the reason string is not always the one
/// `fetch_model_catalogue` builds — a caller's fetcher (or a future library) may echo the URL it was
/// given. The renderer therefore does not trust the text it is handed: anything containing `://` is
/// reduced the same way a base URL is. Defence in depth, and the canary test covers this path.
pub fn sanitize_reason(reason: &str) -> String {
    reason
        .split_whitespace()
        .map(|tok| {
            if tok.contains("://") {
                let trailing: String = tok
                    .chars()
                    .rev()
                    .take_while(|c| matches!(c, '.' | ',' | ')' | ';' | ':'))
                    .collect();
                let core = &tok[..tok.len() - trailing.len()];
                format!(
                    "{}{}",
                    redact_url(core),
                    trailing.chars().rev().collect::<String>()
                )
            } else {
                tok.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether the provider serves the model a route names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelCheck {
    /// The catalogue contains this exact model id.
    Served,
    /// The catalogue was read and this id is not in it. Carries the catalogue size, because "absent
    /// from a catalogue of 82" and "absent from a catalogue of 0" are different situations.
    NotServed { catalogue: usize },
    /// The catalogue could not be read. NEVER reported as served: an unreachable provider is an
    /// unknown, and a diagnostic that guessed here would be worse than none.
    Unreachable { why: String },
    /// The route builds no backend, so there is nothing to ask a provider about.
    NotApplicable,
}

/// Check each route's model against its provider's catalogue. `fetch` takes (base_url, provider) and
/// returns the served model ids; it is a parameter so the tests need no network. One fetch per
/// distinct provider/base-URL pair, not per route. Provider identity is part of the key because one
/// containment proxy URL may front multiple providers with different credentials and catalogues.
pub fn verify_models(
    plan: &[RoleRoute],
    mut fetch: impl FnMut(&str, &str) -> Result<Vec<String>, String>,
) -> Vec<ModelCheck> {
    let mut cache: Vec<(String, String, Result<Vec<String>, String>)> = Vec::new();
    plan.iter()
        .map(|r| {
            if r.status != SpecStatus::Configured {
                return ModelCheck::NotApplicable;
            }
            if !cache
                .iter()
                .any(|(b, p, _)| b == &r.base_url && p == &r.provider)
            {
                let got = fetch(&r.base_url, &r.provider);
                cache.push((r.base_url.clone(), r.provider.clone(), got));
            }
            match cache
                .iter()
                .find(|(b, p, _)| b == &r.base_url && p == &r.provider)
                .map(|(_, _, g)| g)
            {
                Some(Ok(ids)) => {
                    if ids.iter().any(|i| i == &r.model) {
                        ModelCheck::Served
                    } else {
                        ModelCheck::NotServed {
                            catalogue: ids.len(),
                        }
                    }
                }
                Some(Err(why)) => ModelCheck::Unreachable { why: why.clone() },
                None => ModelCheck::Unreachable {
                    why: "no catalogue attempt".to_string(),
                },
            }
        })
        .collect()
}

/// The plan as an operator report. `checks` is `None` for the read that makes no network call — and
/// then the report says so in its own words, rather than letting silence read as health.
pub fn render_role_plan(plan: &[RoleRoute], checks: Option<&[ModelCheck]>) -> String {
    if plan.is_empty() {
        return "ROLE ROUTES — none configured: every function uses the default chain \
                (YM_ROLE_CHAT/RESEARCH/UTIL/VERIFY/CODE/CONSOLIDATE, YM_PRIMARY_BRAIN)."
            .to_string();
    }
    let mut out = String::from("ROLE ROUTES — what each configured function would actually call\n");
    for (i, r) in plan.iter().enumerate() {
        let status = match &r.status {
            SpecStatus::Configured => "configured".to_string(),
            SpecStatus::UnknownProvider => format!(
                "UNKNOWN PROVIDER '{}' — this route builds nothing and the function falls back to the default pool",
                r.provider
            ),
            SpecStatus::MissingKey { key_env } => format!(
                "MISSING KEY {key_env} — the provider is known but its key is unset, so this route builds nothing"
            ),
        };
        let model = if r.model_defaulted {
            format!("{} (provider default; the spec named no model)", r.model)
        } else {
            r.model.clone()
        };
        let check = match checks.and_then(|c| c.get(i)) {
            None => "model existence NOT verified (no network call was made)".to_string(),
            Some(ModelCheck::Served) => "SERVED by the provider".to_string(),
            Some(ModelCheck::NotServed { catalogue }) => format!(
                "NOT SERVED — the provider's catalogue of {catalogue} model(s) does not contain this id, \
                 so calls on this route fail at request time"
            ),
            Some(ModelCheck::Unreachable { why }) => {
                format!(
                    "UNVERIFIED — the catalogue could not be read ({})",
                    sanitize_reason(why)
                )
            }
            Some(ModelCheck::NotApplicable) => "not checked — this route builds nothing".to_string(),
        };
        out.push_str(&format!(
            "{} = {}\n  provider {} · model {} · base {}\n  {}\n  {}\n",
            r.var,
            r.spec,
            r.provider,
            model,
            redact_url(&r.base_url),
            status,
            check
        ));
    }
    out.push_str(
        "\nA route naming a model its provider does not serve looks configured here and in every other \
         report; only the verified read can tell you otherwise.",
    );
    out
}

/// Read a provider's model catalogue over HTTP: `GET {base}/models`, OpenAI-compatible shape.
/// Bounded and best-effort — every failure is a reason string, never a claim that a model is served.
pub fn fetch_model_catalogue(base_url: &str, provider: &str) -> Result<Vec<String>, String> {
    let key_env = provider_catalog(provider).map(|(_, k, _)| k).unwrap_or("");
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut req = ureq::get(&url).timeout(std::time::Duration::from_secs(20));
    if let Some(key) = configured_api_key(key_env) {
        req = req.set("Authorization", &format!("Bearer {key}"));
    }
    // ureq's Display for an error embeds the URL it was called with, and that URL may carry an
    // operator's credential. The reason is built from the error's KIND instead, which never does.
    let body: serde_json::Value = req
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("provider answered HTTP {code}"),
            ureq::Error::Transport(t) => format!("transport failure ({})", t.kind()),
        })?
        .into_json()
        .map_err(|_| "the catalogue response was not readable JSON".to_string())?;
    let ids: Vec<String> = body
        .get("data")
        .and_then(|d| d.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return Err("the catalogue response carried no model ids".to_string());
    }
    Ok(ids)
}

/// `ym why roles` — the plan from the real environment, no network call.
pub fn render_roles_from_env() -> String {
    let plan = role_plan(|k| std::env::var(k).ok());
    render_role_plan(&plan, None)
}

/// `ym why roles verify` — the plan plus a live catalogue check per distinct base URL.
pub fn render_roles_verified_from_env() -> String {
    let plan = role_plan(|k| std::env::var(k).ok());
    let checks = verify_models(&plan, fetch_model_catalogue);
    render_role_plan(&plan, Some(&checks))
}

/// The trimmed `YM_PRIMARY_BRAIN` spec, `None` when unset or blank. Pure (lookup injected).
pub fn primary_brain_spec(lookup: impl Fn(&str) -> Option<String>) -> Option<String> {
    lookup("YM_PRIMARY_BRAIN")
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// The base URL for `provider`: `YM_PROVIDER_BASE_URL_<PROVIDER>` when set to a non-empty value
/// (trimmed, trailing '/' dropped), else `default`. Pure: the lookup is injected so tests never
/// touch process env.
pub fn resolve_provider_base(
    provider: &str,
    default: &str,
    lookup: impl Fn(&str) -> Option<String>,
) -> String {
    let var = format!(
        "YM_PROVIDER_BASE_URL_{}",
        provider.trim().to_uppercase().replace('-', "_")
    );
    match lookup(&var).map(|v| v.trim().trim_end_matches('/').to_owned()) {
        Some(v) if !v.is_empty() => v,
        _ => default.to_owned(),
    }
}

/// The default resilient chain from whatever provider keys are present. CONFIG-DRIVEN precedence:
/// when `YM_LOCAL_OLLAMA_URL` is set, the local model is the PRIMARY brain (owned hardware, fast, and
/// it backs the private lane), with the cloud providers (NanoGPT → Ollama Cloud → MiniMax) as
/// fallback for when local is down. Set `YM_LOCAL_ROLE=fallback` to keep the old survival-tier
/// behavior (cloud primary, local emergency). `None` if neither a local endpoint nor a cloud key is
/// set. Models via `YM_LOCAL_OLLAMA_MODEL` / `YM_MODEL` / `YM_OLLAMA_MODEL` / `YM_MINIMAX_MODEL`.
pub fn default_chain_from_env() -> Option<(Arc<dyn LLMBackend>, String)> {
    let local = local_backend_from_env();
    let local_primary = local.is_some()
        && std::env::var("YM_LOCAL_ROLE")
            .map(|r| r.trim() != "fallback")
            .unwrap_or(true);

    let order = [
        ("nanogpt", std::env::var("YM_MODEL").ok()),
        ("ollama-cloud", std::env::var("YM_OLLAMA_MODEL").ok()),
        ("minimax", std::env::var("YM_MINIMAX_MODEL").ok()),
    ];
    let mut links: Vec<Arc<dyn LLMBackend>> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    // LOCAL FIRST when it's the primary brain — every household turn runs on owned hardware; cloud is
    // only reached when local fails (household lane only — a Private turn never falls through, it
    // fails closed via the dedicated private lane wired in `main`).
    if local_primary {
        if let Some((be, lbl)) = &local {
            links.push(be.clone());
            labels.push(lbl.clone());
        }
    }
    // `YM_PRIMARY_BRAIN=<provider[:model]>` names any catalog provider as the first cloud link — the
    // default chain is otherwise limited to the three providers below. Used by the E.CB2-N scratch
    // instance (a NIM-only brain behind a counting proxy); unset or unresolvable → no change.
    if let Some(spec) = primary_brain_spec(|name| std::env::var(name).ok()) {
        match backend_from_spec(&spec) {
            Some(be) => {
                links.push(be);
                labels.push(spec);
            }
            None => eprintln!(
                "[brain] YM_PRIMARY_BRAIN={spec:?} — unknown provider or missing key; ignored"
            ),
        }
    }
    for (provider, model) in order {
        let spec = match model {
            Some(m) if !m.trim().is_empty() => format!("{provider}:{m}"),
            _ => provider.to_string(),
        };
        if let Some(be) = backend_from_spec(&spec) {
            links.push(be);
            labels.push(spec);
        }
    }
    if links.is_empty() {
        return None; // no local-primary brain and no cloud keys
    }
    // A local SURVIVAL fallback is attached only when local is NOT the primary (old behavior).
    let survival = if local_primary { None } else { local };
    if links.len() == 1 && survival.is_none() {
        return Some((links.pop().unwrap(), labels[0].clone()));
    }
    let mut chain = ChainBackend::new_labeled(links, labels.clone());
    if let Some((local_be, local_label)) = survival {
        chain = chain.with_local_fallback(local_be, local_label);
    }
    Some((Arc::new(chain), labels.join(" -> ")))
}

/// Build the local owned-hardware backend from env (the PRIMARY brain + the private lane when set;
/// see `default_chain_from_env`). Returns `None` if `YM_LOCAL_OLLAMA_URL` is not set (explicit opt-in
/// — avoids false "local available" signals). Point the URL at the owned endpoint (a TLS gateway like
/// `https://aig.mycluster.cyou` is preferred over a plaintext-LAN Ollama). Model/key via env.
/// Per-workload thinking policy, config-overridable — resolves `GenerationConfig.think` for a
/// named call site. `YM_THINK_<ROLE>` (case-insensitive: on/true/1/yes → ON, off/false/0/no → OFF)
/// overrides the baked default; unset → `default`. Lets the dual-mode split (dispatch OFF for
/// fast tool-selection, reasoning ON for quality) be retuned from /etc/yantrik-mind.env without a
/// rebuild — e.g. `YM_THINK_DISPATCH=on` or `YM_THINK_REASONING=off` while the maintainer iterates.
pub fn think_for(role: &str, default: Option<bool>) -> Option<bool> {
    match std::env::var(format!("YM_THINK_{}", role.to_ascii_uppercase()))
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .as_deref()
    {
        Some("on" | "true" | "1" | "yes") => Some(true),
        Some("off" | "false" | "0" | "no") => Some(false),
        _ => default,
    }
}

/// A config-defined pool of LOCAL brain backends with a selectable backup strategy. Set:
///
/// ```text
/// YM_BRAIN_POOL = "url|model[@weight] ; url|model[@weight] ; ..."
/// # e.g. "https://aig.mycluster.cyou|gemma4:e4b@70 ; http://192.168.4.180:11434|qwen3.6:35b-a3b-mtp-q4_K_M@30"
/// YM_BRAIN_STRATEGY = failover | round_robin | weighted
/// # default: weighted if any @weight, else failover
/// ```
/// Every entry is an Ollama endpoint (provider "ollama", native /api/chat), so the per-call `think`
/// flag (dual-mode) flows to whichever link is chosen. Because all links are owned/local, the pool is
/// safe as the PRIVATE lane too: a private turn stays on owned hardware, failover is local-only, and
/// if every link fails the pool returns an error so the lane still FAILS CLOSED (never cloud).
pub fn brain_pool_from_env() -> Option<(Arc<dyn LLMBackend>, String)> {
    let raw = std::env::var("YM_BRAIN_POOL")
        .ok()
        .filter(|s| !s.trim().is_empty())?;
    let mut links: Vec<Arc<dyn LLMBackend>> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut weights: Vec<u32> = Vec::new();
    for entry in raw.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        // trailing "@<n>" is the weight; anything else is part of the spec (model tags contain ':').
        let (spec, weight) = match entry.rsplit_once('@') {
            Some((s, w)) => match w.trim().parse::<u32>() {
                Ok(n) => (s.trim(), n),
                Err(_) => (entry, 1u32),
            },
            None => (entry, 1u32),
        };
        let (url, model) = spec
            .split_once('|')
            .map_or((spec, "gemma4:e4b"), |(u, m)| (u.trim(), m.trim()));
        let be = yantrik_ml::GenericOpenAIBackend::for_provider(
            "ollama",
            url,
            Some("ollama".to_string()),
            model,
        );
        links.push(Arc::new(be) as Arc<dyn LLMBackend>);
        labels.push(format!("ollama-local:{model}"));
        weights.push(weight);
    }
    if links.is_empty() {
        return None;
    }
    let strategy = match std::env::var("YM_BRAIN_STRATEGY")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "failover" => ChainStrategy::Failover,
        "round_robin" | "roundrobin" | "rr" => ChainStrategy::RoundRobin,
        "weighted" | "percent" | "%" => ChainStrategy::Weighted(weights.clone()),
        // No explicit strategy but weights were given → weighted; otherwise failover.
        _ if weights.iter().any(|w| *w != 1) => ChainStrategy::Weighted(weights.clone()),
        _ => ChainStrategy::Failover,
    };
    if links.len() == 1 {
        return Some((links.pop().unwrap(), labels.pop().unwrap()));
    }
    let strat_name = match &strategy {
        ChainStrategy::Failover => "failover",
        ChainStrategy::RoundRobin => "round_robin",
        ChainStrategy::Weighted(_) => "weighted",
    };
    // The reasoner (strong model for think:true and blob-escalations): YM_BRAIN_REASONER names a
    // substring to match a link's model; else auto-detect a MoE/large tag; else the LAST link (the
    // convention is primary = fast dispatch model, reasoner = the capable one).
    let want = std::env::var("YM_BRAIN_REASONER")
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let reasoner_idx = if !want.is_empty() {
        labels
            .iter()
            .position(|l| l.to_ascii_lowercase().contains(&want))
    } else {
        labels
            .iter()
            .position(|l| {
                let l = l.to_ascii_lowercase();
                l.contains("a3b") || l.contains("moe") || l.contains("35b") || l.contains(":31b")
            })
            .or(Some(labels.len() - 1))
    };
    let label = format!("brain-pool/{strat_name}[{}]", labels.join(","));
    let mut chain = ChainBackend::new_labeled(links, labels).with_strategy(strategy);
    if let Some(idx) = reasoner_idx {
        chain = chain.with_reasoner(idx);
    }
    Some((Arc::new(chain) as Arc<dyn LLMBackend>, label))
}

pub fn local_backend_from_env() -> Option<(Arc<dyn LLMBackend>, String)> {
    // A config-defined multi-endpoint brain pool takes precedence: it becomes the local lane (private
    // + primary) with the chosen failover / round-robin / weighted backup strategy.
    if let Some(pool) = brain_pool_from_env() {
        return Some(pool);
    }
    let url = std::env::var("YM_LOCAL_OLLAMA_URL")
        .ok()
        .filter(|u| !u.trim().is_empty())?;
    let model = std::env::var("YM_LOCAL_OLLAMA_MODEL")
        .unwrap_or_else(|_| "qwen3.6:35b-a3b-mtp-q4_K_M".to_string());
    // Provider type "ollama" (NOT "openai"): our endpoint is an Ollama server — self-hosted OR
    // fronted by a TLS gateway that doesn't carry the :11434 auto-detect port. The "openai" path
    // POSTs to <url>/chat/completions (missing /v1 → 404, or /v1 → 307 redirect) AND can't turn off
    // the qwen thinking preamble (OpenAI-compat ignores `think`, burning ~10s/turn). The "ollama"
    // preset routes to native /api/chat, sends `think:false` (fast, clean content), passes tools for
    // the agent loop, and needs no auth. YM_LOCAL_OLLAMA_KEY is accepted but unused (auth "none").
    let key = std::env::var("YM_LOCAL_OLLAMA_KEY").unwrap_or_else(|_| "ollama".to_string());
    // Thinking is a per-workload quality/latency lever on qwen3.6 MoE (binary; reasoning_effort
    // levels don't scale — ollama maintainer, 2026-07-21). Blanket thinking-ON measured ~96s even
    // for a trivial turn (the agent loop multiplies the reasoning chain across steps) — unusable
    // for interactive replies. So default OFF for foreground usability; set YM_LOCAL_THINK=on to
    // force it globally. The proper split — thinking ON only on background planning paths — is the
    // follow-up; this env keeps the fast default while the builder plumbing is already in place.
    let think = std::env::var("YM_LOCAL_THINK")
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "on" | "1" | "true" | "yes"
            )
        })
        .unwrap_or(false);
    let label = format!("ollama-local:{model}");
    Some((
        Arc::new(
            yantrik_ml::GenericOpenAIBackend::for_provider("ollama", &url, Some(key), model)
                .with_thinking(think),
        ) as Arc<dyn LLMBackend>,
        label,
    ))
}

/// Per-function model routing. Each role resolves to its own `InferencePool`; an unconfigured role
/// falls back to the `default` pool. Built once at startup; cloning a pool is cheap (shared Arcs).
pub struct Router {
    roles: HashMap<String, InferencePool>,
    default: InferencePool,
}

/// Build one explicit role pool without losing either half of its route identity. The literal
/// `provider:model` spec is operational metadata (privacy reports, attribution, diagnostics), not
/// a backend default: `InferencePool::new` labels itself `scripted`, so failing to set this made
/// every configured role look like the test seam even while it called a real provider.
fn role_pool(
    default: &InferencePool,
    backend: Arc<dyn LLMBackend>,
    concurrency: usize,
    spec: &str,
) -> InferencePool {
    let mut pool = InferencePool::new(backend, concurrency)
        .with_provider(spec.trim())
        .share_ledger_slot(default);
    if let Some((private_backend, private_label)) = default.private_lane() {
        pool = pool.with_private_backend(private_backend, &private_label);
    }
    pool
}

impl Router {
    /// All roles resolve to one pool (tests, single-backend setups).
    pub fn uniform(default: InferencePool) -> Self {
        Self {
            roles: HashMap::new(),
            default,
        }
    }

    /// Read `YM_ROLE_<ROLE>` for each known function; a set+resolvable spec gets its own pool, else
    /// the role uses `default`. The literal env names, for the config schema and for grepping:
    /// YM_ROLE_CHAT, YM_ROLE_RESEARCH, YM_ROLE_UTIL, YM_ROLE_VERIFY, YM_ROLE_CODE,
    /// YM_ROLE_CONSOLIDATE. Spec format is `provider:model` per `backend_from_spec` — e.g.
    /// `nim:deepseek-ai/deepseek-v4`, `groq:llama-3.3-70b-versatile`, `openrouter:<id>:free`.
    pub fn from_env(default: InferencePool, concurrency: usize) -> Self {
        let mut roles = HashMap::new();
        for role in ["chat", "research", "util", "verify", "code", "consolidate"] {
            let var = format!("YM_ROLE_{}", role.to_uppercase());
            if let Ok(spec) = std::env::var(&var) {
                if !spec.trim().is_empty() {
                    if let Some(be) = backend_from_spec(&spec) {
                        // INHERIT the default's private lane. Without this a configured role has
                        // none, so every private-grounded call it serves escalates to the household
                        // lane and is then refused for not being on the allowlist — landing on the
                        // scripted backend, which returns four characters and chooses no tool.
                        //
                        // That is exactly how the planner died: YM_ROLE_UTIL is set, the planner
                        // asks for pool("util"), and the goal came back as "I couldn't turn that
                        // into steps — rephrase it as concrete actions". The phrasing was never the
                        // problem; the pool had no brain to think with.
                        let pool = role_pool(&default, be, concurrency, &spec);
                        roles.insert(role.to_string(), pool);
                    } else {
                        eprintln!("[router] {var}={spec:?} — unknown provider or missing key; using default");
                    }
                }
            }
        }
        Self { roles, default }
    }

    /// The pool for a function role (falls back to the default pool).
    pub fn pool(&self, role: &str) -> InferencePool {
        self.roles
            .get(role)
            .cloned()
            .unwrap_or_else(|| self.default.clone())
    }

    /// Roles that have an explicit (non-default) backend — for startup reporting.
    pub fn configured_roles(&self) -> Vec<String> {
        let mut v: Vec<String> = self.roles.keys().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod privacy_tests {
    use super::*;

    /// A backend that PANICS if any message it receives contains a canary — a mock "cloud" provider
    /// that fails the test the instant private data reaches it. Counts calls so a test can assert 0.
    struct CanaryTrap {
        canary: String,
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl LLMBackend for CanaryTrap {
        fn chat(
            &self,
            messages: &[ChatMessage],
            _c: &GenerationConfig,
            _t: Option<&[serde_json::Value]>,
        ) -> anyhow::Result<LLMResponse> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            for m in messages {
                assert!(
                    !m.content.contains(&self.canary),
                    "PRIVACY LEAK: private canary reached the cloud backend"
                );
            }
            Ok(LLMResponse {
                thinking: String::new(),
                text: "cloud-ok".into(),
                prompt_tokens: 0,
                completion_tokens: 0,
                tool_calls: vec![],
                api_tool_calls: vec![],
                stop_reason: "stop".into(),
            })
        }
        fn chat_streaming(
            &self,
            m: &[ChatMessage],
            c: &GenerationConfig,
            t: Option<&[serde_json::Value]>,
            _: &mut dyn FnMut(&str),
        ) -> anyhow::Result<LLMResponse> {
            self.chat(m, c, t)
        }
        fn count_tokens(&self, t: &str) -> anyhow::Result<usize> {
            Ok(t.len() / 4)
        }
        fn backend_name(&self) -> &str {
            "canary-cloud"
        }
    }

    /// A local backend that always fails — simulates the local Ollama being down/OOM/timing out.
    struct AlwaysDown;
    impl LLMBackend for AlwaysDown {
        fn chat(
            &self,
            _m: &[ChatMessage],
            _c: &GenerationConfig,
            _t: Option<&[serde_json::Value]>,
        ) -> anyhow::Result<LLMResponse> {
            anyhow::bail!("local ollama down")
        }
        fn chat_streaming(
            &self,
            m: &[ChatMessage],
            c: &GenerationConfig,
            t: Option<&[serde_json::Value]>,
            _: &mut dyn FnMut(&str),
        ) -> anyhow::Result<LLMResponse> {
            self.chat(m, c, t)
        }
        fn count_tokens(&self, t: &str) -> anyhow::Result<usize> {
            Ok(t.len() / 4)
        }
        fn backend_name(&self) -> &str {
            "always-down"
        }
    }

    #[test]
    fn explicit_role_pool_keeps_provider_model_identity_and_private_lane() {
        let backend = Arc::new(AlwaysDown) as Arc<dyn LLMBackend>;
        let default = InferencePool::new(backend.clone(), 1)
            .with_provider("default:model")
            .with_private_backend(backend.clone(), "owned:model");

        let role = role_pool(&default, backend, 2, "  nim:deepseek-ai/deepseek-v4  ");

        assert_eq!(role.provider(), "nim:deepseek-ai/deepseek-v4");
        assert!(
            role.has_private_lane(),
            "role pools must retain fail-closed privacy routing"
        );
    }

    #[test]
    fn provider_base_override_is_per_provider_and_pure() {
        let env = |name: &str| match name {
            "YM_PROVIDER_BASE_URL_NIM" => Some(" http://172.30.0.2:8080/v1/ ".to_string()),
            "YM_PROVIDER_BASE_URL_OLLAMA_CLOUD" => Some("   ".to_string()),
            _ => None,
        };
        assert_eq!(
            resolve_provider_base("nim", "https://integrate.api.nvidia.com/v1", env),
            "http://172.30.0.2:8080/v1"
        );
        // an alias spelled differently is a different variable: no silent cross-over
        assert_eq!(
            resolve_provider_base("nvidia", "https://integrate.api.nvidia.com/v1", env),
            "https://integrate.api.nvidia.com/v1"
        );
        // blank override falls back; '-' maps to '_' in the variable name
        assert_eq!(
            resolve_provider_base("ollama-cloud", "https://ollama.com/v1", env),
            "https://ollama.com/v1"
        );
        assert_eq!(
            resolve_provider_base("groq", "https://api.groq.com/openai/v1", env),
            "https://api.groq.com/openai/v1"
        );
    }

    // ── E.CFG1 ───────────────────────────────────────────────────────────────────────────────
    //
    // The bug these pin: a role can name a model its provider does not serve, and nothing says so
    // until a call fails. Measured live — a provider answered 410 eight times for a model absent
    // from its catalogue of 82, while the router reported the role as configured.

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + Copy + 'a {
        move |k: &str| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn the_plan_carries_only_configured_routes_and_names_what_each_would_call() {
        let e = env(&[
            ("YM_ROLE_CHAT", "nim:openai/gpt-oss-120b"),
            ("YM_ROLE_RESEARCH", "  "),
            ("YM_ROLE_VERIFY", "groq"),
            ("NVIDIA_API_KEY", "k"),
            ("GROQ_API_KEY", "k"),
        ]);
        let plan = role_plan(e);
        assert_eq!(
            plan.len(),
            2,
            "a blank spec is not a configured route: {plan:?}"
        );
        assert_eq!(plan[0].role, "chat");
        assert_eq!(plan[0].model, "openai/gpt-oss-120b");
        assert!(!plan[0].model_defaulted);
        assert_eq!(plan[0].base_url, "https://integrate.api.nvidia.com/v1");
        assert_eq!(plan[0].status, SpecStatus::Configured);
        // a spec with no model is a real configuration, and the report must show what it resolves to
        assert_eq!(plan[1].role, "verify");
        assert!(plan[1].model_defaulted);
        assert_eq!(plan[1].model, "llama-3.3-70b-versatile");
    }

    #[test]
    fn unknown_provider_and_missing_key_are_different_findings() {
        // backend_from_spec collapses both to None, so its one message has to say "unknown provider
        // or missing key". They need different fixes, so the diagnostic separates them.
        let e = env(&[
            ("YM_ROLE_CHAT", "nosuchprovider:some-model"),
            ("YM_ROLE_CODE", "groq:llama-3.3-70b-versatile"),
        ]);
        let plan = role_plan(e);
        assert_eq!(plan[0].status, SpecStatus::UnknownProvider);
        assert_eq!(
            plan[1].status,
            SpecStatus::MissingKey {
                key_env: "GROQ_API_KEY"
            }
        );
        let out = render_role_plan(&plan, None);
        assert!(out.contains("UNKNOWN PROVIDER"), "{out}");
        assert!(out.contains("MISSING KEY GROQ_API_KEY"), "{out}");
        assert!(
            out.contains("NOT verified"),
            "the unverified read must say so rather than let silence read as health: {out}"
        );
    }

    #[test]
    fn a_role_naming_a_model_the_provider_does_not_serve_is_reported_as_not_served() {
        // The live case, reproduced exactly: the staging research role names a GLM the provider does
        // not carry, while the chat role names one it does.
        let e = env(&[
            ("YM_ROLE_RESEARCH", "nim:z-ai/glm-5.2"),
            ("YM_ROLE_CHAT", "nim:openai/gpt-oss-120b"),
            ("NVIDIA_API_KEY", "k"),
        ]);
        let plan = role_plan(e);
        let mut fetches = 0;
        let checks = verify_models(&plan, |_base, _p| {
            fetches += 1;
            Ok(vec![
                "openai/gpt-oss-120b".to_string(),
                "deepseek-ai/deepseek-v4-flash-0731".to_string(),
            ])
        });
        assert_eq!(fetches, 1, "one fetch per distinct base URL, not per route");
        let research = plan.iter().position(|r| r.role == "research").unwrap();
        let chat = plan.iter().position(|r| r.role == "chat").unwrap();
        assert_eq!(checks[research], ModelCheck::NotServed { catalogue: 2 });
        assert_eq!(checks[chat], ModelCheck::Served);
        let out = render_role_plan(&plan, Some(&checks));
        assert!(out.contains("NOT SERVED"), "{out}");
        assert!(
            out.contains("fail at request time"),
            "the report must say what happens, not just that something is wrong: {out}"
        );
    }

    #[test]
    fn providers_sharing_a_proxy_do_not_share_a_catalogue_cache_entry() {
        let e = env(&[
            ("YM_ROLE_CHAT", "nim:openai/gpt-oss-120b"),
            ("YM_ROLE_VERIFY", "groq:llama-3.3-70b-versatile"),
            ("NVIDIA_API_KEY", "k"),
            ("GROQ_API_KEY", "k"),
            ("YM_PROVIDER_BASE_URL_NIM", "http://127.0.0.1:8080/v1"),
            ("YM_PROVIDER_BASE_URL_GROQ", "http://127.0.0.1:8080/v1"),
        ]);
        let plan = role_plan(e);
        let mut fetched = Vec::new();
        let checks = verify_models(&plan, |_base, provider| {
            fetched.push(provider.to_string());
            Ok(match provider {
                "nim" => vec!["openai/gpt-oss-120b".to_string()],
                "groq" => vec!["llama-3.3-70b-versatile".to_string()],
                other => panic!("unexpected provider {other}"),
            })
        });

        assert_eq!(fetched, ["nim", "groq"]);
        assert_eq!(checks, [ModelCheck::Served, ModelCheck::Served]);
    }

    #[test]
    fn an_unreachable_catalogue_is_never_reported_as_served() {
        // The failure mode that would make this diagnostic worse than none.
        let e = env(&[
            ("YM_ROLE_CHAT", "nim:openai/gpt-oss-120b"),
            ("NVIDIA_API_KEY", "k"),
        ]);
        let plan = role_plan(e);
        let checks = verify_models(&plan, |_b, _p| Err("connection refused".to_string()));
        assert_eq!(
            checks[0],
            ModelCheck::Unreachable {
                why: "connection refused".to_string()
            }
        );
        let out = render_role_plan(&plan, Some(&checks));
        assert!(out.contains("UNVERIFIED"), "{out}");
        assert!(!out.contains("SERVED by the provider"), "{out}");
    }

    #[test]
    fn a_route_that_builds_nothing_is_not_asked_about() {
        // Asking a provider about a spec that never resolves would produce a confusing NOT SERVED
        // for what is really an unknown provider or a missing key.
        let e = env(&[
            ("YM_ROLE_CHAT", "nosuchprovider:m"),
            ("YM_ROLE_CODE", "groq:m"),
        ]);
        let plan = role_plan(e);
        let mut fetches = 0;
        let checks = verify_models(&plan, |_b, _p| {
            fetches += 1;
            Ok(vec!["m".to_string()])
        });
        assert_eq!(fetches, 0, "nothing to ask when no backend would be built");
        assert!(checks.iter().all(|c| *c == ModelCheck::NotApplicable));
    }

    #[test]
    fn the_verified_read_follows_the_base_url_override() {
        // E.CB2-N points a provider at a local proxy. A check against the vendor's real endpoint
        // would then be answering a question nobody asked.
        let e = env(&[
            ("YM_ROLE_CHAT", "nim:openai/gpt-oss-120b"),
            ("NVIDIA_API_KEY", "k"),
            ("YM_PROVIDER_BASE_URL_NIM", "http://172.30.0.2:8080/v1"),
        ]);
        let plan = role_plan(e);
        assert_eq!(plan[0].base_url, "http://172.30.0.2:8080/v1");
        let mut seen = String::new();
        let _ = verify_models(&plan, |base, _p| {
            seen = base.to_string();
            Ok(vec!["openai/gpt-oss-120b".to_string()])
        });
        assert_eq!(seen, "http://172.30.0.2:8080/v1");
    }

    // ── E.PORT1 ──────────────────────────────────────────────────────────────────────────────

    /// Records what the backend was asked, so the adapter's rewriting can be observed rather than
    /// assumed.
    struct RecordingBackend {
        seen: std::sync::Mutex<Vec<Option<bool>>>,
    }

    impl LLMBackend for RecordingBackend {
        fn chat(
            &self,
            _messages: &[ChatMessage],
            config: &GenerationConfig,
            _tools: Option<&[serde_json::Value]>,
        ) -> anyhow::Result<LLMResponse> {
            self.seen.lock().unwrap().push(config.think);
            Ok(LLMResponse {
                thinking: String::new(),
                text: "ok".into(),
                prompt_tokens: 7,
                completion_tokens: 9,
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
            Ok(text.len())
        }
        fn backend_name(&self) -> &str {
            "recording"
        }
        // The trait's defaults are "unknown" and false. A double that does not override them cannot
        // detect an adapter that forgets to delegate them — which is exactly how the first version
        // of the transparency test passed while `model_id` fell through to the default.
        fn model_id(&self) -> &str {
            "recording-model-id"
        }
        fn is_degraded(&self) -> bool {
            true
        }
    }

    fn strict_over(inner: Arc<RecordingBackend>) -> StrictOpenAiBackend {
        StrictOpenAiBackend {
            inner: inner as Arc<dyn LLMBackend>,
        }
    }

    #[test]
    fn thinking_off_becomes_no_preference_on_the_strict_path() {
        // The measured failure: think=Some(false) makes the backend emit reasoning_effort:"none",
        // which is not in the OpenAI enum, and a provider that validates it answers 400 to EVERY
        // call. Unset is the only representable way to say "no preference" in that protocol.
        let rec = Arc::new(RecordingBackend {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let strict = strict_over(rec.clone());
        let msg = [ChatMessage {
            role: "user".into(),
            content: "hi".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        for want in [Some(false), Some(true), None] {
            let cfg = GenerationConfig {
                think: want,
                ..Default::default()
            };
            strict.chat(&msg, &cfg, None).expect("chat");
        }
        assert_eq!(
            *rec.seen.lock().unwrap(),
            vec![None, Some(true), None],
            "only OFF is rewritten; ON and unset pass through"
        );
    }

    #[test]
    fn the_adapter_changes_nothing_else_about_the_call() {
        // An adapter that quietly dropped tools, messages or the response would trade one silent
        // failure for another.
        let rec = Arc::new(RecordingBackend {
            seen: std::sync::Mutex::new(Vec::new()),
        });
        let strict = strict_over(rec.clone());
        let msg = [ChatMessage {
            role: "user".into(),
            content: "hi".into(),
            tool_calls: None,
            tool_call_id: None,
            name: None,
        }];
        let cfg = GenerationConfig {
            think: Some(false),
            max_tokens: 4321,
            temperature: 0.42,
            ..Default::default()
        };
        let r = strict.chat(&msg, &cfg, None).expect("chat");
        assert_eq!(r.text, "ok");
        assert_eq!((r.prompt_tokens, r.completion_tokens), (7, 9));
        assert_eq!(
            strict.backend_name(),
            "recording",
            "identity is the inner backend's"
        );
        assert_eq!(
            strict.model_id(),
            "recording-model-id",
            "the adapter must delegate model_id; the trait default would make every strict cloud              backend misreport which model answered"
        );
        assert!(
            strict.is_degraded(),
            "the adapter must delegate is_degraded; the trait default hides a degraded backend"
        );
        assert_eq!(strict.count_tokens("abcd").unwrap(), 4);

        // the caller's own config is not mutated
        assert_eq!(cfg.think, Some(false));
        assert_eq!(cfg.max_tokens, 4321);

        // streaming takes the same path and still streams
        let mut streamed = String::new();
        let r = strict
            .chat_streaming(&msg, &cfg, None, &mut |t| streamed.push_str(t))
            .expect("stream");
        assert_eq!(streamed, "ok");
        assert_eq!(r.stop_reason, "stop");
        assert_eq!(
            *rec.seen.lock().unwrap(),
            vec![None, None],
            "the streaming path is adapted too"
        );
    }

    #[test]
    fn the_local_lane_is_not_wrapped_and_keeps_its_measured_behaviour() {
        // The pairing exists because Ollama's compat endpoint ignores `think`. The local lane is
        // built by local_backend_from_env with the "ollama" preset and must never come through
        // backend_from_spec, or the fix would silently undo the 10s → 0.9s measurement.
        // include_str! resolves next to this file, so the guard works from any working directory.
        const SRC: &str = include_str!("lib.rs");
        let local = SRC
            .split("pub fn local_backend_from_env")
            .nth(1)
            .expect("local_backend_from_env exists");
        let local = &local[..local.find("\n}\n").unwrap_or(local.len())];
        assert!(
            local.contains("for_provider(\"ollama\""),
            "the local lane must keep the ollama preset"
        );
        assert!(
            !local.contains("StrictOpenAiBackend"),
            "the local lane must not be wrapped by the strict adapter"
        );
    }

    #[test]
    fn the_endpoints_that_need_the_nonstandard_field_are_not_wrapped() {
        // Review finding: the table this wraps contains `ollama` and `ollama-cloud`, which IGNORE
        // `think` on the compat path and for which the non-standard value is the only thing that
        // suppresses the preamble. Wrapping them would apply a fix for "the provider REJECTS this"
        // to a provider that needs it. (The 10 s → 0.9 s figure often quoted for this belongs to the
        // LOCAL native endpoint; no measurement of ollama.com/v1 exists. A later review found that
        // retraction made in the production comment and the ledger but not here — the same false
        // claim, twenty lines away.)
        assert!(provider_is_ollama_compatible("ollama"));
        assert!(provider_is_ollama_compatible("ollama-cloud"));
        for strict in [
            "nim",
            "nvidia",
            "groq",
            "cerebras",
            "openrouter",
            "nanogpt",
            "minimax",
            "qwencloud",
            "grok",
        ] {
            assert!(
                !provider_is_ollama_compatible(strict),
                "{strict} validates the enum and must stay on the strict path"
            );
        }
        // The real risk is a NEW catalogue entry that nobody classified. Every provider the table
        // knows is listed here with its decision, so adding one without deciding fails this test
        // rather than silently landing on the strict path.
        const CLASSIFIED: &[(&str, bool)] = &[
            ("nanogpt", false),
            ("ollama-cloud", true),
            ("ollama", true),
            ("minimax", false),
            ("qwencloud", false),
            ("qwen", false),
            ("openrouter", false),
            ("nim", false),
            ("nvidia", false),
            ("groq", false),
            ("cerebras", false),
            ("grok", false),
            ("anthropic", false),
        ];
        for (name, ollama_like) in CLASSIFIED {
            assert!(
                provider_catalog(name).is_some(),
                "{name} is not in the provider table any more"
            );
            assert_eq!(
                provider_is_ollama_compatible(name),
                *ollama_like,
                "{name} changed category without the list being updated"
            );
        }
        // and the list is complete: every arm of the table appears above
        let src = include_str!("lib.rs");
        let table = src
            .split("pub fn provider_catalog(")
            .nth(1)
            .expect("the provider table")
            .split(
                "
}",
            )
            .next()
            .expect("its body");
        // Accumulate until the arrow: rustfmt wraps a long or-pattern so the names sit ABOVE the
        // `=> (` line, and matching per-line would then see only the last name — a new provider
        // added to a wrapped arm would land on the strict path unseen. That is the same blindness
        // as the suffix match this replaced, one shape further out; review found both.
        let mut pending = String::new();
        for raw in table.lines() {
            let line = raw.trim();
            if line.starts_with("//") {
                continue; // a comment containing an arrow is not a table arm
            }
            // Only a PATTERN fragment continues an arm: a quoted name, an or-bar, or the
            // wildcard. Anything else (the signature, the opening brace, a body line) resets the
            // accumulator, so the scan cannot glue unrelated text onto the next arm.
            let fragment = line.starts_with('"') || line.starts_with('|') || line.starts_with('_');
            if !line.contains(" => ") {
                if fragment {
                    pending.push_str(line);
                } else {
                    pending.clear();
                }
                continue;
            }
            let joined = if fragment {
                format!("{pending}{line}")
            } else {
                pending.clear();
                line.to_string()
            };
            pending.clear();
            let line = joined.as_str();
            // Match the arrow ANYWHERE in the line, not a line that ENDS with it. Two of the
            // table's arms are single-line — including `"ollama-cloud" | "ollama" => (...)`, the
            // very pair this test exists to protect — so the suffix form skipped exactly the
            // entries that mattered, and a new single-line provider would have landed on the strict
            // path unseen. (The `or_else` it replaces had two byte-identical branches: dead code
            // that read as if it handled the second shape.)
            if let Some(arms) = line.split_once(" => ").map(|(a, _)| a) {
                for name in arms.split('|') {
                    let name = name.trim().trim_matches('"');
                    if name.is_empty() || name == "_" {
                        continue;
                    }
                    assert!(
                        CLASSIFIED.iter().any(|(n, _)| *n == name),
                        "provider {name:?} is in the table but not classified strict or ollama-like"
                    );
                }
            }
        }
    }

    #[test]
    fn a_rendered_url_never_carries_a_credential() {
        // Review finding: an operator override is free-form, so the effective base URL can hold
        // userinfo or a signed query. The plan is console output and lands in reports; printing it
        // verbatim would turn a diagnostic into a credential disclosure.
        const CANARY: &str = "s3cr3t-canary-value";
        for raw in [
            format!("https://user:{CANARY}@api.example.com/v1"),
            format!("https://api.example.com/v1?key={CANARY}"),
            format!("https://api.example.com/v1#{CANARY}"),
            format!("http://{CANARY}@10.0.0.1:8080/v1?token={CANARY}"),
        ] {
            let shown = redact_url(&raw);
            assert!(
                !shown.contains(CANARY),
                "redaction leaked the canary: {raw} -> {shown}"
            );
            assert!(
                shown.contains("api.example.com") || shown.contains("10.0.0.1"),
                "redaction must still name the host: {raw} -> {shown}"
            );
        }
        // an ordinary URL is shown as it is, minus nothing
        assert_eq!(
            redact_url("https://integrate.api.nvidia.com/v1"),
            "https://integrate.api.nvidia.com/v1"
        );
        assert_eq!(
            redact_url("http://172.30.0.2:8080/v1"),
            "http://172.30.0.2:8080/v1"
        );
    }

    #[test]
    fn the_whole_report_is_free_of_the_credential_in_an_override() {
        // The end-to-end version of the check: a canary in BOTH userinfo and query, carried through
        // role_plan into the rendered report, with the model check unreachable so the error path is
        // exercised too. Zero hits anywhere in the output.
        const CANARY: &str = "s3cr3t-canary-value";
        let override_url = format!("https://u:{CANARY}@proxy.internal/v1?token={CANARY}");
        let pairs: Vec<(&str, String)> = vec![
            ("YM_ROLE_CHAT", "nim:openai/gpt-oss-120b".to_string()),
            ("NVIDIA_API_KEY", "k".to_string()),
            ("YM_PROVIDER_BASE_URL_NIM", override_url.clone()),
        ];
        let lookup = |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        let plan = role_plan(lookup);
        assert_eq!(
            plan[0].base_url, override_url,
            "the RUNTIME url is unchanged; only the rendering is reduced"
        );
        let checks = verify_models(&plan, |_b, _p| {
            // an error text that echoes the URL, which is exactly what ureq's Display does
            Err(format!("failed to fetch {override_url}"))
        });
        let out = render_role_plan(&plan, Some(&checks));
        assert!(
            !out.contains(CANARY),
            "the report disclosed the canary:\n{out}"
        );
        assert!(out.contains("proxy.internal"), "{out}");
    }

    #[test]
    fn no_configured_route_says_so_plainly() {
        let plan = role_plan(env(&[]));
        assert!(plan.is_empty());
        let out = render_role_plan(&plan, None);
        assert!(out.contains("none configured"), "{out}");
        assert!(out.contains("default chain"), "{out}");
    }

    #[test]
    fn primary_brain_spec_is_trimmed_and_blank_is_none() {
        assert_eq!(
            primary_brain_spec(|_| Some("  nim:z-ai/glm-5.2 ".into())),
            Some("nim:z-ai/glm-5.2".to_string())
        );
        assert_eq!(primary_brain_spec(|_| Some("   ".into())), None);
        assert_eq!(primary_brain_spec(|_| None), None);
    }

    /// THE LEAK-PROOF INVARIANT (sol 019f8287): for a Private-grounded turn, ZERO bytes reach a cloud
    /// provider when the local private lane is down — the turn FAILS CLOSED, never escalates. Proven
    /// with a canary the cloud mock panics on and a call-counter asserted to stay 0.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn private_grounded_fails_closed_never_leaks_to_cloud() {
        let canary = "SECRET-CANARY-alice-oncology-47-12-33";
        let cloud_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloud = Arc::new(CanaryTrap {
            canary: canary.into(),
            calls: cloud_calls.clone(),
        }) as Arc<dyn LLMBackend>;
        let local_down = Arc::new(AlwaysDown) as Arc<dyn LLMBackend>;
        // Default/household backend = the cloud trap; PRIVATE lane = the (failing) local-only backend.
        let pool = InferencePool::new(cloud, 1)
            .with_provider("canary-cloud")
            .with_private_backend(local_down, "ollama-local");
        assert!(pool.has_private_lane());

        let messages = vec![ChatMessage::user(format!("remember: {canary}"))];
        let res = pool
            .chat_grounded_tools(messages, GenerationConfig::default(), Vec::new())
            .await;

        // The private lane failed → the turn FAILS CLOSED (Err), and the cloud backend was NEVER called.
        assert!(
            res.is_err(),
            "a down private lane must fail closed, not silently succeed via cloud"
        );
        assert!(
            res.unwrap_err()
                .to_string()
                .contains("refusing to route private context"),
            "explicit fail-closed reason"
        );
        assert_eq!(
            cloud_calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "PRIVACY LEAK: the cloud backend was called for a private-grounded turn"
        );
    }

    // (The no-private-lane escalation path — the documented interim gap — is covered by the existing
    // `chat_grounded_prefers_private_and_audits_escalation` test; not duplicated here because the
    // process-global PRIVACY_ESCALATED counter makes a second escalating test collide with it.)

    /// REAL-MODEL smoke test for the native function-calling path — the one thing the scripted eval
    /// suite structurally cannot prove (SequencedLLM fakes the model). Drives the actual
    /// ApiLLM(Ollama) backend through chat_scoped_tools with an agent-loop-shaped schema and asserts
    /// a STRUCTURED tool call comes back parsed. Ignored by default (needs the homelab desktop's
    /// Ollama up); run manually: cargo test -p mind-inference real_model -- --ignored --nocapture
    /// Override the endpoint/model with YM_SMOKE_OLLAMA_URL / YM_SMOKE_OLLAMA_MODEL.
    #[tokio::test]
    #[ignore = "needs a live local Ollama with a tool-calling model"]
    async fn real_model_native_tool_call_roundtrip() {
        let url = std::env::var("YM_SMOKE_OLLAMA_URL")
            .unwrap_or_else(|_| "http://192.168.4.35:11434".into());
        let model = std::env::var("YM_SMOKE_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3.6:27b".into());
        let backend = yantrik_ml::ApiLLM::new(url, None, model);
        let pool = InferencePool::new(Arc::new(backend) as Arc<dyn LLMBackend>, 1)
            .with_provider("ollama-local");
        let tools = vec![serde_json::json!({"type":"function","function":{
            "name":"weather","description":"current conditions + today's forecast for a city/town",
            "parameters":{"type":"object","properties":{"place":{"description":"place"}},
                          "required":["place"],"additionalProperties":true}}})];
        let messages = vec![
            ChatMessage::system(
                "You are an agent, not a chatbot — you ACT. Use ONE tool, observe, then answer.",
            ),
            ChatMessage::user("what's the weather in pune?"),
        ];
        // Public scope: the smoke prompt carries no private data, and Public routes to any provider.
        let r = pool
            .chat_scoped_tools(
                messages,
                GenerationConfig::default(),
                PrivacyScope::Public,
                tools,
            )
            .await
            .expect("live ollama chat");
        let tc = r
            .tool_calls
            .first()
            .expect("the model should return a STRUCTURED tool call");
        assert_eq!(
            tc.name, "weather",
            "picked the offered tool: {:?}",
            r.tool_calls
        );
        assert!(
            tc.arguments
                .get("place")
                .and_then(|v| v.as_str())
                .is_some_and(|s| s.to_lowercase().contains("pune")),
            "parsed structured args carry the place: {:?}",
            tc.arguments
        );
    }

    #[test]
    fn lanes_route_correctly() {
        let hh = "minimax,nanogpt,scripted";
        let pv = "";
        assert!(scope_allows(PrivacyScope::Public, "minimax", hh, pv));
        assert!(scope_allows(PrivacyScope::Public, "anything", hh, pv));
        assert!(scope_allows(
            PrivacyScope::Household,
            "nanogpt -> minimax",
            hh,
            pv
        ));
        assert!(!scope_allows(
            PrivacyScope::Household,
            "random-cloud",
            hh,
            pv
        ));
        assert!(!scope_allows(PrivacyScope::Private, "minimax", hh, pv));
        assert!(!scope_allows(PrivacyScope::Private, "scripted", hh, pv));
        assert!(scope_allows(
            PrivacyScope::Private,
            "ollama-local:qwen3",
            hh,
            "ollama-local"
        ));
        assert!(!scope_allows(
            PrivacyScope::Private,
            "minimax",
            hh,
            "ollama-local"
        ));
    }

    #[tokio::test]
    async fn chat_grounded_prefers_private_and_audits_escalation() {
        // a cloud-only pool with NO private provider configured → private-grounded turn escalates,
        // still returns a reply (never breaks the turn), and the escalation is counted honestly.
        let pool = InferencePool::new(
            std::sync::Arc::new(ScriptedLLM::new("answer")) as std::sync::Arc<dyn LLMBackend>,
            1,
        )
        .with_provider("minimax");
        let before = PRIVACY_ESCALATED.load(std::sync::atomic::Ordering::Relaxed);
        let out = pool
            .chat_grounded(
                vec![ChatMessage::user("private family context")],
                GenerationConfig::default(),
            )
            .await;
        assert!(out.is_ok(), "chat_grounded must never break the turn");
        let after = PRIVACY_ESCALATED.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            after,
            before + 1,
            "the cloud escalation of a private-grounded turn must be counted"
        );
    }

    #[tokio::test]
    async fn private_scope_refuses_on_cloud_pool() {
        let pool = InferencePool::new(
            std::sync::Arc::new(ScriptedLLM::new("leak")) as std::sync::Arc<dyn LLMBackend>,
            1,
        )
        .with_provider("minimax");
        let out = pool
            .chat_scoped(
                vec![ChatMessage::user("family secret")],
                GenerationConfig::default(),
                PrivacyScope::Private,
            )
            .await;
        assert!(
            out.is_err(),
            "private scope must refuse a cloud-labeled pool"
        );
        let ok = pool
            .chat_scoped(
                vec![ChatMessage::user("hi")],
                GenerationConfig::default(),
                PrivacyScope::Household,
            )
            .await;
        assert!(ok.is_ok());
    }

    /// A lane counter without a producer identity cannot distinguish an allowlisted caller from a
    /// regression. The attributed entry point must update both the structured rows and the report.
    #[tokio::test]
    async fn household_calls_are_attributed_at_dispatch() {
        const SITE: &str = "mind-inference/test:household-attribution";
        let before = household_callsite_stats()
            .into_iter()
            .find_map(|(site, count)| (site == SITE).then_some(count))
            .unwrap_or(0);
        let pool = InferencePool::new(
            std::sync::Arc::new(ScriptedLLM::new("ok")) as std::sync::Arc<dyn LLMBackend>,
            1,
        );

        let out = pool
            .chat_household_attributed(
                vec![ChatMessage::user("public test fact")],
                GenerationConfig::default(),
                SITE,
            )
            .await;

        assert!(out.is_ok());
        let after = household_callsite_stats()
            .into_iter()
            .find_map(|(site, count)| (site == SITE).then_some(count))
            .unwrap_or(0);
        assert_eq!(after, before + 1, "the dispatch boundary owns the count");
        assert!(
            privacy_report("scripted").contains(&format!("{SITE} {after}")),
            "the operator-facing audit must name the producer"
        );
    }

    #[test]
    fn privacy_report_never_calls_pre_dispatch_attempts_service() {
        let report = privacy_report("scripted");
        // Reconciled to the shipped upstream wording during the fold (Codex's map: mind-inference
        // wording is authoritative from upstream). Both sides fixed the same thing; this keeps the
        // intent — the pre-dispatch count reads as exposure, never as service.
        assert!(report.contains("dispatched (exposure"));
        assert!(report.contains("household dispatch sites"));
        assert!(
            !report
                .lines()
                .any(|line| line.trim_start().starts_with("served")),
            "pre-dispatch counters are not proof that a provider answered: {report}"
        );
    }

    #[tokio::test]
    async fn a_failed_allowed_backend_is_a_dispatch_attempt_not_service() {
        const SITE: &str = "mind-inference/test:failed-dispatch-attempt";
        let before = household_callsite_stats()
            .into_iter()
            .find_map(|(site, count)| (site == SITE).then_some(count))
            .unwrap_or(0);
        let pool = InferencePool::new(
            std::sync::Arc::new(AlwaysDown) as std::sync::Arc<dyn LLMBackend>,
            1,
        );

        let out = pool
            .chat_household_attributed(
                vec![ChatMessage::user("this backend must fail")],
                GenerationConfig::default(),
                SITE,
            )
            .await;

        assert!(out.is_err(), "the scripted backend always fails");
        let after = household_callsite_stats()
            .into_iter()
            .find_map(|(site, count)| (site == SITE).then_some(count))
            .unwrap_or(0);
        assert_eq!(after, before + 1, "the failed dispatch is still audited");
        let report = privacy_report("scripted");
        assert!(report.contains(&format!("{SITE} {after}")));
        assert!(
            !report
                .lines()
                .any(|line| line.trim_start().starts_with("served")),
            "a backend error must never become a rendered service claim: {report}"
        );
    }

    /// Attribution describes traffic that actually crossed the lane gate, not attempts the charter
    /// refused. Otherwise a denied provider would look like served Household traffic and send an
    /// operator investigating a non-event.
    #[tokio::test]
    async fn refused_household_calls_do_not_create_attribution_rows() {
        const SITE: &str = "mind-inference/test:refused-household-attribution";
        let count = || {
            household_callsite_stats()
                .into_iter()
                .find_map(|(site, count)| (site == SITE).then_some(count))
                .unwrap_or(0)
        };
        let before = count();
        let pool = InferencePool::new(
            std::sync::Arc::new(ScriptedLLM::new("must not run")) as std::sync::Arc<dyn LLMBackend>,
            1,
        )
        .with_provider("not-on-the-household-allowlist");

        let out = pool
            .chat_household_attributed(
                vec![ChatMessage::user("public refusal test fact")],
                GenerationConfig::default(),
                SITE,
            )
            .await;

        assert!(
            out.is_err(),
            "the Household charter must refuse the provider"
        );
        assert_eq!(
            count(),
            before,
            "a refusal is not served traffic and must not gain a call-site row"
        );
    }

    /// The compatibility `chat` entry point must not create a blind remainder between the
    /// Household lane total and the per-producer rows. Its producer is intentionally coarse, but
    /// visible: callers can be migrated without making today's traffic unauditable meanwhile.
    #[tokio::test]
    async fn compatibility_household_calls_are_visibly_unattributed() {
        let count = || {
            household_callsite_stats()
                .into_iter()
                .find_map(|(site, count)| (site == "unattributed").then_some(count))
                .unwrap_or(0)
        };
        let before = count();
        let pool = InferencePool::new(
            std::sync::Arc::new(ScriptedLLM::new("ok")) as std::sync::Arc<dyn LLMBackend>,
            1,
        );

        let out = pool
            .chat(
                vec![ChatMessage::user("public compatibility test fact")],
                GenerationConfig::default(),
            )
            .await;

        assert!(out.is_ok());
        assert!(
            count() > before,
            "a compatibility Household call must land in the visible unattributed bucket"
        );
    }

    /// An attributed API with an empty code-authored label is attribution in name only. Keep the
    /// operator surface honest by folding it into the visible compatibility bucket.
    #[tokio::test]
    async fn blank_household_attribution_is_never_a_blank_dashboard_row() {
        let count = || {
            household_callsite_stats()
                .into_iter()
                .find_map(|(site, count)| (site == "unattributed").then_some(count))
                .unwrap_or(0)
        };
        let before = count();
        let pool = InferencePool::new(
            std::sync::Arc::new(ScriptedLLM::new("ok")) as std::sync::Arc<dyn LLMBackend>,
            1,
        );

        let out = pool
            .chat_household_attributed(
                vec![ChatMessage::user("public blank-label test fact")],
                GenerationConfig::default(),
                "   ",
            )
            .await;

        assert!(out.is_ok());
        assert!(
            count() > before,
            "blank labels must join the unattributed bucket"
        );
        assert!(
            household_callsite_stats()
                .iter()
                .all(|(site, _)| !site.trim().is_empty()),
            "the audit surface must never contain an unnamed producer"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    // ── E.OBS1c: "served by" is a post-success fact ──────────────────────────────────────────
    //
    // The lane observer is a process-wide OnceLock; these fixtures install ONE collector for the
    // whole test binary (first install wins — same rule production plays by) and every fixture
    // reads its own events by draining after its call. Serialized by a mutex so interleaved
    // fixtures cannot read each other's events.

    static LANE_EVENTS: std::sync::Mutex<Vec<(String, String)>> = std::sync::Mutex::new(Vec::new());
    static LANE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn install_lane_collector() {
        set_lane_observer(Box::new(|scope, label| {
            LANE_EVENTS
                .lock()
                .unwrap()
                .push((scope.to_string(), label.to_string()));
        }));
    }

    /// A link that always errors — the dead first hop of the failover fixture.
    struct DeadLink;
    impl LLMBackend for DeadLink {
        fn chat(
            &self,
            _m: &[ChatMessage],
            _c: &GenerationConfig,
            _t: Option<&[serde_json::Value]>,
        ) -> anyhow::Result<LLMResponse> {
            anyhow::bail!("connection refused (scripted dead link)")
        }
        fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
            Ok(text.len() / 4)
        }
        fn backend_name(&self) -> &str {
            "deadlink"
        }
        fn chat_streaming(
            &self,
            _m: &[ChatMessage],
            _c: &GenerationConfig,
            _t: Option<&[serde_json::Value]>,
            _on_token: &mut dyn FnMut(&str),
        ) -> anyhow::Result<LLMResponse> {
            anyhow::bail!("connection refused (scripted dead link)")
        }
    }

    /// Kill criterion (1): first link fails, second answers — exactly ONE lane event, naming the
    /// SECOND link's label, never the joined route.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_badge_names_the_link_that_answered_not_the_route() {
        let _hold = LANE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        install_lane_collector();
        LANE_EVENTS.lock().unwrap().clear();

        let chain = ChainBackend::new_labeled(
            vec![
                Arc::new(DeadLink) as Arc<dyn LLMBackend>,
                Arc::new(ScriptedLLM::new("answered")) as Arc<dyn LLMBackend>,
            ],
            vec!["deadlink".into(), "goodlink:with/colons".into()],
        );
        let pool = InferencePool::new(Arc::new(chain) as Arc<dyn LLMBackend>, 1)
            .with_provider("chain[deadlink -> goodlink:with/colons]");
        let r = pool
            .chat_scoped(
                vec![ChatMessage::user("hi")],
                GenerationConfig::default(),
                PrivacyScope::Public,
            )
            .await
            .expect("second link answers");
        assert_eq!(r.text, "answered");

        // The collector is process-wide and OTHER suite tests make pool calls concurrently, so
        // fixtures assert over their own UNIQUE labels, never over global counts.
        let events = LANE_EVENTS.lock().unwrap().clone();
        let mine: Vec<_> = events
            .iter()
            .filter(|(_, l)| {
                l.contains("goodlink") || l.contains("deadlink") || l.contains("chain[")
            })
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "exactly one served event for this chain: {mine:?}"
        );
        assert_eq!(mine[0].0, "public");
        assert_eq!(
            mine[0].1, "goodlink:with/colons",
            "the SERVING link, colons intact, never the route or the dead link: {mine:?}"
        );
    }

    /// Kill criterion (2): every link fails — no success-shaped lane event at all.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn total_failure_never_wears_a_served_chip() {
        let _hold = LANE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        install_lane_collector();
        LANE_EVENTS.lock().unwrap().clear();

        let chain = ChainBackend::new_labeled(
            vec![
                Arc::new(DeadLink) as Arc<dyn LLMBackend>,
                Arc::new(DeadLink) as Arc<dyn LLMBackend>,
            ],
            vec!["deadlink-a".into(), "deadlink-b".into()],
        );
        let pool = InferencePool::new(Arc::new(chain) as Arc<dyn LLMBackend>, 1)
            .with_provider("chain[deadlink-a -> deadlink-b]");
        let out = pool
            .chat_scoped(
                vec![ChatMessage::user("hi")],
                GenerationConfig::default(),
                PrivacyScope::Public,
            )
            .await;
        assert!(out.is_err(), "every link is dead");

        let events = LANE_EVENTS.lock().unwrap().clone();
        let mine: Vec<_> = events
            .iter()
            .filter(|(_, l)| l.contains("deadlink") || l.contains("chain["))
            .collect();
        assert!(
            mine.is_empty(),
            "no provider answered, so nothing may claim to have served: {mine:?}"
        );
    }

    /// E.OBS follow-on (Codex): the exposure counters must never be RENDERED as "served" — that
    /// word is reserved for the post-success answer fact, and calling a pre-dispatch count "served"
    /// misleads an operator even while the chip is correct.
    #[test]
    fn the_privacy_report_never_calls_the_exposure_count_served() {
        let report = privacy_report("cloud-main");
        assert!(
            !report.to_lowercase().contains("served"),
            "the exposure count must render as dispatched/exposure, never served: {report}"
        );
        assert!(
            report.contains("dispatched") || report.contains("exposure"),
            "the exposure semantics must be named: {report}"
        );
    }

    /// Codex's c504228 review fixture: a PRIVATE call served by the private lane must badge the
    /// PRIVATE label — never the pool's main provider, which on that path is the household/cloud
    /// backend's name and would be a privacy-misleading badge on exactly the turn that stayed home.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_private_turn_badges_the_private_label_never_the_cloud_main() {
        let _hold = LANE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        install_lane_collector();
        LANE_EVENTS.lock().unwrap().clear();

        // No YM_PRIVATE_PROVIDERS set: the dedicated private backend is SANCTIONED by construction
        // and needs no allowlist value — and mutating a process-global env var would leak into
        // parallel/later tests (Codex's test-hygiene finding).
        let pool = InferencePool::new(
            Arc::new(ScriptedLLM::new("cloud answer")) as Arc<dyn LLMBackend>,
            1,
        )
        .with_provider("cloud-main-fixture")
        .with_private_backend(
            Arc::new(ScriptedLLM::new("stayed home")) as Arc<dyn LLMBackend>,
            "owned-fixture:model",
        );
        let r = pool
            .chat_scoped(
                vec![ChatMessage::user("hi")],
                GenerationConfig::default(),
                PrivacyScope::Private,
            )
            .await
            .unwrap();
        assert_eq!(r.text, "stayed home");
        let events = LANE_EVENTS.lock().unwrap().clone();
        let mine: Vec<_> = events
            .iter()
            .filter(|(_, l)| l.contains("fixture"))
            .collect();
        assert_eq!(mine.len(), 1, "one private served event: {mine:?}");
        assert_eq!(mine[0].0, "private");
        assert_eq!(
            mine[0].1, "owned-fixture:model",
            "the PRIVATE lane's own label, never cloud-main: {mine:?}"
        );
    }

    /// Kill criterion (3): a single-provider pool still names its provider on success — the
    /// configured label IS the server when there is no chain.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_single_backend_pool_names_its_provider() {
        let _hold = LANE_TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        install_lane_collector();
        LANE_EVENTS.lock().unwrap().clear();

        let pool = InferencePool::new(
            Arc::new(ScriptedLLM::new("plain")) as Arc<dyn LLMBackend>,
            1,
        )
        .with_provider("solo-fixture-provider");
        let r = pool
            .chat_scoped(
                vec![ChatMessage::user("hi")],
                GenerationConfig::default(),
                PrivacyScope::Public,
            )
            .await
            .unwrap();
        assert_eq!(r.text, "plain");
        let events = LANE_EVENTS.lock().unwrap().clone();
        let mine: Vec<_> = events
            .iter()
            .filter(|(_, l)| l == "solo-fixture-provider")
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "the single backend's configured label serves: {mine:?}"
        );
    }

    fn resp(text: &str) -> LLMResponse {
        LLMResponse {
            thinking: String::new(),
            text: text.to_string(),
            prompt_tokens: 0,
            completion_tokens: 0,
            tool_calls: vec![],
            api_tool_calls: vec![],
            stop_reason: "stop".into(),
        }
    }

    #[test]
    fn configured_api_key_trims_surrounding_whitespace() {
        let key_env = "YM_TEST_PADDED_API_KEY";
        std::env::set_var(key_env, "  valid-key\n");
        assert_eq!(configured_api_key(key_env).as_deref(), Some("valid-key"));
        std::env::remove_var(key_env);
    }

    #[test]
    fn weighted_index_distributes_by_share() {
        let w = vec![70u32, 30];
        let mut counts = [0usize; 2];
        for c in 0..100 {
            counts[weighted_index(&w, 2, c)] += 1;
        }
        assert_eq!(
            counts,
            [70, 30],
            "70/30 weights → 70/30 split over a full window"
        );
        // Degenerate weights never panic and fall back to the first link.
        assert_eq!(weighted_index(&[0, 0], 2, 5), 0);
        assert_eq!(weighted_index(&[], 3, 9), 0);
    }

    #[test]
    fn round_robin_rotates_start_and_weighted_respects_share() {
        let mk = || -> Vec<Arc<dyn LLMBackend>> {
            vec![
                Arc::new(ScriptedLLM::new("A")) as Arc<dyn LLMBackend>,
                Arc::new(ScriptedLLM::new("B")) as Arc<dyn LLMBackend>,
                Arc::new(ScriptedLLM::new("C")) as Arc<dyn LLMBackend>,
            ]
        };
        let labels = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let cfg = GenerationConfig::default();

        // Round-robin: the FIRST link tried rotates A→B→C→A across calls.
        let rr = ChainBackend::new_labeled(mk(), labels.clone())
            .with_strategy(ChainStrategy::RoundRobin);
        let got: Vec<String> = (0..4)
            .map(|_| rr.chat(&[], &cfg, None).unwrap().text)
            .collect();
        assert_eq!(got, vec!["A", "B", "C", "A"]);

        // Failover: always starts at link 0 (each scripted link succeeds, so always "A").
        let fo =
            ChainBackend::new_labeled(mk(), labels.clone()).with_strategy(ChainStrategy::Failover);
        assert_eq!(fo.chat(&[], &cfg, None).unwrap().text, "A");
        assert_eq!(fo.chat(&[], &cfg, None).unwrap().text, "A");

        // Weighted [2,0,1] over 3 calls: first link twice, then the third — never the zero-weight one.
        let wt = ChainBackend::new_labeled(mk(), labels)
            .with_strategy(ChainStrategy::Weighted(vec![2, 0, 1]));
        let got: Vec<String> = (0..3)
            .map(|_| wt.chat(&[], &cfg, None).unwrap().text)
            .collect();
        assert_eq!(got, vec!["A", "A", "C"]);
    }

    /// Owned hardware gets room; the cloud keeps its budget.
    ///
    /// Measured 2026-08-14 on the local pool: a call costs a fixed 14–28 s, and `num_predict=100`
    /// costs the same as `num_predict=1`. So on local the cap buys nothing while truncation costs a
    /// whole turn — an unparseable tool call, or an empty answer whose budget went to `<think>`.
    /// On a cloud link the same tokens are the invoice, so nothing there may change.
    #[test]
    fn a_local_link_gets_a_generous_budget_and_the_cloud_does_not() {
        let floor = local_min_tokens();
        let small = GenerationConfig {
            max_tokens: 300,
            ..GenerationConfig::default()
        };

        // Local: raised to the floor, and everything else about the config is preserved.
        let raised = local_budget("ollama-local:qwen3.6:35b-a3b-mtp-q4_K_M", &small)
            .expect("a 300-token cap on owned hardware must be raised");
        assert_eq!(raised.max_tokens, floor);
        assert_eq!(
            raised.temperature, small.temperature,
            "only the budget changes"
        );
        assert_eq!(raised.think, small.think);

        // Cloud: untouched, because there tokens are the bill.
        for cloud in [
            "nanogpt:deepseek/deepseek-v4-pro",
            "minimax",
            "ollama-cloud",
            "qwen-cloud",
        ] {
            assert!(
                local_budget(cloud, &small).is_none(),
                "{cloud} must keep its budget"
            );
        }

        // Already generous — nothing to do (never LOWER a caller's budget).
        let big = GenerationConfig {
            max_tokens: floor + 5_000,
            ..GenerationConfig::default()
        };
        assert!(local_budget("ollama-local:gemma4:e4b", &big).is_none());

        // Deliberate brevity is exempt: these callers want one word or one line, and a paragraph
        // where the code expects a token is its own bug.
        for tiny in [12, 80, 90, DELIBERATE_BREVITY - 1] {
            let cfg = GenerationConfig {
                max_tokens: tiny,
                ..GenerationConfig::default()
            };
            assert!(
                local_budget("ollama-local:gemma4:e4b", &cfg).is_none(),
                "a {tiny}-token cap is a deliberate one-liner, not a truncation risk"
            );
        }
        // …and the boundary itself IS a reply budget, so it gets raised.
        let boundary = GenerationConfig {
            max_tokens: DELIBERATE_BREVITY,
            ..GenerationConfig::default()
        };
        assert!(local_budget("ollama-local:gemma4:e4b", &boundary).is_some());
    }

    #[test]
    fn reasoner_routes_think_true_but_dispatch_stays_primary() {
        let links: Vec<Arc<dyn LLMBackend>> = vec![
            Arc::new(ScriptedLLM::new("FAST")) as Arc<dyn LLMBackend>,
            Arc::new(ScriptedLLM::new("REASONER")) as Arc<dyn LLMBackend>,
        ];
        let chain = ChainBackend::new_labeled(links, vec!["fast".into(), "reasoner".into()])
            .with_reasoner(1);
        let mut cfg = GenerationConfig {
            think: Some(false),
            ..GenerationConfig::default()
        };
        // Dispatch (think:false / None) stays on the primary (fast) link.
        assert_eq!(chain.chat(&[], &cfg, None).unwrap().text, "FAST");
        cfg.think = None;
        assert_eq!(chain.chat(&[], &cfg, None).unwrap().text, "FAST");
        // Reasoning (think:true) routes to the reasoner link first.
        cfg.think = Some(true);
        assert_eq!(chain.chat(&[], &cfg, None).unwrap().text, "REASONER");
        // prefer_reasoner routes to the reasoner even with think:false (escalation path).
        cfg.think = Some(false);
        cfg.prefer_reasoner = true;
        assert_eq!(chain.chat(&[], &cfg, None).unwrap().text, "REASONER");
    }

    #[test]
    fn anthropic_spec_uses_anthropic_auth_backend() {
        let key_env = "ANTHROPIC_API_KEY";
        let previous = std::env::var_os(key_env);
        std::env::set_var(key_env, "test-key");
        let backend = backend_from_spec("anthropic:claude-test").expect("configured backend");
        match previous {
            Some(value) => std::env::set_var(key_env, value),
            None => std::env::remove_var(key_env),
        }

        assert_eq!(backend.backend_name(), "anthropic");
    }


    /// E.LOOP-J WIRING: a real served call through the pool must record its own rate.
    ///
    /// Every other case in this module calls `note_generation_rate` directly, so all of them would
    /// still pass with the recording deleted from the pool's served path — the store would simply
    /// never be fed in production and the budget would quietly stay on the guess forever. This is
    /// the only case that fails if the two are not connected.
    #[tokio::test]
    async fn a_served_call_records_its_own_rate() {
        struct Counting;
        impl LLMBackend for Counting {
            fn chat(
                &self,
                _: &[ChatMessage],
                _: &GenerationConfig,
                _: Option<&[serde_json::Value]>,
            ) -> anyhow::Result<LLMResponse> {
                // An in-process backend returns in under a millisecond, and `latency_ms` is then 0
                // — which `note_generation_rate` correctly refuses, since a rate cannot be derived
                // from a zero duration. The first version of this case failed for exactly that
                // reason and looked like broken wiring. Real generations take seconds.
                std::thread::sleep(std::time::Duration::from_millis(10));
                Ok(LLMResponse {
                    thinking: String::new(),
                    text: "generated".into(),
                    prompt_tokens: 100,
                    completion_tokens: 3_000,
                    tool_calls: vec![],
                    api_tool_calls: vec![],
                    stop_reason: "stop".into(),
                })
            }
            fn chat_streaming(
                &self,
                m: &[ChatMessage],
                c: &GenerationConfig,
                tl: Option<&[serde_json::Value]>,
                _: &mut dyn FnMut(&str),
            ) -> anyhow::Result<LLMResponse> {
                self.chat(m, c, tl)
            }
            fn count_tokens(&self, s: &str) -> anyhow::Result<usize> {
                Ok(s.len() / 4)
            }
            fn backend_name(&self) -> &str {
                "counting"
            }
        }

        reset_observed_rates_for_test();
        let pool = InferencePool::new(Arc::new(Counting) as Arc<dyn LLMBackend>, 1);
        for _ in 0..MIN_RATE_SAMPLES {
            let _ = pool
                .chat(
                    vec![ChatMessage::user("go")],
                    GenerationConfig::default(),
                )
                .await;
        }
        let observed = observed_tokens_per_second();
        reset_observed_rates_for_test();
        assert!(
            observed.is_some(),
            "{} served calls reporting 3,000 completion tokens each recorded no rate — the pool's              served path is not feeding the store, so the budget can never leave the guess",
            MIN_RATE_SAMPLES
        );
    }

    /// Configurable test backend: `None` => errors, `Some("")` => empty reply, `Some(x)` => Ok(x).
    struct TestBE {
        reply: Option<String>,
        name: String,
    }
    impl LLMBackend for TestBE {
        fn chat(
            &self,
            _: &[ChatMessage],
            _: &GenerationConfig,
            _: Option<&[serde_json::Value]>,
        ) -> anyhow::Result<LLMResponse> {
            match &self.reply {
                None => anyhow::bail!("{} boom", self.name),
                Some(t) => Ok(resp(t)),
            }
        }
        fn chat_streaming(
            &self,
            m: &[ChatMessage],
            c: &GenerationConfig,
            t: Option<&[serde_json::Value]>,
            _: &mut dyn FnMut(&str),
        ) -> anyhow::Result<LLMResponse> {
            self.chat(m, c, t)
        }
        fn count_tokens(&self, s: &str) -> anyhow::Result<usize> {
            Ok(s.len() / 4)
        }
        fn backend_name(&self) -> &str {
            &self.name
        }
    }

    #[test]
    fn chain_falls_over_past_error_and_empty_then_errors_when_all_dead() {
        let chain = ChainBackend::new(vec![
            Arc::new(TestBE {
                reply: None,
                name: "err".into(),
            }),
            Arc::new(TestBE {
                reply: Some(String::new()),
                name: "empty".into(),
            }),
            Arc::new(TestBE {
                reply: Some("hello from C".into()),
                name: "good".into(),
            }),
        ]);
        let out = chain
            .chat(
                &[ChatMessage::user("hi")],
                &GenerationConfig::default(),
                None,
            )
            .unwrap();
        assert_eq!(
            out.text, "hello from C",
            "chain should skip err+empty links to the first usable reply"
        );

        let dead = ChainBackend::new(vec![
            Arc::new(TestBE {
                reply: None,
                name: "e1".into(),
            }),
            Arc::new(TestBE {
                reply: None,
                name: "e2".into(),
            }),
        ]);
        assert!(
            dead.chat(
                &[ChatMessage::user("hi")],
                &GenerationConfig::default(),
                None
            )
            .is_err(),
            "all-dead chain must error"
        );
    }

    /// A backend whose `chat` blocks the calling thread and records peak concurrency.
    struct ConcBackend {
        active: Arc<AtomicUsize>,
        max: Arc<AtomicUsize>,
        delay_ms: u64,
    }
    impl LLMBackend for ConcBackend {
        fn chat(
            &self,
            messages: &[ChatMessage],
            _config: &GenerationConfig,
            _tools: Option<&[serde_json::Value]>,
        ) -> anyhow::Result<LLMResponse> {
            let cur = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(cur, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(self.delay_ms));
            self.active.fetch_sub(1, Ordering::SeqCst);
            Ok(resp(&format!(
                "echo:{}",
                messages.last().map_or("", |m| m.content.as_str())
            )))
        }
        fn chat_streaming(
            &self,
            messages: &[ChatMessage],
            config: &GenerationConfig,
            tools: Option<&[serde_json::Value]>,
            _on_token: &mut dyn FnMut(&str),
        ) -> anyhow::Result<LLMResponse> {
            self.chat(messages, config, tools)
        }
        fn count_tokens(&self, text: &str) -> anyhow::Result<usize> {
            Ok(text.len() / 4)
        }
        fn backend_name(&self) -> &str {
            "conc-test"
        }
    }

    fn pool(delay_ms: u64, permits: usize) -> (InferencePool, Arc<AtomicUsize>) {
        let max = Arc::new(AtomicUsize::new(0));
        let be = ConcBackend {
            active: Arc::new(AtomicUsize::new(0)),
            max: max.clone(),
            delay_ms,
        };
        (InferencePool::new(Arc::new(be), permits), max)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn blocking_call_does_not_starve_the_async_executor() {
        let (p, _max) = pool(200, 1);
        // An independent async ticker that should keep advancing while inference blocks.
        let ticks = Arc::new(AtomicU64::new(0));
        let t2 = ticks.clone();
        let ticker = tokio::spawn(async move {
            for _ in 0..200 {
                tokio::time::sleep(Duration::from_millis(5)).await;
                t2.fetch_add(1, Ordering::SeqCst);
            }
        });
        let out = p
            .chat(vec![ChatMessage::user("hi")], GenerationConfig::default())
            .await
            .unwrap();
        ticker.abort();
        assert_eq!(out.text, "echo:hi");
        // ~200ms of blocking work elapsed; the async ticker (5ms cadence) must have advanced.
        assert!(ticks.load(Ordering::SeqCst) >= 5, "executor was starved");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn semaphore_serializes_a_local_single_model_backend() {
        let (p, max) = pool(60, 1); // permits = 1
        let mut hs = Vec::new();
        for i in 0..6 {
            let p = p.clone();
            hs.push(tokio::spawn(async move {
                p.chat(
                    vec![ChatMessage::user(format!("q{i}"))],
                    GenerationConfig::default(),
                )
                .await
            }));
        }
        for h in hs {
            h.await.unwrap().unwrap();
        }
        assert_eq!(max.load(Ordering::SeqCst), 1, "permits=1 must serialize");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn higher_permits_allow_real_parallelism() {
        let (p, max) = pool(60, 3); // permits = 3
        let mut hs = Vec::new();
        for i in 0..6 {
            let p = p.clone();
            hs.push(tokio::spawn(async move {
                p.chat(
                    vec![ChatMessage::user(format!("q{i}"))],
                    GenerationConfig::default(),
                )
                .await
            }));
        }
        for h in hs {
            h.await.unwrap().unwrap();
        }
        assert!(max.load(Ordering::SeqCst) >= 2, "permits=3 should overlap");
    }

    /// Simulate total cloud failure: all cloud links error, local fallback answers.
    /// Asserts (1) the reply comes from the local tier, (2) survival mode activates,
    /// (3) survival_status() returns a non-empty degradation notice, and (4) survival
    /// mode clears automatically when a cloud provider recovers.
    #[test]
    fn survival_mode_activates_on_all_cloud_failure_and_clears_on_recovery() {
        // Reset shared global state so this test is hermetic.
        super::SURVIVAL_MODE.store(false, Ordering::SeqCst);
        *super::SURVIVAL_SINCE.lock().unwrap() = None;

        let local_be = Arc::new(ScriptedLLM::new("local-answer")) as Arc<dyn LLMBackend>;

        // Phase 1 — all cloud links fail → local tier answers → survival mode activates.
        let chain = ChainBackend::new_labeled(
            vec![
                Arc::new(TestBE {
                    reply: None,
                    name: "cloud-a".into(),
                }),
                Arc::new(TestBE {
                    reply: None,
                    name: "cloud-b".into(),
                }),
            ],
            vec!["cloud-a".into(), "cloud-b".into()],
        )
        .with_local_fallback(Arc::clone(&local_be), "ollama-local:test");

        let r = chain
            .chat(
                &[ChatMessage::user("ping")],
                &GenerationConfig::default(),
                None,
            )
            .unwrap();
        assert_eq!(
            r.text, "local-answer",
            "local tier must answer when all cloud links fail"
        );
        assert!(
            in_survival_mode(),
            "survival mode must be active after all-cloud failure"
        );
        let notice = survival_status();
        assert!(
            !notice.is_empty(),
            "survival_status must return a degradation notice in survival mode"
        );
        assert!(
            notice.contains("SURVIVAL MODE"),
            "notice must mention SURVIVAL MODE"
        );

        // Phase 2 — cloud recovers → survival mode clears automatically.
        let recovering = ChainBackend::new_labeled(
            vec![Arc::new(TestBE {
                reply: Some("cloud-reply".into()),
                name: "cloud-a".into(),
            })],
            vec!["cloud-a".into()],
        )
        .with_local_fallback(Arc::clone(&local_be), "ollama-local:test");

        let r2 = recovering
            .chat(
                &[ChatMessage::user("ping")],
                &GenerationConfig::default(),
                None,
            )
            .unwrap();
        assert_eq!(
            r2.text, "cloud-reply",
            "cloud reply must reach the caller on recovery"
        );
        assert!(
            !in_survival_mode(),
            "survival mode must clear when a cloud provider answers"
        );
        assert!(
            survival_status().is_empty(),
            "survival_status must be empty when healthy"
        );

        // Clean up so subsequent tests start from a known state.
        super::SURVIVAL_MODE.store(false, Ordering::SeqCst);
        *super::SURVIVAL_SINCE.lock().unwrap() = None;
    }
}

#[cfg(test)]
mod authoring_budget_tests {
    use super::*;

    /// The env var is process-global, so these run under one lock and always restore it.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_deadline<T>(v: Option<&str>, f: impl FnOnce() -> T) -> T {
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // E.LOOP-J: the observed-rate store is process-global too, and the budget now reads it. A
        // case that leaves samples behind would silently change the number a LATER case asserts,
        // so the reset lives here rather than in each test's good intentions — the cases that
        // assert the assumed constant are guaranteed to see it.
        reset_observed_rates_for_test();
        let prev = std::env::var("YM_PROVIDER_DEADLINE_S").ok();
        match v {
            Some(x) => std::env::set_var("YM_PROVIDER_DEADLINE_S", x),
            None => std::env::remove_var("YM_PROVIDER_DEADLINE_S"),
        }
        let out = f();
        match prev {
            Some(x) => std::env::set_var("YM_PROVIDER_DEADLINE_S", x),
            None => std::env::remove_var("YM_PROVIDER_DEADLINE_S"),
        }
        out
    }

    /// NO DEADLINE MEANS NO CHANGE. This is the property that makes the whole change safe to ship:
    /// a provider that has declared nothing behaves exactly as it did, so the Mind keeps the
    /// few-big-calls shape that reading 6 measured as its advantage (2 requests against Hermes's
    /// 3-16) everywhere except where a provider has actually said it will cut us off.
    #[test]
    fn an_undeclared_deadline_leaves_the_request_untouched() {
        with_deadline(None, || {
            assert_eq!(authoring_budget(16_000), 16_000);
            assert_eq!(authoring_budget(250), 250);
        });
    }

    /// NIM's measured deadline must produce a budget that actually FITS it. 302s x 15 tok/s x 0.7
    /// = 3,171 tokens, which at the measured floor of 15.5 tok/s is ~205s -- inside 302 with room
    /// for the model being slower than the floor and for the round trip.
    #[test]
    fn nims_measured_deadline_yields_a_budget_that_fits_it() {
        with_deadline(Some("302"), || {
            let b = authoring_budget(16_000);
            assert_eq!(b, 3_171, "302 * 15 * 7 / 10");
            // The claim that matters is not the number, it is that generating it fits:
            let seconds_at_the_measured_floor = b as f64 / 15.5;
            assert!(
                seconds_at_the_measured_floor < 302.0 * 0.8,
                "a budget of {b} takes {seconds_at_the_measured_floor:.0}s at the measured floor,                  which does not leave the deadline enough room"
            );
        });
    }

    /// A caller asking for LESS than the deadline affords still gets what it asked for. The clamp
    /// only ever lowers -- it must never inflate a deliberately small budget into a long call.
    #[test]
    fn the_clamp_only_lowers_and_never_raises() {
        with_deadline(Some("302"), || {
            assert_eq!(authoring_budget(900), 900);
            assert_eq!(authoring_budget(12), 12);
        });
    }

    /// A deadline too short to author anything still yields a workable request rather than a
    /// fragment: failing loudly beats emitting something guaranteed to be incomplete.
    #[test]
    fn an_impossibly_short_deadline_does_not_produce_a_fragment() {
        with_deadline(Some("5"), || {
            assert_eq!(authoring_budget(16_000), MIN_AUTHORING_TOKENS);
        });
    }


    // ── E.LOOP-J: the budget sized from what was actually observed ────────────────────────────
    //
    // Each case states the kill criterion it stands for. The three that guard AGAINST the
    // observation are first: the whole point is that a bad sample must be unable to shrink a
    // budget, because the resulting truncation is silent at the point it happens.

    /// KILL CRITERION: a zero or absent token count must never become a sample.
    ///
    /// `completion_tokens` falls back to zero whenever a backend exposes no usage object, so "the
    /// provider did not say" arrives indistinguishable from "it generated nothing". Against a
    /// floor-based estimate a single zero reads as infinitely slow, and every later budget would
    /// collapse to the minimum — a permanent, silent degradation from one unreported call.
    #[test]
    fn an_unreported_token_count_never_becomes_a_sample() {
        with_deadline(Some("302"), || {
            for _ in 0..20 {
                note_generation_rate("nim", 0, 5_000);
            }
            assert_eq!(observed_tokens_per_second(), None);
            assert_eq!(authoring_budget(16_000), 3_171, "the assumed constant, untouched");
        });
    }

    /// KILL CRITERION: a generation too small to measure is not a measurement. At a few dozen
    /// tokens the round trip dominates, so the implied rate describes the network.
    #[test]
    fn a_tiny_generation_is_not_a_throughput_measurement() {
        with_deadline(Some("302"), || {
            for _ in 0..20 {
                note_generation_rate("nim", 40, 9_000);
            }
            assert_eq!(observed_tokens_per_second(), None);
        });
    }

    /// KILL CRITERION: too few samples fall back to the assumed constant, unchanged. One fast call
    /// is noise, and acting on it would raise the budget on the flimsiest possible evidence.
    #[test]
    fn too_few_samples_fall_back_to_the_assumed_constant() {
        with_deadline(Some("302"), || {
            for _ in 0..(MIN_RATE_SAMPLES - 1) {
                note_generation_rate("nim", 3_000, 100_000); // 30 tok/s
            }
            assert_eq!(observed_tokens_per_second(), None);
            assert_eq!(authoring_budget(16_000), 3_171);
            // The very next sample crosses the threshold, so the fallback is about the COUNT and
            // not about the samples being unusable.
            note_generation_rate("nim", 3_000, 100_000);
            assert_eq!(observed_tokens_per_second(), Some(30));
        });
    }

    /// Enough samples replace the guess, and the budget moves with them.
    #[test]
    fn enough_samples_replace_the_guess() {
        with_deadline(Some("302"), || {
            for _ in 0..MIN_RATE_SAMPLES {
                note_generation_rate("nim", 3_000, 100_000); // 30 tok/s
            }
            assert_eq!(observed_tokens_per_second(), Some(30));
            assert_eq!(authoring_budget(16_000), 302 * 30 * 7 / 10);
        });
    }

    /// KILL CRITERION: the estimate never exceeds the SLOWEST observation in the window.
    ///
    /// This is the rule that produced 15 from 15.5 / 16.2 / 21.6, and it is the difference between
    /// a budget that fits the deadline on a bad minute and one that fits it on a good one.
    #[test]
    fn the_estimate_is_the_floor_and_not_the_mean() {
        with_deadline(Some("302"), || {
            for _ in 0..4 {
                note_generation_rate("nim", 3_000, 100_000); // 30 tok/s
            }
            note_generation_rate("nim", 1_000, 100_000); // one slow one: 10 tok/s
            assert_eq!(
                observed_tokens_per_second(),
                Some(10),
                "the mean of these is 26; sizing a budget on it would not fit the slow case"
            );
        });
    }

    /// The tightest route wins, because this sizes a safety limit rather than optimising a route.
    #[test]
    fn the_slowest_route_sets_the_limit() {
        with_deadline(Some("302"), || {
            for _ in 0..MIN_RATE_SAMPLES {
                note_generation_rate("fast", 6_000, 100_000); // 60 tok/s
                note_generation_rate("slow", 1_200, 100_000); // 12 tok/s
            }
            assert_eq!(observed_tokens_per_second(), Some(12));
        });
    }

    /// KILL CRITERION: a corrupt sample cannot escape the clamp.
    #[test]
    fn a_corrupt_sample_cannot_escape_the_clamp() {
        with_deadline(Some("302"), || {
            for _ in 0..MIN_RATE_SAMPLES {
                note_generation_rate("nim", 1_000_000, 1); // a nonsensical 1,000,000 tok/s
            }
            assert_eq!(observed_tokens_per_second(), Some(MAX_OBSERVED_RATE));
        });
    }

    /// And with no deadline declared, an observation changes nothing at all — the property that
    /// made the original clamp safe to ship must survive making it adaptive.
    #[test]
    fn an_observation_still_changes_nothing_without_a_deadline() {
        with_deadline(None, || {
            for _ in 0..MIN_RATE_SAMPLES {
                note_generation_rate("nim", 1_000, 100_000); // 10 tok/s, slower than the guess
            }
            assert_eq!(authoring_budget(16_000), 16_000);
        });
    }

    /// Nonsense is not a deadline. Zero, negative and unparseable all mean "none declared" rather
    /// than a budget of zero, which would ask a model for nothing at all.
    #[test]
    fn nonsense_is_treated_as_no_deadline_not_as_a_tiny_one() {
        for bad in ["0", "-1", "abc", "", "  "] {
            with_deadline(Some(bad), || {
                assert_eq!(
                    authoring_budget(16_000),
                    16_000,
                    "{bad:?} must read as no declared deadline"
                );
                assert_eq!(provider_deadline_seconds(), None, "{bad:?}");
            });
        }
    }
}

#[cfg(test)]
mod system_merge_tests {
    use super::*;

    /// A strict chat template must never see a second system message.
    ///
    /// Diagnosed live 2026-08-15 with a logging proxy in front of the endpoint. The mind sends
    /// persona + agent instructions, and inserts pack rules and a format note at index 1, so a
    /// normal turn carries three or four system blocks. qwen3.8's template answers that with
    /// `HTTP 500 — Jinja Exception: System message must be at the beginning.` while gemma's accepts
    /// it — so which model was configured silently decided whether the mind worked at all, and the
    /// failure surfaced only as "Ollama API request failed".
    #[test]
    fn every_system_block_is_merged_into_one_leading_message() {
        let msgs = vec![
            ChatMessage::system("persona"),
            ChatMessage::system("pack rules"),
            ChatMessage::system("agent instructions"),
            ChatMessage::user("what is the weather in Oslo?"),
        ];
        let out = merge_system_messages(msgs);

        assert_eq!(
            out.len(),
            2,
            "three system blocks collapse to one, user untouched"
        );
        assert_eq!(out[0].role, "system");
        assert_eq!(
            out[0].content, "persona\n\npack rules\n\nagent instructions",
            "order preserved, joined readably"
        );
        assert_eq!(out[1].role, "user");

        // A system block arriving after a user turn is hoisted, not left mid-conversation.
        let late = merge_system_messages(vec![
            ChatMessage::system("persona"),
            ChatMessage::user("hi"),
            ChatMessage::system("late context"),
        ]);
        assert_eq!(late.len(), 2);
        assert_eq!(late[0].role, "system");
        assert_eq!(late[0].content, "persona\n\nlate context");
        assert_eq!(late[1].role, "user");

        // Untouched when there is nothing to merge — no allocation, no reordering.
        let one = vec![ChatMessage::system("persona"), ChatMessage::user("hi")];
        assert_eq!(merge_system_messages(one.clone()).len(), 2);
        assert_eq!(merge_system_messages(one)[0].content, "persona");

        // An empty system block contributes nothing rather than a stray blank line.
        let blank = merge_system_messages(vec![
            ChatMessage::system("persona"),
            ChatMessage::system("   "),
            ChatMessage::user("hi"),
        ]);
        assert_eq!(blank[0].content, "persona");
    }
}

#[cfg(test)]
mod lane_failure_tests {
    use super::lane_failure;

    #[test]
    fn a_timeout_names_the_knob_and_the_seconds_not_the_network() {
        // The exact text E.TIMEOUT3 produced with YM_LLM_TIMEOUT_S=5 on staging.
        let (why, hint) = lane_failure("Ollama API request failed: timeout: global");
        assert!(why.contains("TIMED OUT"), "{why}");
        assert!(why.contains("YM_LLM_TIMEOUT_S"), "{why}");
        assert!(why.contains(" s "), "carries the seconds it waited: {why}");
        assert!(!why.contains("unreachable"), "{why}");
        assert!(hint.contains("YM_LLM_TIMEOUT_S"), "{hint}");
    }

    #[test]
    fn a_client_that_stated_its_own_wait_is_relayed_not_second_guessed() {
        // The exact shape yantrik-ml's describe_send_error produces (E.MSG2).
        let d = "Ollama API request failed: timed out after 5 s waiting for qwen3.8:27b-q4_K_M (YM_LLM_TIMEOUT_MODELS / YM_LLM_TIMEOUT_S)";
        let (why, hint) = lane_failure(d);
        assert!(why.contains("TIMED OUT"), "{why}");
        assert!(why.contains("after 5 s") && why.contains("qwen3.8:27b-q4_K_M"), "{why}");
        assert!(!why.contains("300"), "must not add the model-less number: {why}");
        assert!(hint.contains("YM_LLM_TIMEOUT_MODELS"), "{hint}");
    }

    #[test]
    fn the_other_conditions_keep_their_names() {
        assert!(lane_failure("HTTP 429 Too Many Requests after 3 retries").0.contains("RATE LIMITING"));
        assert!(lane_failure("502 Bad Gateway").0.contains("FLAKY"));
        assert!(lane_failure("connection refused (os error 111)").0.contains("unreachable"));
    }
}
