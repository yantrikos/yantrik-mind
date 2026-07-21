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

/// Pure policy: may a pool labeled `provider` serve a request of `scope`, given the two CSV
/// allowlists? Pure so it's testable without env races.
pub fn scope_allows(scope: PrivacyScope, provider: &str, household_csv: &str, private_csv: &str) -> bool {
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

/// Per-scope served/refused counters — the audit trail `ym privacy` renders. Process-lifetime.
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
pub fn privacy_escalated_count() -> u64 {
    PRIVACY_ESCALATED.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn privacy_report(provider: &str) -> String {
    use std::sync::atomic::Ordering;
    let household = std::env::var("YM_HOUSEHOLD_PROVIDERS").unwrap_or_else(|_| DEFAULT_HOUSEHOLD.to_string());
    let private = std::env::var("YM_PRIVATE_PROVIDERS").unwrap_or_default();
    format!(
        "PRIVACY LANES (charter wall — every LLM call declares a scope)\n\
         provider: {provider}\n\
         household allowlist (YM_HOUSEHOLD_PROVIDERS): {household}\n\
         private allowlist (YM_PRIVATE_PROVIDERS): {}\n\
         served  — private {} · household {} · public {}\n\
         refused — private {} · household {} · public {}\n\
         private-grounded turns ESCALATED to cloud: {}  ← should be 0; a non-zero count means private context reached a cloud provider\n\
         Configure YM_PRIVATE_PROVIDERS with an owned/on-device provider to keep private-grounded turns home (escalations auto-drop to 0).",
        if private.is_empty() { "(none — private lane HARD-REFUSES; deterministic fallback only)" } else { private.as_str() },
        PRIVACY_SERVED[0].load(Ordering::Relaxed),
        PRIVACY_SERVED[1].load(Ordering::Relaxed),
        PRIVACY_SERVED[2].load(Ordering::Relaxed),
        PRIVACY_REFUSED[0].load(Ordering::Relaxed),
        PRIVACY_REFUSED[1].load(Ordering::Relaxed),
        PRIVACY_REFUSED[2].load(Ordering::Relaxed),
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
        g.as_ref().map(|t| t.elapsed().as_secs() / 60).unwrap_or(0)
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
        self.chat_scoped(messages, config, PrivacyScope::Household).await
    }

    /// Scope-aware chat: routes or REFUSES per the privacy lanes. A refusal is an error the caller
    /// must handle by deterministic fallback — never by silently downgrading the scope.
    pub async fn chat_scoped(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
        scope: PrivacyScope,
    ) -> anyhow::Result<LLMResponse> {
        self.chat_scoped_tools(messages, config, scope, Vec::new()).await
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
        use std::sync::atomic::Ordering;
        let household = std::env::var("YM_HOUSEHOLD_PROVIDERS").unwrap_or_else(|_| DEFAULT_HOUSEHOLD.to_string());
        let private = std::env::var("YM_PRIVATE_PROVIDERS").unwrap_or_default();
        // A PRIVATE call, when a dedicated local-owned lane exists, is served ONLY by that local-only
        // backend — cloud is unreachable for it by construction (sol 019f8287: enforce at dispatch).
        // Everything else (and Private when no local lane is configured) routes on the default backend.
        // The explicit local-only lane is SANCTIONED BY CONSTRUCTION (built from the owned endpoint),
        // which is stronger evidence than the env CSV ("a declaration, not evidence" — sol #5), so it
        // bypasses the CSV allowlist; the CSV still gates the label-based (non-explicit) paths.
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
        PRIVACY_SERVED[scope_idx(scope)].fetch_add(1, Ordering::Relaxed);
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore never closed");
        tokio::task::spawn_blocking(move || {
            let _permit = permit; // released when the blocking work finishes
            let tools_ref = if tools.is_empty() { None } else { Some(tools.as_slice()) };
            backend.chat(&messages, &config, tools_ref)
        })
        .await?
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
    /// escalation policy as [`chat_grounded`], but forwards the tool schema list so a tool-capable
    /// backend returns structured `tool_calls`. This is the agent loop's inference entry point.
    pub async fn chat_grounded_tools(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
        tools: Vec<serde_json::Value>,
    ) -> anyhow::Result<LLMResponse> {
        match self
            .chat_scoped_tools(messages.clone(), config.clone(), PrivacyScope::Private, tools.clone())
            .await
        {
            Ok(r) => Ok(r), // served locally — private context stayed home
            Err(e) => {
                // FAIL CLOSED when a dedicated local private lane EXISTS but its backend failed
                // (outage / OOM / timeout): do NOT re-send the private prompt to a cloud/household
                // backend — an outage must reduce capability, never confidentiality (sol 019f8287).
                // The turn's caller degrades to deterministic rendering / an honest "unavailable".
                if self.private.is_some() {
                    PRIVACY_REFUSED[scope_idx(PrivacyScope::Private)].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    eprintln!("[privacy] private lane FAILED — failing CLOSED (refusing cloud escalation of private context): {e}");
                    return Err(anyhow::anyhow!(
                        "private inference unavailable — refusing to route private context to a cloud provider (local lane down)"
                    ));
                }
                // No local private lane configured (the documented interim gap): escalate to the
                // household lane so the turn still works, but COUNT + log it — the gap is visible and
                // auto-closes the moment YM_LOCAL_OLLAMA_URL / YM_PRIVATE_PROVIDERS names a local lane.
                PRIVACY_ESCALATED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "[privacy] private-grounded turn ESCALATED to household lane (provider '{}') — no owned-hardware private lane configured; set YM_LOCAL_OLLAMA_URL to keep private context home",
                    self.provider
                );
                self.chat_scoped_tools(messages, config, PrivacyScope::Household, tools).await
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
    last_system: std::sync::Mutex<String>,
    last_user: std::sync::Mutex<String>,
    last_all: std::sync::Mutex<String>,
}

impl ScriptedLLM {
    pub fn new(reply: impl Into<String>) -> Self {
        Self {
            reply: reply.into(),
            last_system: std::sync::Mutex::new(String::new()),
            last_user: std::sync::Mutex::new(String::new()),
            last_all: std::sync::Mutex::new(String::new()),
        }
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
            text: self.reply.clone(),
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
            .map(|o| o.map(|(name, arguments)| ToolCall { name: name.to_string(), arguments }))
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
        self.prompts.lock().unwrap().get(i).cloned().unwrap_or_default()
    }
    /// The tool schemas passed on call `i` (0-based), or empty if none/out of range.
    pub fn tools_at(&self, i: usize) -> Vec<serde_json::Value> {
        self.tools_seen.lock().unwrap().get(i).cloned().unwrap_or_default()
    }
}

impl LLMBackend for SequencedLLM {
    fn chat(
        &self,
        messages: &[ChatMessage],
        _config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        let all = messages.iter().map(|m| format!("{}: {}", m.role, m.content)).collect::<Vec<_>>().join("\n");
        self.prompts.lock().unwrap().push(all);
        self.tools_seen.lock().unwrap().push(tools.map(|t| t.to_vec()).unwrap_or_default());
        let i = self.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let reply = self
            .replies
            .get(i)
            .or_else(|| self.replies.last())
            .cloned()
            .unwrap_or_default();
        let tool_calls = self.native.get(i).cloned().flatten().into_iter().collect::<Vec<_>>();
        let stop_reason = if tool_calls.is_empty() { "stop" } else { "tool_calls" }.to_string();
        Ok(LLMResponse {
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
pub struct ChainBackend {
    links: Vec<Arc<dyn LLMBackend>>,
    labels: Vec<String>,
    name: String,
    /// Local survival-tier backend. Tried last, only when all `links` have failed. When it
    /// answers, survival mode activates globally; cleared automatically when a cloud link recovers.
    local: Option<(Arc<dyn LLMBackend>, String)>,
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
        Self { links, labels, name, local: None }
    }

    /// Attach a local survival-tier backend (e.g. local Ollama). When all cloud links fail, this
    /// is tried last; on success it activates survival mode until a cloud link recovers.
    pub fn with_local_fallback(mut self, backend: Arc<dyn LLMBackend>, label: impl Into<String>) -> Self {
        self.local = Some((backend, label.into()));
        self
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
    static CACHE: std::sync::Mutex<Option<(std::time::Instant, Option<f64>)>> = std::sync::Mutex::new(None);
    {
        let g = CACHE.lock().unwrap();
        if let Some((t, v)) = *g {
            if t.elapsed() < std::time::Duration::from_secs(1800) {
                return v;
            }
        }
    }
    let key = std::env::var("NANOGPT_KEY").ok().filter(|k| !k.trim().is_empty());
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
    let mut agg: std::collections::HashMap<String, (u64, u64, u64, u64, u64)> = std::collections::HashMap::new();
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
    let mut out: Vec<(String, u64, u64, u64, u64, u64)> =
        agg.into_iter().map(|(k, (a, b, c, d, e))| (k, a, b, c, d, e)).collect();
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
        // HEADROOM-FIRST ROUTING: a link whose real quota is nearly burned is tried LAST, not
        // first — it still serves if everything else fails (availability beats thrift), but the
        // default path drains the provider with room. Unknown quota never demotes.
        let demote_at: f64 = std::env::var("YM_DEMOTE_PCT").ok().and_then(|v| v.parse().ok()).unwrap_or(90.0);
        let mut order: Vec<usize> = (0..self.links.len()).collect();
        if self.links.len() > 1 {
            let hot = |i: &usize| -> bool {
                let l = self.labels.get(*i).map(String::as_str).unwrap_or("");
                l.starts_with("nanogpt") && nanogpt_weekly_pct().map(|p| p >= demote_at).unwrap_or(false)
            };
            let (cold, warm): (Vec<usize>, Vec<usize>) = order.iter().partition(|i| !hot(*i));
            if !warm.is_empty() {
                eprintln!("[chain] demoting hot link(s) to last: {:?}", warm.iter().map(|i| self.labels.get(*i).cloned().unwrap_or_default()).collect::<Vec<_>>());
                order = cold.into_iter().chain(warm.into_iter()).collect();
            }
        }
        for i in order {
            let be = &self.links[i];
            let label = self.labels.get(i).map(String::as_str).unwrap_or_else(|| be.backend_name());
            match be.chat(messages, config, tools) {
                Ok(r) if Self::is_usable(&r) => {
                    // Cloud answered: clear survival mode if it was active.
                    if self.local.is_some()
                        && SURVIVAL_MODE.swap(false, std::sync::atomic::Ordering::SeqCst)
                    {
                        *SURVIVAL_SINCE.lock().unwrap() = None;
                        eprintln!("[survival] cloud provider recovered ({label}) — exiting survival mode");
                    }
                    provider_record_usage(label, true, r.prompt_tokens as u64, r.completion_tokens as u64);
                    return Ok(r);
                }
                Ok(_) => {
                    provider_record(label, false);
                    eprintln!("[chain] {} returned empty — failing over", be.backend_name());
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
            match local_be.chat(messages, config, tools) {
                Ok(r) if Self::is_usable(&r) => {
                    if !SURVIVAL_MODE.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        *SURVIVAL_SINCE.lock().unwrap() = Some(std::time::Instant::now());
                        eprintln!("[survival] all cloud providers failed — activating local tier ({local_label})");
                    }
                    provider_record_usage(local_label, true, r.prompt_tokens as u64, r.completion_tokens as u64);
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

pub fn backend_from_spec(spec: &str) -> Option<Arc<dyn LLMBackend>> {
    let (provider, model) = match spec.split_once(':') {
        Some((p, m)) => (p.trim(), m.trim()),
        None => (spec.trim(), ""),
    };
    let (base, key_env, default_model) = match provider {
        "nanogpt" => ("https://nano-gpt.com/api/v1", "NANOGPT_KEY", "deepseek/deepseek-v4-pro-cheaper"),
        "ollama-cloud" | "ollama" => ("https://ollama.com/v1", "OLLAMA_CLOUD_KEY", "glm-4.7"),
        "minimax" => ("https://api.minimax.io/v1", "MINIMAX_API_KEY", "MiniMax-M2.7"),
        "openrouter" => ("https://openrouter.ai/api/v1", "OPEN_ROUTER_KEY", "deepseek/deepseek-chat"),
        "grok" => ("https://api.x.ai/v1", "GROK_API_KEY", "grok-2-latest"),
        // Anthropic direct. Default Sonnet 5 (fast + cheap enough for an
        // always-on brain); swap the model to claude-opus-4-8 or claude-fable-5 (when it un-gates).
        "anthropic" => (
            "https://api.anthropic.com",
            "ANTHROPIC_API_KEY",
            "claude-sonnet-5",
        ),
        _ => return None,
    };
    let key = configured_api_key(key_env)?;
    let model = if model.is_empty() { default_model.to_string() } else { model.to_string() };
    if provider == "anthropic" {
        Some(Arc::new(yantrik_ml::AnthropicBackend::with_base_url(
            key, base, model,
        )) as Arc<dyn LLMBackend>)
    } else {
        Some(Arc::new(yantrik_ml::GenericOpenAIBackend::for_provider("openai", base, Some(key), model)) as Arc<dyn LLMBackend>)
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
        && std::env::var("YM_LOCAL_ROLE").map(|r| r.trim() != "fallback").unwrap_or(true);

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
pub fn local_backend_from_env() -> Option<(Arc<dyn LLMBackend>, String)> {
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
    let key = std::env::var("YM_LOCAL_OLLAMA_KEY")
        .unwrap_or_else(|_| "ollama".to_string());
    // Thinking is a per-workload quality/latency lever on qwen3.6 MoE (binary; reasoning_effort
    // levels don't scale — ollama maintainer, 2026-07-21). Blanket thinking-ON measured ~96s even
    // for a trivial turn (the agent loop multiplies the reasoning chain across steps) — unusable
    // for interactive replies. So default OFF for foreground usability; set YM_LOCAL_THINK=on to
    // force it globally. The proper split — thinking ON only on background planning paths — is the
    // follow-up; this env keeps the fast default while the builder plumbing is already in place.
    let think = std::env::var("YM_LOCAL_THINK")
        .map(|v| matches!(v.trim().to_ascii_lowercase().as_str(), "on" | "1" | "true" | "yes"))
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

impl Router {
    /// All roles resolve to one pool (tests, single-backend setups).
    pub fn uniform(default: InferencePool) -> Self {
        Self { roles: HashMap::new(), default }
    }

    /// Read `YM_ROLE_<ROLE>` for each known function; a set+resolvable spec gets its own pool, else
    /// the role uses `default`. Known roles: chat, research, util, verify, code, consolidate.
    pub fn from_env(default: InferencePool, concurrency: usize) -> Self {
        let mut roles = HashMap::new();
        for role in ["chat", "research", "util", "verify", "code", "consolidate"] {
            let var = format!("YM_ROLE_{}", role.to_uppercase());
            if let Ok(spec) = std::env::var(&var) {
                if !spec.trim().is_empty() {
                    if let Some(be) = backend_from_spec(&spec) {
                        roles.insert(role.to_string(), InferencePool::new(be, concurrency));
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
        self.roles.get(role).cloned().unwrap_or_else(|| self.default.clone())
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
        fn chat(&self, messages: &[ChatMessage], _c: &GenerationConfig, _t: Option<&[serde_json::Value]>) -> anyhow::Result<LLMResponse> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            for m in messages {
                assert!(!m.content.contains(&self.canary), "PRIVACY LEAK: private canary reached the cloud backend");
            }
            Ok(LLMResponse { text: "cloud-ok".into(), prompt_tokens: 0, completion_tokens: 0, tool_calls: vec![], api_tool_calls: vec![], stop_reason: "stop".into() })
        }
        fn chat_streaming(&self, m: &[ChatMessage], c: &GenerationConfig, t: Option<&[serde_json::Value]>, _: &mut dyn FnMut(&str)) -> anyhow::Result<LLMResponse> {
            self.chat(m, c, t)
        }
        fn count_tokens(&self, t: &str) -> anyhow::Result<usize> { Ok(t.len() / 4) }
        fn backend_name(&self) -> &str { "canary-cloud" }
    }

    /// A local backend that always fails — simulates the local Ollama being down/OOM/timing out.
    struct AlwaysDown;
    impl LLMBackend for AlwaysDown {
        fn chat(&self, _m: &[ChatMessage], _c: &GenerationConfig, _t: Option<&[serde_json::Value]>) -> anyhow::Result<LLMResponse> {
            anyhow::bail!("local ollama down")
        }
        fn chat_streaming(&self, m: &[ChatMessage], c: &GenerationConfig, t: Option<&[serde_json::Value]>, _: &mut dyn FnMut(&str)) -> anyhow::Result<LLMResponse> {
            self.chat(m, c, t)
        }
        fn count_tokens(&self, t: &str) -> anyhow::Result<usize> { Ok(t.len() / 4) }
        fn backend_name(&self) -> &str { "always-down" }
    }

    /// THE LEAK-PROOF INVARIANT (sol 019f8287): for a Private-grounded turn, ZERO bytes reach a cloud
    /// provider when the local private lane is down — the turn FAILS CLOSED, never escalates. Proven
    /// with a canary the cloud mock panics on and a call-counter asserted to stay 0.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn private_grounded_fails_closed_never_leaks_to_cloud() {
        let canary = "SECRET-CANARY-alice-oncology-47-12-33";
        let cloud_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cloud = Arc::new(CanaryTrap { canary: canary.into(), calls: cloud_calls.clone() }) as Arc<dyn LLMBackend>;
        let local_down = Arc::new(AlwaysDown) as Arc<dyn LLMBackend>;
        // Default/household backend = the cloud trap; PRIVATE lane = the (failing) local-only backend.
        let pool = InferencePool::new(cloud, 1)
            .with_provider("canary-cloud")
            .with_private_backend(local_down, "ollama-local");
        assert!(pool.has_private_lane());

        let messages = vec![ChatMessage::user(&format!("remember: {canary}"))];
        let res = pool
            .chat_grounded_tools(messages, GenerationConfig::default(), Vec::new())
            .await;

        // The private lane failed → the turn FAILS CLOSED (Err), and the cloud backend was NEVER called.
        assert!(res.is_err(), "a down private lane must fail closed, not silently succeed via cloud");
        assert!(res.unwrap_err().to_string().contains("refusing to route private context"), "explicit fail-closed reason");
        assert_eq!(cloud_calls.load(std::sync::atomic::Ordering::SeqCst), 0, "PRIVACY LEAK: the cloud backend was called for a private-grounded turn");
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
        let url = std::env::var("YM_SMOKE_OLLAMA_URL").unwrap_or_else(|_| "http://192.168.4.35:11434".into());
        let model = std::env::var("YM_SMOKE_OLLAMA_MODEL").unwrap_or_else(|_| "qwen3.6:27b".into());
        let backend = yantrik_ml::ApiLLM::new(url, None, model);
        let pool = InferencePool::new(Arc::new(backend) as Arc<dyn LLMBackend>, 1).with_provider("ollama-local");
        let tools = vec![serde_json::json!({"type":"function","function":{
            "name":"weather","description":"current conditions + today's forecast for a city/town",
            "parameters":{"type":"object","properties":{"place":{"description":"place"}},
                          "required":["place"],"additionalProperties":true}}})];
        let messages = vec![
            ChatMessage::system("You are an agent, not a chatbot — you ACT. Use ONE tool, observe, then answer."),
            ChatMessage::user("what's the weather in pune?"),
        ];
        // Public scope: the smoke prompt carries no private data, and Public routes to any provider.
        let r = pool
            .chat_scoped_tools(messages, GenerationConfig::default(), PrivacyScope::Public, tools)
            .await
            .expect("live ollama chat");
        let tc = r.tool_calls.first().expect("the model should return a STRUCTURED tool call");
        assert_eq!(tc.name, "weather", "picked the offered tool: {:?}", r.tool_calls);
        assert!(
            tc.arguments.get("place").and_then(|v| v.as_str()).map(|s| s.to_lowercase().contains("pune")).unwrap_or(false),
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
        assert!(scope_allows(PrivacyScope::Household, "nanogpt -> minimax", hh, pv));
        assert!(!scope_allows(PrivacyScope::Household, "random-cloud", hh, pv));
        assert!(!scope_allows(PrivacyScope::Private, "minimax", hh, pv));
        assert!(!scope_allows(PrivacyScope::Private, "scripted", hh, pv));
        assert!(scope_allows(PrivacyScope::Private, "ollama-local:qwen3", hh, "ollama-local"));
        assert!(!scope_allows(PrivacyScope::Private, "minimax", hh, "ollama-local"));
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
        let out = pool.chat_grounded(vec![ChatMessage::user("private family context")], GenerationConfig::default()).await;
        assert!(out.is_ok(), "chat_grounded must never break the turn");
        let after = PRIVACY_ESCALATED.load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(after, before + 1, "the cloud escalation of a private-grounded turn must be counted");
    }

    #[tokio::test]
    async fn private_scope_refuses_on_cloud_pool() {
        let pool = InferencePool::new(
            std::sync::Arc::new(ScriptedLLM::new("leak")) as std::sync::Arc<dyn LLMBackend>,
            1,
        )
        .with_provider("minimax");
        let out = pool
            .chat_scoped(vec![ChatMessage::user("family secret")], GenerationConfig::default(), PrivacyScope::Private)
            .await;
        assert!(out.is_err(), "private scope must refuse a cloud-labeled pool");
        let ok = pool
            .chat_scoped(vec![ChatMessage::user("hi")], GenerationConfig::default(), PrivacyScope::Household)
            .await;
        assert!(ok.is_ok());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::time::Duration;

    fn resp(text: &str) -> LLMResponse {
        LLMResponse {
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

    /// Configurable test backend: `None` => errors, `Some("")` => empty reply, `Some(x)` => Ok(x).
    struct TestBE {
        reply: Option<String>,
        name: String,
    }
    impl LLMBackend for TestBE {
        fn chat(&self, _: &[ChatMessage], _: &GenerationConfig, _: Option<&[serde_json::Value]>) -> anyhow::Result<LLMResponse> {
            match &self.reply {
                None => anyhow::bail!("{} boom", self.name),
                Some(t) => Ok(resp(t)),
            }
        }
        fn chat_streaming(&self, m: &[ChatMessage], c: &GenerationConfig, t: Option<&[serde_json::Value]>, _: &mut dyn FnMut(&str)) -> anyhow::Result<LLMResponse> {
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
            Arc::new(TestBE { reply: None, name: "err".into() }),
            Arc::new(TestBE { reply: Some(String::new()), name: "empty".into() }),
            Arc::new(TestBE { reply: Some("hello from C".into()), name: "good".into() }),
        ]);
        let out = chain.chat(&[ChatMessage::user("hi")], &GenerationConfig::default(), None).unwrap();
        assert_eq!(out.text, "hello from C", "chain should skip err+empty links to the first usable reply");

        let dead = ChainBackend::new(vec![
            Arc::new(TestBE { reply: None, name: "e1".into() }),
            Arc::new(TestBE { reply: None, name: "e2".into() }),
        ]);
        assert!(dead.chat(&[ChatMessage::user("hi")], &GenerationConfig::default(), None).is_err(), "all-dead chain must error");
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
                messages.last().map(|m| m.content.as_str()).unwrap_or("")
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
        let out = p.chat(vec![ChatMessage::user("hi")], GenerationConfig::default()).await.unwrap();
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
                p.chat(vec![ChatMessage::user(format!("q{i}"))], GenerationConfig::default())
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
                p.chat(vec![ChatMessage::user(format!("q{i}"))], GenerationConfig::default())
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
                Arc::new(TestBE { reply: None, name: "cloud-a".into() }),
                Arc::new(TestBE { reply: None, name: "cloud-b".into() }),
            ],
            vec!["cloud-a".into(), "cloud-b".into()],
        )
        .with_local_fallback(Arc::clone(&local_be), "ollama-local:test");

        let r = chain.chat(&[ChatMessage::user("ping")], &GenerationConfig::default(), None).unwrap();
        assert_eq!(r.text, "local-answer", "local tier must answer when all cloud links fail");
        assert!(in_survival_mode(), "survival mode must be active after all-cloud failure");
        let notice = survival_status();
        assert!(!notice.is_empty(), "survival_status must return a degradation notice in survival mode");
        assert!(notice.contains("SURVIVAL MODE"), "notice must mention SURVIVAL MODE");

        // Phase 2 — cloud recovers → survival mode clears automatically.
        let recovering = ChainBackend::new_labeled(
            vec![Arc::new(TestBE { reply: Some("cloud-reply".into()), name: "cloud-a".into() })],
            vec!["cloud-a".into()],
        )
        .with_local_fallback(Arc::clone(&local_be), "ollama-local:test");

        let r2 = recovering.chat(&[ChatMessage::user("ping")], &GenerationConfig::default(), None).unwrap();
        assert_eq!(r2.text, "cloud-reply", "cloud reply must reach the caller on recovery");
        assert!(!in_survival_mode(), "survival mode must clear when a cloud provider answers");
        assert!(survival_status().is_empty(), "survival_status must be empty when healthy");

        // Clean up so subsequent tests start from a known state.
        super::SURVIVAL_MODE.store(false, Ordering::SeqCst);
        *super::SURVIVAL_SINCE.lock().unwrap() = None;
    }
}
