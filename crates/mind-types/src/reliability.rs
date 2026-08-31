//! reliability — how often a thing has WORKED, and whether that is enough to act on.
//!
//! One policy, in one place. It was written four times, in three shapes: an SQL predicate in the
//! skill store, a Rust bool in the surface report, a match on a `Prior` in the procedure ranker —
//! and `Prior::is_trustworthy` disagreed with all of them, requiring 5 runs where the rest require
//! 4. A rule that exists in four places is four rules that happen to agree today (ARCH-6 P.5).
//!
//! Lives in `mind-types` rather than `mind-spec`, where the plan put it: `mind-spec` depends on
//! `mind-types`, and `Skill` — whose numbers this describes — lives here, so a `Reliability` over
//! there could never be reached by the type it is about. `mind-spec` re-exports it.

/// What is known about a thing's outcomes, and nothing more.
///
/// Holds the two counts and refuses to answer questions they cannot support. In particular there
/// is no way to get a success RATE out of zero runs, which is the trap this replaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Reliability {
    runs: u32,
    successes: u32,
}

/// Where a thing stands, once. Four rungs, because the two the code had — "fine" and "quarantined"
/// — could not tell "never tried" from "tried and good", and that difference is the whole point.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    /// Never run. NOT the same as perfect — the distinction the old `success_rate()` erased by
    /// returning 1.0 here.
    Untested,
    /// Run, but not often enough for the record to mean anything yet.
    Candidate,
    /// Enough runs, and working more often than not.
    Active,
    /// Enough runs, and failing more often than not.
    Discredited,
}

impl Verdict {
    pub fn label(self) -> &'static str {
        match self {
            Verdict::Untested => "untested",
            Verdict::Candidate => "candidate",
            Verdict::Active => "active",
            Verdict::Discredited => "discredited",
        }
    }
}

/// How many runs before the record is allowed to condemn something.
///
/// THE one threshold. `Prior::is_trustworthy` used 5 while the three quarantine sites used 4; the
/// disagreement was invisible because no test compared them.
pub const MIN_RUNS: u32 = 4;

/// Failing more often than not, over enough runs to mean it.
///
/// Free-standing so a caller holding a rate rather than counts — the procedure ranker holds a
/// `Prior` — calls the SAME predicate instead of writing the comparison again.
pub fn is_discredited(runs: u32, rate: f64) -> bool {
    runs >= MIN_RUNS && rate < 0.5
}

impl Reliability {
    /// `successes` is clamped to `runs`: more successes than attempts is not a state that should be
    /// representable, and clamping here means no caller has to check.
    pub fn new(runs: u32, successes: u32) -> Self {
        Self {
            runs,
            successes: successes.min(runs),
        }
    }

    pub fn runs(&self) -> u32 {
        self.runs
    }

    pub fn successes(&self) -> u32 {
        self.successes
    }

    /// The observed success rate, or `None` when there is nothing to compute it from.
    ///
    /// `Option` rather than a default is the point: a caller rendering this to a human must decide
    /// what "no evidence" looks like, and cannot accidentally print an optimism prior as a
    /// measurement.
    pub fn rate(&self) -> Option<f64> {
        (self.runs > 0).then(|| f64::from(self.successes) / f64::from(self.runs))
    }

    /// The optimism prior for RANKING — 1.0 when untested, on purpose.
    ///
    /// `recall_skills` scores `sim + 0.1 * rank_score()`, so a banked-but-never-run skill keeps a
    /// nudge and gets tried at all; without it a new skill could never earn a first run. This is
    /// the half of the old `success_rate()` that was correct, under a name that says which half it
    /// is. It is a ranking input and never a claim about reliability — for that, ask `verdict()`.
    pub fn rank_score(&self) -> f64 {
        self.rate().unwrap_or(1.0)
    }

    pub fn verdict(&self) -> Verdict {
        match self.rate() {
            None => Verdict::Untested,
            Some(_) if self.runs < MIN_RUNS => Verdict::Candidate,
            Some(rate) if is_discredited(self.runs, rate) => Verdict::Discredited,
            Some(_) => Verdict::Active,
        }
    }

    /// The quarantine question, asked once.
    pub fn is_discredited(&self) -> bool {
        self.verdict() == Verdict::Discredited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_untested_thing_has_no_rate_and_is_not_perfect() {
        let r = Reliability::new(0, 0);
        assert_eq!(r.rate(), None, "there is no rate to compute");
        assert_eq!(r.verdict(), Verdict::Untested);
        assert!(
            !r.is_discredited(),
            "never tried is not the same as failing"
        );
        // The ranking half, kept deliberately: without it a new skill never earns a first run.
        assert_eq!(
            r.rank_score(),
            1.0,
            "the optimism prior survives, under a name that says so"
        );
    }

    #[test]
    fn the_verdict_table() {
        // Below the threshold the record does not get to condemn anything, however bad it looks.
        for (runs, successes) in [(1, 0), (2, 0), (3, 0), (3, 1)] {
            assert_eq!(
                Reliability::new(runs, successes).verdict(),
                Verdict::Candidate,
                "{runs} runs is not enough to judge on"
            );
        }
        // At the threshold it does.
        assert_eq!(Reliability::new(4, 1).verdict(), Verdict::Discredited);
        assert_eq!(Reliability::new(4, 0).verdict(), Verdict::Discredited);
        assert_eq!(
            Reliability::new(4, 2).verdict(),
            Verdict::Active,
            "exactly half is not failing MORE often than not"
        );
        assert_eq!(Reliability::new(4, 4).verdict(), Verdict::Active);
        assert_eq!(Reliability::new(100, 49).verdict(), Verdict::Discredited);
    }

    /// The behaviour this replaces, pinned so the unification cannot quietly retune it.
    #[test]
    fn the_verdict_agrees_with_every_expression_it_replaces() {
        for runs in 0u32..40 {
            for successes in 0..=runs {
                let r = Reliability::new(runs, successes);

                // 1. The SQL predicate: `runs>=4 AND (successes*2) < runs`.
                let sql = runs >= 4 && successes * 2 < runs;
                assert_eq!(
                    r.is_discredited(),
                    sql,
                    "SQL disagrees at {runs}/{successes}"
                );

                // 2. The surface report's Rust bool — the same expression, written again.
                let surface = runs >= 4 && successes * 2 < runs;
                assert_eq!(
                    r.is_discredited(),
                    surface,
                    "surface disagrees at {runs}/{successes}"
                );

                // 3. The procedure ranker, which holds a RATE rather than counts.
                if let Some(rate) = r.rate() {
                    assert_eq!(
                        r.is_discredited(),
                        is_discredited(runs, rate),
                        "the rate form disagrees at {runs}/{successes}"
                    );
                }
            }
        }
    }

    #[test]
    fn more_successes_than_attempts_is_not_representable() {
        let r = Reliability::new(3, 99);
        assert_eq!(r.successes(), 3);
        assert_eq!(r.rate(), Some(1.0));
    }
}
