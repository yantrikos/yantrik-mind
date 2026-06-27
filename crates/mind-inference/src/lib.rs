//! mind-inference — the async facade over yantrik-ml's synchronous, blocking backends.
//!
//! Spike B (Phase 0): prove the **bounded blocking pool**. `LLMBackend::chat` is synchronous and
//! blocking (local candle/llama.cpp backends are additionally `Mutex`-serialized); calling it
//! directly from an async task would block a tokio worker for the whole generation and starve the
//! executor. So every call goes through `spawn_blocking` behind a `Semaphore` (permits = 1 for a
//! local single-model backend, higher for API backends). This queue is also where latency/quality
//! fallback + cost governance will live (Phase 2).

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
