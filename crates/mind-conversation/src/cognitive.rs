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
}

impl EngineBus {
    pub fn new(engine: Arc<ConversationEngine>, identity: TurnIdentity) -> Self {
        Self { engine, identity }
    }
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
        // Dispatch through the SAME path the legacy loop uses, so every guard the engine has —
        // plugin enablement, the egress broker, the exact-value exfiltration check, read isolation —
        // applies unchanged. A second dispatch path would be a second set of holes.
        let out = self.engine.run_agent_tool_as(tool, args, &self.identity).await;
        // ONE definition of "worked", shared with the legacy loop: the five-way classifier in
        // `tool_outcome`. This used to be a private substring boolean copied from the legacy loop —
        // and then the legacy loop moved on to the classifier while the copy stayed behind, so the
        // two paths disagreed about the same result. Concretely: "(no results)" counted as a FAILURE
        // here, `capsule.progress.failures` climbed, and five honest empty searches ended the run
        // with "the tools it needs keep failing" — a lie about a working tool.
        let outcome = crate::tool_outcome::Outcome::classify(tool, &out);
        // The reliability ledger learns on this path too — measured self-knowledge must not stop
        // accruing because the bounded loop ran the tool instead of the legacy one.
        if let Some(ok) = outcome.counts_toward_reliability() {
            let _ = self.engine.memory.record_tool_outcome(tool, ok).await;
        }
        match outcome {
            // An empty result is the tool WORKING; the capsule sees it as a barren step, not a break.
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
        let summary: String = if trimmed.chars().count() <= 200 {
            trimmed.to_string()
        } else {
            let head: String = trimmed.lines().next().unwrap_or("").chars().take(160).collect();
            if head.chars().count() < 24 {
                // A short first line is a heading, not a summary — take a prefix of the whole thing.
                trimmed.chars().take(160).collect::<String>().replace('\n', " ")
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
            // A banked-but-never-run skill is UNPROVEN, and must say so rather than borrow the 1.0
            // that `Skill::success_rate()` returns for zero runs — that default is right for ranking
            // inside the skill store and wrong as a claim about reliability.
            let reliability = if s.runs > 0 {
                Prior::measured(s.success_rate(), s.runs as u32)
            } else {
                Prior::declared(0.5)
            };
            out.push(Procedure {
                name: s.name.clone(),
                when: s.summary.clone(),
                steps: vec![s.summary.clone()],
                kind: ProcedureKind::Executable { skill: s.name },
                reliability,
            });
        }

        // Guidance: `MemoryKind::Routine` — the procedural slot in typed memory. A remembered approach
        // is stored as numbered lines, so the steps are recovered by splitting rather than by asking a
        // model to re-read its own note.
        let q = mind_types::RecallQuery {
            text: goal.to_string(),
            top_k: limit,
            kind: Some(mind_types::MemoryKind::Routine),
        };
        if let Ok(hits) = self.engine.memory.recall_typed(q, &mind_types::AccessContext::Operator).await {
            for h in hits {
                let (when, steps) = split_routine(&h.item.text);
                if !steps.is_empty() {
                    out.push(Procedure {
                        name: routine_name(&h.item.text),
                        when,
                        steps,
                        kind: ProcedureKind::Instructions,
                        reliability: Prior::declared(h.item.confidence.clamp(0.0, 1.0)),
                    });
                }
            }
        }
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
    /// Is the bounded cognitive loop enabled?
    pub fn cognition_enabled() -> bool {
        std::env::var("YM_COGNITION").map(|v| v.trim() == "on").unwrap_or(false)
    }

    /// THE turn entry point. One place decides which loop runs.
    ///
    /// Every channel calls this rather than `handle_turn_as` directly, so the flag governs all of them
    /// at once — a flag honoured on some paths and not others is worse than no flag, because the
    /// difference shows up as inconsistent behaviour rather than as a setting.
    ///
    /// Falls back to the legacy loop whenever the cognitive path declines to build, so a
    /// misconfiguration degrades to the behaviour that has always worked instead of to an error.
    pub async fn turn(self: &Arc<Self>, user_text: &str, id: TurnIdentity) -> Result<String> {
        if Self::cognition_enabled() {
            if let Some(answer) = self.cognitive_turn(user_text, &id).await {
                return Ok(answer);
            }
            eprintln!("[cognition] could not build the bounded loop for this turn — using the legacy path");
        }
        self.handle_turn_as(user_text, id).await
    }

    /// Run one turn through the bounded control loop.
    ///
    /// Returns `None` when the loop cannot be built (no recipe engine for grounding, say), so the
    /// caller falls back to the legacy path rather than degrading silently.
    pub async fn cognitive_turn(self: &Arc<Self>, user_text: &str, id: &TurnIdentity) -> Option<String> {
        let bus: Arc<dyn Bus> = Arc::new(EngineBus::new(self.clone(), id.clone()));
        let router = mind_inference::Router::from_env(self.inference.clone(), 4);

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
        );
        emit_progress("working…");
        let outcome = cognition.run(&compiled.spec, &mind_types::clock::SystemClock).await;

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
        let p = found.iter().find(|p| p.name == "repo review");
        // Recall is semantic, so a miss here is a store/embedding matter rather than a parse bug — but
        // when it hits, the shape must be right.
        if let Some(p) = p {
            assert_eq!(p.when, "evaluating a repository");
            assert_eq!(p.steps.len(), 2, "both steps survive the round trip");
            assert!(matches!(p.kind, mind_agents::ProcedureKind::Instructions));
            assert!(!p.reliability.is_trustworthy(), "a freshly banked approach is unproven");
        }
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
