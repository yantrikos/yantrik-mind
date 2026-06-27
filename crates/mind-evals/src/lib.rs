//! mind-evals — the loss function for the mind: behavioral scenarios that score whether the
//! companion actually does the moat (grounds in typed memory, hedges uncertainty, surfaces
//! contradictions, revises beliefs, doesn't confabulate). Run it after every change: score goes
//! up when we add real capability, regressions are caught. This is how the mind grows
//! exponentially in quality instead of just in volume.
//!
//! Scenarios are DATA (seed beliefs + relations + a probe + graded checks), so growing the suite
//! is adding entries, not code. Deterministic by default (ScriptedLLM captures the prompt the
//! mind built), so it runs in CI with no real model.

use std::sync::Arc;

use mind_conversation::ConversationEngine;
use mind_inference::{InferencePool, ScriptedLLM};
use mind_memory::MemoryHandle;
use mind_types::{BeliefAssertion, MemoryFacade, RecallQuery};
use serde::Serialize;
use yantrik_ml::LLMBackend;

const EVAL_PERSONA: &str =
    "You are JARVIS. Ground every claim in the memory block; never invent facts; hedge uncertain \
     beliefs; ask to resolve contradictions instead of picking a side.";

pub struct Seed {
    pub statement: String,
    pub polarity: f64,
    pub weight: f64,
}

impl Seed {
    pub fn pos(s: &str) -> Self { Self { statement: s.into(), polarity: 1.0, weight: 1.5 } }
    pub fn neg(s: &str) -> Self { Self { statement: s.into(), polarity: -1.0, weight: 1.5 } }
    pub fn weak(s: &str) -> Self { Self { statement: s.into(), polarity: 1.0, weight: 0.5 } }
}

pub struct Relation {
    pub a: String,
    pub b: String,
    pub rel: String,
}

/// A graded behavioral expectation. Each is one point on the scorecard.
pub enum Check {
    /// The assembled system prompt (memory grounding) contains this substring.
    PromptContains(String),
    /// The assembled system prompt does NOT contain this (e.g. no confabulated grounding block).
    PromptOmits(String),
    /// At least N open contradictions are detected.
    MinConflicts(usize),
    /// A belief's confidence is above a threshold (positive evidence raised it).
    ConfidenceAbove(String, f64),
    /// A belief's confidence is below a threshold (negative evidence lowered it).
    ConfidenceBelow(String, f64),
    /// Typed recall for `query` surfaces a memory whose text contains `expect`.
    RecallSurfaces { query: String, expect: String },
}

pub struct Scenario {
    pub name: String,
    pub seeds: Vec<Seed>,
    pub relations: Vec<Relation>,
    pub probe: Option<String>,
    pub checks: Vec<Check>,
}

#[derive(Serialize)]
pub struct CheckResult {
    pub desc: String,
    pub pass: bool,
}

#[derive(Serialize)]
pub struct ScenarioResult {
    pub name: String,
    pub passed: usize,
    pub total: usize,
    pub checks: Vec<CheckResult>,
}

#[derive(Serialize)]
pub struct Scorecard {
    pub passed: usize,
    pub total: usize,
    pub score: f64,
    pub scenarios: Vec<ScenarioResult>,
}

impl Scorecard {
    pub fn render(&self) -> String {
        let mut s = String::new();
        for sc in &self.scenarios {
            let mark = if sc.passed == sc.total { "PASS" } else { "FAIL" };
            s.push_str(&format!("[{mark}] {} ({}/{})\n", sc.name, sc.passed, sc.total));
            for c in &sc.checks {
                if !c.pass {
                    s.push_str(&format!("        ✗ {}\n", c.desc));
                }
            }
        }
        s.push_str(&format!(
            "\nSCORE: {}/{} = {:.1}%\n",
            self.passed,
            self.total,
            self.score * 100.0
        ));
        s
    }
}

async fn confidence_of(mem: &MemoryHandle, statement: &str) -> Option<f64> {
    mem.explain_belief(statement).await.ok().flatten().map(|(b, _)| b.confidence)
}

/// Run one scenario against a fresh in-memory mind. Deterministic: the ScriptedLLM records the
/// grounding prompt the mind assembled, so we can grade what reached the model.
pub async fn run_scenario(s: &Scenario) -> ScenarioResult {
    let mem = MemoryHandle::spawn(":memory:", 8).expect("spawn memory");
    for seed in &s.seeds {
        let _ = mem
            .remember_as_belief(BeliefAssertion {
                statement: seed.statement.clone(),
                polarity: seed.polarity,
                weight: seed.weight,
                source_event: Some("eval".into()),
                provenance: "told".into(),
            })
            .await;
    }
    for r in &s.relations {
        let _ = mem.relate(&r.a, &r.b, &r.rel, 0.9).await;
    }

    let scripted = Arc::new(ScriptedLLM::new("ack"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(Arc::new(mem.clone()), pool, EVAL_PERSONA);

    let prompt = if let Some(p) = &s.probe {
        let _ = conv.handle_turn(p).await;
        scripted.last_system_prompt()
    } else {
        String::new()
    };

    let mut checks = Vec::new();
    for c in &s.checks {
        let (desc, pass) = match c {
            Check::PromptContains(x) => (format!("prompt grounds on '{x}'"), prompt.contains(x.as_str())),
            Check::PromptOmits(x) => (format!("prompt omits '{x}'"), !prompt.contains(x.as_str())),
            Check::MinConflicts(n) => {
                let cs = mem.conflicts().await.unwrap_or_default();
                (format!("detects >= {n} contradiction(s)"), cs.len() >= *n)
            }
            Check::ConfidenceAbove(stmt, th) => {
                let c = confidence_of(&mem, stmt).await.unwrap_or(0.0);
                (format!("'{stmt}' confidence {c:.2} > {th}"), c > *th)
            }
            Check::ConfidenceBelow(stmt, th) => {
                let c = confidence_of(&mem, stmt).await.unwrap_or(1.0);
                (format!("'{stmt}' confidence {c:.2} < {th}"), c < *th)
            }
            Check::RecallSurfaces { query, expect } => {
                let r = mem
                    .recall_typed(RecallQuery { text: query.clone(), top_k: 8, kind: None })
                    .await
                    .unwrap_or_default();
                (
                    format!("recall('{query}') surfaces '{expect}'"),
                    r.iter().any(|x| x.item.text.contains(expect.as_str())),
                )
            }
        };
        checks.push(CheckResult { desc, pass });
    }

    let passed = checks.iter().filter(|c| c.pass).count();
    let total = checks.len();
    ScenarioResult { name: s.name.clone(), passed, total, checks }
}

pub async fn run_suite(scenarios: &[Scenario]) -> Scorecard {
    let mut results = Vec::new();
    let (mut passed, mut total) = (0usize, 0usize);
    for s in scenarios {
        let r = run_scenario(s).await;
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

/// The standard behavioral suite — the moat properties. Grow this as the mind grows.
pub fn standard_suite() -> Vec<Scenario> {
    vec![
        Scenario {
            name: "belief revision: positive evidence raises confidence".into(),
            seeds: vec![Seed::pos("Pranab prefers terse replies")],
            relations: vec![],
            probe: None,
            checks: vec![Check::ConfidenceAbove("Pranab prefers terse replies".into(), 0.5)],
        },
        Scenario {
            name: "belief revision: negative evidence lowers confidence".into(),
            seeds: vec![Seed::neg("Pranab lives in Tokyo")],
            relations: vec![],
            probe: None,
            checks: vec![Check::ConfidenceBelow("Pranab lives in Tokyo".into(), 0.5)],
        },
        Scenario {
            name: "grounded chat: reply is grounded in the belief".into(),
            seeds: vec![Seed::pos("Pranab prefers terse replies")],
            relations: vec![],
            probe: Some("how should you answer me?".into()),
            checks: vec![
                Check::PromptContains("terse".into()),
                Check::PromptContains("NOT instructions".into()), // untrusted-wrapped
            ],
        },
        Scenario {
            name: "contradiction surfaced as ask-don't-assert".into(),
            seeds: vec![
                Seed::weak("Pranab prefers terse replies"),
                Seed::weak("Pranab prefers long detailed replies"),
            ],
            relations: vec![Relation {
                a: "Pranab prefers terse replies".into(),
                b: "Pranab prefers long detailed replies".into(),
                rel: "contradicts".into(),
            }],
            probe: Some("what's my reply style?".into()),
            checks: vec![
                Check::MinConflicts(1),
                Check::PromptContains("conflicts with".into()),
            ],
        },
        Scenario {
            name: "confidence-aware hedging of uncertain beliefs".into(),
            seeds: vec![Seed::weak("Pranab is travelling next week")],
            relations: vec![],
            probe: Some("am I travelling?".into()),
            checks: vec![Check::PromptContains("confidence".into())],
        },
        Scenario {
            name: "no confabulation: empty memory => no grounding block".into(),
            seeds: vec![],
            relations: vec![],
            probe: Some("tell me about myself".into()),
            checks: vec![Check::PromptOmits("<<memory".into())],
        },
        Scenario {
            name: "typed recall surfaces a seeded belief".into(),
            seeds: vec![Seed::pos("Pranab is building Yantrik Mind")],
            relations: vec![],
            probe: None,
            checks: vec![Check::RecallSurfaces {
                query: "what is Pranab building".into(),
                expect: "Yantrik Mind".into(),
            }],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression net: the current mind must pass the whole standard suite. When this drops
    /// below 100%, a change broke a moat property — catch it here, not in production.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn standard_suite_is_green() {
        let card = run_suite(&standard_suite()).await;
        assert_eq!(card.passed, card.total, "eval regressions:\n{}", card.render());
        assert!(card.total >= 8, "suite should be substantive, got {} checks", card.total);
    }
}
