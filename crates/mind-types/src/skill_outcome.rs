//! skill_outcome — what happened when a skill ran, with the two meanings kept apart.
//!
//! `record_skill_outcome(name, ok)` used to take ONE boolean, and every caller had to decide what
//! it meant. They decided differently, and all of them were wrong in the same direction:
//!
//!   * the instruction runner passed `!answer.trim().is_empty()`, so an API outage that arrived as
//!     the text `(sub-agent synthesis error: …)` was banked as a deliverable under a green tick;
//!   * a document that ran perfectly and correctly answered *"I cannot perform this task"* was
//!     banked as a success, because the executor had finished;
//!   * a sandboxed code skill passed `exit_code == 0`, which is a real proxy — so one column held
//!     "the process exited cleanly" and "the model produced something useful" at the same time,
//!     and selection read that column.
//!
//! Codex's correction, adopted here: use a judge, but do NOT wait for the judge to fix the
//! semantics. Split the meaning first, so the judge later fills a field that already exists rather
//! than forcing a schema change. Three questions, asked separately:
//!
//!   `executor_ok`  — did the runner complete without an infrastructure or runtime failure?
//!   `task_success` — was the thing the user asked for actually accomplished? `None` is allowed,
//!                    and is the honest answer for most documents today.
//!   `basis`        — on what evidence. A rate computed from `exit_code` and a rate computed from
//!                    a model's opinion are not the same measurement and must not pool.

/// On what evidence `task_success` was decided.
///
/// A bounded enum on purpose: free-form prose is not policy. Whatever judges deliverables later
/// must land in `StructuredJudge` with its own evidence, not widen this into a description.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskBasis {
    /// A process exited cleanly. A real proxy, and only available to code.
    ExitCode,
    /// A judge read the deliverable and returned a structured verdict.
    StructuredJudge,
    /// The deliverable itself said the task was not done, by a deterministic signal — never by
    /// guessing at prose.
    ExplicitRefusal,
    /// A human said so.
    Operator,
    /// Nobody has judged it. The honest default, and NOT a synonym for failure.
    Unknown,
}

impl TaskBasis {
    pub fn label(self) -> &'static str {
        match self {
            TaskBasis::ExitCode => "exit_code",
            TaskBasis::StructuredJudge => "structured_judge",
            TaskBasis::ExplicitRefusal => "explicit_refusal",
            TaskBasis::Operator => "operator",
            TaskBasis::Unknown => "unknown",
        }
    }
}

/// One run of a skill, as the ledger should hear about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SkillOutcome {
    /// Did the RUNNER finish — no API failure, no crash, no missing sandbox?
    pub executor_ok: bool,
    /// Did the TASK get done? `None` when nothing competent has judged it.
    pub task_success: Option<bool>,
    pub basis: TaskBasis,
}

impl SkillOutcome {
    /// A sandboxed code run. `exit_code == 0` is the one cheap, deterministic proxy available.
    pub fn from_exit(exit_ok: bool) -> Self {
        Self { executor_ok: true, task_success: Some(exit_ok), basis: TaskBasis::ExitCode }
    }

    /// The runner failed — an API outage, a crash, a missing executor.
    ///
    /// `task_success` is forced to `None`: an infrastructure failure says NOTHING about whether the
    /// skill is any good, and letting it count as a task failure would discredit a fine skill for
    /// the provider having a bad afternoon.
    pub fn executor_failed() -> Self {
        Self { executor_ok: false, task_success: None, basis: TaskBasis::Unknown }
    }

    /// The runner finished and nobody has judged the result. The honest state for a document.
    pub fn ungraded() -> Self {
        Self { executor_ok: true, task_success: None, basis: TaskBasis::Unknown }
    }

    /// A judge read the deliverable.
    pub fn judged(ok: bool) -> Self {
        Self { executor_ok: true, task_success: Some(ok), basis: TaskBasis::StructuredJudge }
    }

    /// A deterministic signal in the deliverable said the task was not done.
    pub fn refused() -> Self {
        Self { executor_ok: true, task_success: Some(false), basis: TaskBasis::ExplicitRefusal }
    }

    /// Does this run belong in the denominator of the skill's success rate?
    ///
    /// Only a JUDGED run does. An ungraded one is not a failure and must not be one by omission —
    /// it simply is not evidence, and a rate over runs nobody assessed is the conflation this
    /// module exists to end.
    pub fn is_graded(&self) -> bool {
        self.task_success.is_some()
    }

    /// Does this run count toward the skill's successes?
    pub fn is_task_success(&self) -> bool {
        self.task_success == Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_infrastructure_failure_teaches_nothing_about_the_skill() {
        // Codex's acceptance test 3, and it was live: an "OpenAI-compatible API request failed"
        // reached the job board as a finished deliverable under a green tick.
        let o = SkillOutcome::executor_failed();
        assert!(!o.executor_ok);
        assert_eq!(o.task_success, None, "an outage is not a verdict on the skill");
        assert!(!o.is_graded(), "and it must not enter the denominator either");
        assert!(!o.is_task_success());
    }

    #[test]
    fn an_unjudged_run_is_neither_a_success_nor_a_failure() {
        // Codex's acceptance test 1: a document that emits "I cannot perform this task" records
        // executor_ok = true and MUST NOT improve selection weight. Ungraded achieves that without
        // pretending to know it failed.
        let o = SkillOutcome::ungraded();
        assert!(o.executor_ok, "the runner did finish");
        assert!(!o.is_graded(), "but nothing judged the deliverable");
        assert!(!o.is_task_success(), "so it cannot strengthen the skill");
    }

    #[test]
    fn only_code_may_reach_task_success_through_an_exit_code() {
        // Codex's acceptance test 2: exit 0 maps to task_success ONLY via the code-skill adapter,
        // never the generic path. The basis is what keeps the two measurements from pooling.
        let code = SkillOutcome::from_exit(true);
        assert_eq!(code.basis, TaskBasis::ExitCode);
        assert!(code.is_task_success() && code.is_graded());

        assert_eq!(SkillOutcome::from_exit(false).task_success, Some(false));
        // The generic paths cannot produce an ExitCode basis at all.
        for o in [SkillOutcome::ungraded(), SkillOutcome::executor_failed(), SkillOutcome::judged(true), SkillOutcome::refused()] {
            assert_ne!(o.basis, TaskBasis::ExitCode, "only the code adapter may claim exit_code");
        }
    }

    #[test]
    fn the_basis_is_a_bounded_vocabulary_not_prose() {
        // Codex's acceptance test 5, in the part that can be asserted before a judge exists: the
        // basis is an enum with a fixed label set. Free-form prose is not policy.
        let all = [
            TaskBasis::ExitCode,
            TaskBasis::StructuredJudge,
            TaskBasis::ExplicitRefusal,
            TaskBasis::Operator,
            TaskBasis::Unknown,
        ];
        let labels: Vec<&str> = all.iter().map(|b| b.label()).collect();
        assert_eq!(labels, ["exit_code", "structured_judge", "explicit_refusal", "operator", "unknown"]);
        // And it round-trips, so a stored basis cannot decay into a string nobody parses.
        for b in all {
            let json = serde_json::to_string(&b).unwrap();
            assert_eq!(serde_json::from_str::<TaskBasis>(&json).unwrap(), b);
        }
    }

    #[test]
    fn a_refusal_weakens_rather_than_strengthens() {
        let o = SkillOutcome::refused();
        assert_eq!(o.basis, TaskBasis::ExplicitRefusal);
        assert!(o.is_graded() && !o.is_task_success());
    }
}
