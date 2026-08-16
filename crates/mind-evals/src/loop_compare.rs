//! loop_compare — the PROMOTION GATE, made runnable.
//!
//! cognitive.rs ships the bounded loop behind `YM_COGNITION` with this sentence: "'the new loop is
//! better' is a claim that should be settled by mind-evals scoring both against the same scenarios
//! rather than by whoever wrote it being confident." This module is that settlement: each pair puts
//! the SAME goal, against the SAME engine posture, through BOTH loops — the legacy ReAct loop via
//! `loop_eval` and the bounded loop via `cognition_eval` — and grades the same outcome, with the
//! model-call count alongside so the economics are part of the verdict, not a vibe.
//!
//! The reply scripts necessarily differ per side (the legacy loop speaks `{"tool":…}`, the bounded
//! loop speaks `{"verb":…}`) — what is HELD EQUAL is the goal, the engine, the tools available,
//! and the graded outcome. Terminal-delivery parity is deliberately absent here: both loops call
//! the literal same `terminal_delivery` function, and each side's unit tests pin its behavior — a
//! paired scenario would grade one function against itself.
//!
//! `cargo run -p mind-evals -- loops` prints the scoreboard.

use crate::cognition_eval::{self, CogGrade, CognitionScenario};
use crate::loop_eval::{self, Grade, LoopScenario};
use crate::ScenarioResult;
use mind_spec::control::ReasonCode;
use mind_spec::goal::Budget;

/// One goal, two loops, one graded outcome.
pub struct LoopPair {
    pub name: String,
    pub legacy: LoopScenario,
    pub cognitive: CognitionScenario,
}

pub struct PairOutcome {
    pub name: String,
    pub legacy: ScenarioResult,
    pub cognitive: ScenarioResult,
}

pub async fn run_pairs(pairs: &[LoopPair]) -> Vec<PairOutcome> {
    let mut out = Vec::new();
    for p in pairs {
        out.push(PairOutcome {
            name: p.name.clone(),
            legacy: loop_eval::run_loop_scenario(&p.legacy).await,
            cognitive: cognition_eval::run_cognition_scenario(&p.cognitive).await,
        });
    }
    out
}

/// The side-by-side scoreboard, one row per pair.
pub fn render(rows: &[PairOutcome]) -> String {
    let mut s = String::from(format!(
        "{:<44} {:>16} {:>16}\n{}\n",
        "scenario",
        "legacy",
        "cognitive",
        "-".repeat(78)
    ));
    let (mut lp, mut lt, mut lc, mut cp, mut ct, mut cc) = (0, 0, 0, 0, 0, 0);
    for r in rows {
        s.push_str(&format!(
            "{:<44} {:>10}/{:<2} {}c {:>9}/{:<2} {}c\n",
            r.name.chars().take(43).collect::<String>(),
            r.legacy.passed,
            r.legacy.total,
            r.legacy.calls,
            r.cognitive.passed,
            r.cognitive.total,
            r.cognitive.calls,
        ));
        lp += r.legacy.passed;
        lt += r.legacy.total;
        lc += r.legacy.calls;
        cp += r.cognitive.passed;
        ct += r.cognitive.total;
        cc += r.cognitive.calls;
    }
    s.push_str(&format!(
        "{}\n{:<44} {:>10}/{:<2} {}c {:>9}/{:<2} {}c\n",
        "-".repeat(78),
        "TOTAL",
        lp,
        lt,
        lc,
        cp,
        ct,
        cc
    ));
    s
}

fn tool(t: &str, args: serde_json::Value) -> String {
    serde_json::json!({ "thought": "step", "tool": t, "args": args }).to_string()
}
fn answer(text: &str) -> String {
    serde_json::json!({ "thought": "done", "answer": text }).to_string()
}
fn verb_call(t: &str, args: serde_json::Value) -> String {
    serde_json::json!({ "verb": "CALL_TOOL", "target": t, "args": args, "why": "NEED_EVIDENCE" }).to_string()
}
fn learned_finish(claim: &str, ev: &str) -> String {
    format!(
        r#"{{"learned":{{"findings":[{{"claim":"{claim}","evidence":["{ev}"]}}]}},"verb":"FINISH","why":"SUFFICIENT"}}"#
    )
}
fn budget(max_steps: u32) -> Budget {
    Budget { max_steps, max_model_calls: 12, max_wall_ms: 60_000, max_usd: None }
}

/// The paired suite. Each pair states its equalized goal and its graded outcome.
pub fn pairs() -> Vec<LoopPair> {
    vec![
        // ── Pair 1: fetch → grounded answer. The basic act-observe-answer skeleton. ─────────────
        LoopPair {
            name: "fetch a page, answer from it".into(),
            legacy: LoopScenario {
                name: "legacy".into(),
                seeds: vec![],
                replies: vec![
                    tool("web_fetch", serde_json::json!({ "url": "http://example.com" })),
                    answer("Teal is a blue-green color."),
                ],
                native: vec![],
                turn: "what color is teal?".into(),
                grades: vec![Grade::AnswerContains("blue-green".into())],
            },
            cognitive: CognitionScenario {
                name: "cognitive".into(),
                replies: vec![
                    verb_call("web_fetch", serde_json::json!({ "url": "http://example.com" })),
                    learned_finish("Teal is a blue-green color", "E1"),
                    "Teal is a blue-green color, per E1.".into(),
                ],
                goal: "what color is teal?".into(),
                min_findings: 1,
                budget: budget(6),
                grades: vec![
                    CogGrade::AnswerContains("blue-green".into()),
                    CogGrade::Complete(true),
                ],
            },
        },
        // ── Pair 2: honest empty results. Neither loop may spiral or invent; the difference in ──
        //    how each names the stop is part of the record (legacy: barren guard → compose;
        //    cognitive: budget → partial with failures=0).
        LoopPair {
            name: "three empty searches stay honest".into(),
            legacy: LoopScenario {
                name: "legacy".into(),
                seeds: vec![],
                replies: vec![
                    tool("discover_tools", serde_json::json!({ "query": "zzqx warp one" })),
                    tool("discover_tools", serde_json::json!({ "query": "zzqx warp two" })),
                    tool("discover_tools", serde_json::json!({ "query": "zzqx warp three" })),
                    "I searched and found nothing usable.".into(), // compose (plain text)
                ],
                native: vec![],
                turn: "find a zzqx warp drive skill".into(),
                grades: vec![
                    Grade::AnswerContains("nothing".into()),
                    Grade::MaxCalls(4),
                ],
            },
            cognitive: CognitionScenario {
                name: "cognitive".into(),
                replies: vec![
                    verb_call("discover_tools", serde_json::json!({ "query": "zzqx warp one" })),
                    verb_call("discover_tools", serde_json::json!({ "query": "zzqx warp two" })),
                    verb_call("discover_tools", serde_json::json!({ "query": "zzqx warp three" })),
                    "I searched three ways and found nothing usable.".into(),
                ],
                goal: "find a zzqx warp drive skill".into(),
                min_findings: 1,
                budget: budget(3),
                grades: vec![
                    CogGrade::AnswerContains("nothing".into()),
                    CogGrade::Failures(0),
                    CogGrade::MinBarren(3),
                    CogGrade::StoppedBecause(ReasonCode::StepBudget),
                ],
            },
        },
        // ── Pair 3: an unavailable capability. Both must end honestly, telling the user rather ──
        //    than retrying forever or inventing PR data.
        LoopPair {
            name: "unavailable github ends honestly".into(),
            legacy: LoopScenario {
                name: "legacy".into(),
                seeds: vec![],
                replies: vec![
                    tool("github_repo_items", serde_json::json!({ "repo": "acme/x" })),
                    answer("GitHub isn't connected here, so I can't check your PRs."),
                ],
                native: vec![],
                turn: "what are my open PRs?".into(),
                grades: vec![
                    Grade::AnswerContains("GitHub".into()),
                    Grade::PromptAtContains(1, "do not retry".into()),
                ],
            },
            cognitive: CognitionScenario {
                name: "cognitive".into(),
                replies: vec![
                    r#"{"verb":"CALL_TOOL","target":"github_repo_items","args":{"repo":"acme/x"},"why":"NEED_EVIDENCE"}"#.into(),
                    r#"{"verb":"CALL_TOOL","target":"github_repo_items","args":{"repo":"acme/y"},"why":"NEED_EVIDENCE"}"#.into(),
                    "GitHub isn't connected on this box, so I could not check.".into(),
                ],
                goal: "what are my open PRs?".into(),
                min_findings: 1,
                budget: budget(2),
                grades: vec![
                    CogGrade::AnswerContains("GitHub".into()),
                    CogGrade::FailureNoteContains("not available on this box".into()),
                    CogGrade::Complete(false),
                ],
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate itself: BOTH loops must fully pass every paired scenario. A side that regresses
    /// shows up here as its own failure, with the other side's score as the reference point.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn both_loops_pass_the_paired_suite() {
        let rows = run_pairs(&pairs()).await;
        let table = render(&rows);
        for r in &rows {
            assert_eq!(r.legacy.passed, r.legacy.total, "legacy failed '{}':\n{table}", r.name);
            assert_eq!(r.cognitive.passed, r.cognitive.total, "cognitive failed '{}':\n{table}", r.name);
        }
    }
}
