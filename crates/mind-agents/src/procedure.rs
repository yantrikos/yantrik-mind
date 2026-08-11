//! procedure — remembered ways of doing a KIND of task.
//!
//! # The distinction that makes this worth building
//!
//! A tool is one action. A procedure is an approach: the sequence you learned works for a class of
//! problem. "Search the web" is a tool; "how to evaluate a GitHub project — read the README, then the
//! commit history, then the dependency graph, then distinguish the implementation from the claims" is
//! a procedure. The second is what a competent worker actually carries around.
//!
//! Knowledge memory saves facts. Procedural memory saves REASONING — and reasoning is the expensive
//! part. A loop that re-derives its approach every time pays for planning on every run; a loop that
//! recalls the approach pays once, ever.
//!
//! # Why this is a runtime step and not a tool the model may choose
//!
//! Because it should always happen, and because a model asked "would you like to look for a known
//! approach?" will sometimes say no and then improvise one. Surfacing procedures is cheap semantic
//! recall, so the loop does it before the first decision, the way it retrieves memory — and the
//! recalled steps land in `capsule.plan`, which means the planning model call does not happen at all
//! when a procedure is known. That is the saving, and it is structural rather than hoped for.
//!
//! # Two kinds, deliberately
//!
//! [`ProcedureKind::Instructions`] is prose that shapes an approach — the shape a skill takes when it
//! is knowledge rather than code. [`ProcedureKind::Executable`] is a banked script with a name, run
//! sandboxed by the existing skill machinery. They are not interchangeable: the first changes how the
//! loop reasons, the second is an action the loop takes.
//!
//! # Reliability is measured or it is labelled
//!
//! A banked skill carries real `runs`/`successes`, so its reliability is [`Prior::measured`]. A
//! procedure recalled from memory has no outcome history, so it is [`Prior::declared`] and says so.
//! Following a procedure that has been failing is worse than having none, so [`select`] refuses the
//! ones the record condemns rather than averaging them in.

use mind_spec::{Basis, Prior};
use serde::{Deserialize, Serialize};

/// A remembered approach.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Procedure {
    /// Short stable name — what an outcome is recorded against.
    pub name: String,
    /// The task shape this applies to. Matched semantically by the store, shown to the model so it can
    /// tell whether the recall was apt.
    pub when: String,
    /// The approach, in order. For an executable skill this is a one-line description of what it does.
    pub steps: Vec<String>,
    pub kind: ProcedureKind,
    pub reliability: Prior,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProcedureKind {
    /// Prose guidance. Shapes the plan; is not itself an action.
    Instructions,
    /// A banked script, run through the sandbox by name.
    Executable { skill: String },
}

impl Procedure {
    /// Has this been observed failing often enough that following it is worse than not?
    ///
    /// Mirrors the skill store's own quarantine rule — below half, over enough runs to mean it. A
    /// DECLARED reliability can never trip this: an unmeasured procedure is unproven, not bad, and
    /// treating the two the same would keep the library from ever growing.
    pub fn is_discredited(&self) -> bool {
        matches!(self.reliability.basis, Basis::Measured { runs } if runs >= 4 && self.reliability.value < 0.5)
    }

    /// Ordering key: proven beats plausible, and a longer record breaks ties.
    fn standing(&self) -> (u8, f64, u32) {
        let (tier, runs) = match self.reliability.basis {
            Basis::Measured { runs } if runs >= 4 => (2, runs),
            Basis::Measured { runs } => (1, runs),
            _ => (0, 0),
        };
        (tier, self.reliability.value, runs)
    }

    /// One line for the prompt: what it is for, its steps, and how much to trust it.
    ///
    /// The trust phrasing is not decoration. A model told "this worked 9 of 10 times" follows it; a
    /// model told "untested" is right to deviate when the situation does not fit, and should be able
    /// to tell the difference.
    pub fn render(&self) -> String {
        let trust = match self.reliability.basis {
            Basis::Measured { runs } => {
                format!("worked {:.0}% of {runs} time(s)", self.reliability.value * 100.0)
            }
            Basis::Declared => "not yet tested".to_string(),
            Basis::Estimated => "an estimate, unverified".to_string(),
        };
        let head = match &self.kind {
            ProcedureKind::Executable { skill } => format!("{} (run_skill \"{skill}\" — {trust})", self.name),
            ProcedureKind::Instructions => format!("{} ({trust})", self.name),
        };
        let steps = self
            .steps
            .iter()
            .enumerate()
            .map(|(i, s)| format!("  {}. {s}", i + 1))
            .collect::<Vec<_>>()
            .join("\n");
        if self.when.trim().is_empty() {
            format!("{head}\n{steps}")
        } else {
            format!("{head}\n  when: {}\n{steps}", self.when)
        }
    }
}

/// Choose which recalled procedures to actually follow.
///
/// Discredited ones are dropped outright. Of the rest, at most `keep` survive, best-standing first —
/// because handing a model four competing approaches is worse than handing it one: it will blend them,
/// and a blended procedure is a procedure nobody validated.
pub fn select(mut found: Vec<Procedure>, keep: usize) -> Vec<Procedure> {
    found.retain(|p| !p.is_discredited() && !p.steps.is_empty());
    found.sort_by(|a, b| b.standing().partial_cmp(&a.standing()).unwrap_or(std::cmp::Ordering::Equal));
    found.truncate(keep.max(1));
    found
}

/// The plan a procedure implies — the steps, ready for `capsule.plan`.
///
/// This is where the planning model call is avoided: the plan comes from memory, not from asking.
pub fn as_plan(procedures: &[Procedure], horizon: u8) -> Vec<String> {
    procedures
        .iter()
        .filter(|p| matches!(p.kind, ProcedureKind::Instructions))
        .flat_map(|p| p.steps.iter().cloned())
        .take(horizon.clamp(1, 8) as usize)
        .collect()
}

/// The block that goes into the decision prompt.
pub fn render_block(procedures: &[Procedure]) -> String {
    if procedures.is_empty() {
        return String::new();
    }
    format!(
        "\nKNOWN APPROACH (you have done this kind of thing before \u{2014} follow it unless the situation \
         genuinely differs, and say so if you deviate)\n{}\n",
        procedures.iter().map(|p| p.render()).collect::<Vec<_>>().join("\n")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn instructions(name: &str, rate: Option<(f64, u32)>) -> Procedure {
        Procedure {
            name: name.into(),
            when: "evaluating a repository".into(),
            steps: vec!["read the README".into(), "read the commit history".into()],
            kind: ProcedureKind::Instructions,
            reliability: match rate {
                Some((v, runs)) => Prior::measured(v, runs),
                None => Prior::declared(0.6),
            },
        }
    }

    /// Following a procedure the record condemns is worse than having none — the same rule the skill
    /// store already applies when it quarantines below half.
    #[test]
    fn a_discredited_procedure_is_refused() {
        let bad = instructions("keeps failing", Some((0.2, 10)));
        assert!(bad.is_discredited());
        assert!(select(vec![bad], 2).is_empty(), "a failing approach must not be offered");
    }

    /// An UNMEASURED procedure is unproven, not bad. Treating the two the same would stop the library
    /// ever growing, because a new procedure starts with no record by definition.
    #[test]
    fn an_untested_procedure_is_offered_but_labelled() {
        let new = instructions("brand new", None);
        assert!(!new.is_discredited(), "no record is not a bad record");
        let kept = select(vec![new], 2);
        assert_eq!(kept.len(), 1);
        assert!(kept[0].render().contains("not yet tested"), "{}", kept[0].render());
    }

    /// A thin good record must not outrank a thick one. Two-for-two is luck; nine-of-ten is evidence.
    #[test]
    fn a_proven_procedure_outranks_a_lucky_one() {
        let lucky = instructions("two for two", Some((1.0, 2)));
        let proven = instructions("nine of ten", Some((0.9, 10)));
        let kept = select(vec![lucky, proven], 1);
        assert_eq!(kept[0].name, "nine of ten", "a longer record wins over a higher rate on two runs");
    }

    /// One approach, not four. Handing a model competing procedures invites it to blend them, and a
    /// blended procedure is one nobody validated.
    #[test]
    fn only_a_few_survive_selection() {
        let many: Vec<Procedure> = (0..6).map(|i| instructions(&format!("p{i}"), Some((0.8, 5)))).collect();
        assert_eq!(select(many, 2).len(), 2);
    }

    /// A procedure with no steps is not a procedure.
    #[test]
    fn an_empty_procedure_is_dropped() {
        let mut empty = instructions("hollow", Some((1.0, 9)));
        empty.steps.clear();
        assert!(select(vec![empty], 2).is_empty());
    }

    /// THE TOKEN WIN: the plan comes from memory, so the planning model call does not happen.
    #[test]
    fn instructions_become_the_plan_without_a_model_call() {
        let p = instructions("repo review", Some((0.9, 8)));
        let plan = as_plan(&[p], 3);
        assert_eq!(plan, vec!["read the README", "read the commit history"]);
    }

    /// An executable skill is an ACTION, not guidance — it must not silently become the plan.
    #[test]
    fn an_executable_skill_is_not_treated_as_a_plan() {
        let exe = Procedure {
            name: "sum_to_ten".into(),
            when: "summing".into(),
            steps: vec!["adds the first ten integers".into()],
            kind: ProcedureKind::Executable { skill: "sum_to_ten".into() },
            reliability: Prior::measured(1.0, 6),
        };
        assert!(as_plan(&[exe.clone()], 3).is_empty(), "a script is something to RUN, not a plan to follow");
        // But it is offered, with the exact call the model should make.
        let block = render_block(&[exe]);
        assert!(block.contains("run_skill \"sum_to_ten\""), "{block}");
        assert!(block.contains("worked 100% of 6 time(s)"), "{block}");
    }

    /// The prompt block has to say what the procedure is FOR, or a mis-recall is invisible.
    #[test]
    fn the_block_states_applicability_and_permits_deviation() {
        let block = render_block(&[instructions("repo review", Some((0.9, 8)))]);
        assert!(block.contains("when: evaluating a repository"), "{block}");
        assert!(block.contains("unless the situation genuinely differs"), "deviation must be allowed:\n{block}");
        assert!(block.contains("worked 90% of 8 time(s)"));
    }

    #[test]
    fn no_procedures_means_no_block_at_all() {
        assert!(render_block(&[]).is_empty(), "an empty section is still tokens");
    }
}
