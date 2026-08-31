//! compile — the Intent Compiler. One sentence in, a runnable contract out.
//!
//! # What it is for
//!
//! "Find me the best stocks today" contains three demands the user did not write down: how many
//! candidates count as an answer, that each needs supporting evidence, and that the downside matters.
//! Left implicit, the run finishes whenever the model produces something that reads like a reply. The
//! compiler makes them explicit so [`mind_spec::CompletionCriteria`] can test them.
//!
//! # One model call, and it may fail
//!
//! Compilation is a single structured call and every part of it is optional. A model that returns
//! nothing usable does not block the goal — it degrades to `GoalSpec::simple`, which asks for one
//! evidenced finding and nothing else. Refusing to run because the compiler had a bad day would be
//! the worst possible trade: the user asked a question.
//!
//! # Capabilities are resolved HERE
//!
//! Against the bus's real availability, not against a guess. A goal that needs GitHub on a mind with
//! no token comes back with `missing_capabilities` populated, so the caller can say so in a sentence
//! instead of discovering it as a tool error at step four and improvising around it.

use std::collections::HashSet;

use mind_inference::InferencePool;
use mind_spec::goal::{Budget, CompletionCriteria, Contract, GoalSpec, OutputContract, Risk};
use serde::Deserialize;
use yantrik_ml::{ChatMessage, GenerationConfig};

use crate::bus::Bus;

/// What the model is asked for. Every field optional and defaulted — a partial answer is still
/// useful, and a strict schema would turn a small mistake into a total failure.
#[derive(Debug, Default, Deserialize)]
struct Draft {
    #[serde(default)]
    objective: String,
    #[serde(default)]
    constraints: Vec<String>,
    #[serde(default)]
    requirements: Vec<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    #[serde(default)]
    min_findings: Option<usize>,
    #[serde(default)]
    max_findings: Option<usize>,
    #[serde(default)]
    min_evidence: Option<usize>,
    #[serde(default)]
    needs_risk: Option<bool>,
    #[serde(default)]
    ranked: Option<bool>,
    #[serde(default)]
    outward: Option<bool>,
    #[serde(default)]
    format: Option<String>,
}

/// How the compiler arrived at a spec — surfaced so the UI can show a compiled goal differently from
/// a fallback, and so nobody mistakes a degraded compile for a considered contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// The model produced a usable draft.
    Compiled,
    /// The model failed or returned nothing usable; this is the minimal honest contract.
    Fallback,
}

pub struct Compilation {
    pub spec: GoalSpec,
    pub origin: Origin,
    /// What to tell the user before running, if anything. Populated when a capability is missing or a
    /// requested bound was adjusted.
    pub notes: Vec<String>,
}

const PROMPT: &str = "\
You are compiling a user's request into a CONTRACT that a runtime will test. You are not answering it.

Return ONLY this JSON object:
{\"objective\":\"one line, the user's actual goal\",
 \"constraints\":[\"limits they stated\"],
 \"requirements\":[\"things an acceptable answer MUST address\"],
 \"capabilities\":[\"ids from the AVAILABLE list this needs\"],
 \"min_findings\":1,\"max_findings\":null,\"min_evidence\":1,
 \"needs_risk\":false,\"ranked\":false,\"outward\":false,\"format\":null}

Rules:
- requirements are TESTABLE things, not steps. \"identify the downside\" yes; \"search the web\" no.
- min_findings: how many results would actually answer them. A single fact question is 1. \
\"the top candidates\" is 3+. Do not inflate it.
- min_evidence: 1 normally. 2 when being wrong would matter (money, health, an outward action).
- needs_risk: true when the answer would be irresponsible without stating the downside.
- outward: true ONLY if the request asks to send, post, buy, or change something.
- capabilities: choose ONLY from the AVAILABLE list. Name nothing that is not on it.
- If the request is a simple question, say so with empty requirements and min_findings 1. \
An over-specified contract makes a simple question unanswerable.";

/// Compile a request into a contract.
pub async fn compile(
    pool: &InferencePool,
    bus: &dyn Bus,
    request: &str,
    budget: Budget,
) -> Compilation {
    let available = bus.ready_capabilities();
    let prompt = format!(
        "{PROMPT}\n\nAVAILABLE CAPABILITIES: [{}]\n\nREQUEST: {request}",
        available.join(", ")
    );
    let cfg = GenerationConfig {
        max_tokens: 900,
        // Compilation is a small extraction, not deliberation. Reasoning here buys nothing and costs
        // the whole latency budget before any work begins.
        think: mind_inference::think_for("compile", Some(false)),
        ..GenerationConfig::default()
    };
    // PRIVATE-GROUNDED: the request is the user's own words about their life, so it takes the private
    // lane first and only escalates with an audit.
    let draft = match pool
        .chat_grounded(
            vec![
                ChatMessage::system("You output ONLY the JSON object. No prose, no code fence."),
                ChatMessage::user(&prompt),
            ],
            cfg,
        )
        .await
    {
        Ok(r) => parse(&r.text),
        Err(_) => None,
    };

    match draft {
        Some(d) => assemble(request, d, &available, budget),
        None => Compilation {
            spec: GoalSpec {
                budget,
                ..GoalSpec::simple(request)
            },
            origin: Origin::Fallback,
            notes: Vec::new(),
        },
    }
}

/// Build the spec from a draft, clamping everything the model could get wrong.
fn assemble(request: &str, d: Draft, available: &[String], budget: Budget) -> Compilation {
    let mut notes = Vec::new();
    let ready: HashSet<&str> = available.iter().map(|s| s.as_str()).collect();

    // Split what it asked for into what exists and what does not. A model naming a capability that is
    // not on the list it was given is common enough to handle rather than trust.
    let (required, missing): (Vec<String>, Vec<String>) = d
        .capabilities
        .into_iter()
        .map(|c| c.trim().to_lowercase())
        .filter(|c| !c.is_empty())
        .partition(|c| ready.contains(c.as_str()));
    if !missing.is_empty() {
        notes.push(format!(
            "This needs {} which {} not set up here.",
            humanize(&missing),
            if missing.len() == 1 { "is" } else { "are" }
        ));
    }

    let requirements: Vec<String> = d
        .requirements
        .into_iter()
        .map(|r| r.trim().to_string())
        .filter(|r| r.len() > 3)
        .take(8)
        .collect();

    // A findings floor above what the goal could plausibly produce makes the contract unsatisfiable,
    // and an unsatisfiable contract burns the entire budget before finishing partial. Cap it.
    let min_findings = d.min_findings.unwrap_or(1).clamp(1, 12);
    let max_findings = d.max_findings.filter(|m| *m >= min_findings);
    if d.max_findings.is_some() && max_findings.is_none() {
        notes.push(
            "The result cap it suggested was below its own minimum, so I dropped it.".to_string(),
        );
    }
    // Two sources is a meaningful bar; beyond that the contract is asking for corroboration the web
    // often cannot give, and the run would spend its budget failing the check rather than answering.
    let min_evidence = d.min_evidence.unwrap_or(1).clamp(1, 2);

    let objective = {
        let o = d.objective.trim();
        if o.len() > 3 {
            o.to_string()
        } else {
            request.trim().to_string()
        }
    };

    let spec = GoalSpec {
        id: format!("g-{}", uuid_like(request)),
        goal: objective,
        constraints: d
            .constraints
            .into_iter()
            .map(|c| c.trim().to_string())
            .filter(|c| !c.is_empty())
            .take(6)
            .collect(),
        required_capabilities: required,
        missing_capabilities: missing,
        contract: Contract {
            // Coverage is only enforced when requirements were actually stated. Enforcing it over an
            // empty list would be vacuous; enforcing it over a list the model invented for a simple
            // question would make that question unanswerable.
            completion: CompletionCriteria {
                min_findings,
                max_findings,
                min_evidence_per_finding: min_evidence,
                require_full_coverage: !requirements.is_empty(),
                ..Default::default()
            },
            requirements,
            output: OutputContract {
                ranked: d.ranked.unwrap_or(false),
                show_evidence: true,
                include_risks: d.needs_risk.unwrap_or(false),
                include_confidence: true,
                format: d.format.filter(|f| !f.trim().is_empty()),
            },
        },
        budget,
        horizon: 3,
        risk: if d.outward.unwrap_or(false) {
            Risk::Outward
        } else {
            Risk::ReadOnly
        },
    };

    Compilation {
        spec,
        origin: Origin::Compiled,
        notes,
    }
}

/// Lenient JSON extraction. A reasoner may wrap the object in a `<think>` preamble or a code fence,
/// and neither is a reason to lose the compile.
fn parse(raw: &str) -> Option<Draft> {
    let body = raw.rsplit("</think>").next().unwrap_or(raw);
    let body = body.split("```").find(|s| s.contains('{')).unwrap_or(body);
    let (start, end) = (body.find('{')?, body.rfind('}')?);
    if end <= start {
        return None;
    }
    let d: Draft = serde_json::from_str(&body[start..=end]).ok()?;
    // A draft with no objective AND no requirements told us nothing; the fallback is better than
    // pretending an empty contract was compiled.
    (!d.objective.trim().is_empty() || !d.requirements.is_empty()).then_some(d)
}

/// "a, b and c" — for a sentence, not a debug list.
fn humanize(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// A short stable id derived from the request. Not a UUID: two compiles of the same request should
/// collide, which is what makes an intent-compilation cache possible.
fn uuid_like(request: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in request.trim().to_lowercase().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::tests_support::FakeBus;
    use std::sync::Arc;
    use yantrik_ml::LLMBackend;

    fn pool(reply: &str) -> InferencePool {
        InferencePool::new(
            Arc::new(mind_inference::SequencedLLM::new(vec![reply])) as Arc<dyn LLMBackend>,
            1,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_vague_request_becomes_a_testable_contract() {
        let bus = FakeBus::new(&["web_search", "markets"]);
        let reply = r#"{"objective":"identify notable market candidates today",
            "requirements":["current market activity","supporting catalyst","identify downside/risk"],
            "capabilities":["markets","web_search"],
            "min_findings":3,"max_findings":8,"min_evidence":2,"needs_risk":true,"ranked":true}"#;
        let c = compile(
            &pool(reply),
            &bus,
            "find me the best stocks today",
            Budget::interactive(),
        )
        .await;

        assert_eq!(c.origin, Origin::Compiled);
        assert!(c.spec.is_runnable(), "both capabilities are available");
        assert_eq!(
            c.spec.contract.completion.min_findings, 3,
            "'the best' is not one thing"
        );
        assert_eq!(
            c.spec.contract.completion.min_evidence_per_finding, 2,
            "money deserves corroboration"
        );
        assert!(
            c.spec.contract.completion.require_full_coverage,
            "requirements were stated, so enforce them"
        );
        assert!(
            c.spec.contract.output.include_risks,
            "an advisory answer must state the downside"
        );
        assert!(c.spec.contract.output.ranked);
        assert_eq!(c.spec.contract.requirements.len(), 3);
        assert!(c.notes.is_empty(), "nothing needed saying");
    }

    /// The whole point of resolving at compile time: say it in a sentence now, rather than improvising
    /// around a tool error at step four.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_missing_capability_is_reported_before_running() {
        let bus = FakeBus::new(&["web_search"]);
        let reply = r#"{"objective":"summarise my github","capabilities":["github","web_search"],"min_findings":1}"#;
        let c = compile(
            &pool(reply),
            &bus,
            "what's open on my github?",
            Budget::interactive(),
        )
        .await;

        assert!(!c.spec.is_runnable());
        assert_eq!(c.spec.missing_capabilities, vec!["github"]);
        assert_eq!(
            c.spec.required_capabilities,
            vec!["web_search"],
            "what IS available still resolves"
        );
        assert_eq!(c.notes.len(), 1);
        assert!(
            c.notes[0].contains("github") && c.notes[0].contains("is not set up"),
            "{}",
            c.notes[0]
        );
    }

    /// A model naming a capability that was never on the list it was given is common. It must be
    /// treated as missing, not trusted into the required set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_invented_capability_is_treated_as_missing() {
        let bus = FakeBus::new(&["web_search"]);
        let reply =
            r#"{"objective":"do a thing","capabilities":["quantum_oracle"],"min_findings":1}"#;
        let c = compile(&pool(reply), &bus, "do a thing", Budget::interactive()).await;
        assert_eq!(c.spec.missing_capabilities, vec!["quantum_oracle"]);
        assert!(c.spec.required_capabilities.is_empty());
    }

    /// A simple question must stay answerable. An over-specified contract is how "what's the weather"
    /// turns into a run that cannot finish.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_simple_question_gets_a_simple_contract() {
        let bus = FakeBus::new(&["weather"]);
        let reply = r#"{"objective":"today's weather in Pune","capabilities":["weather"],"min_findings":1}"#;
        let c = compile(
            &pool(reply),
            &bus,
            "what's the weather in pune?",
            Budget::interactive(),
        )
        .await;
        assert_eq!(c.spec.contract.completion.min_findings, 1);
        assert!(
            !c.spec.contract.completion.require_full_coverage,
            "no requirements stated, none enforced"
        );
        assert!(!c.spec.contract.output.include_risks);
        assert_eq!(c.spec.risk, Risk::ReadOnly);
    }

    /// A failed compile must not block the question. The user asked something.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_useless_compile_degrades_instead_of_refusing() {
        let bus = FakeBus::new(&["web_search"]);
        for junk in [
            "I'm sorry, I can't help with that.",
            "",
            "{}",
            "{\"objective\":\"\"}",
        ] {
            let c = compile(
                &pool(junk),
                &bus,
                "why is the sky blue?",
                Budget::interactive(),
            )
            .await;
            assert_eq!(c.origin, Origin::Fallback, "junk {junk:?} should fall back");
            assert!(c.spec.is_runnable(), "the fallback must always be runnable");
            assert_eq!(
                c.spec.goal, "why is the sky blue?",
                "the fallback keeps the user's own words"
            );
            assert_eq!(c.spec.contract.completion.min_findings, 1);
        }
    }

    /// Bounds the model gets wrong must be clamped, not obeyed — an unsatisfiable contract burns the
    /// whole budget before it can finish partial.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn absurd_bounds_are_clamped_and_contradictions_dropped() {
        let bus = FakeBus::new(&["web_search"]);
        let reply = r#"{"objective":"x","requirements":["a"],"min_findings":500,"min_evidence":9}"#;
        let c = compile(&pool(reply), &bus, "x", Budget::interactive()).await;
        assert_eq!(
            c.spec.contract.completion.min_findings, 12,
            "a 500-finding contract cannot be met"
        );
        assert_eq!(
            c.spec.contract.completion.min_evidence_per_finding, 2,
            "beyond 2 the web often cannot corroborate"
        );

        // A cap below the floor is a contradiction: dropping it keeps the goal satisfiable, and the
        // note keeps that visible rather than mysterious.
        let reply2 = r#"{"objective":"x","min_findings":5,"max_findings":2}"#;
        let c2 = compile(&pool(reply2), &bus, "x", Budget::interactive()).await;
        assert_eq!(c2.spec.contract.completion.max_findings, None);
        assert!(
            c2.notes.iter().any(|n| n.contains("below its own minimum")),
            "{:?}",
            c2.notes
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_outward_request_is_marked_outward() {
        let bus = FakeBus::new(&["mail_intel"]);
        let reply =
            r#"{"objective":"email the team the release notes","outward":true,"min_findings":1}"#;
        let c = compile(
            &pool(reply),
            &bus,
            "email the team about the release",
            Budget::interactive(),
        )
        .await;
        assert_eq!(
            c.spec.risk,
            Risk::Outward,
            "the controller stops for a human on an outward goal"
        );
    }

    /// The prompt must offer the model only real capabilities — otherwise it is being invited to
    /// hallucinate one.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_prompt_lists_only_available_capabilities() {
        let backend = Arc::new(mind_inference::SequencedLLM::new(vec![
            r#"{"objective":"x"}"#,
        ]));
        let p = InferencePool::new(backend.clone() as Arc<dyn LLMBackend>, 1);
        let bus = FakeBus::new(&["web_search", "weather"]);
        compile(&p, &bus, "anything", Budget::interactive()).await;
        let seen = backend.prompt_at(0);
        assert!(
            seen.contains("AVAILABLE CAPABILITIES: [web_search, weather]"),
            "{seen}"
        );
        assert!(
            !seen.contains("github"),
            "an unavailable capability must not be suggested"
        );
    }

    /// A stable id is what makes an intent cache possible: the same request twice is the same goal.
    #[test]
    fn the_same_request_compiles_to_the_same_id() {
        assert_eq!(uuid_like("Find me stocks"), uuid_like("  find me stocks  "));
        assert_ne!(uuid_like("find me stocks"), uuid_like("find me bonds"));
    }

    #[test]
    fn a_think_preamble_and_code_fence_do_not_lose_the_compile() {
        let wrapped =
            "<think>Let me consider {this}.</think>\n```json\n{\"objective\":\"real goal\"}\n```";
        assert_eq!(parse(wrapped).unwrap().objective, "real goal");
    }

    #[test]
    fn humanize_reads_as_a_sentence() {
        assert_eq!(humanize(&["github".into()]), "github");
        assert_eq!(
            humanize(&["github".into(), "mail".into()]),
            "github and mail"
        );
        assert_eq!(
            humanize(&["a".into(), "b".into(), "c".into()]),
            "a, b and c"
        );
    }
}
