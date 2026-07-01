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
use yantrik_ml::{ChatMessage, GenerationConfig, LLMBackend, LLMResponse};

/// Bounded async wrapper over a synchronous `LLMBackend`.
#[derive(Clone)]
pub struct InferencePool {
    backend: Arc<dyn LLMBackend>,
    sem: Arc<Semaphore>,
}

impl InferencePool {
    /// `max_concurrency` = 1 for a local single-model backend (the Mutex makes more pointless and
    /// just queues); higher for API backends.
    pub fn new(backend: Arc<dyn LLMBackend>, max_concurrency: usize) -> Self {
        Self {
            backend,
            sem: Arc::new(Semaphore::new(max_concurrency.max(1))),
        }
    }

    /// Run a chat completion on the blocking pool. Holds a permit for the whole call and never
    /// blocks a tokio worker thread.
    pub async fn chat(
        &self,
        messages: Vec<ChatMessage>,
        config: GenerationConfig,
    ) -> anyhow::Result<LLMResponse> {
        let permit = self
            .sem
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore never closed");
        let backend = self.backend.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit; // released when the blocking work finishes
            backend.chat(&messages, &config, None)
        })
        .await?
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

/// A resilience chain over several `LLMBackend`s: try each in order; the first that returns a
/// non-empty success wins. An error OR an empty reply (some reasoning models emit nothing under a
/// tight token budget) falls over to the next link. For an always-on companion this means it keeps
/// answering when the primary provider rate-limits, errors, or returns nothing — the "many LLM
/// supports, just make them click" property. Links are built from whatever provider keys are present
/// (NanoGPT, Ollama Cloud, MiniMax, …), all OpenAI-compatible, so adding a provider is config-only.
pub struct ChainBackend {
    links: Vec<Arc<dyn LLMBackend>>,
    name: String,
}

impl ChainBackend {
    pub fn new(links: Vec<Arc<dyn LLMBackend>>) -> Self {
        let name = format!(
            "chain[{}]",
            links.iter().map(|b| b.backend_name().to_string()).collect::<Vec<_>>().join(" -> ")
        );
        Self { links, name }
    }

    fn is_usable(r: &LLMResponse) -> bool {
        !r.text.trim().is_empty() || !r.tool_calls.is_empty() || !r.api_tool_calls.is_empty()
    }
}

impl LLMBackend for ChainBackend {
    fn chat(
        &self,
        messages: &[ChatMessage],
        config: &GenerationConfig,
        tools: Option<&[serde_json::Value]>,
    ) -> anyhow::Result<LLMResponse> {
        let mut last_err: Option<anyhow::Error> = None;
        for be in &self.links {
            match be.chat(messages, config, tools) {
                Ok(r) if Self::is_usable(&r) => return Ok(r),
                Ok(_) => {
                    eprintln!("[chain] {} returned empty — failing over", be.backend_name());
                    last_err = Some(anyhow::anyhow!("empty response from {}", be.backend_name()));
                }
                Err(e) => {
                    eprintln!("[chain] {} failed ({e}) — failing over", be.backend_name());
                    last_err = Some(e);
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
        // Anthropic direct (OpenAI-compatible endpoint). Default Sonnet 5 (fast + cheap enough for an
        // always-on brain); swap the model to claude-opus-4-8 or claude-fable-5 (when it un-gates).
        "anthropic" => ("https://api.anthropic.com/v1", "ANTHROPIC_API_KEY", "claude-sonnet-5"),
        _ => return None,
    };
    let key = std::env::var(key_env).ok().filter(|k| !k.trim().is_empty())?;
    let model = if model.is_empty() { default_model.to_string() } else { model.to_string() };
    Some(Arc::new(yantrik_ml::GenericOpenAIBackend::for_provider("openai", base, Some(key), model)) as Arc<dyn LLMBackend>)
}

/// The default resilient chain from whatever provider keys are present, in priority order
/// (NanoGPT → Ollama Cloud → MiniMax). `None` if no provider key is set. Models can be overridden
/// per provider via `YM_MODEL` / `YM_OLLAMA_MODEL` / `YM_MINIMAX_MODEL`.
pub fn default_chain_from_env() -> Option<(Arc<dyn LLMBackend>, String)> {
    let order = [
        ("nanogpt", std::env::var("YM_MODEL").ok()),
        ("ollama-cloud", std::env::var("YM_OLLAMA_MODEL").ok()),
        ("minimax", std::env::var("YM_MINIMAX_MODEL").ok()),
    ];
    let mut links: Vec<Arc<dyn LLMBackend>> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
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
        return None;
    }
    let label = labels.join(" -> ");
    if links.len() == 1 {
        return Some((links.pop().unwrap(), label));
    }
    Some((Arc::new(ChainBackend::new(links)), label))
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
}
