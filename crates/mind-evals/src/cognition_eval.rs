//! Cognitive-LOOP eval — behavioral scenarios for the BOUNDED control loop (`YM_COGNITION`),
//! driven through the REAL engine bus so what is graded is the production wiring: `mind_agents::
//! Cognition` over `mind_conversation::cognitive::EngineBus` over `run_agent_tool_as`, with every
//! engine guard live. Complements `loop_eval.rs`, which drives the legacy ReAct loop through the
//! same engine — together they are the promotion gate cognitive.rs names: "settled by mind-evals
//! scoring both against the same scenarios rather than by whoever wrote it being confident."
//!
//! The same enabling primitive as loop_eval: [`mind_inference::SequencedLLM`] scripts the model's
//! reply sequence (NBA decisions, replans, the synthesis) and the grade is over OBSERVED state —
//! the capsule, the verdict, the stop reason — never narrated reasoning.

use std::sync::Arc;

use mind_agents::Cognition;
use mind_conversation::cognitive::EngineBus;
use mind_conversation::{ConversationEngine, TurnIdentity};
use mind_inference::{InferencePool, SequencedLLM};
use mind_memory::MemoryHandle;
use mind_spec::control::ReasonCode;
use mind_spec::goal::{Budget, CompletionCriteria, Contract, GoalSpec, OutputContract};
use mind_types::MemoryFacade;
use yantrik_ml::LLMBackend;

use crate::{CheckResult, ScenarioResult, Scorecard};

/// One cognitive-loop scenario: a scripted reply sequence, a goal contract, and graders over the
/// run's OUTCOME (capsule + verdict + stop reason), which the legacy suite cannot see.
pub struct CognitionScenario {
    pub name: String,
    /// The model's reply on each successive call (NBA decisions, replan arrays, the synthesis).
    pub replies: Vec<String>,
    pub goal: String,
    pub min_findings: usize,
    pub budget: Budget,
    /// Graders over the finished run.
    pub grades: Vec<CogGrade>,
}

pub enum CogGrade {
    /// The contract was met.
    Complete(bool),
    /// The run stopped for exactly this reason.
    StoppedBecause(ReasonCode),
    /// `capsule.progress.failures` is exactly this — the empty-is-not-failure regression lock.
    Failures(u32),
    /// `capsule.progress.barren_steps` is at least this — the stall stayed VISIBLE.
    MinBarren(u32),
    /// A `capsule.failures` entry contains this substring (the recovery hint travelled).
    FailureNoteContains(String),
    /// The final answer contains this.
    AnswerContains(String),
    /// The evidence ids the run holds, exactly.
    EvidenceIds(Vec<String>),
}

fn goal_spec(s: &CognitionScenario) -> GoalSpec {
    GoalSpec {
        contract: Contract {
            requirements: Vec::new(),
            completion: CompletionCriteria {
                min_findings: s.min_findings,
                require_full_coverage: false,
                ..Default::default()
            },
            output: OutputContract::default(),
        },
        budget: s.budget.clone(),
        ..GoalSpec::simple(s.goal.clone())
    }
}

/// Run one scenario against a fresh in-memory mind, through the REAL bus.
pub async fn run_cognition_scenario(s: &CognitionScenario) -> ScenarioResult {
    let mem = MemoryHandle::spawn(":memory:", 8).expect("spawn memory");
    let seq = Arc::new(SequencedLLM::new(s.replies.clone()));
    let pool = InferencePool::new(seq.clone() as Arc<dyn LLMBackend>, 1);
    // Same harness posture as loop_eval: web_fetch succeeds (ScriptedFetcher), github/mail stay
    // unconfigured so the unavailable path is exercisable, no recipes (grounding seam absent →
    // verified must stay None, never Some(true)).
    let engine = Arc::new(
        ConversationEngine::new(
            Arc::new(mem.clone()),
            pool.clone(),
            mind_types::default_persona("the user"),
        )
        .with_web(Arc::new(mind_tools::ScriptedFetcher::new(
            "WEBDOC: Teal is a cyan-family blue-green color.",
        ))),
    );
    let bus = Arc::new(EngineBus::new(engine, TurnIdentity::primary()));
    let cognition = Cognition::new(pool.clone(), pool, bus, "JARVIS");
    let out = cognition.run(&goal_spec(s), &mind_types::clock::SystemClock).await;

    let mut checks = Vec::new();
    for g in &s.grades {
        let (desc, pass) = match g {
            CogGrade::Complete(want) => (format!("complete == {want}"), out.complete() == *want),
            CogGrade::StoppedBecause(code) => (
                format!("stopped because {code:?} (was {:?})", out.stopped_because),
                out.stopped_because == Some(*code),
            ),
            CogGrade::Failures(n) => (
                format!("failures == {n} (was {})", out.capsule.progress.failures),
                out.capsule.progress.failures == *n,
            ),
            CogGrade::MinBarren(n) => (
                format!("barren_steps >= {n} (was {})", out.capsule.progress.barren_steps),
                out.capsule.progress.barren_steps >= *n,
            ),
            CogGrade::FailureNoteContains(x) => (
                format!("a failure note contains '{x}'"),
                out.capsule.failures.iter().any(|f| f.contains(x.as_str())),
            ),
            CogGrade::AnswerContains(x) => {
                (format!("answer contains '{x}'"), out.answer.contains(x.as_str()))
            }
            CogGrade::EvidenceIds(ids) => {
                let got: Vec<String> = out.capsule.evidence.iter().map(|e| e.id.clone()).collect();
                (format!("evidence ids == {ids:?} (was {got:?})"), &got == ids)
            }
        };
        checks.push(CheckResult { desc, pass });
    }
    let passed = checks.iter().filter(|c| c.pass).count();
    let total = checks.len();
    ScenarioResult { name: s.name.clone(), passed, total, checks, calls: seq.call_count() }
}

pub async fn run_cognition_suite(scenarios: &[CognitionScenario]) -> Scorecard {
    let (mut passed, mut total) = (0usize, 0usize);
    let mut results = Vec::new();
    for s in scenarios {
        let r = run_cognition_scenario(s).await;
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

/// An NBA tool-call decision.
fn call(tool: &str, query: &str) -> String {
    format!(r#"{{"verb":"CALL_TOOL","target":"{tool}","args":{{"query":"{query}"}},"why":"NEED_EVIDENCE"}}"#)
}
/// An NBA decision that reports what the last step established AND finishes.
fn learned_then_finish(claim: &str, ev: &str) -> String {
    format!(
        r#"{{"learned":{{"findings":[{{"claim":"{claim}","evidence":["{ev}"]}}]}},"verb":"FINISH","why":"SUFFICIENT"}}"#
    )
}

fn budget(max_steps: u32, max_model_calls: u32) -> Budget {
    Budget { max_steps, max_model_calls, max_wall_ms: 60_000, max_usd: None }
}

/// The standard cognitive-loop behavioral suite.
pub fn cognition_suite() -> Vec<CognitionScenario> {
    vec![
        // 1. HAPPY PATH PARITY with loop_eval #1: fetch through the real engine, promote to a
        //    finding, meet the contract, synthesize. The skeleton every comparison hangs off.
        CognitionScenario {
            name: "evidence is gathered through the real bus, then the contract is met".into(),
            replies: vec![
                format!(r#"{{"verb":"CALL_TOOL","target":"web_fetch","args":{{"url":"http://example.com"}},"why":"NEED_EVIDENCE"}}"#),
                learned_then_finish("Teal is a blue-green color", "E1"),
                "Teal is a blue-green color, per E1.".into(),
            ],
            goal: "what color is teal?".into(),
            min_findings: 1,
            budget: budget(6, 12),
            grades: vec![
                CogGrade::Complete(true),
                CogGrade::StoppedBecause(ReasonCode::ContractMet),
                CogGrade::EvidenceIds(vec!["E1".into()]),
                CogGrade::Failures(0),
                CogGrade::AnswerContains("blue-green".into()),
            ],
        },
        // 2. EMPTY IS NOT FAILURE — the classifier-parity regression lock. Three honest empty
        //    searches: the tool WORKED each time, so `failures` must stay 0 — while the stall stays
        //    visible as barren steps so the controller still has its signal. Under the old private
        //    boolean each "(no tool or saved skill matches)" counted as a break, and five of them
        //    ended a run with "the tools it needs keep failing".
        CognitionScenario {
            name: "honest empty results are barren steps, never tool failures".into(),
            replies: vec![
                call("discover_tools", "zzqx warp drive one"),
                call("discover_tools", "zzqx warp drive two"),
                call("discover_tools", "zzqx warp drive three"),
                "I searched three ways and found nothing usable.".into(),
            ],
            goal: "find a zzqx warp drive skill".into(),
            min_findings: 1,
            budget: budget(3, 12),
            grades: vec![
                CogGrade::Complete(false),
                CogGrade::StoppedBecause(ReasonCode::StepBudget),
                CogGrade::Failures(0),  // ← the whole point
                CogGrade::MinBarren(3), // …and the stall signal survived the fix
            ],
        },
        // 3. A REAL DEAD END IS RECORDED WITH ITS KIND: an unconfigured capability fails the step,
        //    and the failure note carries the classifier's reroute hint — so the next decision is
        //    told "different route", not just "it broke".
        CognitionScenario {
            name: "an unavailable capability is a recorded dead end with a reroute hint".into(),
            replies: vec![
                r#"{"verb":"CALL_TOOL","target":"github_repo_items","args":{"repo":"acme/x"},"why":"NEED_EVIDENCE"}"#.into(),
                r#"{"verb":"CALL_TOOL","target":"github_repo_items","args":{"repo":"acme/y"},"why":"NEED_EVIDENCE"}"#.into(),
                "GitHub isn't connected on this box, so I could not check.".into(),
            ],
            goal: "list the open items in acme/x".into(),
            min_findings: 1,
            budget: budget(2, 12),
            grades: vec![
                CogGrade::Complete(false),
                CogGrade::Failures(2),
                CogGrade::FailureNoteContains("not available on this box".into()),
                CogGrade::AnswerContains("GitHub".into()),
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn cognitive_loop_behavioral_suite_passes() {
        let card = run_cognition_suite(&cognition_suite()).await;
        assert_eq!(card.passed, card.total, "cognitive-loop eval regressions:\n{}", card.render());
    }
}
