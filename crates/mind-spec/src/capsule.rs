//! capsule — the rolling state the runtime owns, and the reducer that advances it.
//!
//! # Replace, do not append
//!
//! This is the whole idea. A transcript grows without bound and buries its own signal; a capsule is
//! rewritten each step and stays the same size. `old capsule + observation -> reducer -> new capsule`
//! is a fold, not a log. The consequence is that step 30 costs what step 3 cost.
//!
//! # Evidence lives outside
//!
//! The capsule holds evidence by REFERENCE — an id, a one-line summary, and where it came from. The
//! full 12 KB article stays in the store. When a decision genuinely needs the body, the loop pages it
//! in for that one call and drops it again. Context becomes something you page rather than something
//! you accumulate.
//!
//! # Budget for the whole thing
//!
//! 500–2,000 tokens. [`Capsule::render`] enforces that by construction: it emits the sections in
//! priority order and stops. A capsule that cannot be rendered inside its budget is a capsule that
//! has stopped being a summary, so the reducer prunes on the way in ([`Capsule::compact`]) rather
//! than letting the prompt builder truncate mid-sentence.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::Millis;

/// A claim the run is prepared to stand behind, and what supports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    pub claim: String,
    /// Evidence ids. NOT the evidence bodies — a finding that inlined its sources would defeat the
    /// point of the capsule.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// Which of the contract's requirements this addresses. Drives the coverage check, and is why
    /// coverage is a count rather than a model asking itself whether it covered everything.
    #[serde(default)]
    pub addresses: Vec<String>,
    /// The downside. Present when the output contract asks for risk, so a synthesis step cannot
    /// quietly omit it.
    #[serde(default)]
    pub risk: Option<String>,
    #[serde(default)]
    pub rank: Option<u8>,
}

/// A named thing the run does not know, with how much it matters.
///
/// The list, not a single number, is what makes next-action selection tractable: resolve the
/// highest-importance unresolved question. A scalar "confidence: 0.67" tells the controller nothing
/// about what to do next.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Uncertainty {
    pub question: String,
    /// How much the answer changes the conclusion, 0–1.
    pub importance: f64,
    /// How sure the run currently is, 0–1. CALIBRATE before acting on this: raw self-report from a
    /// model is not a measurement (see `MemoryFacade::foresight_reliability`).
    pub confidence: f64,
    #[serde(default)]
    pub resolved: bool,
}

impl Uncertainty {
    /// Important enough that finishing while it is open would be dishonest.
    pub fn is_critical(&self) -> bool {
        self.importance >= 0.6
    }
    /// Worth spending an action on: it matters and we do not know it.
    pub fn is_worth_resolving(&self) -> bool {
        !self.resolved && self.importance >= 0.4 && self.confidence < 0.7
    }
    /// Ordering key for "what should I look into next" — impact times ignorance.
    pub fn priority(&self) -> f64 {
        if self.resolved {
            return 0.0;
        }
        self.importance * (1.0 - self.confidence)
    }
}

/// A pointer to material held outside the capsule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceRef {
    /// Short stable id (E17). What a finding cites and what a FETCH names.
    pub id: String,
    /// One line. This is all the model normally sees.
    pub summary: String,
    /// Where it came from, so a claim can be traced without paging the body.
    pub source: String,
    /// How useful it turned out to be. Starts as a prior and is corrected by outcome, which is what
    /// eventually lets retrieval learn.
    #[serde(default)]
    pub utility: f64,
    /// Set when the body has been paged in during this run — so the loop does not fetch it twice.
    #[serde(default)]
    pub loaded: bool,
}

/// Evidence with its body, as held by the store. Never inside a capsule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: String,
    pub summary: String,
    pub source: String,
    pub body: String,
    pub captured_ms: Millis,
}

impl Evidence {
    pub fn as_ref_with(&self, utility: f64) -> EvidenceRef {
        EvidenceRef {
            id: self.id.clone(),
            summary: self.summary.clone(),
            source: self.source.clone(),
            utility,
            loaded: false,
        }
    }
}

/// What one executed action produced, already normalized.
///
/// Tools return their raw shape to an adapter, and the adapter produces this. The point is that the
/// model never sees 12 KB of HTML: by the time an observation exists, the extraction has happened.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Observation {
    /// The action that produced it, for the attempt log.
    pub action: String,
    /// Did it work? A failure is information too, and one the controller must see to stop repeating it.
    pub ok: bool,
    /// New evidence this action produced.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
    /// Findings it established.
    #[serde(default)]
    pub findings: Vec<Finding>,
    /// Uncertainties it raised or answered. A matching question (case-insensitive) updates rather
    /// than duplicates.
    #[serde(default)]
    pub uncertainties: Vec<Uncertainty>,
    /// Facts worth carrying that are not findings — context, not conclusions.
    #[serde(default)]
    pub notes: Vec<String>,
    /// A short phrase for the completed-work list. Not prose: "screened 14 candidates".
    #[serde(default)]
    pub did: Option<String>,
    /// Why it failed, when it did.
    #[serde(default)]
    pub error: Option<String>,
    /// A named verification that passed.
    #[serde(default)]
    pub check_passed: Option<String>,
}

/// How the run is doing, in numbers the controller can compare across steps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Progress {
    pub steps: u32,
    pub model_calls: u32,
    pub failures: u32,
    pub replans: u32,
    /// Consecutive steps that added no evidence and resolved nothing. The stall signal — and the
    /// reason the loop can notice going in circles without a model being asked to introspect.
    pub barren_steps: u32,
}

/// THE state capsule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Capsule {
    pub goal_id: String,
    pub goal: String,
    /// What the run is doing right now, one line. Rewritten, not appended to.
    #[serde(default)]
    pub subgoal: Option<String>,
    /// The current working hypothesis, when the goal is investigative.
    #[serde(default)]
    pub hypothesis: Option<String>,
    /// Short phrases for work already done. Capped; oldest are folded away.
    #[serde(default)]
    pub completed: Vec<String>,
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub evidence: Vec<EvidenceRef>,
    #[serde(default)]
    pub uncertainties: Vec<Uncertainty>,
    #[serde(default)]
    pub notes: Vec<String>,
    /// Contradictions the run has noticed. Their presence is a critic trigger, not a step to log.
    #[serde(default)]
    pub contradictions: Vec<String>,
    /// Actions attempted, as signatures. Deduplication and loop detection read this — code's job,
    /// not the model's.
    #[serde(default)]
    pub attempted: Vec<String>,
    /// Failures with their reasons, so the next action can avoid the same wall.
    #[serde(default)]
    pub failures: Vec<String>,
    #[serde(default)]
    pub checks_passed: Vec<String>,
    /// Overall confidence, 0–1. Derived, not self-reported: see [`Capsule::recompute_confidence`].
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub progress: Progress,
    /// The 1–4 actions currently planned. Replaced on replan.
    #[serde(default)]
    pub plan: Vec<String>,
}

/// Caps that keep a capsule a summary. Exceeding one prunes rather than truncating a prompt later.
const MAX_COMPLETED: usize = 8;
const MAX_EVIDENCE: usize = 24;
const MAX_NOTES: usize = 8;
const MAX_ATTEMPTED: usize = 24;
const MAX_FAILURES: usize = 6;

impl Capsule {
    pub fn new(goal_id: impl Into<String>, goal: impl Into<String>) -> Self {
        Self {
            goal_id: goal_id.into(),
            goal: goal.into(),
            subgoal: None,
            hypothesis: None,
            completed: Vec::new(),
            findings: Vec::new(),
            evidence: Vec::new(),
            uncertainties: Vec::new(),
            notes: Vec::new(),
            contradictions: Vec::new(),
            attempted: Vec::new(),
            failures: Vec::new(),
            checks_passed: Vec::new(),
            confidence: 0.0,
            progress: Progress::default(),
            plan: Vec::new(),
        }
    }

    /// Fold one observation in, returning the NEW capsule.
    ///
    /// Consumes `self` on purpose. An `&mut` reducer invites a caller to keep the old capsule around
    /// and accidentally append to history — taking ownership makes replacement the only option the
    /// type system offers.
    pub fn reduce(mut self, obs: Observation) -> Self {
        self.progress.steps += 1;
        let gained_evidence = !obs.evidence.is_empty();
        let mut resolved_something = false;

        self.attempted.push(obs.action.clone());

        if !obs.ok {
            self.progress.failures += 1;
            if let Some(e) = &obs.error {
                // Keep the action with the reason: "web_fetch: 502" is avoidable next time,
                // "something failed" is not.
                self.failures.push(format!("{}: {}", obs.action, first_line(e, 120)));
            }
        }

        for ev in obs.evidence {
            // A repeat of the same evidence is not new information; keep the first sighting.
            if !self.evidence.iter().any(|e| e.id == ev.id) {
                self.evidence.push(ev.as_ref_with(0.5));
            }
        }

        for f in obs.findings {
            // Same claim twice is one finding with the union of its support, not two findings —
            // otherwise a min_findings contract could be satisfied by saying one thing twice.
            match self.findings.iter_mut().find(|x| same_claim(&x.claim, &f.claim)) {
                Some(existing) => {
                    for e in f.evidence {
                        if !existing.evidence.contains(&e) {
                            existing.evidence.push(e);
                        }
                    }
                    for a in f.addresses {
                        if !existing.addresses.contains(&a) {
                            existing.addresses.push(a);
                        }
                    }
                    if existing.risk.is_none() {
                        existing.risk = f.risk;
                    }
                }
                None => self.findings.push(f),
            }
        }

        for u in obs.uncertainties {
            match self.uncertainties.iter_mut().find(|x| same_claim(&x.question, &u.question)) {
                Some(existing) => {
                    if u.resolved && !existing.resolved {
                        resolved_something = true;
                    }
                    // Confidence may only be revised by evidence, so take the newer reading.
                    existing.confidence = u.confidence;
                    existing.resolved = existing.resolved || u.resolved;
                    existing.importance = existing.importance.max(u.importance);
                }
                None => {
                    // A question arriving already answered is still information gained — the step
                    // learned something, even though nothing was open to transition. Only a NEW OPEN
                    // question is not progress: raising a doubt is useful, but it is not an answer.
                    if u.resolved {
                        resolved_something = true;
                    }
                    self.uncertainties.push(u);
                }
            }
        }

        for n in obs.notes {
            if !self.notes.iter().any(|x| same_claim(x, &n)) {
                self.notes.push(n);
            }
        }
        if let Some(d) = obs.did {
            self.completed.push(d);
        }
        if let Some(c) = obs.check_passed {
            if !self.checks_passed.contains(&c) {
                self.checks_passed.push(c);
            }
        }

        // The stall signal. A step that neither found evidence nor resolved a question got nowhere,
        // whatever the model narrated about it.
        if gained_evidence || resolved_something {
            self.progress.barren_steps = 0;
        } else {
            self.progress.barren_steps += 1;
        }

        self.recompute_confidence();
        self.compact();
        self
    }

    /// Confidence DERIVED from the state, never self-reported.
    ///
    /// A model asked "how confident are you?" answers about its own fluency. This asks the capsule
    /// three questions arithmetic can answer: is anything actually supported, is anything important
    /// still unknown, and is anything contradictory. That is not a perfect estimator, but it cannot
    /// be talked into 0.95 by a well-written paragraph.
    pub fn recompute_confidence(&mut self) {
        if self.findings.is_empty() {
            self.confidence = 0.0;
            return;
        }
        // How well-supported the findings are.
        //
        // The mapping is a DECLARED prior, not a measurement: 0 sources → 0.0, one → 0.5
        // (supported), two → 0.8 (corroborated), three or more → 1.0. The steps are steep at the
        // start because the gap between "nothing backs this" and "something does" is the biggest one
        // there is, while the fourth source adds very little.
        //
        // The exact numbers matter for a reason worth stating: a linear /3 saturation caps a
        // single-source finding at 0.33, which the default contract's 0.5 confidence floor can never
        // accept — so a goal that explicitly allowed one source per finding would be unsatisfiable.
        // Thresholds and floors have to be designed against each other.
        let support: f64 = self
            .findings
            .iter()
            .map(|f| match f.evidence.len() {
                0 => 0.0,
                1 => 0.5,
                2 => 0.8,
                _ => 1.0,
            })
            .sum::<f64>()
            / self.findings.len() as f64;

        // What the run itself admits it does not know, weighted by how much it matters.
        let doubt: f64 = self
            .uncertainties
            .iter()
            .filter(|u| !u.resolved)
            .map(|u| u.priority())
            .fold(0.0, f64::max);

        // An unresolved contradiction is the strongest reason to distrust a conclusion.
        let clash = if self.contradictions.is_empty() { 0.0 } else { 0.3 };

        self.confidence = (support - doubt - clash).clamp(0.0, 1.0);
    }

    /// Prune to the caps, keeping what is most useful.
    ///
    /// Ordering matters: evidence is dropped by lowest utility (not oldest), because the useful thing
    /// found first should survive the noise found later. Completed work folds into a count so the
    /// capsule keeps knowing that earlier work happened without listing it.
    pub fn compact(&mut self) {
        if self.completed.len() > MAX_COMPLETED {
            let folded = self.completed.len() - (MAX_COMPLETED - 1);
            self.completed.drain(..folded);
            self.completed.insert(0, format!("(+{folded} earlier steps)"));
        }
        if self.evidence.len() > MAX_EVIDENCE {
            self.evidence.sort_by(|a, b| b.utility.partial_cmp(&a.utility).unwrap_or(std::cmp::Ordering::Equal));
            self.evidence.truncate(MAX_EVIDENCE);
        }
        self.notes.truncate(MAX_NOTES);
        if self.attempted.len() > MAX_ATTEMPTED {
            let drop = self.attempted.len() - MAX_ATTEMPTED;
            self.attempted.drain(..drop);
        }
        if self.failures.len() > MAX_FAILURES {
            let drop = self.failures.len() - MAX_FAILURES;
            self.failures.drain(..drop);
        }
        // Resolved, unimportant uncertainties have served their purpose.
        self.uncertainties.retain(|u| !(u.resolved && u.importance < 0.4));
    }

    /// Does anything found so far address this requirement?
    pub fn covers(&self, requirement: &str) -> bool {
        self.findings.iter().any(|f| f.addresses.iter().any(|a| same_claim(a, requirement)))
    }

    /// Unresolved questions important enough to block finishing, most impactful first.
    pub fn open_critical_uncertainties(&self) -> impl Iterator<Item = &Uncertainty> {
        let mut v: Vec<&Uncertainty> =
            self.uncertainties.iter().filter(|u| !u.resolved && u.is_critical()).collect();
        v.sort_by(|a, b| b.priority().partial_cmp(&a.priority()).unwrap_or(std::cmp::Ordering::Equal));
        v.into_iter()
    }

    /// The single question most worth spending the next action on, if any.
    pub fn next_uncertainty(&self) -> Option<&Uncertainty> {
        self.uncertainties
            .iter()
            .filter(|u| u.is_worth_resolving())
            .max_by(|a, b| a.priority().partial_cmp(&b.priority()).unwrap_or(std::cmp::Ordering::Equal))
    }

    /// How many times this exact action has been attempted. The loop guard's input, and the reason
    /// it can catch A→B→A→B rather than only an immediate repeat.
    pub fn attempts_of(&self, action: &str) -> usize {
        self.attempted.iter().filter(|a| a.as_str() == action).count()
    }

    /// Distinct actions tried. A run with many attempts but few distinct ones is going in circles.
    pub fn distinct_attempts(&self) -> usize {
        self.attempted.iter().collect::<BTreeSet<_>>().len()
    }

    /// Render for a prompt: sections in priority order, hard-stopped at `max_chars`.
    ///
    /// Priority is deliberate. Goal and current state always fit. Uncertainties come before evidence
    /// because they drive the next decision. Attempt history comes last because its only job is to
    /// stop a repeat, and the controller already enforces that in code — if it is cut, nothing
    /// breaks.
    pub fn render(&self, max_chars: usize) -> String {
        let mut s = String::with_capacity(max_chars.min(4096));
        let mut push = |section: String, s: &mut String| {
            if s.len() + section.len() <= max_chars {
                s.push_str(&section);
            }
        };

        s.push_str(&format!("GOAL\n{}\n", self.goal));
        if let Some(sg) = &self.subgoal {
            s.push_str(&format!("NOW\n{sg}\n"));
        }
        if let Some(h) = &self.hypothesis {
            s.push_str(&format!("HYPOTHESIS\n{h}\n"));
        }
        s.push_str(&format!(
            "PROGRESS\nstep {} \u{b7} confidence {:.0}%{}\n",
            self.progress.steps,
            self.confidence * 100.0,
            if self.progress.barren_steps > 0 {
                format!(" \u{b7} {} step(s) without new information", self.progress.barren_steps)
            } else {
                String::new()
            }
        ));

        if !self.findings.is_empty() {
            let mut sec = String::from("FINDINGS\n");
            for f in &self.findings {
                sec.push_str(&format!("- {} [{}]\n", f.claim, f.evidence.join(",")));
            }
            push(sec, &mut s);
        }
        let open: Vec<&Uncertainty> = self.uncertainties.iter().filter(|u| !u.resolved).collect();
        if !open.is_empty() {
            let mut sec = String::from("OPEN QUESTIONS (most important first)\n");
            let mut sorted = open;
            sorted.sort_by(|a, b| b.priority().partial_cmp(&a.priority()).unwrap_or(std::cmp::Ordering::Equal));
            for u in sorted {
                sec.push_str(&format!("- {} (matters {:.0}%, known {:.0}%)\n", u.question, u.importance * 100.0, u.confidence * 100.0));
            }
            push(sec, &mut s);
        }
        if !self.contradictions.is_empty() {
            push(format!("CONTRADICTIONS\n- {}\n", self.contradictions.join("\n- ")), &mut s);
        }
        if !self.evidence.is_empty() {
            let mut sec = String::from("EVIDENCE (ids only \u{2014} FETCH one to read it)\n");
            for e in &self.evidence {
                sec.push_str(&format!("- {}: {} ({})\n", e.id, e.summary, e.source));
            }
            push(sec, &mut s);
        }
        if !self.completed.is_empty() {
            push(format!("DONE\n- {}\n", self.completed.join("\n- ")), &mut s);
        }
        if !self.failures.is_empty() {
            push(format!("FAILED (do not repeat)\n- {}\n", self.failures.join("\n- ")), &mut s);
        }
        if !self.notes.is_empty() {
            push(format!("NOTES\n- {}\n", self.notes.join("\n- ")), &mut s);
        }
        s
    }
}

/// Two strings naming the same thing, tolerantly. Case and surrounding punctuation differ constantly
/// between a model's two mentions of one claim, and treating those as different claims is how a
/// findings count gets inflated by restatement.
fn same_claim(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim()
            .trim_end_matches(['.', '!', '?'])
            .to_lowercase()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    };
    norm(a) == norm(b)
}

fn first_line(s: &str, max: usize) -> String {
    s.lines().next().unwrap_or("").chars().take(max).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(action: &str) -> Observation {
        Observation { action: action.into(), ok: true, ..Default::default() }
    }

    fn ev(id: &str, summary: &str) -> Evidence {
        Evidence {
            id: id.into(),
            summary: summary.into(),
            source: "test".into(),
            body: "x".repeat(4000),
            captured_ms: 0,
        }
    }

    fn finding(claim: &str, evidence: &[&str]) -> Finding {
        Finding {
            claim: claim.into(),
            evidence: evidence.iter().map(|s| s.to_string()).collect(),
            addresses: Vec::new(),
            risk: None,
            rank: None,
        }
    }

    /// THE property the whole design rests on: the capsule does not grow with the number of steps.
    /// If this ever fails, the runtime has started keeping a transcript again.
    #[test]
    fn a_capsule_does_not_grow_with_step_count() {
        let mut c = Capsule::new("g", "find things");
        for i in 0..3 {
            c = c.reduce(Observation {
                action: format!("search:{i}"),
                ok: true,
                evidence: vec![ev(&format!("E{i}"), "a source")],
                did: Some(format!("searched {i}")),
                ..Default::default()
            });
        }
        // Measure once the caps are saturated and again much later. Comparing against a nearly-empty
        // capsule would only prove that filling up makes it bigger, which is not the claim — the
        // claim is that size CONVERGES, so step 60 costs what step 30 cost.
        for i in 3..30 {
            c = c.reduce(Observation {
                action: format!("search:{i}"),
                ok: true,
                evidence: vec![ev(&format!("E{i}"), "a source")],
                did: Some(format!("searched {i}")),
                ..Default::default()
            });
        }
        let at_30 = c.render(2000).len();

        for i in 30..120 {
            c = c.reduce(Observation {
                action: format!("search:{i}"),
                ok: true,
                evidence: vec![ev(&format!("E{i}"), "a source")],
                did: Some(format!("searched {i}")),
                ..Default::default()
            });
        }
        let at_120 = c.render(2000).len();
        assert_eq!(c.progress.steps, 120);
        assert!(at_120 <= 2000, "the render budget is a hard cap, got {at_120}");
        // Ninety further steps may change the CONTENT but must not change the SIZE.
        assert!(
            at_120 <= at_30 + 40,
            "capsule grew from {at_30} to {at_120} chars over 90 extra steps \u{2014} it is accumulating, not folding"
        );
        assert!(c.evidence.len() <= MAX_EVIDENCE, "evidence is capped, got {}", c.evidence.len());
        assert!(c.completed.len() <= MAX_COMPLETED);
        assert!(c.completed[0].contains("earlier steps"), "folded work is counted, not silently dropped");
    }

    /// Evidence bodies must never reach the capsule — that is what makes it small.
    #[test]
    fn evidence_is_held_by_reference_not_by_body() {
        let c = Capsule::new("g", "goal").reduce(Observation {
            action: "fetch".into(),
            ok: true,
            evidence: vec![ev("E1", "guidance raised")],
            ..Default::default()
        });
        let rendered = c.render(2000);
        assert!(rendered.contains("E1: guidance raised"), "the summary and id are shown");
        assert!(!rendered.contains(&"x".repeat(100)), "the 4KB body must not be in the capsule");
        assert!(!c.evidence[0].loaded, "nothing is paged in until something needs it");
    }

    /// Saying the same thing twice must not satisfy a findings count.
    #[test]
    fn a_restated_finding_merges_instead_of_counting_twice() {
        let c = Capsule::new("g", "goal")
            .reduce(Observation { action: "a".into(), ok: true, findings: vec![finding("XYZ volume is 4.3x average", &["E1"])], ..Default::default() })
            .reduce(Observation { action: "b".into(), ok: true, findings: vec![finding("xyz volume is 4.3x average.", &["E2"])], ..Default::default() });
        assert_eq!(c.findings.len(), 1, "one claim, however it was phrased");
        assert_eq!(c.findings[0].evidence, vec!["E1", "E2"], "but the support is the union");
    }

    /// Confidence is derived. A run with one thinly-sourced finding and a big open question cannot
    /// be highly confident, and no amount of narration changes that.
    #[test]
    fn confidence_is_derived_from_state_not_asserted() {
        let mut c = Capsule::new("g", "goal").reduce(Observation {
            action: "a".into(),
            ok: true,
            findings: vec![finding("thin claim", &["E1"])],
            uncertainties: vec![Uncertainty { question: "is it news-driven?".into(), importance: 0.9, confidence: 0.2, resolved: false }],
            ..Default::default()
        });
        assert!(c.confidence < 0.4, "one source + a big unknown is not confidence, got {}", c.confidence);

        // Resolve the question and add support: confidence rises because the STATE changed.
        c = c.reduce(Observation {
            action: "b".into(),
            ok: true,
            findings: vec![finding("thin claim", &["E2", "E3"])],
            uncertainties: vec![Uncertainty { question: "is it news-driven?".into(), importance: 0.9, confidence: 0.95, resolved: true }],
            ..Default::default()
        });
        assert!(c.confidence > 0.8, "three sources and nothing important open, got {}", c.confidence);

        // A contradiction knocks it down again.
        c.contradictions.push("two sources disagree on the volume figure".into());
        c.recompute_confidence();
        assert!(c.confidence < 0.8, "an open contradiction must reduce confidence");
    }

    /// The stall signal: steps that produce nothing are counted, so the controller can act without
    /// asking a model whether it is making progress.
    #[test]
    fn barren_steps_are_counted_and_reset_by_real_progress() {
        let mut c = Capsule::new("g", "goal");
        for i in 0..3 {
            c = c.reduce(obs(&format!("noop{i}")));
        }
        assert_eq!(c.progress.barren_steps, 3);

        c = c.reduce(Observation { action: "real".into(), ok: true, evidence: vec![ev("E9", "something")], ..Default::default() });
        assert_eq!(c.progress.barren_steps, 0, "new evidence resets the stall counter");

        // Resolving an uncertainty also counts as progress, even with no new evidence.
        c = c.reduce(Observation {
            action: "resolve".into(),
            ok: true,
            uncertainties: vec![Uncertainty { question: "q".into(), importance: 0.8, confidence: 0.9, resolved: true }],
            ..Default::default()
        });
        assert_eq!(c.progress.barren_steps, 0);
    }

    /// Repeat detection sees the whole history, so alternating between two dead ends is visible —
    /// the failure the old one-comparison loop guard could not catch.
    #[test]
    fn repeat_detection_catches_alternation_not_just_immediate_repeats() {
        let mut c = Capsule::new("g", "goal");
        for a in ["A", "B", "A", "B", "A"] {
            c = c.reduce(obs(a));
        }
        assert_eq!(c.attempts_of("A"), 3);
        assert_eq!(c.attempts_of("B"), 2);
        assert_eq!(c.distinct_attempts(), 2, "5 steps, 2 distinct actions \u{2014} going in circles");
    }

    /// A failure carries its reason forward, because "web_fetch: 502" changes the next action and
    /// "something went wrong" does not.
    #[test]
    fn failures_keep_their_reason() {
        let c = Capsule::new("g", "goal").reduce(Observation {
            action: "web_fetch".into(),
            ok: false,
            error: Some("502 Bad Gateway from upstream\nstack trace follows".into()),
            ..Default::default()
        });
        assert_eq!(c.progress.failures, 1);
        assert_eq!(c.failures[0], "web_fetch: 502 Bad Gateway from upstream");
        assert!(c.render(2000).contains("do not repeat"));
    }

    /// Next-action selection has an obvious answer: the most impactful thing still unknown.
    #[test]
    fn the_next_uncertainty_is_the_highest_impact_unknown() {
        let mut c = Capsule::new("g", "goal");
        c.uncertainties = vec![
            Uncertainty { question: "liquidity sufficient?".into(), importance: 0.7, confidence: 0.96, resolved: false },
            Uncertainty { question: "news-driven?".into(), importance: 0.9, confidence: 0.35, resolved: false },
            Uncertainty { question: "logo colour?".into(), importance: 0.1, confidence: 0.1, resolved: false },
        ];
        assert_eq!(c.next_uncertainty().unwrap().question, "news-driven?");
        // Nearly-known and unimportant questions are not worth an action.
        assert!(!c.uncertainties[0].is_worth_resolving(), "96% known is not worth a step");
        assert!(!c.uncertainties[2].is_worth_resolving(), "10% important is not worth a step");
    }

    /// The render budget is a hard cap, and the sections that survive are the ones a decision needs.
    #[test]
    fn render_respects_its_budget_and_keeps_what_decides() {
        let mut c = Capsule::new("g", "a goal that is being worked".repeat(2));
        for i in 0..40 {
            c = c.reduce(Observation {
                action: format!("s{i}"),
                ok: true,
                evidence: vec![ev(&format!("E{i}"), &format!("a fairly wordy evidence summary number {i}"))],
                notes: vec![format!("note {i} with some length to it")],
                did: Some(format!("did thing {i}")),
                ..Default::default()
            });
        }
        c.uncertainties.push(Uncertainty { question: "the critical unknown".into(), importance: 0.95, confidence: 0.1, resolved: false });
        let small = c.render(600);
        assert!(small.len() <= 600, "budget exceeded: {}", small.len());
        assert!(small.contains("GOAL"), "the goal always survives");
        assert!(small.contains("PROGRESS"), "progress always survives");
        // Even in a tight budget, what the next decision needs is present.
        assert!(small.contains("the critical unknown"), "open questions outrank evidence:\n{small}");
    }

    #[test]
    fn covers_matches_requirements_tolerantly() {
        let mut c = Capsule::new("g", "goal");
        let mut f = finding("liquidity is fine", &["E1"]);
        f.addresses = vec!["Sufficient Liquidity.".into()];
        c.findings.push(f);
        assert!(c.covers("sufficient liquidity"), "case and punctuation must not defeat coverage");
        assert!(!c.covers("identify downside/risk"));
    }
}
