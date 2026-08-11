//! control — the decisions a computer should be making.
//!
//! The loop asks this BEFORE it considers asking a model anything. Everything here is a counter, a
//! comparison, or a set membership test, and every one of them would be slower, dearer, and less
//! reliable as a prompt. A model that is asked "are you making progress?" will answer about its own
//! prose; [`Controller::decide`] answers about the state.
//!
//! # Why the return type is a Decision and not a bool
//!
//! The controller does not merely veto. It says what should happen instead — replan, escalate to a
//! stronger model, ask the user, verify, finish with what we have — because "no" leaves the loop with
//! nowhere to go, and a loop with nowhere to go improvises. Every variant also carries a
//! [`ReasonCode`], which is what makes a run legible afterwards without storing paragraphs.

use serde::{Deserialize, Serialize};

use crate::capsule::Capsule;
use crate::goal::{Budget, Contract, Verdict};
use crate::Millis;

/// What the runtime decided to do next, without consulting a model.
///
/// `Proceed` is the only variant that hands control back to the model. Everything else is the
/// runtime taking the decision itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Nothing is wrong. Ask the model for the next action.
    Proceed,
    /// The contract is satisfied. Verify, then synthesize.
    Verify { reason: ReasonCode },
    /// Something is structurally stuck. Rebuild the plan before acting again.
    Replan { reason: ReasonCode },
    /// Route the next model call to a stronger tier. Not a failure — a considered escalation.
    Escalate { reason: ReasonCode },
    /// Only the user can unblock this. Stop and ask.
    AskUser { reason: ReasonCode, question: String },
    /// Out of budget, or out of ideas. Answer with what exists and disclose the shortfall.
    FinishPartial { reason: ReasonCode, shortfalls: Vec<String> },
}

impl Decision {
    /// Does this hand control to the model for a next-action choice?
    pub fn is_proceed(&self) -> bool {
        matches!(self, Self::Proceed)
    }
    /// Does this end the loop?
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Verify { .. } | Self::AskUser { .. } | Self::FinishPartial { .. })
    }
    pub fn reason(&self) -> Option<ReasonCode> {
        match self {
            Self::Proceed => None,
            Self::Verify { reason }
            | Self::Replan { reason }
            | Self::Escalate { reason }
            | Self::AskUser { reason, .. }
            | Self::FinishPartial { reason, .. } => Some(*reason),
        }
    }
}

/// A short machine tag for why something happened.
///
/// Deliberately an enum of codes rather than free text. A run's history should be queryable — "how
/// often do runs stall?" is a `GROUP BY` over these, and would be a text-mining problem over
/// sentences. The prose belongs in the final answer, where a human reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasonCode {
    /// Every completion criterion is met.
    ContractMet,
    /// The step budget is spent.
    StepBudget,
    /// The model-call budget is spent — the one that actually costs money.
    ModelBudget,
    /// The wall-clock ceiling is reached.
    Timeout,
    /// The same action has been tried too many times.
    RepeatedAction,
    /// Several steps in a row produced nothing new.
    NoProgress,
    /// Many attempts, few distinct actions: going in circles.
    Circling,
    /// Sources disagree and it has not been resolved.
    Contradiction,
    /// Confidence went DOWN, which usually means the picture got worse, not better.
    ConfidenceDropped,
    /// Repeated tool failures — the environment is not cooperating.
    ToolFailures,
    /// A question only the user can answer is blocking progress.
    NeedsUserInput,
    /// A high-consequence action is about to be taken.
    HighConsequence,
}

impl ReasonCode {
    /// One line for an operator. The UI shows this; the enum is what gets aggregated.
    pub fn describe(self) -> &'static str {
        match self {
            Self::ContractMet => "everything the goal asked for is in hand",
            Self::StepBudget => "reached the step limit for this goal",
            Self::ModelBudget => "reached the reasoning budget for this goal",
            Self::Timeout => "took too long",
            Self::RepeatedAction => "the same action kept being retried",
            Self::NoProgress => "several steps in a row found nothing new",
            Self::Circling => "cycling between the same few actions",
            Self::Contradiction => "sources disagree and it is unresolved",
            Self::ConfidenceDropped => "the picture got less clear, not more",
            Self::ToolFailures => "the tools it needs keep failing",
            Self::NeedsUserInput => "it needs something only you can answer",
            Self::HighConsequence => "the next step has real consequences",
        }
    }
}

/// Thresholds. Named and adjustable rather than scattered as literals, because these are the
/// numbers worth tuning from real run history once the ledger exists.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Limits {
    /// Attempts of one identical action before forcing a replan. Two is the point at which a third
    /// would be superstition: the result is already in the state.
    pub max_same_action: usize,
    /// Consecutive steps with no new evidence and nothing resolved before forcing a replan.
    pub max_barren_steps: u32,
    /// Total tool failures before giving up on this approach.
    pub max_failures: u32,
    /// Steps after which a run with very few distinct actions counts as circling.
    pub circling_after_steps: u32,
    /// Distinct-action ratio below which a run counts as circling.
    pub circling_ratio: f64,
    /// Replans allowed before accepting that replanning is not the problem.
    pub max_replans: u32,
    /// Confidence drop that triggers escalation rather than another cheap step.
    pub confidence_drop: f64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_same_action: 2,
            max_barren_steps: 3,
            max_failures: 5,
            circling_after_steps: 6,
            circling_ratio: 0.4,
            max_replans: 3,
            confidence_drop: 0.15,
        }
    }
}

/// What the loop learned from the last step, handed to the controller.
///
/// Only two things, and both are facts rather than opinions: what confidence was before the step, and
/// whether the action about to be taken has an outward effect.
#[derive(Debug, Clone, Copy, Default)]
pub struct StepOutcome {
    pub confidence_before: f64,
    /// The next action would have an effect outside the mind. The harm gate still governs the action
    /// itself; this only tells the controller to slow down and check first.
    pub next_is_outward: bool,
}

/// The deterministic controller.
#[derive(Debug, Clone, Default)]
pub struct Controller {
    pub limits: Limits,
}

impl Controller {
    pub fn new(limits: Limits) -> Self {
        Self { limits }
    }

    /// Decide what happens next. Pure: same inputs, same decision, no clock read, no model.
    ///
    /// Order is the design. Budgets first, because an exhausted run must stop even if it is also
    /// stalled — reporting "stalled" to someone whose budget ran out would be misleading about why.
    /// Then the contract, so a run that is genuinely done is not sent off to replan. Then the stall
    /// and failure conditions. Escalation last, because it is the only non-structural signal and the
    /// cheapest to be wrong about.
    pub fn decide(
        &self,
        capsule: &Capsule,
        contract: &Contract,
        budget: &Budget,
        elapsed_ms: Millis,
        last: StepOutcome,
    ) -> Decision {
        let verdict = contract.completion.evaluate(capsule, &contract.requirements);

        // ── Budgets: hard stops. ────────────────────────────────────────────────────────────────
        // A run out of budget with a met contract should still be reported as met, so the contract
        // check is folded in here rather than losing the good outcome to a limit.
        if let Some(code) = self.budget_exceeded(capsule, budget, elapsed_ms) {
            return if verdict.met {
                Decision::Verify { reason: ReasonCode::ContractMet }
            } else {
                Decision::FinishPartial { reason: code, shortfalls: describe(&verdict) }
            };
        }

        // ── Done? ──────────────────────────────────────────────────────────────────────────────
        if verdict.met {
            return Decision::Verify { reason: ReasonCode::ContractMet };
        }

        // ── High consequence: check before acting, not after. ──────────────────────────────────
        if last.next_is_outward {
            return Decision::AskUser {
                reason: ReasonCode::HighConsequence,
                question: "This next step has an effect outside the mind. Go ahead?".to_string(),
            };
        }

        // ── Structurally stuck. ────────────────────────────────────────────────────────────────
        // Replanning is only worth suggesting while replans are still plausibly useful; past that,
        // more replans are the same superstition as more retries.
        let replans_left = capsule.progress.replans < self.limits.max_replans;

        if let Some(action) = self.repeated_action(capsule) {
            return if replans_left {
                Decision::Replan { reason: ReasonCode::RepeatedAction }
            } else {
                Decision::FinishPartial {
                    reason: ReasonCode::RepeatedAction,
                    shortfalls: with_note(&verdict, format!("kept retrying {action}")),
                }
            };
        }
        if capsule.progress.barren_steps >= self.limits.max_barren_steps {
            return if replans_left {
                Decision::Replan { reason: ReasonCode::NoProgress }
            } else {
                Decision::FinishPartial { reason: ReasonCode::NoProgress, shortfalls: describe(&verdict) }
            };
        }
        if self.is_circling(capsule) {
            return if replans_left {
                Decision::Replan { reason: ReasonCode::Circling }
            } else {
                Decision::FinishPartial { reason: ReasonCode::Circling, shortfalls: describe(&verdict) }
            };
        }
        if capsule.progress.failures >= self.limits.max_failures {
            return Decision::FinishPartial {
                reason: ReasonCode::ToolFailures,
                shortfalls: with_note(&verdict, "the tools it needed kept failing".to_string()),
            };
        }

        // ── Escalate rather than grind. ────────────────────────────────────────────────────────
        // An unresolved contradiction is exactly the case worth a stronger model: it is a judgment
        // call, which is the one thing cheap tiers are worst at.
        if !capsule.contradictions.is_empty() {
            return Decision::Escalate { reason: ReasonCode::Contradiction };
        }
        // Confidence falling means the last step made the picture worse. Another cheap step will
        // probably do the same.
        if last.confidence_before - capsule.confidence >= self.limits.confidence_drop {
            return Decision::Escalate { reason: ReasonCode::ConfidenceDropped };
        }

        Decision::Proceed
    }

    /// Which budget, if any, is spent.
    fn budget_exceeded(&self, capsule: &Capsule, budget: &Budget, elapsed_ms: Millis) -> Option<ReasonCode> {
        if capsule.progress.steps >= budget.max_steps {
            return Some(ReasonCode::StepBudget);
        }
        if capsule.progress.model_calls >= budget.max_model_calls {
            return Some(ReasonCode::ModelBudget);
        }
        if elapsed_ms >= budget.max_wall_ms {
            return Some(ReasonCode::Timeout);
        }
        None
    }

    /// An action tried too many times. Returns it, so the message can name it.
    fn repeated_action(&self, capsule: &Capsule) -> Option<String> {
        capsule
            .attempted
            .iter()
            .find(|a| capsule.attempts_of(a) > self.limits.max_same_action)
            .cloned()
    }

    /// Many steps, few distinct actions. Catches the alternating dead end that a
    /// compare-with-previous guard walks straight past.
    fn is_circling(&self, capsule: &Capsule) -> bool {
        let steps = capsule.progress.steps;
        if steps < self.limits.circling_after_steps {
            return false;
        }
        let attempts = capsule.attempted.len();
        if attempts == 0 {
            return false;
        }
        (capsule.distinct_attempts() as f64 / attempts as f64) < self.limits.circling_ratio
    }

    /// Should the critic run at this boundary?
    ///
    /// Not after every step — that is three model calls per action for a check that usually says
    /// "fine". Only where being wrong is expensive: at the completion boundary, on a contradiction,
    /// or when confidence moved the wrong way.
    pub fn should_criticize(&self, capsule: &Capsule, verdict: &Verdict, last: StepOutcome) -> bool {
        verdict.met
            || !capsule.contradictions.is_empty()
            || (last.confidence_before - capsule.confidence) >= self.limits.confidence_drop
    }

    /// Is this action worth taking at all, or does the state already answer it?
    ///
    /// Pure deduplication, and the cheapest possible saving: a repeat of an action whose result is
    /// already in the capsule costs a tool call and a model call to learn nothing.
    pub fn is_redundant(&self, capsule: &Capsule, action: &str) -> bool {
        capsule.attempts_of(action) >= self.limits.max_same_action
    }
}

fn describe(v: &Verdict) -> Vec<String> {
    v.shortfalls.iter().map(|s| s.describe()).collect()
}

fn with_note(v: &Verdict, note: String) -> Vec<String> {
    let mut out = describe(v);
    out.push(note);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule::{Finding, Observation, Uncertainty};
    use crate::goal::{CompletionCriteria, OutputContract};

    fn contract(min_findings: usize) -> Contract {
        Contract {
            requirements: Vec::new(),
            completion: CompletionCriteria { min_findings, require_full_coverage: false, ..Default::default() },
            output: OutputContract::default(),
        }
    }

    fn capsule() -> Capsule {
        Capsule::new("g", "find things")
    }

    fn met_capsule() -> Capsule {
        let mut c = capsule();
        c.findings = vec![Finding {
            claim: "a".into(),
            evidence: vec!["E1".into()],
            addresses: vec![],
            risk: None,
            rank: None,
        }];
        c.recompute_confidence();
        c
    }

    fn tiny_budget() -> Budget {
        Budget { max_steps: 5, max_model_calls: 4, max_wall_ms: 10_000, max_usd: None }
    }

    #[test]
    fn a_healthy_run_proceeds() {
        let d = Controller::default().decide(&capsule(), &contract(3), &tiny_budget(), 0, StepOutcome::default());
        assert_eq!(d, Decision::Proceed);
        assert!(d.is_proceed());
        assert!(d.reason().is_none(), "proceeding needs no explanation");
    }

    #[test]
    fn a_met_contract_goes_to_verify() {
        let d = Controller::default().decide(&met_capsule(), &contract(1), &tiny_budget(), 0, StepOutcome::default());
        assert_eq!(d, Decision::Verify { reason: ReasonCode::ContractMet });
        assert!(d.is_terminal());
    }

    /// The ordering that matters most: a run that is BOTH out of budget and finished must be
    /// reported as finished. Losing a good outcome to a limit would be the wrong story.
    #[test]
    fn a_met_contract_survives_an_exhausted_budget() {
        let mut c = met_capsule();
        c.progress.steps = 99;
        let d = Controller::default().decide(&c, &contract(1), &tiny_budget(), 999_999, StepOutcome::default());
        assert_eq!(d, Decision::Verify { reason: ReasonCode::ContractMet }, "done is done, even at the limit");
    }

    /// An exhausted budget with an unmet contract finishes PARTIAL and says what is missing — never
    /// silently, and never pretending it is complete.
    #[test]
    fn an_exhausted_budget_finishes_partial_and_discloses() {
        let mut c = capsule();
        c.progress.steps = 5;
        let d = Controller::default().decide(&c, &contract(3), &tiny_budget(), 0, StepOutcome::default());
        match d {
            Decision::FinishPartial { reason, shortfalls } => {
                assert_eq!(reason, ReasonCode::StepBudget);
                assert!(shortfalls.iter().any(|s| s.contains("of 3 findings")), "{shortfalls:?}");
            }
            other => panic!("expected FinishPartial, got {other:?}"),
        }
    }

    /// Each budget is distinguishable, because "out of steps" and "out of money" are different facts
    /// for whoever tunes this later.
    #[test]
    fn each_budget_reports_its_own_reason() {
        let ctl = Controller::default();
        let mut c = capsule();
        c.progress.model_calls = 4;
        assert_eq!(
            ctl.decide(&c, &contract(3), &tiny_budget(), 0, StepOutcome::default()).reason(),
            Some(ReasonCode::ModelBudget)
        );
        let d = ctl.decide(&capsule(), &contract(3), &tiny_budget(), 10_000, StepOutcome::default());
        assert_eq!(d.reason(), Some(ReasonCode::Timeout));
    }

    /// The loop guard the old runtime lacked: alternation between two dead ends.
    #[test]
    fn alternating_between_two_dead_ends_forces_a_replan() {
        let mut c = capsule();
        for a in ["A", "B", "A", "B", "A", "B"] {
            c = c.reduce(Observation { action: a.into(), ok: true, ..Default::default() });
        }
        // Six steps, two distinct actions — and A has been tried three times.
        let d = Controller::default().decide(&c, &contract(3), &Budget::background(), 0, StepOutcome::default());
        assert!(
            matches!(d, Decision::Replan { reason: ReasonCode::RepeatedAction | ReasonCode::Circling }),
            "got {d:?}"
        );
    }

    #[test]
    fn stalling_forces_a_replan_then_eventually_stops() {
        let ctl = Controller::default();
        let mut c = capsule();
        for i in 0..3 {
            c = c.reduce(Observation { action: format!("distinct{i}"), ok: true, ..Default::default() });
        }
        assert_eq!(c.progress.barren_steps, 3);
        assert_eq!(
            ctl.decide(&c, &contract(3), &Budget::background(), 0, StepOutcome::default()),
            Decision::Replan { reason: ReasonCode::NoProgress }
        );

        // Once replanning has been tried enough, more of it is superstition — stop and disclose.
        c.progress.replans = 3;
        match ctl.decide(&c, &contract(3), &Budget::background(), 0, StepOutcome::default()) {
            Decision::FinishPartial { reason: ReasonCode::NoProgress, .. } => {}
            other => panic!("expected a partial finish once replans are spent, got {other:?}"),
        }
    }

    /// A contradiction is a judgment call, which is what a stronger model is FOR. It must not be
    /// ground at by more cheap steps.
    #[test]
    fn a_contradiction_escalates_rather_than_grinding() {
        let mut c = capsule();
        c.contradictions.push("two sources disagree on the volume figure".into());
        let d = Controller::default().decide(&c, &contract(3), &Budget::background(), 0, StepOutcome::default());
        assert_eq!(d, Decision::Escalate { reason: ReasonCode::Contradiction });
        assert!(!d.is_terminal(), "escalation continues the run at a higher tier");
    }

    #[test]
    fn a_confidence_drop_escalates() {
        let mut c = met_capsule();
        c.confidence = 0.4;
        let last = StepOutcome { confidence_before: 0.8, next_is_outward: false };
        let d = Controller::default().decide(&c, &contract(3), &Budget::background(), 0, last);
        assert_eq!(d, Decision::Escalate { reason: ReasonCode::ConfidenceDropped });
    }

    /// An outward action stops for a human first. The harm gate governs the action itself; this is
    /// the controller refusing to walk into it without asking.
    #[test]
    fn an_outward_next_action_asks_the_user_first() {
        let last = StepOutcome { confidence_before: 0.0, next_is_outward: true };
        match Controller::default().decide(&capsule(), &contract(3), &Budget::background(), 0, last) {
            Decision::AskUser { reason: ReasonCode::HighConsequence, question } => {
                assert!(question.contains("outside the mind"));
            }
            other => panic!("expected AskUser, got {other:?}"),
        }
    }

    /// The critic is expensive, so it runs at boundaries — not after every step.
    #[test]
    fn the_critic_runs_at_boundaries_not_every_step() {
        let ctl = Controller::default();
        let healthy = capsule();
        let unmet = contract(3).completion.evaluate(&healthy, &[]);
        assert!(!ctl.should_criticize(&healthy, &unmet, StepOutcome::default()), "no critic mid-run");

        // At the completion boundary it always runs.
        let done = met_capsule();
        let met = contract(1).completion.evaluate(&done, &[]);
        assert!(ctl.should_criticize(&done, &met, StepOutcome::default()));

        // And on a contradiction.
        let mut clashing = capsule();
        clashing.contradictions.push("x".into());
        let v = contract(3).completion.evaluate(&clashing, &[]);
        assert!(ctl.should_criticize(&clashing, &v, StepOutcome::default()));

        // And when the picture got worse.
        assert!(ctl.should_criticize(&healthy, &unmet, StepOutcome { confidence_before: 0.9, next_is_outward: false }));
    }

    /// The cheapest saving there is: an action whose answer is already in the state costs a tool call
    /// and a model call to learn nothing.
    #[test]
    fn a_redundant_action_is_caught_without_a_model() {
        let ctl = Controller::default();
        let mut c = capsule();
        assert!(!ctl.is_redundant(&c, "search:foo"));
        c = c.reduce(Observation { action: "search:foo".into(), ok: true, ..Default::default() });
        c = c.reduce(Observation { action: "search:foo".into(), ok: true, ..Default::default() });
        assert!(ctl.is_redundant(&c, "search:foo"), "twice is enough; the result is already in state");
    }

    #[test]
    fn repeated_tool_failures_stop_the_run() {
        let mut c = capsule();
        c.progress.failures = 5;
        match Controller::default().decide(&c, &contract(3), &Budget::background(), 0, StepOutcome::default()) {
            Decision::FinishPartial { reason: ReasonCode::ToolFailures, shortfalls } => {
                assert!(shortfalls.iter().any(|s| s.contains("kept failing")));
            }
            other => panic!("expected a partial finish, got {other:?}"),
        }
    }

    /// Every reason code must have an operator-readable line — these are shown in the UI verbatim.
    #[test]
    fn every_reason_code_reads_as_english() {
        for code in [
            ReasonCode::ContractMet, ReasonCode::StepBudget, ReasonCode::ModelBudget, ReasonCode::Timeout,
            ReasonCode::RepeatedAction, ReasonCode::NoProgress, ReasonCode::Circling, ReasonCode::Contradiction,
            ReasonCode::ConfidenceDropped, ReasonCode::ToolFailures, ReasonCode::NeedsUserInput,
            ReasonCode::HighConsequence,
        ] {
            let d = code.describe();
            assert!(d.len() > 12, "{code:?} needs a real description");
            assert!(d.chars().next().unwrap().is_lowercase(), "{code:?} reads mid-sentence in the UI");
        }
    }

    /// The uncertainty list is what makes the next step obvious, so it must survive a reduce cycle
    /// with its ordering intact.
    #[test]
    fn the_controller_and_capsule_agree_on_what_matters_next() {
        let c = capsule().reduce(Observation {
            action: "screen".into(),
            ok: true,
            uncertainties: vec![
                Uncertainty { question: "catalyst?".into(), importance: 0.9, confidence: 0.2, resolved: false },
                Uncertainty { question: "liquidity?".into(), importance: 0.5, confidence: 0.9, resolved: false },
            ],
            ..Default::default()
        });
        assert_eq!(c.next_uncertainty().unwrap().question, "catalyst?");
        // And it blocks completion, so the controller will not let the run finish around it.
        let v = contract(0).completion.evaluate(&c, &[]);
        assert!(!v.met);
        assert!(v.summarize().contains("catalyst?"), "{}", v.summarize());
    }
}
