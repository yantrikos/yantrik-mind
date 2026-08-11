//! goal — a compiled goal and the CONTRACT that decides when it is met.
//!
//! The problem this solves: "am I done?" answered by a language model is a feeling. It will say yes
//! when it has produced text that looks like an answer, which is not the same as having done the
//! work. So the contract states, before execution begins, what would have to be true — and the
//! runtime checks it.
//!
//! That is why [`CompletionCriteria::evaluate`] returns the list of criteria that FAILED rather than
//! a boolean. "Not done" is not actionable; "two findings short, and one has a single source" tells
//! the controller what to go and get.

use serde::{Deserialize, Serialize};

use crate::capsule::Capsule;

/// What the user wants, made executable.
///
/// This is the Intent Compiler's output. It is deliberately small: a goal, the shape of an
/// acceptable answer, and the bounds. Everything about HOW is decided step by step at runtime,
/// because a twenty-step plan authored up front is obsolete by step three.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoalSpec {
    pub id: String,
    /// The objective in one line, in the user's terms.
    pub goal: String,
    /// Restrictions the run must respect (sources to avoid, scope, tone).
    #[serde(default)]
    pub constraints: Vec<String>,
    /// Capability ids the goal needs, resolved against the registry at compile time. A goal that
    /// names an unavailable capability is refused BEFORE running, rather than discovering it as a
    /// tool error and improvising around it.
    #[serde(default)]
    pub required_capabilities: Vec<String>,
    /// Capabilities the goal wanted that this mind does not have. Non-empty means the compiler has
    /// something to tell the user; it does not necessarily mean the goal is impossible.
    #[serde(default)]
    pub missing_capabilities: Vec<String>,
    pub contract: Contract,
    pub budget: Budget,
    /// How many actions to plan at once. Rolling horizon: 1–4. A longer plan is not more
    /// intelligent, it is more obsolete.
    #[serde(default = "default_horizon")]
    pub horizon: u8,
    #[serde(default)]
    pub risk: Risk,
}

fn default_horizon() -> u8 {
    3
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Reads only. Nothing outside the mind changes.
    #[default]
    ReadOnly,
    /// Touches the user's own data.
    Personal,
    /// Has an outward effect. Every such action still rides the harm gate and the confirmation
    /// handshake — this field is for routing and presentation, never a substitute for the gate.
    Outward,
}

impl GoalSpec {
    /// A goal is runnable when nothing it declared as required is missing.
    pub fn is_runnable(&self) -> bool {
        self.missing_capabilities.is_empty()
    }
}

/// What an acceptable answer looks like, in testable terms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contract {
    /// Things the answer must address. Each becomes a coverage check.
    #[serde(default)]
    pub requirements: Vec<String>,
    pub completion: CompletionCriteria,
    pub output: OutputContract,
}

/// The completion test.
///
/// Every field is a question about the capsule that arithmetic can answer. Nothing here asks the
/// model whether it feels finished.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionCriteria {
    /// How many findings the answer needs. A research goal wanting "the top candidates" is not
    /// satisfied by one.
    #[serde(default = "one")]
    pub min_findings: usize,
    /// A cap, where more findings would be worse than fewer — "give me three options" means three.
    #[serde(default)]
    pub max_findings: Option<usize>,
    /// Evidence each finding must be able to point at. This is the anti-confabulation lever: a
    /// finding with no evidence reference cannot survive the check.
    #[serde(default = "one")]
    pub min_evidence_per_finding: usize,
    /// How many high-importance unresolved uncertainties may remain. Default zero: if the run
    /// itself flagged something important as unknown, finishing while it is unknown is a bug.
    #[serde(default)]
    pub max_open_critical_uncertainties: usize,
    /// Calibrated confidence floor. Raw self-reported confidence must be calibrated before it gets
    /// here (see the `foresight_reliability` ledger) — an uncalibrated 0.8 is not a measurement.
    #[serde(default = "half")]
    pub min_confidence: f64,
    /// Named checks a verifier must have passed. Empty for goals where cross-checking is meaningless.
    #[serde(default)]
    pub required_checks: Vec<String>,
    /// Must every requirement in the contract be covered by at least one finding or fact?
    #[serde(default = "yes")]
    pub require_full_coverage: bool,
}

fn one() -> usize {
    1
}
fn half() -> f64 {
    0.5
}
fn yes() -> bool {
    true
}

impl Default for CompletionCriteria {
    fn default() -> Self {
        Self {
            min_findings: 1,
            max_findings: None,
            min_evidence_per_finding: 1,
            max_open_critical_uncertainties: 0,
            min_confidence: 0.5,
            required_checks: Vec::new(),
            require_full_coverage: true,
        }
    }
}

/// Why a run may not finish yet. One variant per criterion, each carrying the numbers, so the
/// controller can act on it and the operator can read it without a translation layer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Shortfall {
    NotEnoughFindings { have: usize, need: usize },
    TooManyFindings { have: usize, max: usize },
    UnderEvidenced { finding: String, have: usize, need: usize },
    OpenCriticalUncertainty { question: String, importance: f64 },
    LowConfidence { have: f64, need: f64 },
    MissingCheck { check: String },
    UncoveredRequirement { requirement: String },
}

impl Shortfall {
    /// One line an operator can read. The UI shows these verbatim, which is why they are phrased as
    /// what is missing rather than as an error.
    pub fn describe(&self) -> String {
        match self {
            Self::NotEnoughFindings { have, need } => {
                format!("{have} of {need} findings so far")
            }
            Self::TooManyFindings { have, max } => {
                format!("{have} findings, but the goal asked for at most {max}")
            }
            Self::UnderEvidenced { finding, have, need } => {
                format!("\u{201c}{finding}\u{201d} rests on {have} source(s); {need} required")
            }
            Self::OpenCriticalUncertainty { question, importance } => {
                format!("still unresolved and it matters ({importance:.0}%): {question}")
            }
            Self::LowConfidence { have, need } => {
                format!("confidence {:.0}%, below the {:.0}% this goal needs", have * 100.0, need * 100.0)
            }
            Self::MissingCheck { check } => format!("the {check} check has not run"),
            Self::UncoveredRequirement { requirement } => {
                format!("nothing found yet addresses: {requirement}")
            }
        }
    }
}

/// The result of testing a capsule against a contract.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    pub met: bool,
    /// Empty exactly when `met`. Ordered most-actionable first.
    pub shortfalls: Vec<Shortfall>,
}

impl Verdict {
    pub fn met() -> Self {
        Self { met: true, shortfalls: Vec::new() }
    }
    /// A one-line summary for a status row.
    pub fn summarize(&self) -> String {
        if self.met {
            return "all completion criteria met".to_string();
        }
        match self.shortfalls.len() {
            0 => "not met".to_string(),
            1 => self.shortfalls[0].describe(),
            n => format!("{} \u{2014} and {} more", self.shortfalls[0].describe(), n - 1),
        }
    }
}

impl CompletionCriteria {
    /// Test a capsule against this contract.
    ///
    /// Pure and total: no clock, no I/O, no model. Every shortfall is derived from a count or a
    /// comparison, which is the whole reason FINISH can be trusted here — the same capsule always
    /// produces the same verdict, and a test can construct any situation directly.
    pub fn evaluate(&self, capsule: &Capsule, requirements: &[String]) -> Verdict {
        let mut out = Vec::new();

        // Coverage first: it is the most likely thing to be actually missing, and naming an
        // unaddressed requirement points the controller at real work rather than at a threshold.
        if self.require_full_coverage {
            for req in requirements {
                if !capsule.covers(req) {
                    out.push(Shortfall::UncoveredRequirement { requirement: req.clone() });
                }
            }
        }

        let n = capsule.findings.len();
        if n < self.min_findings {
            out.push(Shortfall::NotEnoughFindings { have: n, need: self.min_findings });
        }
        if let Some(max) = self.max_findings {
            if n > max {
                out.push(Shortfall::TooManyFindings { have: n, max });
            }
        }

        for f in &capsule.findings {
            if f.evidence.len() < self.min_evidence_per_finding {
                out.push(Shortfall::UnderEvidenced {
                    finding: f.claim.clone(),
                    have: f.evidence.len(),
                    need: self.min_evidence_per_finding,
                });
            }
        }

        // A run that flagged something important as unknown must not finish while it is unknown.
        // This is the check that stops a confident-sounding answer built on an admitted gap.
        let open: Vec<_> = capsule.open_critical_uncertainties().collect();
        if open.len() > self.max_open_critical_uncertainties {
            for u in open.iter().skip(self.max_open_critical_uncertainties) {
                out.push(Shortfall::OpenCriticalUncertainty {
                    question: u.question.clone(),
                    importance: u.importance * 100.0,
                });
            }
        }

        if capsule.confidence < self.min_confidence {
            out.push(Shortfall::LowConfidence { have: capsule.confidence, need: self.min_confidence });
        }
        for c in &self.required_checks {
            if !capsule.checks_passed.iter().any(|p| p == c) {
                out.push(Shortfall::MissingCheck { check: c.clone() });
            }
        }

        Verdict { met: out.is_empty(), shortfalls: out }
    }
}

/// The shape the answer must take. Drives synthesis and lets the UI lay out a result rather than a
/// wall of prose.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OutputContract {
    /// Rank the findings rather than listing them.
    #[serde(default)]
    pub ranked: bool,
    /// Show the sources behind each finding.
    #[serde(default = "yes")]
    pub show_evidence: bool,
    /// State the downside/risk of each finding. For anything advisory this is not optional.
    #[serde(default)]
    pub include_risks: bool,
    /// State confidence and what remains uncertain.
    #[serde(default = "yes")]
    pub include_confidence: bool,
    /// A free-form shape hint ("table", "short brief", "one paragraph").
    #[serde(default)]
    pub format: Option<String>,
}

/// What a run may consume.
///
/// Held in the spec rather than the loop so that a goal can carry its own limits — a background
/// monitor and an interactive question deserve very different ceilings, and the loop should not have
/// to guess which it is running.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Budget {
    /// Hard cap on actions. The loop stops and answers with what it has.
    pub max_steps: u32,
    /// Hard cap on model calls, which is the cost that actually matters — a step that hits a cache
    /// or runs a deterministic tool is nearly free.
    pub max_model_calls: u32,
    /// Wall-clock ceiling in milliseconds.
    pub max_wall_ms: u64,
    /// Optional spend ceiling in USD. `None` = ungoverned, which is honest rather than a fake number.
    #[serde(default)]
    pub max_usd: Option<f64>,
}

/// Bounds on what an operator may configure.
///
/// A ceiling because an unbounded step count is a runaway loop with a config blessing, and a floor
/// because a one-step budget cannot complete any goal that needs a tool — it would look like the
/// mind had become useless rather than like a setting being wrong.
pub const MIN_STEPS: u32 = 2;
pub const MAX_STEPS_CEILING: u32 = 500;
pub const MAX_WALL_MS_CEILING: u64 = 2 * 60 * 60 * 1000;

impl Budget {
    /// An interactive turn.
    ///
    /// 100 steps, not the 5 the loop shipped with. Five steps cannot do real work: a research
    /// question spends two of them discovering it needs to search, and a repository audit has barely
    /// opened a file. The cap exists to stop a runaway, not to define how hard the mind may think.
    ///
    /// The two limits are INDEPENDENT bounds, not a matched pair, and which one binds depends
    /// entirely on the work. A step that hits a cache, dedups, or runs a deterministic tool costs
    /// milliseconds; a reasoning call costs seconds. So a turn made of cheap steps can reach 100
    /// inside the clock, while a turn that reasons at every step will hit 180 seconds after twenty or
    /// so — and that is the intended behaviour, because the clock is a promise to whoever is waiting
    /// and the step count is a guard against a runaway. Neither is a target.
    ///
    /// What matters is that the operator is always TOLD which one stopped the run: the controller
    /// reports `Timeout` and `StepBudget` as distinct reasons precisely so "it ran out of time" is
    /// never mistaken for "it ran out of ideas".
    pub fn interactive() -> Self {
        Self { max_steps: 100, max_model_calls: 100, max_wall_ms: 180_000, max_usd: None }
    }
    /// A delegated or scheduled run: nobody is watching the clock, so depth is worth more.
    pub fn background() -> Self {
        Self { max_steps: 150, max_model_calls: 150, max_wall_ms: 45 * 60_000, max_usd: None }
    }

    /// Apply operator overrides, clamped.
    ///
    /// Takes `Option`s rather than reading the environment itself: this crate stays a pure function
    /// of its inputs, so the wiring layer owns config and a test can construct any budget directly.
    ///
    /// Clamping is silent on purpose at this layer — it returns a valid budget rather than an error,
    /// because a mistyped setting must not stop the mind from answering. The caller reports what it
    /// adjusted (see `clamp_note`).
    pub fn with_overrides(
        mut self,
        max_steps: Option<u32>,
        max_model_calls: Option<u32>,
        max_wall_ms: Option<u64>,
        max_usd: Option<f64>,
    ) -> Self {
        if let Some(s) = max_steps {
            self.max_steps = s.clamp(MIN_STEPS, MAX_STEPS_CEILING);
        }
        match max_model_calls {
            // Model calls cannot exceed steps: a step is what makes a call, so a higher figure is
            // not a bigger budget, it is a number that can never bind.
            Some(m) => self.max_model_calls = m.clamp(1, self.max_steps),
            // No explicit call budget: track the step budget.
            //
            // This is the whole reason the two are coupled here. Nearly every step makes a model
            // call, so raising the iteration limit to 20 while leaving the call ceiling at 5 means
            // the call ceiling binds first and the new setting does nothing — the operator changes
            // the number, restarts, and sees no difference. My first version only ever LOWERED the
            // ceiling here, which had exactly that effect; the test that was supposed to catch it
            // asserted `calls <= steps`, which a stuck 5 satisfies.
            None => self.max_model_calls = self.max_steps,
        }
        if let Some(w) = max_wall_ms {
            self.max_wall_ms = w.clamp(5_000, MAX_WALL_MS_CEILING);
        }
        if let Some(u) = max_usd {
            self.max_usd = (u > 0.0).then_some(u);
        }
        self
    }

    /// What was adjusted, for the operator. `None` when the request was honoured exactly — so a
    /// clamped setting is visible rather than mysteriously ignored.
    pub fn clamp_note(&self, requested_steps: Option<u32>) -> Option<String> {
        let r = requested_steps?;
        (r != self.max_steps).then(|| {
            format!("step limit {r} was adjusted to {} (allowed {MIN_STEPS}\u{2013}{MAX_STEPS_CEILING})", self.max_steps)
        })
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::interactive()
    }
}

impl GoalSpec {
    /// A minimal spec for a plain question — the common case, and one that should not require the
    /// compiler to invent criteria it has no basis for.
    pub fn simple(goal: impl Into<String>) -> Self {
        Self {
            id: format!("g-{}", uuid7::uuid7()),
            goal: goal.into(),
            constraints: Vec::new(),
            required_capabilities: Vec::new(),
            missing_capabilities: Vec::new(),
            contract: Contract {
                requirements: Vec::new(),
                completion: CompletionCriteria { require_full_coverage: false, ..Default::default() },
                output: OutputContract::default(),
            },
            budget: Budget::interactive(),
            horizon: default_horizon(),
            risk: Risk::ReadOnly,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capsule::{Capsule, Finding, Uncertainty};

    fn capsule_with(findings: Vec<Finding>) -> Capsule {
        let mut c = Capsule::new("g1", "find things");
        c.findings = findings;
        c.confidence = 0.9;
        c
    }

    fn finding(claim: &str, evidence: &[&str]) -> Finding {
        Finding {
            claim: claim.to_string(),
            evidence: evidence.iter().map(|e| e.to_string()).collect(),
            addresses: Vec::new(),
            risk: None,
            rank: None,
        }
    }

    #[test]
    fn a_met_contract_reports_no_shortfalls() {
        let c = capsule_with(vec![finding("A is up", &["E1", "E2"])]);
        let crit = CompletionCriteria { min_evidence_per_finding: 2, require_full_coverage: false, ..Default::default() };
        let v = crit.evaluate(&c, &[]);
        assert!(v.met, "{:?}", v.shortfalls);
        assert!(v.shortfalls.is_empty());
        assert_eq!(v.summarize(), "all completion criteria met");
    }

    /// The core property: FINISH is arithmetic. A capsule two findings short cannot pass, however
    /// confident the run is about itself.
    #[test]
    fn confidence_cannot_buy_its_way_past_a_count() {
        let mut c = capsule_with(vec![finding("only one", &["E1"])]);
        c.confidence = 1.0;
        let crit = CompletionCriteria { min_findings: 3, require_full_coverage: false, ..Default::default() };
        let v = crit.evaluate(&c, &[]);
        assert!(!v.met);
        assert_eq!(v.shortfalls, vec![Shortfall::NotEnoughFindings { have: 1, need: 3 }]);
        assert_eq!(v.summarize(), "1 of 3 findings so far");
    }

    /// A finding nothing supports is the confabulation case, and it must fail by construction rather
    /// than by a reviewer noticing.
    #[test]
    fn an_unevidenced_finding_fails_the_contract() {
        let c = capsule_with(vec![finding("grounded", &["E1"]), finding("invented", &[])]);
        let crit = CompletionCriteria { require_full_coverage: false, ..Default::default() };
        let v = crit.evaluate(&c, &[]);
        assert!(!v.met);
        assert!(v.shortfalls.iter().any(
            |s| matches!(s, Shortfall::UnderEvidenced { finding, have: 0, .. } if finding == "invented")
        ));
        assert!(v.shortfalls[0].describe().contains("invented"));
    }

    /// A run that admitted an important unknown must not finish while it is unknown. An unimportant
    /// one is allowed through — otherwise nothing would ever complete.
    #[test]
    fn an_open_important_uncertainty_blocks_completion_but_a_trivial_one_does_not() {
        let mut c = capsule_with(vec![finding("A", &["E1"])]);
        c.uncertainties = vec![
            Uncertainty { question: "is the move news-driven?".into(), importance: 0.9, confidence: 0.3, resolved: false },
            Uncertainty { question: "what colour is the logo?".into(), importance: 0.1, confidence: 0.2, resolved: false },
        ];
        let crit = CompletionCriteria { require_full_coverage: false, ..Default::default() };
        let v = crit.evaluate(&c, &[]);
        assert!(!v.met);
        let qs: Vec<&str> = v
            .shortfalls
            .iter()
            .filter_map(|s| match s {
                Shortfall::OpenCriticalUncertainty { question, .. } => Some(question.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(qs, vec!["is the move news-driven?"], "only the important one blocks");

        // Resolving it clears the block.
        c.uncertainties[0].resolved = true;
        assert!(crit.evaluate(&c, &[]).met);
    }

    /// Coverage is checked against the contract's own requirements, so "identify risk" not being
    /// addressed is a named shortfall rather than a silently thin answer.
    #[test]
    fn an_unaddressed_requirement_is_named() {
        let mut c = capsule_with(vec![finding("liquidity is fine", &["E1"])]);
        c.findings[0].addresses = vec!["sufficient liquidity".to_string()];
        let crit = CompletionCriteria::default();
        let reqs = vec!["sufficient liquidity".to_string(), "identify downside/risk".to_string()];
        let v = crit.evaluate(&c, &reqs);
        assert!(!v.met);
        assert_eq!(
            v.shortfalls[0],
            Shortfall::UncoveredRequirement { requirement: "identify downside/risk".into() }
        );
        assert!(v.shortfalls[0].describe().contains("identify downside/risk"));
    }

    #[test]
    fn a_cap_on_findings_is_enforced_too() {
        let c = capsule_with(vec![finding("a", &["E1"]), finding("b", &["E1"]), finding("c", &["E1"])]);
        let crit = CompletionCriteria { max_findings: Some(2), require_full_coverage: false, ..Default::default() };
        let v = crit.evaluate(&c, &[]);
        assert!(v.shortfalls.contains(&Shortfall::TooManyFindings { have: 3, max: 2 }));
    }

    #[test]
    fn a_simple_goal_does_not_invent_criteria_it_has_no_basis_for() {
        let g = GoalSpec::simple("what's the weather in pune?");
        assert!(g.is_runnable());
        assert!(!g.contract.completion.require_full_coverage, "no requirements were stated, so none are enforced");
        assert_eq!(g.contract.completion.min_findings, 1);
        assert_eq!(g.horizon, 3, "rolling horizon, not a 20-step plan");
        // One evidenced finding satisfies it.
        let c = capsule_with(vec![finding("28C and clear", &["E1"])]);
        assert!(g.contract.completion.evaluate(&c, &g.contract.requirements).met);
    }

    #[test]
    fn a_goal_needing_a_missing_capability_is_not_runnable() {
        let mut g = GoalSpec::simple("check my github");
        g.required_capabilities = vec!["github".into()];
        g.missing_capabilities = vec!["github".into()];
        assert!(!g.is_runnable(), "refuse before running, not after a tool error");
    }

    #[test]
    fn budgets_differ_by_who_is_waiting() {
        assert!(Budget::interactive().max_steps < Budget::background().max_steps);
        assert!(Budget::interactive().max_wall_ms < Budget::background().max_wall_ms);
        assert!(Budget::interactive().max_usd.is_none(), "an absent ceiling is None, not a fake number");
    }

    #[test]
    fn an_operator_can_raise_the_step_limit() {
        let b = Budget::interactive().with_overrides(Some(20), None, None, None);
        assert_eq!(b.max_steps, 20);
        assert!(b.clamp_note(Some(20)).is_none(), "an honoured setting needs no note");
    }

    /// A nonsense setting must not stop the mind answering, and must not be silently ignored either.
    #[test]
    fn an_absurd_step_limit_is_clamped_and_reported() {
        let b = Budget::interactive().with_overrides(Some(100_000), None, None, None);
        assert_eq!(b.max_steps, MAX_STEPS_CEILING, "an unbounded loop is not a valid configuration");
        let note = b.clamp_note(Some(100_000)).expect("a clamped setting must be reported");
        assert!(note.contains("100000") && note.contains("500"), "{note}");

        // And the floor: one step cannot complete any goal needing a tool, so it would look like the
        // mind had broken rather than like a setting being wrong.
        let low = Budget::interactive().with_overrides(Some(0), None, None, None);
        assert_eq!(low.max_steps, MIN_STEPS);
    }

    /// The trap this guards: nearly every step makes a model call, so raising the iteration limit
    /// while the call ceiling stays put means the call ceiling binds first and the setting does
    /// nothing visible.
    ///
    /// Asserted as EQUALITY, not `<=`. The first version of this test used `<=`, which a call ceiling
    /// stuck at its old value satisfies perfectly — so it passed while the setting was broken.
    #[test]
    fn raising_the_iteration_limit_actually_raises_what_binds() {
        let b = Budget { max_steps: 5, max_model_calls: 5, ..Budget::interactive() }
            .with_overrides(Some(30), None, None, None);
        assert_eq!(b.max_steps, 30);
        assert_eq!(b.max_model_calls, 30, "an unraised call ceiling would silently cap the run at 5");

        // An explicit call budget is respected, and is what binds when it is the lower of the two.
        let explicit = Budget::interactive().with_overrides(Some(30), Some(4), None, None);
        assert_eq!(explicit.max_steps, 30);
        assert_eq!(explicit.max_model_calls, 4, "an explicit call budget is the operator's choice");

        // Lowering steps pulls the ceiling down with it: a call ceiling above the step ceiling can
        // never bind, so it is not a bigger budget, just a misleading number.
        let tight = Budget::background().with_overrides(Some(3), None, None, None);
        assert_eq!((tight.max_steps, tight.max_model_calls), (3, 3));
    }

    #[test]
    fn a_zero_spend_ceiling_means_ungoverned_not_zero_dollars() {
        let b = Budget::interactive().with_overrides(None, None, None, Some(0.0));
        assert!(b.max_usd.is_none(), "0 is how an operator clears a limit, not a $0 budget");
        let set = Budget::interactive().with_overrides(None, None, None, Some(2.5));
        assert_eq!(set.max_usd, Some(2.5));
    }
}
