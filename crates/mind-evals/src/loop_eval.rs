//! Agent-LOOP eval — deterministic behavioral scenarios that score the AGENTIC LOOP's machinery
//! (not model quality): tool dispatch + observation feedback, the loop-guard, budget/termination,
//! failed-tool recovery, and grounding. Complements the memory/grounding suite in `lib.rs` (which
//! runs the deterministic dispatch chain); here we drive the ReAct loop itself.
//!
//! The enabling primitive is [`mind_inference::SequencedLLM`]: it returns a scripted SEQUENCE of
//! replies (one per model call) and records the prompt seen on each call, so a scenario can script a
//! multi-step trajectory (step 0 → tool call, step 1 → answer, …) and grade what the loop did with no
//! real model. The loop is driven through `ConversationEngine::agent_loop_for_eval` (a test seam that
//! bypasses the deterministic turn interceptors).
//!
//! Design follows the SOTA eval guidance (rid 019f5e93): grade OBSERVED actions + final answer +
//! per-step prompts (state), never narrated reasoning; use objective substring/count checks, no judge.

use std::sync::Arc;

use mind_conversation::{ConversationEngine, TurnIdentity};
use mind_inference::{InferencePool, SequencedLLM};
use mind_memory::MemoryHandle;
use mind_types::{BeliefAssertion, MemoryFacade};
use yantrik_ml::LLMBackend;

use crate::{CheckResult, Scorecard, ScenarioResult};

/// A graded expectation about the loop's behavior on a scenario.
pub enum Grade {
    /// The final answer returned to the user contains this substring.
    AnswerContains(String),
    /// The final answer does NOT contain this (anti-confabulation / anti-leak).
    AnswerOmits(String),
    /// The model was called AT MOST n times (step efficiency; a loop-guard/budget bound).
    MaxCalls(usize),
    /// The model was called AT LEAST n times (proves the loop iterated / composed).
    MinCalls(usize),
    /// The prompt on model call `i` (0-based) contains this substring — e.g. an observation fed
    /// forward, the "FAILED" guidance, or the step-budget note.
    PromptAtContains(usize, String),
    /// The prompt on model call `i` does NOT contain this substring — e.g. an irrelevant tool's
    /// detail line gated out of the catalog (the retrieval-gating token-cut property).
    PromptAtOmits(usize, String),
    /// A native tool SCHEMA with this function name was passed to the backend on call `i` — the
    /// core property of the function-calling migration (the loop hands the model structured tools).
    SchemaAt(usize, String),
}

/// One agent-loop scenario: seed memory, a scripted reply SEQUENCE, the user turn, and graders.
pub struct LoopScenario {
    pub name: String,
    /// Facts to seed before the turn (statement, positive).
    pub seeds: Vec<String>,
    /// The model's reply on each successive call (step 0, step 1, …; compose call included). A reply
    /// is either a tool-call JSON `{"thought":"…","tool":"…","args":{…}}` or an answer
    /// `{"thought":"…","answer":"…"}`; the compose step takes its reply as plain text.
    pub replies: Vec<String>,
    /// Optional NATIVE tool call scripted per step (parallel to `replies`): `Some((tool, args))`
    /// makes that step return a STRUCTURED function call (the native path) instead of a free-text
    /// JSON blob. Empty ⇒ every step is free-text (the fallback path). Lets one suite exercise both.
    pub native: Vec<Option<(String, serde_json::Value)>>,
    pub turn: String,
    pub grades: Vec<Grade>,
}

/// Build a native-call scenario step: `(reply_text, Some((tool, args)))`. The reply text can be
/// empty — a native model carries its intent in the structured call, not the content.
fn native(tool: &str, args: serde_json::Value) -> Option<(String, serde_json::Value)> {
    Some((tool.to_string(), args))
}

/// Run one loop scenario against a fresh in-memory mind driven by a `SequencedLLM`.
pub async fn run_loop_scenario(s: &LoopScenario) -> ScenarioResult {
    let mem = MemoryHandle::spawn(":memory:", 8).expect("spawn memory");
    for stmt in &s.seeds {
        let _ = mem
            .remember_as_belief(BeliefAssertion {
                statement: stmt.clone(),
                polarity: 1.0,
                weight: 1.5,
                source_event: Some("eval".into()),
                provenance: "told".into(),
            })
            .await;
    }
    let mut seq = SequencedLLM::new(s.replies.clone());
    if !s.native.is_empty() {
        seq = seq.with_native(
            s.native
                .iter()
                .map(|o| o.as_ref().map(|(n, a)| (n.as_str(), a.clone())))
                .collect(),
        );
    }
    let seq = Arc::new(seq);
    let pool = InferencePool::new(seq.clone() as Arc<dyn LLMBackend>, 1);
    // agent_primary(true) is the default; web_fetch succeeds (ScriptedFetcher), while github/mail/home
    // are intentionally left UNCONFIGURED so calls to them return a failure observation — the harness
    // needs both a success and a failure tool path. NO egress broker / recipes are wired, so the loop
    // runs without extra model calls (egress-clean re-authoring + cited_answer stay inert).
    let conv = ConversationEngine::new(
        Arc::new(mem.clone()),
        pool,
        mind_types::default_persona("the user"),
    )
    .with_web(Arc::new(mind_tools::ScriptedFetcher::new("WEBDOC: Teal is a cyan-family blue-green color.")));

    let answer = conv
        .agent_loop_for_eval(&s.turn, &TurnIdentity::primary())
        .await
        .unwrap_or_else(|e| format!("(error: {e})"));

    let calls = seq.call_count();
    let mut checks = Vec::new();
    for g in &s.grades {
        let (desc, pass) = match g {
            Grade::AnswerContains(x) => (format!("answer contains '{x}'"), answer.contains(x.as_str())),
            Grade::AnswerOmits(x) => (format!("answer omits '{x}'"), !answer.contains(x.as_str())),
            Grade::MaxCalls(n) => (format!("model called <= {n} (was {calls})"), calls <= *n),
            Grade::MinCalls(n) => (format!("model called >= {n} (was {calls})"), calls >= *n),
            Grade::PromptAtContains(i, x) => (
                format!("prompt[{i}] contains '{x}'"),
                seq.prompt_at(*i).contains(x.as_str()),
            ),
            Grade::PromptAtOmits(i, x) => (
                format!("prompt[{i}] omits '{x}'"),
                !seq.prompt_at(*i).contains(x.as_str()),
            ),
            Grade::SchemaAt(i, name) => (
                format!("tool schema '{name}' passed on call {i}"),
                seq.tools_at(*i)
                    .iter()
                    .any(|t| t["function"]["name"].as_str() == Some(name.as_str())),
            ),
        };
        checks.push(CheckResult { desc, pass });
    }
    let passed = checks.iter().filter(|c| c.pass).count();
    let total = checks.len();
    ScenarioResult { name: s.name.clone(), passed, total, checks }
}

pub async fn run_loop_suite(scenarios: &[LoopScenario]) -> Scorecard {
    let (mut passed, mut total) = (0usize, 0usize);
    let mut results = Vec::new();
    for s in scenarios {
        let r = run_loop_scenario(s).await;
        passed += r.passed;
        total += r.total;
        results.push(r);
    }
    Scorecard {
        passed,
        total,
        score: if total == 0 { 0.0 } else { passed as f64 / total as f64 },
        scenarios: results,
    }
}

/// A tool-call reply for the scripted sequence.
fn call(tool: &str, args: serde_json::Value) -> String {
    serde_json::json!({ "thought": "step", "tool": tool, "args": args }).to_string()
}
/// An answer reply for the scripted sequence.
fn answer(text: &str) -> String {
    serde_json::json!({ "thought": "done", "answer": text }).to_string()
}

/// The standard agent-loop behavioral suite — grow as the loop grows. Each scenario locks one
/// machinery property so a future refactor (native tool-calling, retrieval-gating) can't silently
/// regress it.
pub fn loop_suite() -> Vec<LoopScenario> {
    vec![
        // 1. Clean tool → answer: the observation is fed forward into the next prompt, and the
        //    scripted answer is returned. Proves the core reason→act→observe→answer flow.
        LoopScenario {
            name: "tool result feeds forward, then answers".into(),
            seeds: vec![],
            replies: vec![
                call("web_fetch", serde_json::json!({ "url": "http://example.com" })),
                answer("Teal is a blue-green color."),
            ],
            native: vec![],
            turn:"what color is teal?".into(),
            grades: vec![
                Grade::PromptAtContains(1, "WEBDOC".into()), // the tool obs reached the next step
                Grade::AnswerContains("Teal is a blue-green".into()),
                Grade::MaxCalls(2), // one tool step + the answer step; no compose needed
            ],
        },
        // 2. Loop-guard: an identical repeated tool call must stop the loop early (not run all 5
        //    steps). Directly targets the dominant failure mode (verbatim-retry loop).
        LoopScenario {
            name: "loop-guard stops an identical repeated call".into(),
            seeds: vec![],
            replies: vec![
                call("home", serde_json::json!({})),
                call("home", serde_json::json!({})), // identical → loop-guard breaks
                call("home", serde_json::json!({})),
                call("home", serde_json::json!({})),
                call("home", serde_json::json!({})),
                answer("Here's the home status."),
            ],
            native: vec![],
            turn:"check the house".into(),
            // step 0 (home) + step 1 (identical → break) + compose = 3 calls, not 5+compose.
            grades: vec![Grade::MaxCalls(3)],
        },
        // 3. Budget/termination: a model that never emits an answer must still produce a final
        //    composed answer at the step cap (no infinite loop, no empty reply).
        LoopScenario {
            name: "step budget forces a composed final answer".into(),
            seeds: vec![],
            replies: vec![
                call("now", serde_json::json!({})),
                call("web_fetch", serde_json::json!({ "url": "http://a.com" })),
                call("wikipedia", serde_json::json!({ "query": "x" })),
                call("weather", serde_json::json!({ "place": "pune" })),
                call("crypto", serde_json::json!({ "coin": "btc" })),
                "Composed answer from the work log.".into(), // the compose call (plain text)
            ],
            native: vec![],
            turn:"do a bunch of lookups".into(),
            grades: vec![
                Grade::MinCalls(6), // 5 loop steps + 1 compose
                Grade::AnswerContains("Composed answer".into()),
            ],
        },
        // 4. Failed-tool recovery: an unconfigured tool returns a failure observation, and the next
        //    prompt must carry the explicit "FAILED … do NOT repeat" guidance (not a verbatim retry).
        LoopScenario {
            name: "failed tool result changes the next action".into(),
            seeds: vec![],
            replies: vec![
                call("github_repo_items", serde_json::json!({ "repo": "acme/x" })), // github unconfigured → fails
                answer("I couldn't reach GitHub."),
            ],
            native: vec![],
            turn:"what are my open PRs?".into(),
            grades: vec![
                Grade::PromptAtContains(1, "FAILED".into()), // failure guidance injected
                Grade::AnswerContains("couldn't reach GitHub".into()),
            ],
        },
        // 5. Retrieval-gating (Tier hybrid): a message-relevant tool keeps its full detail line,
        //    an irrelevant one is abbreviated to its NAME in the tail — smaller prompt, but the
        //    tool stays visible (an absent tool → confabulation was the original scar).
        LoopScenario {
            name: "gated catalog: relevant detailed, irrelevant name-only".into(),
            seeds: vec![],
            replies: vec![
                call("weather", serde_json::json!({ "place": "pune" })),
                answer("Sunny in Pune today."),
            ],
            native: vec![],
            turn:"what's the weather in pune?".into(),
            grades: vec![
                Grade::PromptAtContains(0, "- weather {place}".into()), // relevant → full line
                Grade::PromptAtOmits(0, "growup_reel {name}".into()),   // irrelevant → no detail line
                Grade::PromptAtContains(0, "growup_reel".into()),       // …but the NAME survives in the tail
                Grade::AnswerContains("Sunny in Pune".into()),
            ],
        },
        // 6. Every tool reachable: a tool rendered NAME-ONLY for this turn must still dispatch
        //    when called — gating is prompt-presentation, never a capability wall.
        LoopScenario {
            name: "name-only tail tool still dispatches".into(),
            seeds: vec![],
            replies: vec![
                call("home", serde_json::json!({})), // irrelevant to the turn → name-only in the catalog
                answer("The house systems look fine."),
            ],
            native: vec![],
            turn:"what color is teal?".into(),
            grades: vec![
                Grade::PromptAtOmits(0, "- home {}".into()), // gated out of the detailed section
                Grade::PromptAtContains(1, "home".into()),   // yet the call ran: its result is in the work log
                Grade::AnswerContains("house systems look fine".into()),
            ],
        },
        // 7. The escape hatch: discover_tools searches the NATIVE catalog too, so a gated-out tool's
        //    full description is one call away (not just the skill library).
        LoopScenario {
            name: "discover_tools surfaces a gated native tool".into(),
            seeds: vec![],
            replies: vec![
                call("discover_tools", serde_json::json!({ "query": "track a price drop" })),
                answer("I can watch that price for you — say the word."),
            ],
            native: vec![],
            turn:"help me with something".into(),
            grades: vec![
                Grade::PromptAtContains(1, "watch_price".into()), // the native tool came back with detail
                Grade::AnswerContains("watch that price".into()),
            ],
        },
        // 8. NATIVE FUNCTION-CALLING dispatch: step 0 returns a STRUCTURED tool call (not free-text
        //    JSON), the loop dispatches it, and the observation feeds forward exactly as on the
        //    free-text path. Also proves the loop PASSED tool schemas to the backend.
        LoopScenario {
            name: "native tool call dispatches, observation feeds forward".into(),
            seeds: vec![],
            replies: vec![
                String::new(),                       // native model carries intent in the call, not content
                answer("Teal is a blue-green color."), // step 1: free-text answer (fallback path still works)
            ],
            native: vec![
                native("web_fetch", serde_json::json!({ "url": "http://example.com" })),
                None,
            ],
            turn: "what color is teal?".into(),
            grades: vec![
                Grade::SchemaAt(0, "recall".into()),        // core schema always offered
                Grade::SchemaAt(0, "web_fetch".into()),     // the tool it called was in the schema set
                Grade::PromptAtContains(1, "WEBDOC".into()), // native call dispatched → obs fed forward
                Grade::AnswerContains("Teal is a blue-green".into()),
                Grade::MaxCalls(2),
            ],
        },
        // 9. NATIVE ANSWER: a backend that models the final reply as an answer(text) tool call is
        //    normalized to a plain answer (no extra compose call, no JSON leaking into the chat).
        LoopScenario {
            name: "native answer(text) call returns the reply".into(),
            seeds: vec![],
            replies: vec![String::new()],
            native: vec![native("answer", serde_json::json!({ "text": "All good at home." }))],
            turn: "everything ok at the house?".into(),
            grades: vec![
                Grade::AnswerContains("All good at home".into()),
                Grade::AnswerOmits("{".into()), // no raw JSON in the user-facing reply
                Grade::MaxCalls(1),
            ],
        },
        // 10. SCHEMA/PROSE LOCKSTEP: the native schema set mirrors the gated prose — a message-
        //     relevant tool gets a schema, and the free-text fallback path STILL receives schemas
        //     (belt-and-suspenders: a non-tool-calling model can ignore them, a capable one can't).
        LoopScenario {
            name: "schemas passed mirror the gated set (free-text path too)".into(),
            seeds: vec![],
            replies: vec![
                call("weather", serde_json::json!({ "place": "pune" })), // free-text call (no native)
                answer("It's sunny in Pune."),
            ],
            native: vec![],
            turn: "what's the weather in pune?".into(),
            grades: vec![
                Grade::SchemaAt(0, "weather".into()), // relevant tool → schema present (mirrors prose gate)
                Grade::SchemaAt(0, "now".into()),     // core always present
                Grade::AnswerContains("sunny in Pune".into()),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn agent_loop_behavioral_suite_passes() {
        let card = run_loop_suite(&loop_suite()).await;
        assert_eq!(card.passed, card.total, "agent-loop eval regressions:\n{}", card.render());
    }
}
