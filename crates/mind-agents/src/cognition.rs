//! cognition — the bounded control loop.
//!
//! ```text
//! GOAL ─▶ [ CONTROL ─▶ NEXT BEST ACTION ─▶ EXECUTE ─▶ NORMALIZE ─▶ REDUCE ]* ─▶ VERIFY ─▶ SYNTHESIZE
//! ```
//!
//! # What is deliberately cheap
//!
//! CONTROL costs nothing — it is [`mind_spec::Controller`], pure arithmetic over the capsule, and it
//! runs FIRST on every iteration. Most of what a naive loop asks a model ("am I making progress?",
//! "have I tried this?", "should I stop?") is answered here for free, and the model is only consulted
//! when the answer is `Proceed`.
//!
//! NBA costs one small call with a ~2 KB prompt, because the capsule is the entire history the model
//! sees. EXECUTE and NORMALIZE cost no model at all.
//!
//! # Where the tokens go
//!
//! At the completion boundary, on purpose. Eight cheap decisions plus one strong verification and
//! synthesis beats twelve expensive deliberations and a hurried answer — the intelligence is spent
//! where it changes the output rather than where it narrates the process.
//!
//! # What this loop will not do
//!
//! It will not claim a check it did not run. If the grounding seam is unavailable, the answer is
//! marked unverified rather than passed by default. It will not finish because a model said it felt
//! done — [`mind_spec::CompletionCriteria`] decides. And it will not act outwardly without the
//! controller stopping for a human first, independently of the harm gate that also governs it.

use std::sync::Arc;

use mind_inference::InferencePool;
use mind_spec::capsule::{Capsule, Observation};
use mind_spec::control::{Controller, Decision, ReasonCode, StepOutcome};
use mind_spec::goal::{GoalSpec, Verdict};
use mind_types::clock::{Clock, UnixMillis};
use yantrik_ml::{ChatMessage, GenerationConfig};

use crate::bus::Bus;
use crate::nba::{self, Action, Verb};

/// One thing the loop did, for the trace. Reason codes rather than prose, so a run is queryable.
#[derive(Debug, Clone)]
pub struct Step {
    pub n: u32,
    pub action: String,
    pub ok: bool,
    /// The controller's decision that led here, when it was not a plain `Proceed`.
    pub decision: Option<ReasonCode>,
    pub elapsed_ms: u64,
}

/// How a run ended, and everything needed to render or audit it.
#[derive(Debug)]
pub struct Outcome {
    pub answer: String,
    pub capsule: Capsule,
    pub verdict: Verdict,
    /// Why the loop stopped, when it was not simply "the contract was met".
    pub stopped_because: Option<ReasonCode>,
    /// Did the grounding pass actually run and accept the answer?
    ///
    /// Three states, not two: an absent verifier is NOT a pass. A UI must be able to say "unverified"
    /// rather than implying a check that never happened.
    pub verified: Option<bool>,
    /// A question for the user, when the run stopped needing one.
    pub question: Option<String>,
    pub trace: Vec<Step>,
}

impl Outcome {
    /// Did this run answer the goal, as the contract defined it?
    pub fn complete(&self) -> bool {
        self.verdict.met
    }
}

pub struct Cognition {
    /// Cheap/fast lane for the loop's per-step decisions.
    step_pool: InferencePool,
    /// Strong lane for synthesis and for escalated steps.
    reason_pool: InferencePool,
    bus: Arc<dyn Bus>,
    controller: Controller,
    persona: String,
}

impl Cognition {
    pub fn new(
        step_pool: InferencePool,
        reason_pool: InferencePool,
        bus: Arc<dyn Bus>,
        persona: impl Into<String>,
    ) -> Self {
        Self { step_pool, reason_pool, bus, controller: Controller::default(), persona: persona.into() }
    }

    pub fn with_controller(mut self, controller: Controller) -> Self {
        self.controller = controller;
        self
    }

    /// Run a compiled goal to an answer.
    pub async fn run(&self, spec: &GoalSpec, clock: &dyn Clock) -> Outcome {
        let started: UnixMillis = clock.now_ms();
        let mut capsule = Capsule::new(&spec.id, &spec.goal);
        let mut trace: Vec<Step> = Vec::new();
        let mut next_evidence = 1u32;
        let mut escalated = false;
        let mut stopped_because = None;
        let mut question = None;

        loop {
            let elapsed = clock.now_ms().saturating_sub(started);
            let confidence_before = capsule.confidence;

            // ── CONTROL. Free, and first. ───────────────────────────────────────────────────────
            let last = StepOutcome { confidence_before, next_is_outward: false };
            match self.controller.decide(&capsule, &spec.contract, &spec.budget, elapsed, last) {
                Decision::Proceed => {}
                Decision::Escalate { reason } => {
                    // Not a failure — a considered move to a stronger tier for one decision.
                    escalated = true;
                    trace.push(Step { n: capsule.progress.steps, action: "escalate".into(), ok: true, decision: Some(reason), elapsed_ms: elapsed });
                }
                Decision::Replan { reason } => {
                    capsule.progress.replans += 1;
                    capsule.progress.barren_steps = 0; // the stall is being addressed; do not re-trigger on it
                    capsule.plan = self.replan(spec, &capsule).await;
                    capsule.progress.model_calls += 1;
                    trace.push(Step { n: capsule.progress.steps, action: "replan".into(), ok: true, decision: Some(reason), elapsed_ms: elapsed });
                    continue;
                }
                Decision::AskUser { reason, question: q } => {
                    stopped_because = Some(reason);
                    question = Some(q);
                    break;
                }
                Decision::FinishPartial { reason, .. } => {
                    stopped_because = Some(reason);
                    break;
                }
                Decision::Verify { reason } => {
                    stopped_because = Some(reason);
                    break;
                }
            }

            // ── NEXT BEST ACTION. One small call. ──────────────────────────────────────────────
            let verdict = spec.contract.completion.evaluate(&capsule, &spec.contract.requirements);
            let shortfalls: Vec<String> = verdict.shortfalls.iter().map(|s| s.describe()).collect();
            let pool = if escalated { &self.reason_pool } else { &self.step_pool };
            let choice = nba::choose(pool, self.bus.as_ref(), spec, &capsule, &shortfalls, escalated).await;
            capsule.progress.model_calls += 1;
            escalated = false; // escalation is per-decision, not sticky for the rest of the run

            let Some(choice) = choice else {
                // Nothing usable back. Answer with what exists rather than invent a step.
                stopped_because = Some(ReasonCode::NoProgress);
                break;
            };
            let action = choice.action;

            // ── Fold in what the last step established. ────────────────────────────────────────
            // This is what promotes evidence into findings, and without it the contract could never
            // be met however much the run gathered. Citations are checked against the evidence the
            // capsule actually holds, so the loop cannot manufacture its own support.
            if !choice.learned.is_empty() {
                let known: Vec<String> = capsule.evidence.iter().map(|e| e.id.clone()).collect();
                let contradictions = choice.learned.contradictions.clone();
                capsule = capsule.reduce(choice.learned.into_observation("extract".to_string(), &known));
                // Contradictions live in their own capsule field because the controller reads that
                // field to decide whether to escalate — leaving them in `notes` would make the
                // strongest reason to distrust a conclusion invisible to the thing that acts on it.
                for c in contradictions {
                    if !c.trim().is_empty() && !capsule.contradictions.iter().any(|x| x == &c) {
                        capsule.contradictions.push(c);
                    }
                }
                capsule.recompute_confidence();
                // Extraction is bookkeeping, not work: it must not consume the step budget or reset
                // the stall counter, or a run could look busy while going nowhere.
                capsule.progress.steps = capsule.progress.steps.saturating_sub(1);
            }

            // Re-test the contract now that findings may have landed — otherwise a run that just
            // satisfied itself would take one more pointless action before noticing.
            let verdict = spec.contract.completion.evaluate(&capsule, &spec.contract.requirements);
            if verdict.met && !matches!(action.verb, Verb::AskUser) {
                stopped_because = Some(ReasonCode::ContractMet);
                break;
            }

            // Terminal verbs the model may choose.
            match action.verb {
                Verb::Finish => {
                    // A model wanting to finish does not decide it. The contract does — and if it is
                    // unmet, saying so is more useful than accepting the claim.
                    if verdict.met {
                        stopped_because = Some(ReasonCode::ContractMet);
                        break;
                    }
                    // Treat a premature finish as a stalled step rather than obeying it.
                    capsule = capsule.reduce(Observation {
                        action: "premature_finish".into(),
                        ok: false,
                        error: Some(format!("wanted to finish with {} criteria unmet", verdict.shortfalls.len())),
                        ..Default::default()
                    });
                    continue;
                }
                Verb::AskUser => {
                    stopped_because = Some(ReasonCode::NeedsUserInput);
                    question = Some(if action.target.is_empty() { "I need something from you to continue.".into() } else { action.target.clone() });
                    break;
                }
                Verb::Replan => {
                    capsule.progress.replans += 1;
                    capsule.plan = self.replan(spec, &capsule).await;
                    capsule.progress.model_calls += 1;
                    continue;
                }
                Verb::Verify => {
                    // Asked for explicitly; the boundary check below does the work.
                    stopped_because = Some(ReasonCode::ContractMet);
                    break;
                }
                _ => {}
            }

            // ── The outward check, before acting. ──────────────────────────────────────────────
            // Asked here rather than in the controller because only now is the concrete tool known.
            // The harm gate governs the action itself; this is a second, independent stop.
            if action.verb == Verb::CallTool && self.bus.is_outward(&action.target) {
                stopped_because = Some(ReasonCode::HighConsequence);
                question = Some(format!(
                    "This would use {} to act outside the mind. Go ahead?",
                    action.target
                ));
                break;
            }

            // ── Dedup, free. ──────────────────────────────────────────────────────────────────
            let sig = action.signature();
            if self.controller.is_redundant(&capsule, &sig) {
                capsule = capsule.reduce(Observation {
                    action: sig,
                    ok: false,
                    error: Some("already tried; its result is already in state".into()),
                    ..Default::default()
                });
                continue;
            }

            // ── EXECUTE + NORMALIZE. No model. ────────────────────────────────────────────────
            let mut obs = self.execute(&action).await;
            // Evidence ids belong to the run, not the bus — so they are stable and citable.
            for e in obs.evidence.iter_mut() {
                if e.id.is_empty() {
                    e.id = format!("E{next_evidence}");
                    next_evidence += 1;
                }
            }
            let ok = obs.ok;
            let sig = obs.action.clone();
            capsule = capsule.reduce(obs);
            trace.push(Step {
                n: capsule.progress.steps,
                action: sig,
                ok,
                decision: None,
                elapsed_ms: clock.now_ms().saturating_sub(started),
            });
        }

        // ── The completion boundary: this is where the tokens go. ───────────────────────────────
        let verdict = spec.contract.completion.evaluate(&capsule, &spec.contract.requirements);
        let mut verified = None;
        let mut answer = if question.is_some() {
            question.clone().unwrap_or_default()
        } else {
            let synthesized = self.synthesize(spec, &capsule, &verdict).await;
            capsule.progress.model_calls += 1;
            synthesized
        };

        // Grounding runs only when there is evidence to ground against — running it over an empty
        // capsule would strip an honest "I could not find out" down to nothing.
        if question.is_none() && !capsule.evidence.is_empty() {
            let evidence = capsule
                .evidence
                .iter()
                .map(|e| format!("{}: {} ({})", e.id, e.summary, e.source))
                .collect::<Vec<_>>()
                .join("\n");
            match self.bus.ground(&spec.goal, &evidence).await {
                Some(grounded) if !grounded.trim().is_empty() => {
                    answer = grounded;
                    verified = Some(true);
                }
                Some(_) => verified = Some(false), // it ran and left nothing standing
                None => verified = None,           // no verifier — NOT a pass
            }
        }

        Outcome { answer, capsule, verdict, stopped_because, verified, question, trace }
    }

    /// Perform one action through the bus.
    async fn execute(&self, action: &Action) -> Observation {
        let (tool, args) = match action.verb {
            Verb::CallTool => (action.target.clone(), action.args.clone()),
            Verb::RecallMemory => ("recall".to_string(), serde_json::json!({ "query": action.target })),
            // Paging in an evidence body is the bus's `fetch`, addressed by id.
            Verb::Fetch => ("fetch".to_string(), serde_json::json!({ "id": action.target })),
            _ => return Observation { action: action.signature(), ok: false, error: Some("not an executable action".into()), ..Default::default() },
        };
        match self.bus.call(&tool, &args).await {
            Ok(raw) => self.bus.normalize(&tool, &args, &raw, true),
            Err(e) => self.bus.normalize(&tool, &args, &e.to_string(), false),
        }
    }

    /// Rebuild the short plan. Rolling horizon — a few actions, not twenty.
    async fn replan(&self, spec: &GoalSpec, capsule: &Capsule) -> Vec<String> {
        let prompt = format!(
            "{state}\n\nThe approach so far is not working. Propose the next {n} actions ONLY, as a \
             JSON array of short strings. Do not repeat anything under FAILED. No prose.",
            state = capsule.render(1500),
            n = spec.horizon.clamp(1, 4),
        );
        let cfg = GenerationConfig { max_tokens: 250, think: mind_inference::think_for("replan", Some(false)), prefer_reasoner: true, ..GenerationConfig::default() };
        let text = match self
            .reason_pool
            .chat_grounded(vec![ChatMessage::system("Output ONLY a JSON array of short strings."), ChatMessage::user(&prompt)], cfg)
            .await
        {
            Ok(r) => r.text,
            Err(_) => return Vec::new(),
        };
        let body = text.rsplit("</think>").next().unwrap_or(&text);
        match (body.find('['), body.rfind(']')) {
            (Some(a), Some(b)) if b > a => serde_json::from_str::<Vec<String>>(&body[a..=b]).unwrap_or_default(),
            _ => Vec::new(),
        }
        .into_iter()
        .take(4)
        .collect()
    }

    /// The one strong call: turn the capsule into the answer the output contract asks for.
    async fn synthesize(&self, spec: &GoalSpec, capsule: &Capsule, verdict: &Verdict) -> String {
        let out = &spec.contract.output;
        let mut shape = Vec::new();
        if out.ranked {
            shape.push("Rank the findings, best first.");
        }
        if out.show_evidence {
            shape.push("Cite the evidence id behind each claim.");
        }
        if out.include_risks {
            shape.push("State the downside or risk of each.");
        }
        if out.include_confidence {
            shape.push("End with how confident you are and what is still unknown.");
        }
        // A partial answer must SAY it is partial. An answer that reads complete when it is not is the
        // single most damaging thing this loop could produce.
        let disclosure = if verdict.met {
            String::new()
        } else {
            format!(
                "\n\nThis run did not fully meet its own criteria. Say so plainly, near the start, in \
                 one sentence — do not bury it. What is missing: {}",
                verdict.shortfalls.iter().map(|s| s.describe()).collect::<Vec<_>>().join("; ")
            )
        };

        let prompt = format!(
            "{state}\n\nWrite the answer to the goal above.\n{shape}{format}{disclosure}\n\n\
             Use ONLY what is in the state above. Every claim must trace to a finding or an evidence \
             summary — if the state does not support something, leave it out. Do not describe the \
             process; give the result.",
            state = capsule.render(2500),
            shape = shape.join(" "),
            format = out.format.as_deref().map(|f| format!(" Format: {f}.")).unwrap_or_default(),
        );
        let cfg = GenerationConfig { max_tokens: 2000, think: mind_inference::think_for("synthesize", None), prefer_reasoner: true, ..GenerationConfig::default() };
        self.reason_pool
            .chat_grounded(vec![ChatMessage::system(&self.persona), ChatMessage::user(&prompt)], cfg)
            .await
            .map(|r| r.text.trim().to_string())
            .unwrap_or_else(|_| "I did the work but could not put the answer together.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::tests_support::FakeBus;
    use mind_spec::goal::{Budget, CompletionCriteria, Contract, OutputContract};
    use mind_types::clock::TestClock;
    use yantrik_ml::LLMBackend;

    fn pools(replies: Vec<&str>) -> (InferencePool, InferencePool, Arc<mind_inference::SequencedLLM>) {
        let backend = Arc::new(mind_inference::SequencedLLM::new(replies));
        let p = InferencePool::new(backend.clone() as Arc<dyn LLMBackend>, 1);
        (p.clone(), p, backend)
    }

    fn goal(min_findings: usize) -> GoalSpec {
        GoalSpec {
            contract: Contract {
                requirements: vec![],
                completion: CompletionCriteria { min_findings, require_full_coverage: false, ..Default::default() },
                output: OutputContract::default(),
            },
            budget: Budget { max_steps: 12, max_model_calls: 12, max_wall_ms: 600_000, max_usd: None },
            ..GoalSpec::simple("find the thing")
        }
    }

    fn call(tool: &str, q: &str) -> String {
        format!(r#"{{"verb":"CALL_TOOL","target":"{tool}","args":{{"query":"{q}"}},"why":"NEED_EVIDENCE"}}"#)
    }

    /// A reply that reports what the previous step established AND chooses to finish. This is the
    /// realistic shape: the same call does the state update and the decision.
    fn learned_then_finish(claim: &str, ev: &str) -> String {
        format!(
            r#"{{"learned":{{"findings":[{{"claim":"{claim}","evidence":["{ev}"]}}]}},"verb":"FINISH","why":"SUFFICIENT"}}"#
        )
    }

    /// The happy path, and the property that matters: the model is called a SMALL number of times,
    /// and the loop reaches a real answer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_run_gathers_evidence_then_answers() {
        let f = learned_then_finish("the thing is X", "E1");
        let (step, reason, backend) = pools(vec![&call("search", "the thing"), &f, "The thing is X, per E1."]);
        let bus = Arc::new(FakeBus::new(&["search"]).returning("search", "The thing is X"));
        let c = Cognition::new(step, reason, bus.clone(), "JARVIS");
        let out = c.run(&goal(1), &TestClock::new(0)).await;

        assert!(out.complete(), "one evidenced finding should meet a min_findings=1 contract: {:?}", out.verdict.shortfalls);
        assert_eq!(out.stopped_because, Some(ReasonCode::ContractMet));
        assert_eq!(bus.called(), vec!["search|{\"query\":\"the thing\"}"], "exactly one tool call");
        assert!(backend.call_count() <= 3, "2 decisions + 1 synthesis, got {}", backend.call_count());
        assert!(out.answer.contains('X'));
    }

    /// A model wanting to finish early does NOT get to decide. The contract does.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_premature_finish_is_refused_not_obeyed() {
        let f = learned_then_finish("a real finding", "E1");
        let (step, reason, _) = pools(vec![
            r#"{"verb":"FINISH","why":"SUFFICIENT"}"#, // step 1: wants out with 0 findings
            &call("search", "again"),                  // refused, so it must actually work
            &f,                                        // now it reports a finding, and the contract is met
            "Answer with evidence.",
        ]);
        let bus = Arc::new(FakeBus::new(&["search"]).returning("search", "a real finding"));
        let out = Cognition::new(step, reason, bus.clone(), "JARVIS").run(&goal(1), &TestClock::new(0)).await;

        assert_eq!(bus.called().len(), 1, "the refused finish forced actual work");
        assert!(out.capsule.failures.iter().any(|f| f.contains("premature_finish")), "{:?}", out.capsule.failures);
        assert!(out.complete());
    }

    /// An outward tool stops for a human BEFORE it runs. Nothing is called.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_outward_action_stops_and_asks_without_running() {
        let (step, reason, _) = pools(vec![&call("send_email", "x")]);
        let bus = Arc::new(FakeBus::new(&["send_email"]).returning("send_email", "sent!").outward(&["send_email"]));
        let out = Cognition::new(step, reason, bus.clone(), "JARVIS").run(&goal(1), &TestClock::new(0)).await;

        assert_eq!(out.stopped_because, Some(ReasonCode::HighConsequence));
        assert!(out.question.as_deref().unwrap().contains("send_email"));
        assert!(bus.called().is_empty(), "an outward action must NOT have run before asking");
    }

    /// The time limit binds without the step limit, and reports its own reason.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_clock_stops_a_run_and_says_it_was_the_clock() {
        let (step, reason, _) = pools(vec![&call("search", "a"), "partial answer"]);
        let bus = Arc::new(FakeBus::new(&["search"]).returning("search", "something"));
        let mut g = goal(9); // unreachable, so only a limit can stop it
        // Zero, so the ceiling is already reached at the first control check. `elapsed` is measured
        // from the clock read at entry, so pre-setting a TestClock proves nothing — it moves the start
        // line too. (That was the bug in the first version of this test.)
        g.budget.max_wall_ms = 0;
        let out = Cognition::new(step, reason, bus, "JARVIS").run(&g, &TestClock::new(0)).await;

        assert_eq!(out.stopped_because, Some(ReasonCode::Timeout));
        assert!(!out.complete());
    }

    /// A partial answer must SAY it is partial — the synthesis prompt has to carry the disclosure.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_partial_run_is_told_to_disclose_it() {
        let (step, reason, backend) = pools(vec!["the answer"]);
        let bus = Arc::new(FakeBus::new(&["search"]));
        let mut g = goal(5);
        g.budget.max_steps = 0; // stop immediately, nothing found
        let out = Cognition::new(step, reason, bus, "JARVIS").run(&g, &TestClock::new(0)).await;

        assert_eq!(out.stopped_because, Some(ReasonCode::StepBudget));
        let synth = backend.prompt_at(0);
        assert!(synth.contains("did not fully meet its own criteria"), "{synth}");
        assert!(synth.contains("0 of 5 findings"), "the specific shortfall must be named:\n{synth}");
        assert!(out.verified.is_none(), "nothing was verified, so it must not read as verified");
    }

    /// An absent verifier is NOT a pass. This is the distinction a UI needs to avoid implying a check
    /// that never ran.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn verification_has_three_states_and_absent_is_not_pass() {
        // No grounding seam configured.
        let f = learned_then_finish("found a thing", "E1");
        let (s1, r1, _) = pools(vec![&call("search", "a"), &f, "answer"]);
        let bus1 = Arc::new(FakeBus::new(&["search"]).returning("search", "found"));
        let out1 = Cognition::new(s1, r1, bus1, "JARVIS").run(&goal(1), &TestClock::new(0)).await;
        assert_eq!(out1.verified, None, "no verifier means UNVERIFIED, never verified");

        // A grounding seam that accepts.
        let (s2, r2, _) = pools(vec![&call("search", "a"), &f, "answer"]);
        let bus2 = Arc::new(FakeBus::new(&["search"]).returning("search", "found").grounding("grounded answer"));
        let out2 = Cognition::new(s2, r2, bus2, "JARVIS").run(&goal(1), &TestClock::new(0)).await;
        assert_eq!(out2.verified, Some(true));
        assert_eq!(out2.answer, "grounded answer", "the grounded text replaces the draft");

        // A grounding seam that strips everything: the answer did not survive its own check.
        let (s3, r3, _) = pools(vec![&call("search", "a"), &f, "answer"]);
        let bus3 = Arc::new(FakeBus::new(&["search"]).returning("search", "found").grounding("   "));
        let out3 = Cognition::new(s3, r3, bus3, "JARVIS").run(&goal(1), &TestClock::new(0)).await;
        assert_eq!(out3.verified, Some(false));
    }

    /// A repeated call is caught by the runtime, at no model cost, and the tool is not called twice.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_repeated_call_is_refused_without_hitting_the_tool_again() {
        let same = call("search", "identical");
        let (step, reason, _) = pools(vec![&same, &same, &same, &same, "answer"]);
        let bus = Arc::new(FakeBus::new(&["search"]).returning("search", "same thing"));
        let mut g = goal(9); // never satisfiable, so the loop keeps trying
        g.budget.max_steps = 6;
        let out = Cognition::new(step, reason, bus.clone(), "JARVIS").run(&g, &TestClock::new(0)).await;

        assert_eq!(bus.called().len(), 2, "the 3rd+ identical call must never reach the tool: {:?}", bus.called());
        assert!(out.capsule.failures.iter().any(|f| f.contains("already tried")), "{:?}", out.capsule.failures);
    }

    /// Nothing usable from the model means answer with what we have — never an invented action.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_unusable_decision_ends_the_run_rather_than_guessing() {
        let (step, reason, _) = pools(vec!["I'm not sure what to do here!", "partial"]);
        let bus = Arc::new(FakeBus::new(&["search"]));
        let out = Cognition::new(step, reason, bus.clone(), "JARVIS").run(&goal(1), &TestClock::new(0)).await;
        assert_eq!(out.stopped_because, Some(ReasonCode::NoProgress));
        assert!(bus.called().is_empty(), "no tool should have been invented");
    }

    /// A failing tool does not wedge the loop: the failure enters the capsule with its reason, and the
    /// run ends on a limit rather than spinning.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failing_tool_is_recorded_and_the_run_still_ends() {
        let (step, reason, _) = pools(vec![
            &call("missing_tool", "a"), &call("missing_tool", "b"), &call("missing_tool", "c"),
            &call("missing_tool", "d"), &call("missing_tool", "e"), &call("missing_tool", "f"),
            "partial answer",
        ]);
        let bus = Arc::new(FakeBus::new(&["search"])); // every call fails
        let mut g = goal(3);
        g.budget.max_steps = 10;
        let out = Cognition::new(step, reason, bus, "JARVIS").run(&g, &TestClock::new(0)).await;

        assert!(out.capsule.progress.failures > 0);
        assert!(out.capsule.failures.iter().any(|f| f.contains("no such tool")), "{:?}", out.capsule.failures);
        assert!(out.stopped_because.is_some(), "it must stop for a stated reason");
        assert!(!out.complete());
    }

    /// Evidence ids are assigned by the RUN, so they are stable and citable — a bus must not invent
    /// them, and the ids must actually appear on the findings' evidence lists.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_run_assigns_stable_evidence_ids() {
        let f = learned_then_finish("two sources agree", "E2");
        let (step, reason, _) = pools(vec![&call("search", "a"), &call("news", "b"), &f, "answer"]);
        let bus = Arc::new(
            FakeBus::new(&["search", "news"]).returning("search", "first source").returning("news", "second source"),
        );
        let out = Cognition::new(step, reason, bus, "JARVIS").run(&goal(1), &TestClock::new(0)).await;
        let ids: Vec<&str> = out.capsule.evidence.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, vec!["E1", "E2"], "ids are sequential and owned by the run");
    }

    /// THE ECONOMIC CLAIM, measured.
    ///
    /// A classic loop threads its transcript back on every step, so step 20 carries everything from
    /// steps 1–19 and the prompt grows without bound. This asserts the opposite: over a long run, the
    /// prompt the model sees stays flat. If this ever fails, the runtime has started keeping a
    /// transcript again and every other argument in this crate is void.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_prompt_stays_flat_over_a_long_run() {
        // Twenty distinct tool calls, each producing a substantial body, then an answer.
        let mut replies: Vec<String> = (0..20).map(|i| call("search", &format!("query number {i}"))).collect();
        replies.push("the answer".to_string());
        let refs: Vec<&str> = replies.iter().map(|s| s.as_str()).collect();
        let (step, reason, backend) = pools(refs);

        let bus = Arc::new(FakeBus::new(&["search"]).returning(
            "search",
            // A realistic tool result: a headline plus a lot of body.
            &format!("A finding worth noting\n{}", "supporting detail. ".repeat(600)),
        ));
        let mut g = goal(99); // unreachable, so the run uses its whole step budget
        g.budget.max_steps = 20;
        g.budget.max_model_calls = 40;
        let out = Cognition::new(step, reason, bus.clone(), "JARVIS").run(&g, &TestClock::new(0)).await;

        assert_eq!(bus.called().len(), 20, "the run really did twenty steps");
        assert_eq!(out.stopped_because, Some(ReasonCode::StepBudget));

        let first = backend.prompt_at(0).len();
        let last = backend.prompt_at(19).len();
        assert!(
            last <= first + 1200,
            "prompt grew from {first} to {last} bytes over 20 steps \u{2014} the runtime is accumulating \
             history instead of folding it"
        );
        // And no tool body ever reached the model.
        for i in 0..20 {
            assert!(
                !backend.prompt_at(i).contains("supporting detail. supporting detail."),
                "step {i} leaked a raw tool body into the prompt"
            );
        }
    }

    /// A contradiction must reach the capsule field the CONTROLLER reads, not just the notes — the
    /// controller escalates on that field, so a contradiction parked anywhere else is invisible to the
    /// thing that acts on it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_reported_contradiction_reaches_the_controller() {
        let clash = r#"{"learned":{"contradictions":["two sources disagree on the volume figure"]},
            "verb":"CALL_TOOL","target":"search","args":{"query":"reconcile"},"why":"RECONCILE"}"#;
        let (step, reason, _) = pools(vec![&call("search", "first"), clash, &call("news", "second"), "answer"]);
        let bus = Arc::new(FakeBus::new(&["search", "news"]).returning("search", "a").returning("news", "b"));
        let mut g = goal(9);
        g.budget.max_steps = 4;
        let out = Cognition::new(step, reason, bus, "JARVIS").run(&g, &TestClock::new(0)).await;

        assert_eq!(out.capsule.contradictions.len(), 1, "the clash must be where the controller looks");
        assert!(out.trace.iter().any(|s| s.decision == Some(ReasonCode::Contradiction)), "and must have escalated: {:?}", out.trace);
    }

    /// The trace records what happened, with reason codes — the observability substrate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_trace_records_steps_and_decisions() {
        let f = learned_then_finish("a finding", "E1");
        let (step, reason, _) = pools(vec![&call("search", "a"), &f, "answer"]);
        let bus = Arc::new(FakeBus::new(&["search"]).returning("search", "found"));
        let out = Cognition::new(step, reason, bus, "JARVIS").run(&goal(1), &TestClock::new(0)).await;
        assert_eq!(out.trace.len(), 1);
        assert!(out.trace[0].action.starts_with("search"));
        assert!(out.trace[0].ok);
    }
}
