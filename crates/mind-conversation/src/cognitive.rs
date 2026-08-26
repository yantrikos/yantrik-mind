//! cognitive — the production wiring for the bounded control loop.
//!
//! Two things live here, and nothing else. [`EngineBus`] implements `mind_agents::Bus` over the real
//! `ConversationEngine`, which is what lets the loop reach 92 tools without `mind-agents` depending on
//! this crate. And [`ConversationEngine::cognitive_turn`] runs a turn through it.
//!
//! # Behind a flag, deliberately
//!
//! The existing `agent_loop` remains primary. This runs only when `YM_COGNITION=on`, because "the new
//! loop is better" is a claim that should be settled by `mind-evals` scoring both against the same
//! scenarios rather than by whoever wrote it being confident. The old path is 100+ tests of accumulated
//! behaviour about this household; replacing it on assertion would be careless.

use std::sync::Arc;

use mind_agents::bus::{signature, Bus};
use mind_spec::capsule::{Evidence, Observation};
use serde_json::Value;

use super::*;

/// The capability bus over a live engine.
pub struct EngineBus {
    engine: Arc<ConversationEngine>,
    identity: TurnIdentity,
    /// The user's literal request, for the guard pipeline's egress checks. Empty when unknown
    /// (tests, tools-only use), which makes the guards STRICTER, never looser: nothing reads as
    /// "the user typed it".
    user_text: String,
    /// Per-turn guard-pipeline state — the same `guards::GuardState` the legacy loop keeps, so
    /// the unavailable-ban and the egress provenance behave identically on both paths.
    guard_state: std::sync::Mutex<crate::guards::GuardState>,
    /// The run's flight-recorder trace, declared by the loop before its first call. Tool
    /// prediction/observation events become spans UNDER this trace instead of orphans.
    trace_root: std::sync::Mutex<Option<String>>,
}

impl EngineBus {
    pub fn new(engine: Arc<ConversationEngine>, identity: TurnIdentity) -> Self {
        Self { engine, identity, user_text: String::new(), guard_state: std::sync::Mutex::new(Default::default()), trace_root: std::sync::Mutex::new(None) }
    }

    fn current_trace(&self) -> Option<String> {
        self.trace_root.lock().unwrap().clone()
    }

    /// Carry the user's literal request so per-call guards can distinguish "the user typed this
    /// value" from "the model injected it".
    pub fn for_turn(mut self, user_text: &str) -> Self {
        self.user_text = user_text.to_string();
        self
    }

    /// The tool's own measured success rate — the ONLY confidence this loop predicts with.
    /// The bandit stores a Beta(1,1)-smoothed posterior per tool (`alpha/(alpha+beta)`), so
    /// the mean is already shrunk toward 0.5 while observations are few — one honest prior,
    /// not two stacked ones. `n` rides along so events can declare how much history stands
    /// behind the number.
    async fn empirical_prior(&self, tool: &str) -> EmpiricalPrior {
        let row = self
            .engine
            .memory
            .tool_track_record()
            .await
            .ok()
            .and_then(|rows| rows.into_iter().find(|(t, _, _)| t == tool));
        match row {
            Some((_, rate, n)) if n > 0 => EmpiricalPrior { rate, n },
            _ => EmpiricalPrior { rate: 0.5, n: 0 }, // uninformed prior — honestly labeled in the event
        }
    }
}

/// Shrunken empirical prior for one tool, plus the sample count it came from.
struct EmpiricalPrior {
    rate: f64,
    n: u64,
}

#[async_trait::async_trait]
impl Bus for EngineBus {
    /// The relevance-gated catalog for this goal — the same one the legacy loop sees, so the two
    /// paths cannot disagree about what tools exist.
    fn catalog(&self, goal: &str) -> String {
        let src = self.engine.catalog_source();
        let (detailed, tail) = tool_catalog::gate_catalog(goal, &src);
        if tail.is_empty() {
            detailed
        } else {
            format!("{detailed}\n{tail}")
        }
    }

    /// Only capabilities whose backing client is actually present. This is what makes the compiler's
    /// refusal honest rather than a guess.
    fn ready_capabilities(&self) -> Vec<String> {
        self.engine
            .capability_report()
            .capabilities
            .into_iter()
            .filter(|c| matches!(c.availability, crate::surface::Availability::Ready))
            .map(|c| c.id)
            .collect()
    }

    /// Would this tool act outside the mind?
    ///
    /// Read from the registry's declared security level, not a name list — a capability marked
    /// `gated_write` is outward by declaration. The harm gate still governs the call itself; this is
    /// the second, independent stop, and it deliberately errs toward asking: an unknown tool counts as
    /// outward if the registry cannot vouch for it being read-only.
    fn is_outward(&self, tool: &str) -> bool {
        let reg = self.engine.plugins.lock().unwrap();
        match reg.security_for_tool(tool) {
            Some(crate::plugins::SecurityLevel::GatedWrite) => true,
            Some(_) => false,
            // A tool no capability claims is a core tool (recall/remember/now) — those are read-only
            // or self-directed, and the dispatch refuses anything it does not know.
            None => false,
        }
    }

    async fn call(&self, tool: &str, args: &Value) -> anyhow::Result<String> {
        // ── THE CLOSED LEARNING CHAIN, one tool call wide (Phase-2 §2). ────────────────────────
        // PREDICTION first — from the EMPIRICAL PRIOR only: the tool's own measured track record
        // (the bandit's Beta(1,1)-smoothed posterior). The model's confidence is NOT consulted
        // and not invented. Events are spans UNDER the run's declared trace (declare_trace), so
        // `ym why run-…` reconstructs which calls served which goal.
        let trace = self.current_trace().unwrap_or_else(|| format!("toolcall-{}", mind_observability::now_ms()));
        // THE ARGUMENT BOUNDARY, before prediction, before egress, and before anything derived from
        // the arguments is written: `signature` embeds the arguments, and the refusal exists
        // precisely to keep those out of the record — so a refused call carries a constant id. What
        // the boundary admits is the NORMALIZED value (content-block wrappers unwrapped, the OpenAI
        // string form parsed), and that is the only shape used from here on: the identity the
        // legacy loop's guards compare, the broker's input, the tool's input (Codex's review of P.2d).
        let args = match self.engine.admit_args(tool, args) {
            Ok(admitted) => admitted,
            Err(msg) => {
                self.engine.recorder().record({
                    let mut e = mind_observability::DecisionEvent::span(&trace, None, "tool_observed");
                    e.object_id = Some(format!("{tool}:malformed"));
                    e.outcome = Some(msg.chars().take(160).collect());
                    e.verdict = Some("malformed".into());
                    e.lesson = Some("malformed: excluded from reliability — the model's arguments did not fit the tool; the planner's failure, not the tool's".into());
                    e
                });
                anyhow::bail!("{msg}");
            }
        };
        let args = &args;
        let prior = self.empirical_prior(tool).await;
        let object_id = format!("{}:{}", tool, signature(tool, args));
        let predicted = {
            let mut e = mind_observability::DecisionEvent::span(&trace, None, "tool_predicted");
            e.object_id = Some(object_id.clone());
            e.goal = Some(self.user_text.chars().take(120).collect());
            e.chosen = Some(tool.to_string());
            e.predicted = Some(format!("tool {tool} returns usable output"));
            e.confidence = Some(prior.rate);
            e.policy = vec![format!("empirical prior n={}{}", prior.n, if prior.n < 5 { " (low-N shrinkage)" } else { "" })];
            let id = e.event_id.clone();
            self.engine.recorder().record(e);
            id
        };

        let clean = match crate::guards::pre(&self.engine, &self.guard_state, &self.identity, &self.user_text, tool, args.clone(), "bus").await {
            crate::guards::PreVerdict::Proceed(a) => a,
            crate::guards::PreVerdict::Refuse { msg, .. } => {
                // A refusal is the SAFETY machinery observed, not the tool observed: no
                // prediction error is computed (counts_toward_reliability is None for Denied),
                // but the chain still records what happened.
                self.engine.recorder().record({
                    let mut e = mind_observability::DecisionEvent::span(&trace, predicted.as_deref(), "tool_observed");
                    e.object_id = Some(object_id);
                    e.outcome = Some(msg.chars().take(160).collect());
                    e.verdict = Some("denied".into());
                    e.lesson = Some("refusal recorded; excluded from reliability by design — feeds P(permitted | context), not P(success)".into());
                    e
                });
                anyhow::bail!("{msg}");
            }
        };
        let out = self.engine.run_agent_tool_as(tool, &clean, &self.identity).await;
        // ONE definition of "worked": the five-way outcome, recorded and classified in `post`.
        // An empty result is the tool WORKING; the capsule sees it as a barren step, not a break.
        let verdict = crate::guards::post(&self.engine, &self.guard_state, tool, &out).await;
        // ── REAL OUTCOME + BRIER LOSS → the bandit update happens inside `post`; here we persist
        // the pair so calibration is auditable per call and bucketable by confidence later.
        {
            let success: Option<f64> = match verdict {
                crate::tool_outcome::Outcome::Ok | crate::tool_outcome::Outcome::Empty => Some(1.0),
                crate::tool_outcome::Outcome::Failed => Some(0.0),
                // Unavailable/Denied say nothing about whether the tool WOULD have worked — they
                // feed availability/permission learning, not capability accuracy.
                _ => None,
            };
            let semantic = match verdict {
                // Ok carried substance; Empty ran fine and found nothing — execution succeeded,
                // semantics did not. The distinction the three-success design asks for.
                crate::tool_outcome::Outcome::Ok => Some(true),
                crate::tool_outcome::Outcome::Empty => Some(false),
                _ => None,
            };
            let mut e = mind_observability::DecisionEvent::span(&trace, predicted.as_deref(), "tool_observed");
            e.object_id = Some(object_id);
            e.outcome = Some(out.chars().take(160).collect());
            e.verdict = Some(verdict.badge().into());
            e.semantic_success = semantic;
            match success {
                Some(s) => {
                    e.brier = Some((prior.rate - s).powi(2));
                    e.prediction_error = Some(s - prior.rate);
                    e.lesson = Some(match verdict {
                        crate::tool_outcome::Outcome::Failed =>
                            format!("prior said {:.2}, it broke — future estimate for {tool} drops", prior.rate),
                        crate::tool_outcome::Outcome::Empty =>
                            "ran fine, found nothing — execution held, semantics did not".into(),
                        _ => format!("estimate held within band (prior {:.2})", prior.rate),
                    });
                }
                None => {
                    e.lesson = Some(match verdict {
                        crate::tool_outcome::Outcome::Malformed => "malformed: excluded from reliability — the model's arguments did not fit the tool; the planner's failure, not the tool's".to_string(),
                        _ => format!("{}: excluded from reliability (capability gap or gate)", verdict.badge()),
                    });
                }
            }
            self.engine.recorder().record(e);
        }
        match verdict {
            crate::tool_outcome::Outcome::Ok | crate::tool_outcome::Outcome::Empty => Ok(out),
            _ => anyhow::bail!("{out}"),
        }
    }

    /// Shape a raw result into an observation.
    ///
    /// The one non-default behaviour that matters: a tool whose whole output IS the answer (a news
    /// brief, a published URL) keeps its text as the evidence summary rather than being reduced to its
    /// first line, because for those the first line is a heading and the substance is below it.
    fn normalize(&self, tool: &str, args: &Value, raw: &str, ok: bool) -> Observation {
        if !ok {
            // Carry the classifier's recovery hint with the failure, so the capsule's FAILED list
            // tells the next decision what KIND of dead end this was — "not configured" wants a
            // different route, a timeout wants one retry — instead of leaving the model to re-derive
            // that from the same words the classifier already read.
            let note = crate::tool_outcome::Outcome::classify(tool, raw).note();
            return Observation {
                action: signature(tool, args),
                ok: false,
                error: Some(format!("{}{note}", raw.chars().take(300).collect::<String>())),
                ..Default::default()
            };
        }
        // An honest empty answer is not evidence — promoting "(no results)" to an evidence ref would
        // reset the capsule's stall counter, making a run of fruitless searches read as progress and
        // hiding the very signal the controller replans on. It becomes a NOTE (context, not
        // conclusions) and the step stays barren.
        if crate::tool_outcome::Outcome::classify(tool, raw) == crate::tool_outcome::Outcome::Empty {
            return Observation {
                action: signature(tool, args),
                ok: true,
                notes: vec![format!("{tool} ran fine and found nothing — a different query or source may help")],
                did: Some(format!("used {tool} (found nothing)")),
                ..Default::default()
            };
        }
        let trimmed = raw.trim();
        // A one-line answer needs no summarizing; a long one gets its opening as the summary and keeps
        // the whole thing as the body for paging.
        //
        // 360 chars, not 160: the synthesis step sees ONLY these summaries unless the model pages a
        // body in, and at 160 a fetched page was reduced to its masthead — the live turn answered
        // "the source only shows the page title and a Sign in link" about a page whose tagline and
        // benchmark were sitting in the unread body. The capsule's render budget still caps the
        // total; a summary's job is to carry enough substance to answer from, not just to name.
        let summary: String = if trimmed.chars().count() <= 400 {
            trimmed.replace('\n', " ")
        } else {
            let head: String = trimmed.lines().next().unwrap_or("").chars().take(360).collect();
            if head.chars().count() < 80 {
                // A short first line is a heading, not a summary — take a prefix of the whole thing.
                trimmed.chars().take(360).collect::<String>().replace('\n', " ")
            } else {
                head
            }
        };
        Observation {
            action: signature(tool, args),
            ok: true,
            evidence: (!trimmed.is_empty())
                .then(|| {
                    vec![Evidence {
                        id: String::new(), // the run assigns ids
                        summary,
                        source: tool.to_string(),
                        body: trimmed.chars().take(20_000).collect(),
                        captured_ms: 0,
                    }]
                })
                .unwrap_or_default(),
            did: Some(format!("used {tool}")),
            ..Default::default()
        }
    }

    /// The real verifier: the recipe engine's ThinkCited→Validate, which strips uncited claims
    /// deterministically rather than asking a model whether it was truthful.
    async fn ground(&self, question: &str, evidence: &str) -> Option<String> {
        self.engine.recipes.as_ref()?.cited_answer(question, evidence).await
    }

    /// The engine's ONE terminal-delivery definition — the same list the legacy loop consults, so
    /// a published URL or delegation ack is delivered verbatim on both paths, never synthesized.
    fn is_terminal(&self, tool: &str, obs: &str) -> bool {
        self.engine.terminal_delivery(tool, obs)
    }

    /// The loop declares its run trace before acting; every tool span lands under it.
    fn declare_trace(&self, trace_id: &str) {
        *self.trace_root.lock().unwrap() = Some(trace_id.to_string());
    }

    /// Run-completion grading of the THIRD success kind: did this tool's evidence advance the
    /// goal? Recorded per tool under the run's trace; aggregated by `ym why contribution`.
    async fn grade_goal(&self, trace_id: &str, goal: &str, met: bool, contributors: &[(String, bool)]) {
        for (tool, contributed) in contributors {
            let mut g = mind_observability::DecisionEvent::span(trace_id, None, "tool_goal_graded");
            g.object_id = Some(format!("tool:{tool}"));
            g.goal = Some(goal.to_string());
            g.trigger = Some("run completion — contract evaluated".into());
            g.verdict = Some(if *contributed { "evidence_used" } else { "ran_unused" }.into());
            g.semantic_success = Some(*contributed);
            g.policy = vec![format!("goal_met={met}")];
            self.engine.recorder().record(g);
        }
    }

    /// Remembered approaches, from BOTH kinds of procedural memory this mind keeps.
    ///
    /// Banked skills carry real `runs`/`successes`, so their reliability is measured and the loop can
    /// prefer what works. Routine memories carry no outcome history, so they are declared and labelled
    /// as untested — the distinction is the whole reason `Prior` exists rather than a bare f64.
    async fn procedures(&self, goal: &str, limit: usize) -> Vec<mind_agents::Procedure> {
        use mind_agents::{Procedure, ProcedureKind};
        use mind_spec::Prior;
        let mut out = Vec::new();

        // Executable: the sandboxed skill bank. `recall_skills` already excludes quarantined ones.
        for s in self.engine.memory.recall_skills(goal, limit).await.unwrap_or_default() {
            // A banked-but-never-run skill is UNPROVEN. The guard that used to stand here is gone
            // because the type no longer hands out a rate it does not have: `rate()` is `None` at
            // zero runs, so the untested case cannot be forgotten (E.P5a).
            let reliability = match s.reliability().rate() {
                Some(rate) => Prior::measured(rate, s.runs as u32),
                None => Prior::declared(0.5),
            };
            out.push(Procedure {
                name: s.name.clone(),
                when: s.summary.clone(),
                steps: vec![s.summary.clone()],
                kind: ProcedureKind::Executable { skill: s.name },
                reliability,
            });
        }

        // MOUNTED-PACK CRAFT: a pack can teach the loop HOW to work, not just what is true. Pack
        // rows that parse as procedures (APPROACH:/WHEN:/numbered steps — the same shape banking
        // writes) join the candidates, labeled with their provenance: a publisher's claimed way of
        // working must never read as the household's own tested one. Reliability is DECLARED, not
        // measured — the local outcome ledger has never seen it run — so a proven local procedure
        // outranks it at selection, which is exactly right.
        if let Ok(hits) = self.engine.memory.recall_from_packs(goal, limit).await {
            for hit in hits {
                let text = hit.text;
                let (when, steps) = split_routine(&text);
                if steps.len() >= 2 {
                    out.push(Procedure {
                        name: routine_name(&text),
                        when: if when.is_empty() { format!("from pack {}", hit.pack_id) } else { format!("{when} [from pack {}]", hit.pack_id) },
                        steps,
                        kind: ProcedureKind::Instructions,
                        reliability: Prior::declared(0.5),
                    });
                }
            }
        }

        // Guidance: the mind's own BANKED approaches, enumerated deterministically and ranked by
        // word overlap with the goal. This used to go through `recall_typed`, which scores only
        // Belief-kind nodes — while banking writes episodic memories — so every approach the loop
        // ever banked was unreachable from the moment it was saved, and the library was
        // write-only. Enumeration + cheap overlap ranking is deliberately not semantic search:
        // the approach corpus is small (hundreds at most) and a deterministic read can never
        // silently lose the library again.
        let goal_words: std::collections::HashSet<String> = goal
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 4)
            .map(String::from)
            .collect();
        let mut ranked: Vec<(usize, Procedure)> = Vec::new();
        for t in self.engine.memory.list_approaches(200).await.unwrap_or_default() {
            let (when, steps) = split_routine(&t);
            if steps.is_empty() {
                continue;
            }
            let tl = t.to_lowercase();
            let overlap = goal_words.iter().filter(|w| tl.contains(w.as_str())).count();
            ranked.push((
                overlap,
                Procedure {
                    name: routine_name(&t),
                    when,
                    steps,
                    kind: ProcedureKind::Instructions,
                    reliability: Prior::declared(0.5),
                },
            ));
        }
        ranked.sort_by(|a, b| b.0.cmp(&a.0));
        out.extend(ranked.into_iter().take(limit).map(|(_, p)| p));
        out
    }

    /// A followed procedure earns or loses standing.
    ///
    /// Only executable skills have an outcome ledger today (`record_skill_outcome`, which
    /// auto-quarantines below half over four runs). A guidance procedure has nowhere to record to yet,
    /// so this is a no-op for it rather than a silent lie about being tracked.
    async fn record_procedure_outcome(&self, name: &str, ok: bool) {
        let _ = self.engine.memory.record_skill_outcome(name, ok).await;
    }

    /// Bank an approach that worked.
    ///
    /// Stored as a `Routine` observation — the procedural slot — rather than as a belief, because
    /// "how to do X" is not a claim about the world and should not be weighed against evidence the way
    /// a belief is.
    async fn bank_procedure(&self, name: &str, when: &str, steps: &[String]) -> bool {
        if steps.len() < 2 {
            // A one-step "approach" is not a procedure; remembering it would fill the library with
            // noise that then competes with real ones at recall time.
            return false;
        }
        let text = format!(
            "APPROACH: {name}\nWHEN: {when}\n{}",
            steps.iter().enumerate().map(|(i, s)| format!("{}. {s}", i + 1)).collect::<Vec<_>>().join("\n")
        );
        self.engine
            .memory
            .remember_observation(&text, mind_types::safety::ProvenanceCategory::SubAgent)
            .await
            .is_ok()
    }
}

/// Pull the "when" line and the numbered steps out of a stored routine.
fn split_routine(text: &str) -> (String, Vec<String>) {
    let mut when = String::new();
    let mut steps = Vec::new();
    for line in text.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("WHEN:").or_else(|| t.strip_prefix("when:")) {
            when = rest.trim().to_string();
            continue;
        }
        // A step is a numbered or bulleted line. Anything else is prose around the procedure.
        let step = t
            .split_once(". ")
            .filter(|(n, _)| n.chars().all(|c| c.is_ascii_digit()) && !n.is_empty())
            .map(|(_, s)| s)
            .or_else(|| t.strip_prefix("- "));
        if let Some(s) = step {
            if s.trim().len() > 2 {
                steps.push(s.trim().to_string());
            }
        }
    }
    (when, steps)
}

/// The stored routine's name, or a readable fallback.
fn routine_name(text: &str) -> String {
    for line in text.lines() {
        let t = line.trim();
        for tag in ["APPROACH:", "SKILL:", "PROCEDURE:"] {
            if let Some(rest) = t.strip_prefix(tag) {
                return rest.trim().to_string();
            }
        }
    }
    text.lines().next().unwrap_or("remembered approach").trim().chars().take(60).collect()
}

impl ConversationEngine {
    /// Is the bounded cognitive loop enabled? (env; per-engine override via `with_cognition`)
    pub fn cognition_enabled() -> bool {
        std::env::var("YM_COGNITION").map(|v| v.trim() == "on").unwrap_or(false)
    }

    /// The flag as THIS engine sees it — the test seam wins over the process env.
    pub(crate) fn cognition_on(&self) -> bool {
        self.cognition_force.unwrap_or_else(Self::cognition_enabled)
    }

    /// Record the tool-call learning chain for ONE call: the empirical prediction before, and the
    /// observed outcome after.
    ///
    /// Extracted so BOTH loops share one definition. The chain was written inside `EngineBus`, which
    /// only the bounded cognitive loop uses - and that loop is OFF by default (`YM_COGNITION`), so on
    /// the live box every prediction/observation event was dark while the classic loop carried every
    /// real turn. The bandit itself was never affected: it updates in `guards::post`, which both
    /// loops call, so reliability learning and discover_tools ranking were live throughout. What was
    /// missing was the RECORD of what had been predicted and how wrong it turned out - `ym why
    /// calibration` had nothing to read.
    ///
    /// Copying the block into the classic loop was the other option and the wrong one: two copies of
    /// a scoring rule drift, and then the calibration numbers depend on which loop happened to run.
    /// The tool's measured track record, on the ENGINE so both loops read the same number.
    ///
    /// The bandit stores a Beta(1,1)-smoothed posterior per tool, so the mean is already shrunk
    /// toward 0.5 while observations are few — one honest prior, never two stacked (an earlier
    /// version added a second shrinkage layer and got 5/9 where 2/3 was correct; its own test
    /// caught it).
    pub(crate) async fn empirical_prior_for(&self, tool: &str) -> (f64, u64) {
        let row = self
            .memory
            .tool_track_record()
            .await
            .ok()
            .and_then(|rows| rows.into_iter().find(|(t, _, _)| t == tool));
        match row {
            Some((_, rate, n)) if n > 0 => (rate, n),
            _ => (0.5, 0), // uninformed — labelled as such in the event
        }
    }

    pub(crate) fn record_tool_prediction(
        &self,
        trace: &str,
        tool: &str,
        goal: &str,
        prior_rate: f64,
        prior_n: u64,
        object_id: &str,
    ) -> Option<String> {
        let mut e = mind_observability::DecisionEvent::span(trace, None, "tool_predicted");
        e.object_id = Some(object_id.to_string());
        e.goal = Some(goal.chars().take(120).collect());
        e.chosen = Some(tool.to_string());
        e.predicted = Some(format!("tool {tool} returns usable output"));
        e.confidence = Some(prior_rate);
        e.policy = vec![format!(
            "empirical prior n={prior_n}{}",
            if prior_n < 5 { " (low-N shrinkage)" } else { "" }
        )];
        let id = e.event_id.clone();
        self.recorder().record(e);
        id
    }

    /// The observed half: five-way verdict, Brier loss and the lesson, parented to the prediction.
    pub(crate) fn record_tool_observation(
        &self,
        trace: &str,
        parent: Option<&str>,
        tool: &str,
        object_id: &str,
        verdict: crate::tool_outcome::Outcome,
        out: &str,
        prior_rate: f64,
    ) {
        use crate::tool_outcome::Outcome;
        // Unavailable/Denied say nothing about whether the tool WOULD have worked: they feed
        // availability and permission learning, never capability accuracy.
        let success: Option<f64> = match verdict {
            Outcome::Ok | Outcome::Empty => Some(1.0),
            Outcome::Failed => Some(0.0),
            _ => None,
        };
        // Ok carried substance; Empty ran fine and found nothing - execution succeeded, semantics
        // did not. That distinction is what stops "it ran" hardening into "it worked".
        let semantic = match verdict {
            Outcome::Ok => Some(true),
            Outcome::Empty => Some(false),
            _ => None,
        };
        let mut e = mind_observability::DecisionEvent::span(trace, parent, "tool_observed");
        e.object_id = Some(object_id.to_string());
        e.outcome = Some(out.chars().take(160).collect());
        e.verdict = Some(verdict.badge().into());
        e.semantic_success = semantic;
        match success {
            Some(s) => {
                e.brier = Some((prior_rate - s).powi(2));
                e.prediction_error = Some(s - prior_rate);
                e.lesson = Some(match verdict {
                    Outcome::Failed => format!("prior said {prior_rate:.2}, it broke - future estimate for {tool} drops"),
                    Outcome::Empty => "ran fine, found nothing - execution held, semantics did not".into(),
                    _ => format!("estimate held within band (prior {prior_rate:.2})"),
                });
            }
            None => {
                e.lesson = Some(match verdict {
                    Outcome::Malformed => "malformed: excluded from reliability — the model's arguments did not fit the tool; the planner's failure, not the tool's".to_string(),
                    _ => format!("{}: excluded from reliability (capability gap or gate)", verdict.badge()),
                });
            }
        }
        self.recorder().record(e);
    }


    /// THE turn entry point. Every channel calls this rather than `handle_turn_as` directly.
    ///
    /// The bounded loop is NOT dispatched here — it runs inside `handle_turn_as`, in the exact slot
    /// the classic loop occupies, AFTER the deterministic interceptors and with the same grounding.
    /// Its first live night proved why: preempting the whole chain sent "remember that…" to a
    /// tool-choosing model and answered a memory question from a stale belief. This function's jobs
    /// are the ones that belong to every turn regardless of loop: lending the engine handle the
    /// bus needs, and delivering held results.
    pub async fn turn(self: &Arc<Self>, user_text: &str, id: TurnIdentity) -> Result<String> {
        *self.self_ref.lock().unwrap() = Arc::downgrade(self);
        // Grade what was said LAST time by the shape of what arrived NOW — before this turn
        // overwrites it. Primary lane only: a correction is graded against the answer its own
        // conversation produced, and another member's message must not grade the owner's.
        if matches!(&id.viewer(), mind_types::Scope::Private(v) if v == mind_types::PRIMARY) {
            self.grade_previous_turn(user_text).await;
        }
        let answer = self.handle_turn_as(user_text, id.clone()).await;
        if let Ok(a) = &answer {
            if matches!(&id.viewer(), mind_types::Scope::Private(v) if v == mind_types::PRIMARY) {
                self.note_turn_answer(a).await;
            }
        }
        // FOLLOW-THROUGH, every channel: a delegated result that finished while no chat was
        // reachable is delivered on the very next exchange, appended after the answer — "also,
        // the thing you asked for is done." Primary-viewer only: a held result was produced for
        // the household's owner lane, and another member's next turn must not receive it.
        if matches!(&id.viewer(), mind_types::Scope::Private(v) if v == mind_types::PRIMARY) {
            let held = self.take_held_notes();
            if !held.is_empty() {
                let mut a = answer?;
                a.push_str("\n\n— finished while you were away —\n");
                a.push_str(&held.join("\n\n"));
                // Same credential rule as below — held results are answers too.
                return Ok(crate::redact::redact_answer(&a));
            }
        }
        // THE ANSWER'S display rule: personal values pass (asking for them is what the mind is
        // for); CREDENTIAL-shaped values never render in chat, asked or not — a key's home is the
        // env file and the masked settings row. The transcript stored the true text inside the
        // loop, before this edge; memory is never corrupted by display masking.
        answer.map(|a| crate::redact::redact_answer(&a))
    }

    /// Run one turn through the bounded control loop.
    ///
    /// Returns `None` when the loop cannot be built (no recipe engine for grounding, say), so the
    /// caller falls back to the legacy path rather than degrading silently.
    pub async fn cognitive_turn(self: &Arc<Self>, user_text: &str, id: &TurnIdentity) -> Option<String> {
        // The run's trace id is minted inside Cognition::run (and declared to the bus, so tool
        // spans parent under it); the compile-time refusal below still mints its own.
        let trace_id = format!("run-{}", mind_observability::now_ms());
        let bus: Arc<dyn Bus> = Arc::new(EngineBus::new(self.clone(), id.clone()).for_turn(user_text));
        let router = mind_inference::Router::from_env(self.inference.clone(), 4);

        // The SAME grounding assembly the classic loop uses — one function, two loops, zero drift.
        emit_progress("grounding from memory…");
        let grounding = self.turn_grounding(user_text, id, &trace_id).await;

        emit_progress("understanding the goal…");
        let compiled = mind_agents::compile(
            &router.pool("util"),
            bus.as_ref(),
            user_text,
            crate::config_panel::agent_budget(),
        )
        .await;

        // A goal needing something this mind does not have is said plainly, before any work. The old
        // loop would have improvised around the gap and reported something that sounded like progress.
        if !compiled.spec.is_runnable() {
            self.recorder.record({
                let mut e = mind_observability::DecisionEvent::new(&trace_id, "cognitive_run_refused");
                e.actor = Some("cognition".into());
                e.subject = Some(id.owner.clone());
                e.goal = Some(user_text.to_string());
                e.trigger = Some("capability missing at compile time".into());
                e.rejected = compiled.spec.missing_capabilities.clone();
                e.outcome = Some("refused up front rather than improvising around the gap".into());
                e.verdict = Some("refused".into());
                e
            });
            return Some(format!(
                "{} Set it up and ask me again \u{2014} I did not want to guess around it.",
                compiled.notes.join(" ")
            ));
        }

        let cognition = mind_agents::Cognition::new(
            router.pool("chat"),
            router.pool("research"),
            bus,
            self.persona.clone(),
        )
        .with_grounding(grounding);
        emit_progress("working…");
        let outcome = cognition.run(&compiled.spec, &mind_types::clock::SystemClock).await;

        // ── FLIGHT RECORDER: the run's causal path from persisted state — what was known
        // (evidence ids), why it stopped (chosen), where it hit walls (failures), the budget
        // story (policy line), and derived confidence. Same trace the loop declared, so every
        // tool span this run caused sits beneath this completion event.
        {
            let mut e = mind_observability::DecisionEvent::new(&outcome.trace_id, "cognitive_run");
            e.actor = Some("cognition".into());
            e.subject = Some(id.owner.clone());
            e.goal = Some(compiled.spec.goal.clone());
            e.trigger = Some("interactive user request".into());
            e.evidence_ids = outcome.capsule.evidence.iter().map(|ev| ev.id.clone()).collect();
            e.chosen = Some(outcome.stopped_because.map(|r| r.describe().to_string()).unwrap_or_else(|| "completed".into()));
            e.rejected = outcome.capsule.failures.iter().take(4).cloned().collect();
            e.policy = vec![format!(
                "steps={} model_calls={} barren={} failures={}",
                outcome.capsule.progress.steps,
                outcome.capsule.progress.model_calls,
                outcome.capsule.progress.barren_steps,
                outcome.capsule.progress.failures
            )];
            e.confidence = Some(outcome.capsule.confidence);
            e.verdict = Some(match (outcome.complete(), outcome.verified) {
                (true, Some(true)) => "complete+verified".into(),
                (true, _) => "complete".into(),
                (_, Some(false)) => "partial+unverified".into(),
                (false, _) => "partial".into(),
            });
            e.lesson = outcome.capsule.contradictions.first().map(|c| format!("contradiction surfaced: {c}"));
            self.recorder.record(e);
            // GOAL CONTRIBUTION is graded by the run itself (Bus::grade_goal) — the run owns
            // its capsule and its contract verdict; the turn wrapper must not duplicate it.
        }

        // The trace is real execution, so it is safe to narrate — every line corresponds to a tool
        // call that happened.
        for step in &outcome.trace {
            emit_progress(&format!("{} {}", if step.ok { "\u{2713}" } else { "\u{2717}" }, step.action));
        }

        let mut answer = outcome.answer;
        // An unverified answer says so. The alternative — silence — reads as verified.
        if outcome.verified == Some(false) {
            answer.push_str("\n\n(I could not ground all of that in what I actually found.)");
        }
        if let Some(note) = compiled.notes.first() {
            answer.push_str(&format!("\n\n({note})"));
        }

        let _ = self.memory.append_message_scoped("user", user_text, id.write_scope()).await;
        let _ = self.memory.append_message_scoped("assistant", &answer, id.write_scope()).await;
        Some(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mind_memory::MemoryHandle;

    fn engine(mem: &MemoryHandle) -> Arc<ConversationEngine> {
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("ok")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        Arc::new(ConversationEngine::new(
            Arc::new(mem.clone()) as Arc<dyn MemoryFacade>,
            pool,
            "JARVIS",
        ))
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_bus_reports_only_really_available_capabilities() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());
        let ready = bus.ready_capabilities();
        // A bare engine has no clients, so anything requiring one must be absent — the same
        // false-green guard as the capability report itself.
        assert!(!ready.contains(&"github".to_string()), "github has no token here");
        assert!(!ready.contains(&"web_search".to_string()), "no searcher is wired");
        assert!(ready.contains(&"calculator".to_string()), "pure compute is always ready: {ready:?}");
    }

    /// A gated-write capability must read as outward, from the registry's declaration rather than a
    /// hardcoded list — so a new outward tool is protected the day it is declared.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn outwardness_comes_from_the_declared_security_level() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());
        // `code` belongs to the coder capability, declared GatedWrite.
        assert!(bus.is_outward("code"), "a gated-write tool is outward by declaration");
        assert!(!bus.is_outward("calc"), "arithmetic is not");
        assert!(!bus.is_outward("recall"), "a core read tool is not");
    }

    /// The bus must offer the SAME catalog the legacy loop sees, or the two paths disagree about what
    /// the mind can do.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_bus_catalog_matches_the_engines_own() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = engine(&mem);
        let bus = EngineBus::new(eng.clone(), TurnIdentity::primary());
        let cat = bus.catalog("what's the weather in pune?");
        assert!(cat.contains("weather"), "the relevant tool is detailed:\n{cat}");
        // Everything else stays reachable by name — the same never-remove rule as the legacy gate.
        assert!(cat.contains("OTHER TOOLS"), "the name-only tail must survive:\n{cat}");
    }

    /// ONE definition of "worked" across both loops: the bus classifies with the same five-way
    /// `tool_outcome` the legacy loop uses. The old private boolean here counted "(no results)" as
    /// a failure — so five honest empty searches killed a cognitive run with "tools keep failing".
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_empty_result_is_not_a_failure_on_the_cognitive_path() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());
        // discover_tools over a query nothing matches: the tool WORKED, the world was empty.
        let r = bus.call("discover_tools", &serde_json::json!({ "query": "zzqx warp drive" })).await;
        assert!(r.is_ok(), "an honest empty answer must not be classified as a break: {r:?}");
        // An unconfigured capability is still a dead end the run must not walk into.
        let r = bus.call("github_repo_items", &serde_json::json!({ "repo": "acme/x" })).await;
        assert!(r.is_err(), "an unavailable tool must surface as one");
        // A short correct answer is a result. The old boolean called anything ≤10 chars a failure.
        let r = bus.call("calc", &serde_json::json!({ "expr": "6*7" })).await;
        assert!(matches!(r.as_deref(), Ok(s) if s.contains("42")), "42 is an answer, not a failure: {r:?}");
    }

    /// An empty result folds into the capsule as a NOTE, never as evidence — evidence resets the
    /// stall counter, and a run of fruitless searches must stay visible as a stall.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_empty_result_stays_a_barren_step_in_the_capsule() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());
        let obs = bus.normalize("web_search", &serde_json::json!({"query":"x"}), "(no results for 'x')", true);
        assert!(obs.ok, "the tool worked");
        assert!(obs.evidence.is_empty(), "absence is not evidence");
        assert!(obs.notes[0].contains("found nothing"));
        let c = mind_spec::capsule::Capsule::new("g", "goal").reduce(obs);
        assert_eq!(c.progress.failures, 0, "no failure was invented");
        assert_eq!(c.progress.barren_steps, 1, "and the stall signal still sees the step");
    }

    /// A real failure keeps its recovery hint, so the FAILED list tells the next decision what kind
    /// of dead end it was.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_failure_observation_carries_the_recovery_hint() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());
        let obs = bus.normalize("github_repo_items", &serde_json::json!({}), "(github not configured)", false);
        assert!(!obs.ok);
        let err = obs.error.unwrap();
        assert!(err.contains("not available on this box"), "the reroute hint travels with the failure: {err}");
    }

    /// A long result keeps a useful summary; a heading-first result does not get reduced to its
    /// heading, because the substance is below it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn normalization_summarizes_without_losing_the_substance() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());

        let short = bus.normalize("now", &serde_json::json!({}), "Monday 11 August, 10:42", true);
        assert_eq!(short.evidence[0].summary, "Monday 11 August, 10:42", "a short answer IS its summary");

        let heading = format!("NEWS\nThe substantive first story is about {}", "x".repeat(400));
        let n = bus.normalize("news", &serde_json::json!({}), &heading, true);
        assert!(n.evidence[0].summary.contains("substantive"), "a 4-char heading must not become the summary: {}", n.evidence[0].summary);
        assert!(n.evidence[0].body.len() > 200, "the body keeps the whole thing for paging");
    }

    /// The bus re-runs the EXACT-VALUE EXFIL GUARD the legacy loop runs: a distinctive stored
    /// private value the model wrote into an external tool's args — that the user did not type —
    /// is refused before dispatch. The bounded loop was the one dispatch route that skipped it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_bus_refuses_a_model_injected_private_value() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let _ = mem
            .remember_as_belief(mind_types::BeliefAssertion {
                statement: "Pranab's private email is secret.owner@example.com".into(),
                polarity: 1.0,
                weight: 1.5,
                source_event: None,
                provenance: "told".into(),
            })
            .await;
        // The user asked about laptops; the model smuggled the stored email into the query.
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary()).for_turn("find me a good laptop");
        let r = bus.call("web_search", &serde_json::json!({ "query": "laptops for secret.owner@example.com" })).await;
        assert!(r.is_err(), "a stored private value the user never typed must not leave: {r:?}");

        // The user typing the value themselves is their call, not a model exfil.
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary())
            .for_turn("search the web for secret.owner@example.com");
        let r = bus.call("web_search", &serde_json::json!({ "query": "secret.owner@example.com" })).await;
        assert!(r.is_ok() || !format!("{r:?}").contains("private detail"), "user-typed values pass the guard: {r:?}");
    }

    /// The bus serves the ENGINE's terminal-delivery list — a published URL, a delegation ack, a
    /// rich brief all read as terminal through the bounded loop exactly as they do in the legacy
    /// loop, because it is literally the same function. A second list is how the classifier forked.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn terminal_delivery_is_one_definition_across_both_loops() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());
        assert!(bus.is_terminal("publish_page", "Done — published (works on your home network):\nhttp://192.168.4.90:8088/x.html"));
        assert!(bus.is_terminal("code", "On it — building \"a page\" in the background (isolated sandbox)"));
        assert!(bus.is_terminal("news", &format!("MORNING BRIEF\n{}", "story with sources. ".repeat(20))));
        assert!(!bus.is_terminal("publish_page", "(couldn't publish the page)"), "a failed publish is not an answer");
        assert!(!bus.is_terminal("news", "quiet day"), "a stub brief goes through synthesis like anything else");
        assert!(!bus.is_terminal("web_fetch", "http://example.com returned a page"), "an ordinary fetch is material, not an answer");
    }

    /// A stored routine round-trips: what it is for, and its steps in order.
    #[test]
    fn a_stored_routine_parses_back_into_a_procedure() {
        let text = "APPROACH: repo review\nWHEN: evaluating a repository\n\
                    1. read the README\n2. read the commit history\n3. check open issues";
        assert_eq!(routine_name(text), "repo review");
        let (when, steps) = split_routine(text);
        assert_eq!(when, "evaluating a repository");
        assert_eq!(steps, vec!["read the README", "read the commit history", "check open issues"]);
    }

    /// Bulleted steps are as valid as numbered ones — a procedure written by hand should not be lost
    /// to a formatting preference.
    #[test]
    fn bulleted_routines_parse_too() {
        let (_, steps) = split_routine("PROCEDURE: x\n- first thing\n- second thing");
        assert_eq!(steps, vec!["first thing", "second thing"]);
        assert_eq!(routine_name("PROCEDURE: x\n- a\n- b"), "x");
    }

    /// Prose that merely mentions a procedure is NOT one. Without this, ordinary memories would be
    /// recalled as approaches and the loop would follow a paragraph as if it were a plan.
    #[test]
    fn prose_without_steps_is_not_a_procedure() {
        let (_, steps) = split_routine("Pranab prefers concise answers and dislikes bullet lists.");
        assert!(steps.is_empty(), "a sentence is not an approach");
    }

    /// Banking guards against noise: a single-step "approach" would compete with real procedures at
    /// recall time while carrying no reusable reasoning.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_one_step_approach_is_not_worth_banking() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());
        assert!(!bus.bank_procedure("trivial", "when x", &["did one thing".into()]).await);
        assert!(bus.bank_procedure("real", "when x", &["step one".into(), "step two".into()]).await);
    }

    /// A banked approach is recallable as a procedure — the round trip through real memory, which is
    /// what makes the library compound across runs rather than only within one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_banked_approach_comes_back_as_a_procedure() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let bus = EngineBus::new(engine(&mem), TurnIdentity::primary());
        assert!(
            bus.bank_procedure(
                "repo review",
                "evaluating a repository",
                &["read the README".into(), "read the commit history".into()],
            )
            .await
        );
        let found = bus.procedures("how should I evaluate this repository?", 5).await;
        // UNCONDITIONAL, deliberately. This assertion used to sit behind an `if let` excusing a
        // semantic-recall miss — which is exactly how the library being WRITE-ONLY went unnoticed:
        // banking wrote episodic memories, recall read only beliefs, the test shrugged at the
        // permanent miss, and every banked approach was unreachable. The read is deterministic
        // enumeration now, so a miss IS a bug.
        let p = found
            .iter()
            .find(|p| p.name == "repo review")
            .expect("a banked approach must come back — the library was write-only once already");
        assert_eq!(p.when, "evaluating a repository");
        assert_eq!(p.steps.len(), 2, "both steps survive the round trip");
        assert!(matches!(p.kind, mind_agents::ProcedureKind::Instructions));
        assert!(!p.reliability.is_trustworthy(), "a freshly banked approach is unproven");
    }

    /// FOLLOW-THROUGH: a result that finished while no chat was reachable is delivered appended to
    /// the very next exchange — and exactly once. This is what makes "I'll send the result here
    /// when it's done" true on channels the notify loop cannot reach.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_held_result_is_delivered_on_the_next_turn_exactly_once() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = engine(&mem);
        eng.hold_for_next_turn("🛠️ Code — your page is ready: http://192.168.4.90:8088/x.html");

        let a = eng.turn("thanks, and what else?", TurnIdentity::primary()).await.unwrap();
        assert!(a.contains("finished while you were away"), "{a}");
        assert!(a.contains("your page is ready"), "{a}");

        let b = eng.turn("ok", TurnIdentity::primary()).await.unwrap();
        assert!(!b.contains("your page is ready"), "a held note must deliver exactly once: {b}");
    }

    /// A held result is the PRIMARY lane's. Another household member's next turn must not
    /// receive it — and must not consume it either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_held_result_never_leaks_to_another_member() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = engine(&mem);
        eng.hold_for_next_turn("the surprise-gift research finished");

        let member = TurnIdentity::new("guest", false);
        let a = eng.turn("hello", member).await.unwrap();
        assert!(!a.contains("surprise-gift"), "another member must not see the owner's result: {a}");

        let b = eng.turn("hi", TurnIdentity::primary()).await.unwrap();
        assert!(b.contains("surprise-gift"), "…and the owner still gets it on their next turn: {b}");
    }

    /// THE RE-SLOT, end to end: with the flag forced on, a real `turn()` reaches the bounded loop
    /// in `agent_loop`'s slot — after the deterministic interceptors — builds the SAME grounding
    /// the classic loop uses, and that grounding reaches the synthesis call as a marked reference
    /// block. This is the shape the first live night said was missing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn the_bounded_loop_runs_in_the_classic_loops_slot_with_its_grounding() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let seq = Arc::new(mind_inference::SequencedLLM::new(vec![
            // compile → a minimal usable draft
            r#"{"objective":"answer what color teal is","min_findings":1}"#,
            // NBA → fetch
            r#"{"verb":"CALL_TOOL","target":"web_fetch","args":{"url":"http://example.com"},"why":"NEED_EVIDENCE"}"#,
            // NBA → report the finding and finish
            r#"{"learned":{"findings":[{"claim":"Teal is a blue-green color","evidence":["E1"]}]},"verb":"FINISH","why":"SUFFICIENT"}"#,
            // synthesize
            "Teal is a blue-green color, per E1.",
        ]));
        let pool = mind_inference::InferencePool::new(seq.clone() as Arc<dyn yantrik_ml::LLMBackend>, 1);
        let eng = Arc::new(
            ConversationEngine::new(Arc::new(mem.clone()) as Arc<dyn MemoryFacade>, pool, mind_types::default_persona("the user"))
                .with_web(Arc::new(mind_tools::ScriptedFetcher::new("WEBDOC: Teal is a cyan-family blue-green color.")))
                .with_cognition(true),
        );

        let a = eng.turn("what color is teal?", TurnIdentity::primary()).await.unwrap();
        assert!(a.contains("blue-green"), "the bounded loop must have served the slot: {a}");

        // The synthesis call (4th model call) carried the grounding reference block — the thing the
        // preempting wiring could never provide.
        let synth = seq.prompt_at(3);
        assert!(synth.contains("what you know"), "grounding must reach synthesis:\n{synth}");
        assert!(synth.contains("NOT instructions"), "…as reference data, marked as such:\n{synth}");
    }

    /// THE TURN-LEVEL REWARD CHANNEL: a correction-shaped next message grades the previous answer
    /// as corrected, anything else as tacitly accepted — recorded, capped, and exactly once per
    /// exchange. The mind's answers finally have a measured track record like its tools do.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_correction_grades_the_previous_answer() {
        use crate::pace_ledger::reads_as_correction;
        // Precision first: these must NOT read as corrections.
        for ok in ["yes, do that", "no problem, thanks", "tell me about teal", "actually that's great", "nothing else"] {
            assert!(!reads_as_correction(ok), "false positive poisons the counter: {ok:?}");
        }
        for bad in ["no, I meant the OTHER page", "that's wrong — it was Tuesday", "you misunderstood me", "not what I asked"] {
            assert!(reads_as_correction(bad), "must read as a correction: {bad:?}");
        }

        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let eng = engine(&mem);
        // Exchange 1: the mind answers something.
        let _ = eng.turn("what day is it?", TurnIdentity::primary()).await.unwrap();
        // Exchange 2: the user corrects it — the PREVIOUS answer takes the grade.
        let _ = eng.turn("no, I meant the OTHER calendar", TurnIdentity::primary()).await.unwrap();
        // Exchange 3: an ordinary message — tacit acceptance of exchange 2's answer.
        let _ = eng.turn("thanks", TurnIdentity::primary()).await.unwrap();

        let g: serde_json::Value =
            serde_json::from_str(&mem.profile_get("turn_grades").await.unwrap().unwrap()).unwrap();
        assert_eq!(g["corrected"].as_u64(), Some(1), "{g}");
        assert_eq!(g["accepted"].as_u64(), Some(1), "exchange 3 tacitly accepted exchange 2: {g}");
        let recent = g["recent"].as_array().unwrap();
        assert_eq!(recent.len(), 1);
        assert!(recent[0]["correction"].as_str().unwrap().contains("OTHER calendar"));
        assert!(!recent[0]["answer"].as_str().unwrap().is_empty(), "what was corrected is kept beside how");
    }

    /// The flag is off unless explicitly turned on: the legacy loop stays primary until evals say
    /// otherwise, not until someone feels ready.
    #[test]
    fn cognition_is_off_by_default() {
        let prev = std::env::var("YM_COGNITION").ok();
        std::env::remove_var("YM_COGNITION");
        assert!(!ConversationEngine::cognition_enabled());
        std::env::set_var("YM_COGNITION", "off");
        assert!(!ConversationEngine::cognition_enabled());
        std::env::set_var("YM_COGNITION", "on");
        assert!(ConversationEngine::cognition_enabled());
        match prev {
            Some(v) => std::env::set_var("YM_COGNITION", v),
            None => std::env::remove_var("YM_COGNITION"),
        }
    }
}
 
#[cfg(test)]
mod learning_chain_tests {
    use super::*;
    use mind_memory::MemoryHandle;

    /// THE FIRST CLOSED LEARNING CHAIN, one tool call wide, proven end to end:
    /// predict (empirical prior only) → act → observe (five-way) → prediction error → lesson.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_tool_call_leaves_a_predict_observe_pair_with_its_error() {
        let p = mind_types::scratch::file("chain_calc", "jsonl");

        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("ok")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let engine = Arc::new(
            ConversationEngine::new(
                Arc::new(mem.clone()) as Arc<dyn MemoryFacade>,
                pool,
                mind_types::default_persona("the user"),
            )
            .with_recorder(Arc::new(mind_observability::DecisionLog::open(&p))),
        );
        let bus = EngineBus::new(engine.clone(), TurnIdentity::primary());
        // The loop declares its run trace; every tool span must land UNDER it as children.
        Bus::declare_trace(&bus, "run-test");

        // CALL 1: pure compute succeeds. No history exists yet → the prior is the honest
        // uninformed 0.5, labeled low-N in the event.
        let r = Bus::call(&bus, "calc", &serde_json::json!({ "expr": "6*7" })).await;
        assert!(r.is_ok());
        let events = mind_observability::read_events(&p);
        assert_eq!(events.len(), 2, "predict + observe per call: {events:?}");
        assert_eq!(events[0].kind, "tool_predicted");
        assert_eq!(events[0].trace_id, "run-test", "spans root under the declared run trace");
        assert!(events[0].event_id.is_some());
        assert_eq!(events[0].confidence, Some(0.5), "uninformed prior before any data");
        assert!(events[0].policy[0].contains("n=0"), "sample size is stated, not hidden");
        assert_eq!(events[1].kind, "tool_observed");
        assert_eq!(events[1].trace_id, "run-test");
        assert_eq!(
            events[1].parent_event_id.as_deref(),
            events[0].event_id.as_deref(),
            "the observation parents to ITS prediction — a causal pair, not two labels"
        );
        assert_eq!(events[1].verdict.as_deref(), Some("ok"));
        assert_eq!(events[1].semantic_success, Some(true));
        assert_eq!(events[1].brier, Some(0.25), "(0.5 − 1)² — the calibration metric, not just signed error");
        assert_eq!(events[1].prediction_error, Some(0.5), "success minus 0.5 prior");

        // CALL 2: same tool again — the bandit now holds ONE success, so the empirical prior
        // must have MOVED (shrunken): (1*1+1)/(1+2) = 2/3.
        let _ = Bus::call(&bus, "calc", &serde_json::json!({ "expr": "6*8" })).await;
        let events = mind_observability::read_events(&p);
        let pred2 = events.iter().rev().find(|e| e.kind == "tool_predicted").unwrap();
        assert!(
            (pred2.confidence.unwrap() - 2.0 / 3.0).abs() < 1e-9,
            "the prior must learn from observed history: got {}", pred2.confidence.unwrap()
        );
        assert!(pred2.policy[0].contains("n=1"), "sample size travels with the number");
        assert!(pred2.policy[0].contains("low-N"), "one observation is still declared small");

        // CHAIN INTEGRITY on disk through real traffic.
        assert_eq!(mind_observability::verify_log(&p), Ok(events.len()));
        let _ = std::fs::remove_file(&p);
    }

    /// A capability gap is OBSERVED but does not count as a wrong prediction: Unavailable is
    /// excluded from reliability by design, so there is no error number to grade — the event
    /// says why instead.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_unavailable_capability_records_no_false_prediction_error() {
        let p = mind_types::scratch::file("chain_unavail", "jsonl");

        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("ok")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let engine = Arc::new(
            ConversationEngine::new(
                Arc::new(mem.clone()) as Arc<dyn MemoryFacade>,
                pool,
                mind_types::default_persona("the user"),
            )
            .with_recorder(Arc::new(mind_observability::DecisionLog::open(&p))),
        );
        let bus = EngineBus::new(engine, TurnIdentity::primary());

        let r = Bus::call(&bus, "github_repo_items", &serde_json::json!({ "repo": "acme/x" })).await;
        assert!(r.is_err(), "unconfigured capability must surface as a dead end");

        let events = mind_observability::read_events(&p);
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].verdict.as_deref(), Some("unavailable"));
        assert_eq!(events[1].prediction_error, None, "no error number without a meaningful outcome class");
        assert!(events[1].lesson.as_ref().unwrap().contains("excluded from reliability"));
        let _ = std::fs::remove_file(&p);
    }
}
 
#[cfg(test)]
mod goal_contribution_tests {
    use super::*;
    use mind_agents::Cognition;
    use mind_memory::MemoryHandle;
    use mind_spec::goal::GoalSpec;

    /// THE THIRD SUCCESS KIND, graded end to end: a run whose finding CITED the fetch's
    /// evidence marks that tool `contributed`; the contract verdict is the goal-level outcome.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_completed_run_grades_its_tools_goal_contribution() {
        let p = mind_types::scratch::file("contrib", "jsonl");

        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let seq = Arc::new(mind_inference::SequencedLLM::new(vec![
            r#"{"verb":"CALL_TOOL","target":"web_fetch","args":{"url":"http://example.com"},"why":"NEED_EVIDENCE"}"#.to_string(),
            r#"{"learned":{"findings":[{"claim":"Teal is a blue-green color","evidence":["E1"]}]},"verb":"FINISH","why":"SUFFICIENT"}"#.to_string(),
            "Teal is a blue-green color, per E1.".to_string(),
        ]));
        let pool = mind_inference::InferencePool::new(seq.clone() as Arc<dyn yantrik_ml::LLMBackend>, 1);
        let engine = Arc::new(
            ConversationEngine::new(
                Arc::new(mem.clone()) as Arc<dyn MemoryFacade>,
                pool.clone(),
                mind_types::default_persona("the user"),
            )
            .with_recorder(Arc::new(mind_observability::DecisionLog::open(&p)))
            .with_web(Arc::new(mind_tools::ScriptedFetcher::new(
                "WEBDOC: Teal is a cyan-family blue-green color.",
            ))),
        );
        let bus: Arc<dyn Bus> = Arc::new(EngineBus::new(engine, TurnIdentity::primary()).for_turn("what color is teal?"));
        let spec = GoalSpec {
            contract: mind_spec::goal::Contract {
                requirements: vec![],
                completion: mind_spec::goal::CompletionCriteria { min_findings: 1, require_full_coverage: false, ..Default::default() },
                output: mind_spec::goal::OutputContract::default(),
            },
            budget: mind_spec::goal::Budget { max_steps: 6, max_model_calls: 12, max_wall_ms: 60_000, max_usd: None },
            ..GoalSpec::simple("what color is teal?")
        };
        let cognition = Cognition::new(pool.clone(), pool, bus, "JARVIS");
        let out = cognition.run(&spec, &mind_types::clock::SystemClock).await;
        assert!(out.complete(), "the scenario must meet its contract for grading to mean anything");

        // The completion event plus one goal grade per evidence-producing tool (web_fetch).
        let events = mind_observability::read_events(&p);
        let grades: Vec<_> = events.iter().filter(|e| e.kind == "tool_goal_graded").collect();
        assert_eq!(grades.len(), 1, "one producing tool, one grade: {grades:?}");
        assert_eq!(grades[0].object_id.as_deref(), Some("tool:web_fetch"));
        assert_eq!(grades[0].verdict.as_deref(), Some("evidence_used"), "its evidence was cited by the finding");
        assert!(grades[0].policy.iter().any(|l| l.contains("goal_met=true")));
        assert_eq!(grades[0].trace_id, out.trace_id, "the grade lives under the same run trace");

        // The report turns the ledger into the richer capability sentence.
        let report = mind_observability::render_goal_contribution(&events);
        assert!(report.contains("web_fetch"), "{report}");
        assert!(report.contains("too few runs to rank") || report.contains("1/1"), "young numbers are declared young: {report}");
        assert_eq!(mind_observability::verify_log(&p), Ok(events.len()));
        let _ = std::fs::remove_file(&p);
    }

    /// P.2e (Codex's review of P.2d): on the bounded loop's bus the boundary runs before ANYTHING is
    /// derived from the arguments — no prediction, no object id built from the raw call — and it
    /// judges the normalized form. Every recorded event is serialised whole and searched for the
    /// sentinel: a leak through any field fails, not just through `outcome`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_malformed_call_on_the_bus_is_refused_before_prediction_and_no_field_carries_a_value() {
        let dir = mind_types::scratch::dir("p2e_bus");
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("bus.decisions.jsonl");
        let _ = std::fs::remove_file(&log);
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("ok")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let eng = Arc::new(
            ConversationEngine::new(Arc::new(mem.clone()) as Arc<dyn MemoryFacade>, pool, "JARVIS")
                .with_recorder(Arc::new(mind_observability::DecisionLog::open(&log))),
        );
        let bus = EngineBus::new(eng.clone(), TurnIdentity::primary());
        // The sentinel is a PIN-shaped number the model might have copied from the person.
        let r = bus.call("run_skill", &serde_json::json!({ "name": 447193, "target": 447193 })).await;
        assert!(matches!(r.as_deref(), Err(e) if e.to_string().starts_with("(malformed call")), "{r:?}");
        let r = bus.call("discover_tools", &serde_json::json!({ "query": null })).await;
        assert!(r.is_err(), "{r:?}");
        let r = bus.call("run_skill", &serde_json::json!({ "target": "https://example.org/x.csv" })).await;
        assert!(matches!(r.as_deref(), Err(e) if e.to_string().contains("missing required name")), "{r:?}");
        // Normalized BEFORE the boundary: a content-block name is a name, and the tool runs on it.
        let r = bus.call("run_skill", &serde_json::json!({ "name": [{ "type": "text", "content": "csv-clean" }] })).await;
        assert!(!format!("{r:?}").contains("malformed call"), "a content-block name is a name: {r:?}");

        let events = eng.recorder().read_all();
        let predicted: Vec<_> = events.iter().filter(|e| e.kind == "tool_predicted").collect();
        let malformed: Vec<_> = events.iter().filter(|e| e.kind == "tool_observed" && e.verdict.as_deref() == Some("malformed")).collect();
        assert_eq!(malformed.len(), 3, "{events:?}");
        assert_eq!(predicted.len(), 1, "only the call that could be made was predicted: {events:?}");
        assert!(predicted[0].object_id.as_deref().unwrap_or("").starts_with("run_skill"), "{predicted:?}");
        assert!(malformed.iter().all(|e| matches!(e.object_id.as_deref(), Some("run_skill:malformed") | Some("discover_tools:malformed"))), "a refused call carries a constant id: {malformed:?}");
        for e in &events {
            let s = serde_json::to_string(e).unwrap();
            assert!(!s.contains("447193"), "the sentinel reached the record through some field: {s}");
            assert!(!s.contains("example.org"), "a value reached the record: {s}");
        }
        let track = mem.tool_track_record().await.unwrap();
        assert!(!track.iter().any(|(t, _, _)| t == "discover_tools"), "a tool that never ran must not be on the record: {track:?}");
        let _ = std::fs::remove_file(&log);
    }
}
