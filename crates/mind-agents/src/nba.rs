//! nba — Next Best Action.
//!
//! # A constrained choice, not an open question
//!
//! "What should you do next?" invites a paragraph and a tool name that may not exist. This asks for
//! one of a fixed set of verbs, a target, and a short reason code — a reply of maybe forty tokens.
//! The prose budget belongs at the completion boundary, where a human reads the output.
//!
//! # The runtime narrows the menu FIRST
//!
//! This is the part that matters. Before the model is asked anything, the loop removes the actions it
//! would refuse anyway: an already-tried call, a tool for a capability that is not configured, FETCH
//! when there is nothing to fetch, VERIFY when there is nothing to verify. A model offered a
//! redundant action will sometimes take it, and then the runtime rejects it — one wasted model call to
//! learn what the runtime already knew. Narrowing the menu is strictly cheaper than validating the
//! answer.
//!
//! # No expected-gain number
//!
//! The action shape deliberately has no `expected_gain` field. A model cannot know the information
//! gain of an action it has not taken; asked for one it returns a plausible decimal, and a plausible
//! decimal is indistinguishable from a measured one once it is in a struct. The reason code is what it
//! can honestly supply. Utility is the runtime's arithmetic to do, from history — see
//! [`mind_spec::Prior`] for how an unmeasured number is kept visible as unmeasured.

use mind_inference::InferencePool;
use mind_spec::capsule::Capsule;
use mind_spec::goal::GoalSpec;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use yantrik_ml::{ChatMessage, GenerationConfig};

use crate::bus::{signature, Bus};

/// The fixed verb set. A model cannot invent a tenth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Verb {
    /// Call a tool from the catalog.
    CallTool,
    /// Page in the body of an evidence reference already in the capsule.
    Fetch,
    /// Search this mind's own typed memory.
    RecallMemory,
    /// Run a banked skill by name — a procedure that is code rather than guidance.
    RunSkill,
    /// Ask the user something only they can answer.
    AskUser,
    /// The plan is wrong; rebuild it.
    Replan,
    /// Check the work against the contract.
    Verify,
    /// Answer now.
    Finish,
}

/// Why, in a token instead of a paragraph.
///
/// Aggregatable: "how often does a run fetch to verify a catalyst?" is a `GROUP BY` over these, and
/// would be a text-mining problem over sentences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Why {
    /// Resolving the highest-impact open question.
    ResolveUncertainty,
    /// Getting the first evidence for a claim that has none.
    NeedEvidence,
    /// Corroborating a claim that has one source.
    Corroborate,
    /// Reconciling sources that disagree.
    Reconcile,
    /// Establishing basic facts before anything else.
    Groundwork,
    /// The contract's remaining shortfall points here.
    CloseShortfall,
    /// Nothing further would change the answer.
    Sufficient,
}

/// One chosen action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub verb: Verb,
    /// Tool name for CallTool, evidence id for Fetch, query for RecallMemory, question for AskUser.
    /// Empty for Verb::Finish and Verb::Replan.
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub args: Value,
    pub why: Why,
}

/// What the last step established, read off the evidence it produced.
///
/// # Why this rides along with the action
///
/// Something has to turn "here is a page" into "this claim is now supported", and it needs semantic
/// judgment — a deterministic adapter can shape a GitHub response into fields, but it cannot decide
/// which of them answers the goal. Without this step a run gathers evidence forever and never
/// satisfies its own contract: findings stay at zero because nothing ever promotes evidence into a
/// claim. That is exactly what happened on the first version of this loop.
///
/// The alternative was a separate extraction call per step, which doubles the model calls in the
/// hottest part of the loop. Folding it into the same reply costs a handful of tokens instead: the
/// model is already looking at the capsule, so asking "what did that teach you, and what next?" is one
/// question, not two.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Learned {
    /// Claims now supported, each citing the evidence ids that support it.
    #[serde(default)]
    pub findings: Vec<LearnedFinding>,
    /// Questions raised or answered.
    #[serde(default)]
    pub uncertainties: Vec<LearnedUncertainty>,
    /// Sources that disagree. Their presence escalates the next decision to a stronger model.
    #[serde(default)]
    pub contradictions: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LearnedFinding {
    pub claim: String,
    /// Evidence ids. A finding citing nothing is dropped — see `Learned::into_observation`.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// Which contract requirement this addresses, if any.
    #[serde(default)]
    pub addresses: Vec<String>,
    #[serde(default)]
    pub risk: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LearnedUncertainty {
    pub question: String,
    #[serde(default)]
    pub importance: f64,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub resolved: bool,
}

impl Learned {
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty() && self.uncertainties.is_empty() && self.contradictions.is_empty()
    }

    /// Fold into an observation the capsule can reduce.
    ///
    /// `known_evidence` is the gate that makes this safe: a claim citing an id that does not exist is
    /// a claim about nothing, and accepting it would let the loop manufacture support for itself. Such
    /// citations are dropped, and a finding left with none is dropped entirely — the contract's
    /// evidence floor would have rejected it anyway, but silently. Better to never let it in.
    pub fn into_observation(
        self,
        action: String,
        known_evidence: &[String],
    ) -> mind_spec::capsule::Observation {
        use mind_spec::capsule::{Finding, Uncertainty};
        let findings: Vec<Finding> = self
            .findings
            .into_iter()
            .filter(|f| f.claim.trim().len() > 3)
            .map(|f| Finding {
                claim: f.claim.trim().to_string(),
                evidence: f
                    .evidence
                    .into_iter()
                    .filter(|e| known_evidence.iter().any(|k| k == e))
                    .collect(),
                addresses: f
                    .addresses
                    .into_iter()
                    .filter(|a| !a.trim().is_empty())
                    .collect(),
                risk: f.risk.filter(|r| !r.trim().is_empty()),
                rank: None,
            })
            .filter(|f| !f.evidence.is_empty())
            .collect();

        mind_spec::capsule::Observation {
            action,
            ok: true,
            findings,
            uncertainties: self
                .uncertainties
                .into_iter()
                .filter(|u| u.question.trim().len() > 3)
                .map(|u| Uncertainty {
                    question: u.question.trim().to_string(),
                    importance: u.importance.clamp(0.0, 1.0),
                    confidence: u.confidence.clamp(0.0, 1.0),
                    resolved: u.resolved,
                })
                .collect(),
            notes: self.contradictions,
            ..Default::default()
        }
    }
}

/// The model's whole reply for one step: what it learned, and what to do next.
#[derive(Debug, Clone, PartialEq)]
pub struct StepChoice {
    pub action: Action,
    pub learned: Learned,
}

impl Action {
    pub fn finish(why: Why) -> Self {
        Self {
            verb: Verb::Finish,
            target: String::new(),
            args: Value::Null,
            why,
        }
    }
    /// The signature the capsule records — what dedup and loop detection compare.
    pub fn signature(&self) -> String {
        match self.verb {
            Verb::CallTool => signature(&self.target, &self.args),
            Verb::Fetch => format!("fetch:{}", self.target),
            Verb::RecallMemory => format!("recall:{}", self.target),
            Verb::RunSkill => format!("skill:{}", self.target),
            other => format!("{other:?}"),
        }
    }
}

/// Which verbs are worth offering, given the state.
///
/// Every exclusion here is a model call not spent discovering something the runtime already knew.
pub fn allowed_verbs(
    capsule: &Capsule,
    spec: &GoalSpec,
    procedures: &[crate::procedure::Procedure],
) -> Vec<Verb> {
    let mut v = vec![Verb::CallTool, Verb::RecallMemory, Verb::Finish];

    // RUN_SKILL only when a banked script was actually recalled. Offering it otherwise invites the
    // model to name a skill that does not exist, which costs a call to find out.
    if procedures
        .iter()
        .any(|p| matches!(p.kind, crate::procedure::ProcedureKind::Executable { .. }))
    {
        v.push(Verb::RunSkill);
    }

    // FETCH only when there is an unread body to fetch. Offering it with nothing loadable invites a
    // call that fails and teaches nothing.
    if capsule.evidence.iter().any(|e| !e.loaded) {
        v.push(Verb::Fetch);
    }
    // VERIFY needs something to check.
    if !capsule.findings.is_empty() {
        v.push(Verb::Verify);
    }
    // REPLAN is pointless before a plan exists, and past the replan budget it is superstition.
    if capsule.progress.steps > 0 && capsule.progress.replans < 3 {
        v.push(Verb::Replan);
    }
    // ASK_USER is a real cost to the user's attention, so it is offered only once the run has
    // actually got stuck — otherwise a model reaches for it as a first resort.
    if capsule.progress.barren_steps >= 2 || !spec.missing_capabilities.is_empty() {
        v.push(Verb::AskUser);
    }
    v
}

/// The catalog with already-exhausted tools removed.
///
/// A tool tried twice with the same arguments has told us what it is going to tell us; leaving it on
/// the menu is how a run spends its budget re-reading the same page.
pub fn open_catalog(capsule: &Capsule, catalog: &str, max_same: usize) -> String {
    catalog
        .lines()
        .filter(|line| {
            let name = line.trim_start().strip_prefix("- ").unwrap_or(line);
            let name = name.split([' ', '{', ':']).next().unwrap_or("");
            name.is_empty() || capsule.attempts_of(name) < max_same
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Ask for the next action.
///
/// Returns `None` when the model produced nothing usable — the caller then finishes with what it has
/// rather than guessing, because an invented action is worse than a short answer.
pub async fn choose(
    pool: &InferencePool,
    bus: &dyn Bus,
    spec: &GoalSpec,
    capsule: &Capsule,
    shortfalls: &[String],
    // Approaches already surfaced from memory for this goal. Shown to the model so it follows a
    // known way of working rather than deriving one, which is the whole point of keeping them.
    procedures: &[crate::procedure::Procedure],
    prefer_stronger: bool,
) -> Option<StepChoice> {
    let verbs = allowed_verbs(capsule, spec, procedures);
    let catalog = open_catalog(capsule, &bus.catalog(&spec.goal), 2);

    // The capsule is the ENTIRE history the model sees. No transcript, no prior tool outputs — this is
    // the line where the token argument is either honoured or quietly abandoned.
    let prompt = format!(
        "{state}{known}\n\nWHAT IS STILL MISSING\n{missing}\n\nTOOLS\n{catalog}\n\n\
         Do two things. First, read the EVIDENCE above and say what it establishes — a claim counts as \
         a finding only if an evidence id supports it. Then choose ONE next action.\n\n\
         Reply with ONLY this JSON:\n\
         {{\"learned\":{{\"findings\":[{{\"claim\":\"...\",\"evidence\":[\"E1\"],\"addresses\":[\"a requirement\"],\"risk\":null}}],\
\"uncertainties\":[{{\"question\":\"...\",\"importance\":0.9,\"confidence\":0.3,\"resolved\":false}}],\"contradictions\":[]}},\n\
          \"verb\":\"{verbs}\",\"target\":\"tool name / evidence id / query\",\"args\":{{}},\"why\":\"{whys}\"}}\n\n\
         Cite only evidence ids that appear above — a claim citing anything else is discarded. Leave \
         `learned` empty if the last step established nothing. Pick the action that closes the biggest \
         gap; if an open question is listed, resolving it usually beats gathering more of what you \
         already have. Choose FINISH only if nothing further would change the answer.",
        state = capsule.render(2000),
        known = crate::procedure::render_block(procedures),
        missing = if shortfalls.is_empty() { "(nothing \u{2014} the contract is met)".to_string() } else { shortfalls.join("\n") },
        catalog = catalog,
        verbs = verbs.iter().map(verb_name).collect::<Vec<_>>().join("|"),
        whys = "RESOLVE_UNCERTAINTY|NEED_EVIDENCE|CORROBORATE|RECONCILE|GROUNDWORK|CLOSE_SHORTFALL|SUFFICIENT",
    );

    let cfg = GenerationConfig {
        max_tokens: 300, // a choice, not an essay
        think: mind_inference::think_for("nba", Some(false)),
        prefer_reasoner: prefer_stronger,
        ..GenerationConfig::default()
    };
    let text = pool
        .chat_grounded(
            vec![
                ChatMessage::system(
                    "You choose the next action for a running agent. Output ONLY the JSON object. \
                     Never explain.",
                ),
                ChatMessage::user(&prompt),
            ],
            cfg,
        )
        .await
        .ok()?
        .text;

    let choice = parse(&text)?;
    // Reject a verb that was not on the menu. A model that reaches for one anyway must not be able to
    // widen its own permissions — the same reason the tool allow-list is enforced at dispatch.
    if !verbs.contains(&choice.action.verb) {
        return None;
    }
    Some(choice)
}

fn verb_name(v: &Verb) -> &'static str {
    match v {
        Verb::CallTool => "CALL_TOOL",
        Verb::Fetch => "FETCH",
        Verb::RecallMemory => "RECALL_MEMORY",
        Verb::RunSkill => "RUN_SKILL",
        Verb::AskUser => "ASK_USER",
        Verb::Replan => "REPLAN",
        Verb::Verify => "VERIFY",
        Verb::Finish => "FINISH",
    }
}

/// Lenient parse. A missing `why` defaults rather than losing the action — the verb is the decision,
/// the reason code is telemetry.
fn parse(raw: &str) -> Option<StepChoice> {
    #[derive(Deserialize)]
    struct Raw {
        verb: String,
        #[serde(default)]
        target: String,
        #[serde(default)]
        args: Value,
        #[serde(default)]
        why: Option<String>,
        /// Absent is fine and common — most steps establish nothing new.
        #[serde(default)]
        learned: Learned,
    }
    let body = raw.rsplit("</think>").next().unwrap_or(raw);
    let body = body.split("```").find(|s| s.contains('{')).unwrap_or(body);
    let (a, b) = (body.find('{')?, body.rfind('}')?);
    let r: Raw = serde_json::from_str(body.get(a..=b)?).ok()?;

    let verb = match r
        .verb
        .trim()
        .to_uppercase()
        .replace(['-', ' '], "_")
        .as_str()
    {
        "CALL_TOOL" | "CALLTOOL" | "TOOL" => Verb::CallTool,
        "FETCH" => Verb::Fetch,
        "RECALL_MEMORY" | "RECALL" | "RETRIEVE_MEMORY" => Verb::RecallMemory,
        "RUN_SKILL" | "RUNSKILL" | "SKILL" => Verb::RunSkill,
        "ASK_USER" | "ASK" => Verb::AskUser,
        "REPLAN" => Verb::Replan,
        "VERIFY" => Verb::Verify,
        "FINISH" | "ANSWER" | "DONE" => Verb::Finish,
        _ => return None,
    };
    // An action that needs a target but has none is unusable; better to lose the step than to call a
    // tool named "".
    if matches!(
        verb,
        Verb::CallTool | Verb::Fetch | Verb::RecallMemory | Verb::RunSkill
    ) && r.target.trim().is_empty()
    {
        return None;
    }
    let why = match r
        .why
        .unwrap_or_default()
        .trim()
        .to_uppercase()
        .replace(['-', ' '], "_")
        .as_str()
    {
        "RESOLVE_UNCERTAINTY" => Why::ResolveUncertainty,
        "NEED_EVIDENCE" => Why::NeedEvidence,
        "CORROBORATE" => Why::Corroborate,
        "RECONCILE" => Why::Reconcile,
        "GROUNDWORK" => Why::Groundwork,
        "SUFFICIENT" => Why::Sufficient,
        _ => Why::CloseShortfall,
    };
    Some(StepChoice {
        action: Action {
            verb,
            target: r.target.trim().to_string(),
            args: r.args,
            why,
        },
        learned: r.learned,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::tests_support::FakeBus;
    use mind_spec::capsule::{Evidence, Finding, Observation, Uncertainty};
    use std::sync::Arc;
    use yantrik_ml::LLMBackend;

    fn spec() -> GoalSpec {
        GoalSpec::simple("find things")
    }

    fn pool(reply: &str) -> InferencePool {
        InferencePool::new(
            Arc::new(mind_inference::SequencedLLM::new(vec![reply])) as Arc<dyn LLMBackend>,
            1,
        )
    }

    fn finding(claim: &str, ev: &[&str]) -> Finding {
        Finding {
            claim: claim.into(),
            evidence: ev.iter().map(|s| s.to_string()).collect(),
            addresses: vec![],
            risk: None,
            rank: None,
        }
    }

    /// A fresh run must not be offered FETCH (nothing to page), VERIFY (nothing found), REPLAN (no
    /// plan) or ASK_USER (not stuck). Each of those would be a wasted model call.
    #[test]
    fn a_fresh_run_is_offered_only_the_verbs_that_could_work() {
        let v = allowed_verbs(&Capsule::new("g", "goal"), &spec(), &[]);
        assert!(
            v.contains(&Verb::CallTool)
                && v.contains(&Verb::RecallMemory)
                && v.contains(&Verb::Finish)
        );
        assert!(!v.contains(&Verb::Fetch), "nothing to fetch yet");
        assert!(!v.contains(&Verb::Verify), "nothing to verify yet");
        assert!(!v.contains(&Verb::Replan), "no plan to replan");
        assert!(
            !v.contains(&Verb::AskUser),
            "asking the user is not a first resort"
        );
    }

    #[test]
    fn verbs_appear_as_the_state_makes_them_useful() {
        let mut c = Capsule::new("g", "goal").reduce(Observation {
            action: "search".into(),
            ok: true,
            evidence: vec![Evidence {
                id: "E1".into(),
                summary: "s".into(),
                source: "web".into(),
                body: "b".into(),
                captured_ms: 0,
            }],
            findings: vec![finding("a claim", &["E1"])],
            ..Default::default()
        });
        let v = allowed_verbs(&c, &spec(), &[]);
        assert!(
            v.contains(&Verb::Fetch),
            "an unread body makes FETCH useful"
        );
        assert!(v.contains(&Verb::Verify), "a finding makes VERIFY useful");
        assert!(v.contains(&Verb::Replan));

        // Stalling unlocks ASK_USER — the run has earned the right to interrupt.
        c = c.reduce(Observation {
            action: "x".into(),
            ok: true,
            ..Default::default()
        });
        c = c.reduce(Observation {
            action: "y".into(),
            ok: true,
            ..Default::default()
        });
        assert!(allowed_verbs(&c, &spec(), &[]).contains(&Verb::AskUser));
    }

    /// A missing capability unlocks ASK_USER immediately: only the user can fix it, and grinding is
    /// pointless.
    #[test]
    fn a_missing_capability_unlocks_asking_at_once() {
        let mut s = spec();
        s.missing_capabilities = vec!["github".into()];
        assert!(allowed_verbs(&Capsule::new("g", "goal"), &s, &[]).contains(&Verb::AskUser));
    }

    /// An exhausted tool leaves the menu, so the run cannot spend its budget re-reading one page.
    #[test]
    fn an_exhausted_tool_is_removed_from_the_catalog() {
        let cat =
            "- search {query}: web search\n- weather {place}: forecast\n- news {topic}: headlines";
        let mut c = Capsule::new("g", "goal");
        for _ in 0..2 {
            c = c.reduce(Observation {
                action: "search".into(),
                ok: true,
                ..Default::default()
            });
        }
        let open = open_catalog(&c, cat, 2);
        assert!(!open.contains("search"), "twice is enough:\n{open}");
        assert!(
            open.contains("weather") && open.contains("news"),
            "the rest stay:\n{open}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_well_formed_choice_parses() {
        let reply = r#"{"verb":"CALL_TOOL","target":"web_search","args":{"query":"xyz catalyst"},"why":"RESOLVE_UNCERTAINTY"}"#;
        let c = choose(
            &pool(reply),
            &FakeBus::new(&["web_search"]),
            &spec(),
            &Capsule::new("g", "goal"),
            &["1 of 3 findings".into()],
            &[],
            false,
        )
        .await
        .expect("a valid action");
        assert_eq!(c.action.verb, Verb::CallTool);
        assert_eq!(c.action.target, "web_search");
        assert_eq!(c.action.why, Why::ResolveUncertainty);
        assert_eq!(
            c.action.signature(),
            "web_search|{\"query\":\"xyz catalyst\"}"
        );
        assert!(
            c.learned.is_empty(),
            "this reply carried no learned block, and that is normal"
        );
    }

    /// A verb the runtime did not offer must be REJECTED, not executed. Otherwise a model could widen
    /// its own permissions by naming a verb it was not given.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_verb_that_was_not_offered_is_refused() {
        // VERIFY is not on a fresh run's menu.
        let reply = r#"{"verb":"VERIFY","target":"","why":"SUFFICIENT"}"#;
        let got = choose(
            &pool(reply),
            &FakeBus::new(&["web_search"]),
            &spec(),
            &Capsule::new("g", "goal"),
            &[],
            &[],
            false,
        )
        .await;
        assert!(got.is_none(), "an un-offered verb must not execute");
    }

    /// Nothing usable back means finish with what we have — never an invented action.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn junk_yields_no_action_rather_than_a_guess() {
        for junk in [
            "I think we should search the web!",
            "",
            "{}",
            r#"{"verb":"TELEPORT"}"#,
            r#"{"verb":"CALL_TOOL","target":"  "}"#,
        ] {
            let got = choose(
                &pool(junk),
                &FakeBus::new(&["web_search"]),
                &spec(),
                &Capsule::new("g", "goal"),
                &[],
                &[],
                false,
            )
            .await;
            assert!(got.is_none(), "junk {junk:?} produced an action");
        }
    }

    /// The prompt the model sees must be the CAPSULE, not a transcript. If a prior tool's full output
    /// ever appears here, the entire token argument is gone.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_prompt_carries_the_capsule_and_no_transcript() {
        let backend = Arc::new(mind_inference::SequencedLLM::new(vec![
            r#"{"verb":"FINISH","why":"SUFFICIENT"}"#,
        ]));
        let p = InferencePool::new(backend.clone() as Arc<dyn LLMBackend>, 1);
        let c = Capsule::new("g", "identify strong equities").reduce(Observation {
            action: "screen".into(),
            ok: true,
            evidence: vec![Evidence {
                id: "E1".into(),
                summary: "volume 4.3x baseline".into(),
                source: "markets".into(),
                body: "RAW_TOOL_PAYLOAD_".repeat(500),
                captured_ms: 0,
            }],
            uncertainties: vec![Uncertainty {
                question: "is the move news-driven?".into(),
                importance: 0.9,
                confidence: 0.2,
                resolved: false,
            }],
            ..Default::default()
        });
        choose(
            &p,
            &FakeBus::new(&["markets"]),
            &spec(),
            &c,
            &["2 of 3 findings so far".into()],
            &[],
            false,
        )
        .await;

        let seen = backend.prompt_at(0);
        assert!(
            seen.contains("identify strong equities"),
            "the goal is present"
        );
        assert!(
            seen.contains("volume 4.3x baseline"),
            "the evidence SUMMARY is present"
        );
        assert!(
            seen.contains("is the move news-driven?"),
            "the open question is present \u{2014} it drives the choice"
        );
        assert!(
            seen.contains("2 of 3 findings so far"),
            "the shortfall is present"
        );
        assert!(
            !seen.contains("RAW_TOOL_PAYLOAD_"),
            "a raw tool body must NEVER reach the model here"
        );
        assert!(
            seen.len() < 6000,
            "the whole prompt stays small, got {}",
            seen.len()
        );
    }

    /// The menu in the prompt must match what the runtime will accept, or the model is being invited
    /// to pick something that gets thrown away.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_prompt_offers_exactly_the_allowed_verbs() {
        let backend = Arc::new(mind_inference::SequencedLLM::new(vec![
            r#"{"verb":"FINISH","why":"SUFFICIENT"}"#,
        ]));
        let p = InferencePool::new(backend.clone() as Arc<dyn LLMBackend>, 1);
        choose(
            &p,
            &FakeBus::new(&["web_search"]),
            &spec(),
            &Capsule::new("g", "goal"),
            &[],
            &[],
            false,
        )
        .await;
        let seen = backend.prompt_at(0);
        assert!(seen.contains("CALL_TOOL|RECALL_MEMORY|FINISH"), "{seen}");
        assert!(
            !seen.contains("VERIFY"),
            "an un-offered verb must not appear in the menu"
        );
    }

    #[test]
    fn a_missing_reason_code_does_not_lose_the_action() {
        let c = parse(r#"{"verb":"FINISH"}"#).unwrap();
        assert_eq!(c.action.verb, Verb::Finish);
        assert_eq!(
            c.action.why,
            Why::CloseShortfall,
            "the verb is the decision; the reason is telemetry"
        );
    }

    /// A finding citing an evidence id the capsule does not hold is a claim about nothing. Accepting
    /// it would let the loop manufacture its own support and satisfy its own contract with fiction.
    #[test]
    fn a_finding_citing_unknown_evidence_is_dropped() {
        let learned = Learned {
            findings: vec![
                LearnedFinding {
                    claim: "grounded claim".into(),
                    evidence: vec!["E1".into()],
                    ..Default::default()
                },
                LearnedFinding {
                    claim: "invented claim".into(),
                    evidence: vec!["E99".into()],
                    ..Default::default()
                },
                LearnedFinding {
                    claim: "uncited claim".into(),
                    evidence: vec![],
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let obs = learned.into_observation("extract".into(), &["E1".to_string()]);
        let claims: Vec<&str> = obs.findings.iter().map(|f| f.claim.as_str()).collect();
        assert_eq!(
            claims,
            vec!["grounded claim"],
            "only what the capsule can actually support survives"
        );
    }

    /// Confidence and importance arriving out of range are clamped, not trusted — the controller
    /// compares against thresholds, so an importance of 5.0 would make every question critical.
    #[test]
    fn out_of_range_uncertainty_numbers_are_clamped() {
        let learned = Learned {
            uncertainties: vec![LearnedUncertainty {
                question: "is it real?".into(),
                importance: 5.0,
                confidence: -2.0,
                resolved: false,
            }],
            ..Default::default()
        };
        let obs = learned.into_observation("extract".into(), &[]);
        assert_eq!(obs.uncertainties[0].importance, 1.0);
        assert_eq!(obs.uncertainties[0].confidence, 0.0);
    }

    /// The learned block and the action arrive in ONE reply — that is what keeps the loop at one model
    /// call per step instead of two.
    #[test]
    fn one_reply_carries_both_what_was_learned_and_what_is_next() {
        let raw = r#"{"learned":{"findings":[{"claim":"volume is 4.3x baseline","evidence":["E1"],
            "addresses":["current market activity"]}],
            "uncertainties":[{"question":"is it news-driven?","importance":0.9,"confidence":0.2,"resolved":false}],
            "contradictions":[]},
            "verb":"CALL_TOOL","target":"news","args":{"topic":"xyz"},"why":"RESOLVE_UNCERTAINTY"}"#;
        let c = parse(raw).unwrap();
        assert_eq!(c.action.verb, Verb::CallTool);
        assert_eq!(c.action.target, "news");
        assert_eq!(c.learned.findings.len(), 1);
        assert_eq!(
            c.learned.findings[0].addresses,
            vec!["current market activity"]
        );
        assert_eq!(c.learned.uncertainties[0].importance, 0.9);
    }

    #[test]
    fn common_verb_spellings_are_accepted() {
        for (raw, want) in [
            ("call_tool", Verb::CallTool),
            ("CALL-TOOL", Verb::CallTool),
            ("retrieve_memory", Verb::RecallMemory),
            ("answer", Verb::Finish),
            ("done", Verb::Finish),
        ] {
            let json = format!(r#"{{"verb":"{raw}","target":"x"}}"#);
            assert_eq!(parse(&json).unwrap().action.verb, want, "{raw}");
        }
    }
}
