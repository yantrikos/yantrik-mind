//! brain_bench — score a CANDIDATE BRAIN on the workload this mind actually runs.
//!
//! Leaderboards answer "is this model good?". That is not the question. The question is "can this
//! model drive OUR agent loop?" — pick the right tool from OUR retrieval-gated catalog, emit
//! structured args our dispatcher can use, and decline to call a tool when none fits. A model can
//! top SWE-bench and still be useless here if it fumbles native tool-calling under a 20-schema
//! prompt; a smaller model can be perfect for it. Only this measurement decides.
//!
//! It is also the neutral instrument for a question I cannot answer neutrally. When a vendor claims
//! their model beats the one writing this code, the honest response is not an opinion — it is to
//! run both against the same fixed workload and publish the numbers.
//!
//! Provider-agnostic on purpose: anything speaking the OpenAI protocol (QwenCloud, NanoGPT, MiniMax,
//! a local Ollama, an on-box llama.cpp) is scored identically, so candidates are comparable rather
//! than each being judged on its own marketing.
//!
//! Run:
//!   YM_BENCH_URL=https://... YM_BENCH_KEY=sk-... YM_BENCH_MODEL=qwen3.8-max \
//!     cargo test -p mind-evals brain_bench -- --ignored --nocapture

use std::sync::Arc;

use mind_inference::InferencePool;
use yantrik_ml::{ChatMessage, GenerationConfig, LLMBackend};

/// One graded case: a user turn, and what a correct brain does with it.
struct Case {
    turn: &'static str,
    /// The tool a competent brain should select. `None` = it should NOT call a tool at all.
    expect_tool: Option<&'static str>,
    /// An argument value that must appear (lowercased substring) when a tool is called.
    expect_arg: Option<&'static str>,
    why: &'static str,
}

/// The cases are drawn from what this mind actually gets asked, including the two failure modes
/// that hurt in production: calling a tool for a turn that needs none (noise), and picking a
/// plausible-but-wrong neighbour from a catalog full of similar tools (the retrieval-gating risk).
fn cases() -> Vec<Case> {
    vec![
        Case {
            turn: "what's the weather in pune?",
            expect_tool: Some("weather"),
            expect_arg: Some("pune"),
            why: "the simplest possible dispatch — if this fails, nothing else matters",
        },
        Case {
            turn: "what do you remember about my wife's birthday?",
            expect_tool: Some("recall"),
            expect_arg: None,
            why: "memory questions must hit recall, not be answered from thin air",
        },
        Case {
            turn: "my daughter started kindergarten today",
            expect_tool: Some("remember"),
            expect_arg: None,
            why: "a durable fact volunteered in passing must be STORED, unprompted",
        },
        Case {
            turn: "find me a good pair of running shoes under $120",
            expect_tool: Some("deals"),
            expect_arg: None,
            why: "shopping must reach the native deals tool, not a web search or a built skill",
        },
        Case {
            turn: "thanks, that's great",
            expect_tool: None,
            expect_arg: None,
            why:
                "ACID TEST: a turn needing no tool must call none. Tool-happy models fail here and \
                  every spurious call costs a step, a second, and a chance to confabulate",
        },
        Case {
            turn: "what time is it?",
            expect_tool: Some("now"),
            expect_arg: None,
            why: "never guess something the mind can simply look up",
        },
    ]
}

/// The tool schemas a candidate is judged against — deliberately the SHAPE the live loop sends
/// (a gated working set, not one tool), because selecting from many similar options is the hard part.
fn schemas() -> Vec<serde_json::Value> {
    let t = |name: &str, desc: &str, arg: Option<&str>| {
        let (props, req) = match arg {
            Some(a) => (
                serde_json::json!({ a: { "description": a } }),
                serde_json::json!([a]),
            ),
            None => (serde_json::json!({}), serde_json::json!([])),
        };
        serde_json::json!({"type":"function","function":{
            "name": name, "description": desc,
            "parameters": {"type":"object","properties": props, "required": req, "additionalProperties": true}}})
    };
    vec![
        t(
            "recall",
            "search your typed memory for what you already know about the user",
            Some("query"),
        ),
        t(
            "remember",
            "store a durable fact about the user or their world",
            Some("text"),
        ),
        t("now", "the current date and time", None),
        t(
            "weather",
            "current conditions and today's forecast for a city or town",
            Some("place"),
        ),
        t(
            "deals",
            "find and compare REAL purchasable deals on something",
            Some("query"),
        ),
        t("search", "web search — find pages or facts", Some("query")),
        t("web_fetch", "read a specific web page by URL", Some("url")),
        t("calendar", "the unified upcoming calendar view", None),
        t(
            "family",
            "the people tracked in the user's life and their key dates",
            None,
        ),
        t(
            "photo_send",
            "find a real photo in the user's library and send it",
            Some("query"),
        ),
    ]
}

/// The result of scoring one candidate.
pub struct BenchResult {
    pub model: String,
    pub correct: usize,
    pub total: usize,
    pub wrong_tool: Vec<String>,
    pub spurious: Vec<String>,
    pub missed: Vec<String>,
    pub bad_args: Vec<String>,
    pub no_native_calls: bool,
}

impl BenchResult {
    pub fn render(&self) -> String {
        let pct = if self.total == 0 {
            0.0
        } else {
            self.correct as f64 * 100.0 / self.total as f64
        };
        let mut s = format!(
            "\n=== BRAIN BENCH — {} ===\n  tool selection: {}/{} ({pct:.0}%)\n",
            self.model, self.correct, self.total
        );
        if self.no_native_calls {
            s.push_str(
                "  ⚠ NO NATIVE TOOL CALLS AT ALL — this endpoint ignored the `tools` parameter.\n     \
                 The loop would fall back to free-text JSON parsing for every turn, which is the\n     \
                 fragility the native migration removed. Disqualifying on its own.\n",
            );
        }
        for (label, items) in [
            ("picked the WRONG tool", &self.wrong_tool),
            ("called a tool when NONE was needed", &self.spurious),
            ("called NO tool when one was needed", &self.missed),
            ("right tool, unusable ARGS", &self.bad_args),
        ] {
            if !items.is_empty() {
                s.push_str(&format!("  {label}:\n"));
                for i in items {
                    s.push_str(&format!("    · {i}\n"));
                }
            }
        }
        s.push_str(
            "  (scored on THIS mind's workload — tool selection under a gated catalog — not on a\n   \
             public leaderboard. A model can top SWE-bench and still fail here.)\n",
        );
        s
    }
}

/// Score one candidate brain. Any OpenAI-compatible endpoint.
///
/// ENDPOINT CAVEAT, learned by getting it wrong. `ApiLLM` decides an endpoint is Ollama-native by
/// looking for `:11434` / `ollama.com` in the URL; anything else is talked to OpenAI-style at
/// `<base>/chat/completions`. The live brain pool's primary link is an Ollama gateway on a plain
/// HTTPS host with no port (`https://aig.mycluster.cyou`), so passing the bare host makes the bench
/// POST to the wrong path and score a perfectly healthy model 0/6 with "request failed" — which
/// nearly got reported as "your primary brain is broken".
///
/// PASS THE PROTOCOL-CORRECT BASE URL: that gateway also serves OpenAI-compatible routes, so
/// `https://aig.mycluster.cyou/v1` benches it correctly. An instrument that misreports a healthy
/// subject is worse than no instrument, so when a candidate scores 0 with request failures, suspect
/// the URL before the model.
pub async fn bench(url: &str, key: Option<String>, model: &str) -> BenchResult {
    let backend = yantrik_ml::ApiLLM::new(url, key, model);
    let pool =
        InferencePool::new(Arc::new(backend) as Arc<dyn LLMBackend>, 1).with_provider("bench");
    let tools = schemas();
    let mut r = BenchResult {
        model: model.to_string(),
        correct: 0,
        total: 0,
        wrong_tool: vec![],
        spurious: vec![],
        missed: vec![],
        bad_args: vec![],
        no_native_calls: true,
    };
    // The same instruction the live loop uses, so the candidate is judged in our conditions.
    const SYS: &str = "You are an agent, not a chatbot — you ACT. Think, use ONE tool if one fits, \
                       observe, then answer. If no tool fits the user's message, just answer — do NOT \
                       call a tool for the sake of it.";
    for c in cases() {
        r.total += 1;
        let messages = vec![ChatMessage::system(SYS), ChatMessage::user(c.turn)];
        let resp = pool
            .chat_scoped_tools(
                messages,
                GenerationConfig {
                    max_tokens: 400,
                    ..GenerationConfig::default()
                },
                mind_inference::PrivacyScope::Public,
                tools.clone(),
            )
            .await;
        let Ok(resp) = resp else {
            r.missed.push(format!("{:?} — request failed", c.turn));
            continue;
        };
        let called = resp
            .tool_calls
            .first()
            .map(|t| (t.name.clone(), t.arguments.to_string()));
        if called.is_some() {
            r.no_native_calls = false;
        }
        match (called, c.expect_tool) {
            (None, None) => r.correct += 1, // correctly stayed quiet
            (None, Some(want)) => r
                .missed
                .push(format!("{:?} — wanted {want} ({})", c.turn, c.why)),
            (Some((got, _)), None) => r
                .spurious
                .push(format!("{:?} — called {got} ({})", c.turn, c.why)),
            (Some((got, args)), Some(want)) => {
                if got != want {
                    r.wrong_tool
                        .push(format!("{:?} — got {got}, wanted {want}", c.turn));
                } else if let Some(a) = c.expect_arg {
                    if args.to_lowercase().contains(a) {
                        r.correct += 1;
                    } else {
                        r.bad_args
                            .push(format!("{:?} — {got} args missing {a:?}: {args}", c.turn));
                    }
                } else {
                    r.correct += 1;
                }
            }
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Score whatever endpoint the env points at. Ignored by default — it costs real tokens and
    /// needs a live provider.
    ///
    ///   YM_BENCH_URL=https://api.example.com/v1 YM_BENCH_KEY=sk-... YM_BENCH_MODEL=qwen3.8-max \
    ///     cargo test -p mind-evals brain_bench -- --ignored --nocapture
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    #[ignore = "needs a live provider endpoint; costs tokens"]
    async fn brain_bench_candidate() {
        let url =
            std::env::var("YM_BENCH_URL").unwrap_or_else(|_| "http://192.168.4.35:11434".into());
        let key = std::env::var("YM_BENCH_KEY").ok().filter(|k| !k.is_empty());
        let model = std::env::var("YM_BENCH_MODEL").unwrap_or_else(|_| "qwen3.6:27b".into());
        let r = bench(&url, key, &model).await;
        println!("{}", r.render());
        // Reported, not asserted: the point is the NUMBER for comparing candidates, and a hard
        // threshold here would just encode today's best guess as tomorrow's false failure.
        assert!(r.total > 0);
    }

    /// The scoring logic itself is graded without a network, so a bench bug cannot masquerade as a
    /// model result.
    #[test]
    fn a_spurious_call_is_counted_against_a_candidate() {
        let mut r = BenchResult {
            model: "x".into(),
            correct: 5,
            total: 6,
            wrong_tool: vec![],
            spurious: vec!["\"thanks, that's great\" — called search".into()],
            missed: vec![],
            bad_args: vec![],
            no_native_calls: false,
        };
        let out = r.render();
        assert!(out.contains("called a tool when NONE was needed"), "{out}");
        assert!(out.contains("83%"), "score reflects the miss: {out}");
        r.no_native_calls = true;
        assert!(
            r.render().contains("ignored the `tools` parameter"),
            "a non-tool-calling endpoint is flagged"
        );
    }

    #[test]
    fn the_workload_covers_selection_abstention_and_args() {
        let cs = cases();
        assert!(
            cs.iter().any(|c| c.expect_tool.is_none()),
            "must test ABSTENTION, not just selection"
        );
        assert!(
            cs.iter().any(|c| c.expect_arg.is_some()),
            "must test that ARGS are usable"
        );
        assert!(
            schemas().len() >= 8,
            "candidates must choose from many similar tools, not one"
        );
    }
}
