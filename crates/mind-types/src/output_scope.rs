//! output_scope — what a turn is allowed to SAY, which is not the same question as where it thinks.
//!
//! Codex, driving an operator `/chat` probe told not to run skills, not to call tools and not to
//! reveal private facts: it ran nothing, the privacy lane stayed local, `PRIVACY_ESCALATED` never
//! moved — and the answer surfaced concrete private examples anyway.
//!
//! Two different properties, and conflating them is how the first would have hidden the second:
//!
//!   INFERENCE SCOPE — where the tokens went. Owned by the privacy lanes. It was correct.
//!   OUTPUT SCOPE    — what the answer may name. Owned by this module. It was not.
//!
//! Filing a disclosure failure under lane security would have let a green escalation counter argue
//! against the finding (E.SEC8).
//!
//! # The one property that makes an imperfect detector safe
//!
//! Recognising "do not reveal private facts" in a user's message is a text matcher, and this
//! codebase has been punished four times this week for text matchers — a parser firing on "saved",
//! a guard matching `t[` inside `next["id"]`, a detector calling a confidence float a credit card,
//! a leak assertion colliding with a timestamp. Every one of them could fail in BOTH directions.
//!
//! This one cannot. [`OutputPolicy::tighten`] is **monotonic toward silence**: it may only ever
//! narrow what is permitted, never widen it. So a false positive costs a more generic answer, and a
//! false negative leaves the surface default standing. Neither can open something that was shut.
//! The harm gate already reasons this way about its normalised views; this is the same trick.

/// Where the answer is going, ordered from most permissive to least.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputScope {
    /// The owner, on a surface only they reach. Concrete detail is the point of the product here.
    OperatorPrivate,
    /// A paired member of the household. Their own context, not the operator's whole life.
    HouseholdMember,
    /// Anything that leaves the household — a shared page, an export, an outside witness.
    PublicShare,
    /// A log or artifact meant to be READ BY SOMEONE ELSE while proving something. Kinds and
    /// counts, never values — the rule the flight recorder and the corpus audit already keep.
    AuditRedacted,
}

impl OutputScope {
    pub fn label(self) -> &'static str {
        match self {
            OutputScope::OperatorPrivate => "operator-private",
            OutputScope::HouseholdMember => "household-member",
            OutputScope::PublicShare => "public-share",
            OutputScope::AuditRedacted => "audit-redacted",
        }
    }

    /// Does a violation here BLOCK, or merely get recorded?
    ///
    /// Operator-private starts diagnostic on purpose (Codex's call): the owner is already entitled
    /// to the content, so the failure is one of obedience rather than exposure, and a fail-closed
    /// guard on the owner's own surface would break daily use to enforce a preference. Everything
    /// that can leave the household fails closed.
    pub fn fails_closed(self) -> bool {
        !matches!(self, OutputScope::OperatorPrivate)
    }
}

/// Classes of concrete thing an answer might name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityClass {
    Person,
    Place,
    Task,
    Account,
    Purchase,
    Event,
    Project,
    /// A fact the mind remembered, quoted or paraphrased back.
    RememberedFact,
}

impl EntityClass {
    pub const ALL: &'static [EntityClass] = &[
        EntityClass::Person,
        EntityClass::Place,
        EntityClass::Task,
        EntityClass::Account,
        EntityClass::Purchase,
        EntityClass::Event,
        EntityClass::Project,
        EntityClass::RememberedFact,
    ];
}

/// What THIS turn's answer is permitted to contain.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OutputPolicy {
    pub scope: OutputScope,
    /// May the answer illustrate with concrete instances from the household's life?
    pub examples_allowed: bool,
    /// Which classes of concrete entity may be NAMED. Empty means none.
    pub entity_classes: Vec<EntityClass>,
    /// How many pieces of remembered evidence may be surfaced at all.
    pub max_evidence_items: usize,
}

impl OutputPolicy {
    /// The default for a surface, before any instruction in the turn itself.
    pub fn for_scope(scope: OutputScope) -> Self {
        match scope {
            OutputScope::OperatorPrivate => Self {
                scope,
                examples_allowed: true,
                entity_classes: EntityClass::ALL.to_vec(),
                max_evidence_items: usize::MAX,
            },
            OutputScope::HouseholdMember => Self {
                scope,
                examples_allowed: true,
                // A member sees household life, not the operator's accounts.
                entity_classes: vec![
                    EntityClass::Person,
                    EntityClass::Place,
                    EntityClass::Task,
                    EntityClass::Event,
                ],
                max_evidence_items: 8,
            },
            OutputScope::PublicShare | OutputScope::AuditRedacted => Self {
                scope,
                examples_allowed: false,
                entity_classes: Vec::new(),
                max_evidence_items: 0,
            },
        }
    }

    /// The policy stated to the model, as DEFENCE IN DEPTH — never as the boundary.
    ///
    /// Codex was explicit that this is secondary: the context has already been filtered by
    /// [`admit_working_set`] before this sentence is written, so it explains a decision that has
    /// already been enforced rather than requesting one. Telling a model not to reveal facts it can
    /// still see IS the live failure; this line only exists so the answer reads as a deliberate
    /// choice ("I can't cite private examples under this scope") instead of an unexplained blank.
    ///
    /// `None` when the policy constrains nothing, so an ordinary operator turn carries no extra
    /// instruction and behaves exactly as it did before slice 4.
    pub fn prompt_note(&self) -> Option<String> {
        if self.scope == OutputScope::OperatorPrivate
            && self.examples_allowed
            && self.entity_classes.len() == EntityClass::ALL.len()
        {
            return None;
        }
        let body = if self.entity_classes.is_empty() {
            "You may NOT cite concrete private details in this answer — no names, tasks, accounts, \
             purchases, places or remembered facts. The supporting context has ALREADY been withheld \
             from you, so do not guess at it or apologise for its absence: answer structurally, and \
             say plainly that you can't cite private examples here."
                .to_string()
        } else {
            format!(
                "OUTPUT SCOPE: {}. Supporting context has already been limited to what this scope \
                 permits{}. Answer from what you were given; do not speculate about what was withheld.",
                self.scope.label(),
                if self.examples_allowed { "" } else { ", and worked examples are not permitted" }
            )
        };
        Some(body)
    }

    /// Narrow this policy. NEVER widens — that is the invariant the whole module rests on.
    ///
    /// Takes the stricter of each field, so combining policies in any order lands in the same
    /// place and no caller can accidentally re-open something another caller shut.
    pub fn tighten_to(&self, other: &OutputPolicy) -> OutputPolicy {
        OutputPolicy {
            scope: self.scope.max(other.scope),
            examples_allowed: self.examples_allowed && other.examples_allowed,
            entity_classes: self
                .entity_classes
                .iter()
                .filter(|c| other.entity_classes.contains(c))
                .copied()
                .collect(),
            max_evidence_items: self.max_evidence_items.min(other.max_evidence_items),
        }
    }

    /// Apply a minimization request made in the turn itself.
    ///
    /// Only ever narrows. A false positive costs a more generic answer; a false negative leaves the
    /// surface default. Neither can open something that was shut, which is what makes a text
    /// matcher acceptable here when it was not acceptable anywhere else this week.
    pub fn tighten(&self, request: MinimizationRequest) -> OutputPolicy {
        match request {
            MinimizationRequest::None => self.clone(),
            MinimizationRequest::NoExamples => OutputPolicy { examples_allowed: false, ..self.clone() },
            MinimizationRequest::NoPrivateFacts => OutputPolicy {
                scope: self.scope,
                examples_allowed: false,
                entity_classes: Vec::new(),
                max_evidence_items: 0,
            },
        }
    }

    /// Is naming this class of thing permitted?
    pub fn may_name(&self, class: EntityClass) -> bool {
        self.entity_classes.contains(&class)
    }
}

/// Every route by which evidence reaches a model on a turn (E.CTX1).
///
/// # Why this is an enum and not a set of booleans
///
/// Thirteen times in one session the same defect appeared: a channel gated, and the one beside it
/// not. A line break, a variable name, one code path of three, one evidence channel of seven, one
/// tool door of three, a contradiction fetched twice eight hundred lines apart. Each was fixed
/// correctly and separately — which produced THREE different expressions all asking "may this turn
/// name things?", in three different functions, each added the moment another ungated channel
/// turned up.
///
/// The defect was never the missing check. It was that nothing forced the question to be ASKED for
/// a new channel. An exhaustive match does: adding a variant here without giving it an arm below is
/// a compile error, not something a probe discovers three hours later. That mechanism has already
/// out-performed my attention once — the compiler, not I, found the second uncertainty renderer in
/// E.SEC11.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    /// The rendered working set — recalled beliefs and facts.
    Grounding,
    /// Recent dialogue. Carries the mind's own earlier, less-restricted answers.
    Transcript,
    MailDigest,
    GithubDigest,
    /// Per-turn scratch notes and the agent work log.
    ScratchNotes,
    /// The household roster, with names and relationships.
    PeopleRoster,
    /// The rolling summary of older turns — private conversation, distilled.
    ConversationSummary,
    /// Open contradictions. Two belief TEXTS, so a disclosure despite being an instruction.
    Contradictions,
    /// An inference about how the user lives, worn as a voice instruction.
    RelationshipLens,
    /// The tool catalogue and schemas — a channel because a model that can CALL recall can pull
    /// what a filter withheld.
    ToolSurface,
    /// A fetched web page. Public by construction.
    WebPage,
    /// Mounted-pack knowledge: a labelled third-party publisher's claims, not the household's.
    PackContext,
    /// The MIND's own degraded-state note. About itself, not about the user.
    MetacogNote,
}

impl OutputPolicy {
    /// May this channel reach the model under this policy?
    ///
    /// THE one place the question is answered. Every gate in the conversation crate defers here, so
    /// a fourteenth channel cannot arrive with its own private opinion about what "private" means.
    ///
    /// The match is exhaustive and deliberately un-defaulted: no `_ =>` arm, because a catch-all
    /// would silently admit a new channel and reintroduce exactly the failure this exists to end.
    pub fn admits(&self, channel: Channel) -> bool {
        // A policy permitting no entity class is a total prohibition: the turn may name nothing.
        let names_anything = !self.entity_classes.is_empty();
        match channel {
            // HOUSEHOLD CONTENT — everything that carries the user's own life.
            Channel::Grounding
            | Channel::Transcript
            | Channel::MailDigest
            | Channel::GithubDigest
            | Channel::ScratchNotes
            | Channel::PeopleRoster
            | Channel::ConversationSummary
            | Channel::Contradictions
            | Channel::RelationshipLens
            | Channel::ToolSurface => names_anything,

            // NOT the household's life, and withholding them costs the answer for nothing:
            // a fetched page is public, a pack is a labelled publisher's claims, and the metacog
            // note reports the MIND's own state — telling the model to hedge when evidence is thin
            // is exactly right on a turn that has been stripped of evidence.
            Channel::WebPage | Channel::PackContext | Channel::MetacogNote => true,
        }
    }
}

/// The structural record of ONE policy decision. COUNTS AND ENUMS ONLY.
///
/// Codex's rule for operator-private telemetry, and the reason this type has no `String` in it and
/// never will: production records what the policy DID, never what it saw. "Policy admitted 0 of 12
/// on a household-member surface after a NoPrivateFacts request" is a fact about the mechanism.
/// "The answer looked private" is a content judgement, and recording it would mean scanning the
/// owner's own answers for private-looking strings — which is exactly what the scratch canary
/// harness exists to do instead, with known tokens and a deterministic instrument (E.SEC8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidenceDecision {
    pub scope: OutputScope,
    pub request: MinimizationRequest,
    /// Every item hydrated, contradictions included.
    pub before: usize,
    pub admitted: usize,
    pub dropped: usize,
    /// Broken out because these are exempt from the budget and the distinction is the point.
    pub contradictions_kept: usize,
}

/// Apply an output policy to the TYPED working set, before any of it becomes prompt text.
///
/// This is slice 4's mechanism and it sits where Codex said it must: over typed evidence items
/// BEFORE rendering, not in the prompt as an instruction. The live failure was a model told not to
/// reveal private facts while private facts sat in its context; a stronger instruction repeats that
/// shape, a filter does not. A filter also has invariants checkable BEFORE generation, which an
/// instruction can only ever be graded on afterwards.
///
/// # A CONSTRAINT IS NOT EVIDENCE
///
/// `active_contradictions` is not a disclosure — it is the instruction "ASK to resolve, do NOT
/// assert either side". Dropping it to satisfy `max_evidence_items` would leave the model unaware a
/// fact is contested and free to assert one side, so a PRIVACY filter would have caused a
/// dishonest answer. Contradictions are therefore exempt from the budget, while still subject to
/// total prohibition: under "reveal nothing" they carry belief text like anything else.
///
/// # What this does NOT do
///
/// No class-level filtering. Evidence items carry no entity-class labels, substring guesses for
/// Person-vs-Account are the fuzzy-matcher failure this codebase has retired four times, and Codex
/// ruled them out explicitly. A policy permitting SOME classes admits its already-retrieval-scoped
/// items, capped. That gap is a recorded test, not a silence.
pub fn admit_working_set(
    policy: &OutputPolicy,
    request: MinimizationRequest,
    ws: &crate::memory::WorkingSet,
) -> (crate::memory::WorkingSet, EvidenceDecision) {
    let disclosive = ws.stable_facts.len()
        + ws.preferences.len()
        + ws.commitments.len()
        + ws.recent_events.len()
        + ws.uncertain_beliefs.len();
    let before = disclosive + ws.active_contradictions.len();

    // ACCESS-PROVENANCE ADMISSION (E.SEC10, Codex). Read-isolation may authorise a member turn,
    // but only when the proof TRAVELS: an unstamped set cannot show it was ever narrowed, and an
    // OPERATOR-hydrated set carries a stamp proving the opposite — unfiltered by construction.
    // Both are DENY here. Absence is not permission, and the endpoint identity is never consulted.
    //
    // The owner's own surface is exempt because it IS the operator: there is no slice to prove
    // membership of when the reader is entitled to all of it.
    if policy.scope != OutputScope::OperatorPrivate
        && !ws.provenance.as_ref().is_some_and(|p| p.isolated_to_principal())
    {
        return (
            crate::memory::WorkingSet::default(),
            EvidenceDecision { scope: policy.scope, request, before, admitted: 0, dropped: before, contradictions_kept: 0 },
        );
    }

    // TOTAL PROHIBITION. Nothing survives, contradictions included — there is no answer to keep
    // honest when there is no evidence to be honest about.
    if policy.entity_classes.is_empty() || policy.max_evidence_items == 0 {
        return (
            crate::memory::WorkingSet::default(),
            EvidenceDecision { scope: policy.scope, request, before, admitted: 0, dropped: before, contradictions_kept: 0 },
        );
    }

    // Budget spent most-useful-first, deterministically, so the same turn always yields the same
    // prompt. Stable facts before guesses; contradictions never compete for the budget at all.
    let mut budget = policy.max_evidence_items;
    let mut out = crate::memory::WorkingSet::default();
    fn take<T: Clone>(dst: &mut Vec<T>, src: &[T], budget: &mut usize) {
        let n = src.len().min(*budget);
        dst.extend_from_slice(&src[..n]);
        *budget -= n;
    }
    take(&mut out.stable_facts, &ws.stable_facts, &mut budget);
    take(&mut out.preferences, &ws.preferences, &mut budget);
    take(&mut out.commitments, &ws.commitments, &mut budget);
    take(&mut out.recent_events, &ws.recent_events, &mut budget);
    take(&mut out.uncertain_beliefs, &ws.uncertain_beliefs, &mut budget);
    out.active_contradictions = ws.active_contradictions.clone();

    let admitted = out.stable_facts.len()
        + out.preferences.len()
        + out.commitments.len()
        + out.recent_events.len()
        + out.uncertain_beliefs.len()
        + out.active_contradictions.len();
    (
        out,
        EvidenceDecision {
            scope: policy.scope,
            request,
            before,
            admitted,
            dropped: before - admitted,
            contradictions_kept: ws.active_contradictions.len(),
        },
    )
}

/// How many evidence items a policy permits, and which survive.
///
/// PURE, and deliberately not wired to anything yet: whether the mind ends up FILTERING context or
/// merely INSTRUCTING the model is still an open question with Codex, and both answers need to know
/// which items are disallowed. This is the half that is the same either way.
///
/// # What it can and cannot do, stated plainly
///
/// It enforces the EVIDENCE BUDGET (`max_evidence_items`) and the total prohibition (a policy that
/// permits no entity classes admits nothing). It does NOT filter per class, because deciding that
/// a given sentence names a Person rather than a Project is entity classification — a real
/// problem, unbuilt, and one I am not going to fake with a substring rule. Until it exists, a
/// policy permitting SOME classes is treated as permitting the items it is given, capped.
///
/// That gap is the honest state and is recorded here rather than hidden behind a partial
/// implementation that would look like protection (E.SEC8).
pub fn admitted_evidence<T: Clone>(policy: &OutputPolicy, items: &[T]) -> Vec<T> {
    if policy.entity_classes.is_empty() || policy.max_evidence_items == 0 {
        return Vec::new();
    }
    items.iter().take(policy.max_evidence_items).cloned().collect()
}

/// What the user asked for, in this turn, about disclosure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimizationRequest {
    None,
    /// "answer without examples from my life"
    NoExamples,
    /// "do not reveal private facts", "summarize without naming current tasks"
    NoPrivateFacts,
}

/// Phrases that ask for no concrete private detail at all.
const NO_PRIVATE_FACTS: &[&str] = &[
    "do not reveal", "don't reveal", "dont reveal",
    "do not mention", "don't mention", "dont mention",
    "do not name", "don't name", "dont name",
    "do not share", "don't share", "dont share",
    "do not disclose", "don't disclose", "dont disclose",
    "without revealing", "without naming", "without mentioning", "without disclosing",
    "without specifics", "no specifics", "no private", "nothing private",
    "keep it general", "in general terms", "generic terms",
];

/// Phrases that ask only for no worked illustrations, which is weaker.
const NO_EXAMPLES: &[&str] = &[
    "without examples", "no examples", "without example",
    "don't give examples", "do not give examples", "dont give examples",
    "without illustrations", "no illustrations",
];

/// Openings that mean the user is asking ABOUT disclosure rather than instructing on it.
///
/// "how do I stop revealing private facts?" is a question about the topic. Treating it as an
/// instruction would only cost specificity — the failure is one-directional — but answering a
/// question about privacy with a deliberately vague answer is a bad product, so the obvious cases
/// are excluded.
const TOPIC_QUESTION: &[&str] = &[
    "how do i", "how do you", "how does", "how would i", "how can i",
    "why do", "why does", "what happens if", "what does it mean",
    "explain how", "tell me how", "what is the difference",
];

/// What did the user ask for, about disclosure, in this turn?
///
/// DELIBERATELY LIBERAL. Because [`OutputPolicy::tighten`] can only ever narrow, a false positive
/// costs a more generic answer and a false negative leaves the surface default — so when in doubt
/// this tightens. That asymmetry is the entire licence for matching on text here, and it is why the
/// phrase lists may be added to freely but the monotonicity invariant may not be relaxed (E.SEC8).
pub fn detect_minimization(user_text: &str) -> MinimizationRequest {
    let t = user_text.to_ascii_lowercase();
    let asking_about_it = TOPIC_QUESTION.iter().any(|q| t.trim_start().starts_with(q));
    if asking_about_it {
        return MinimizationRequest::None;
    }
    if NO_PRIVATE_FACTS.iter().any(|m| t.contains(m)) {
        return MinimizationRequest::NoPrivateFacts;
    }
    if NO_EXAMPLES.iter().any(|m| t.contains(m)) {
        return MinimizationRequest::NoExamples;
    }
    MinimizationRequest::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instruction_to_minimize_is_recognised() {
        // The live shape Codex probed with.
        for text in [
            "do not reveal private facts",
            "summarize my posture but do not name current tasks",
            "answer without naming anyone",
            "keep it general please",
            "give me the state in general terms",
            "DO NOT MENTION any of my projects",
        ] {
            assert_eq!(
                detect_minimization(text),
                MinimizationRequest::NoPrivateFacts,
                "should have tightened fully: {text:?}"
            );
        }

        for text in ["answer without examples from my life", "no examples please", "without illustrations"] {
            assert_eq!(detect_minimization(text), MinimizationRequest::NoExamples, "{text:?}");
        }
    }

    #[test]
    fn a_question_about_disclosure_is_not_an_instruction_about_it() {
        // Tightening here would only cost specificity — the failure is one-directional — but
        // answering a question ABOUT privacy with a deliberately vague answer is a bad product.
        for text in [
            "how do i stop revealing private facts",
            "how does the mind decide what to share",
            "why do you not name people sometimes",
            "explain how without naming works",
            "what happens if i say do not reveal private facts",
        ] {
            assert_eq!(detect_minimization(text), MinimizationRequest::None, "topic question: {text:?}");
        }
    }

    #[test]
    fn an_ordinary_turn_is_left_alone() {
        for text in [
            "what is on my calendar tomorrow",
            "run skill csv-sum",
            "summarise the week",
            "",
        ] {
            assert_eq!(detect_minimization(text), MinimizationRequest::None, "{text:?}");
        }
    }

    #[test]
    fn the_detector_can_only_ever_tighten_whatever_it_says() {
        // THE LICENCE, restated as a property over the DETECTOR rather than over `tighten` alone:
        // whatever this returns for any input, applying it never widens the policy. So the phrase
        // lists can be wrong in either direction without opening anything.
        let inputs = [
            "do not reveal private facts",
            "how do i stop revealing private facts",
            "what is on my calendar",
            "no private lane is configured on this box",
            "",
        ];
        for scope in [OutputScope::OperatorPrivate, OutputScope::HouseholdMember, OutputScope::PublicShare] {
            let base = OutputPolicy::for_scope(scope);
            for text in inputs {
                let after = base.tighten(detect_minimization(text));
                assert!(!after.examples_allowed || base.examples_allowed);
                assert!(after.max_evidence_items <= base.max_evidence_items);
                assert!(after.entity_classes.iter().all(|c| base.entity_classes.contains(c)));
            }
        }
    }

    #[test]
    fn a_total_prohibition_admits_nothing_at_all() {
        // The case the live failure needs: operator-private inference, an explicit "do not reveal
        // private facts", and therefore no evidence reaches the answer regardless of how much was
        // recalled. This is the half of slice 4 that is the same whether the policy ends up
        // filtering context or instructing the model.
        let items: Vec<&str> = vec!["a", "b", "c"];
        let asked = OutputPolicy::for_scope(OutputScope::OperatorPrivate)
            .tighten(MinimizationRequest::NoPrivateFacts);
        assert!(admitted_evidence(&asked, &items).is_empty());

        // And a public surface admits nothing before anyone even asks.
        let public = OutputPolicy::for_scope(OutputScope::PublicShare);
        assert!(admitted_evidence(&public, &items).is_empty());
    }

    #[test]
    fn the_evidence_budget_is_enforced() {
        let items: Vec<u8> = (0..50).collect();
        let member = OutputPolicy::for_scope(OutputScope::HouseholdMember);
        let kept = admitted_evidence(&member, &items);
        assert_eq!(kept.len(), member.max_evidence_items, "a member surface is capped");
        assert_eq!(kept[0], 0, "and keeps the highest-ranked, not a random slice");

        let operator = OutputPolicy::for_scope(OutputScope::OperatorPrivate);
        assert_eq!(admitted_evidence(&operator, &items).len(), 50, "the owner is not capped");
    }

    #[test]
    fn per_class_filtering_is_not_implemented_and_does_not_pretend_to_be() {
        // A policy permitting SOME classes admits what it is given, capped. Deciding that a given
        // sentence names a Person rather than a Project is entity classification — unbuilt, and a
        // substring rule would be the fifth fuzzy matcher this week wearing a new hat.
        let items: Vec<&str> = vec!["ZQCANARY-PERSON-4a1 handles the rota"];
        let member = OutputPolicy::for_scope(OutputScope::HouseholdMember);
        assert!(!member.may_name(EntityClass::Account), "the CONTRACT forbids accounts here");
        assert_eq!(
            admitted_evidence(&member, &items).len(),
            1,
            "and the FILTER cannot yet tell what this item names — recorded, not hidden"
        );
    }

    /// The prompt line must stay SECONDARY: silent when nothing is constrained.
    #[test]
    fn an_unconstrained_operator_turn_carries_no_extra_instruction() {
        assert!(
            OutputPolicy::for_scope(OutputScope::OperatorPrivate).prompt_note().is_none(),
            "an ordinary owner turn must read exactly as it did before slice 4"
        );
        // And once anything is asked for, it speaks.
        let asked = OutputPolicy::for_scope(OutputScope::OperatorPrivate)
            .tighten(MinimizationRequest::NoPrivateFacts);
        let note = asked.prompt_note().expect("a total prohibition must be stated");
        assert!(note.contains("ALREADY been withheld"), "it explains an enforced decision, not a request: {note}");
    }

    // ---- E.SEC8 slice 4: the typed gate ----

    fn item(id: &str) -> crate::memory::MemoryItem {
        crate::memory::MemoryItem {
            id: id.into(),
            kind: crate::memory::MemoryKind::Belief,
            text: format!("fact {id}"),
            confidence: 0.9,
            certainty: 0.9,
            updated_ms: 0,
            evidence_count: 1,
        }
    }

    fn contradiction(id: &str) -> crate::memory::Contradiction {
        crate::memory::Contradiction {
            id: id.into(),
            belief_a: "dinner is at seven".into(),
            belief_b: "dinner is at eight".into(),
            severity: 0.8,
            status: "open".into(),
        }
    }

    fn ws(facts: usize, contradictions: usize) -> crate::memory::WorkingSet {
        crate::memory::WorkingSet {
            stable_facts: (0..facts).map(|i| item(&format!("f{i}"))).collect(),
            active_contradictions: (0..contradictions).map(|i| contradiction(&format!("c{i}"))).collect(),
            ..Default::default()
        }
    }

    // ---- E.SEC10: access-provenance admission (Codex's acceptance list) ----

    fn isolated_to(person: &str) -> crate::memory::ReadProvenance {
        crate::memory::ReadProvenance {
            viewer: Some(crate::memory::Scope::Private(person.into())),
            purpose: "conversation".into(),
        }
    }

    fn operator_read() -> crate::memory::ReadProvenance {
        // A truthful stamp that proves the WRONG thing: the operator sees past every scope wall.
        crate::memory::ReadProvenance { viewer: None, purpose: "audit".into() }
    }

    fn stamped(p: Option<crate::memory::ReadProvenance>, facts: usize, contradictions: usize) -> crate::memory::WorkingSet {
        crate::memory::WorkingSet { provenance: p, ..ws(facts, contradictions) }
    }

    /// CODEX CRITERION 3 — missing provenance admits none, even on a member surface.
    #[test]
    fn an_unstamped_set_is_denied_on_a_member_surface() {
        let member = OutputPolicy::for_scope(OutputScope::HouseholdMember);
        let (kept, d) = admit_working_set(&member, MinimizationRequest::None, &stamped(None, 12, 2));
        assert_eq!(d.admitted, 0, "absence of proof is not permission");
        assert_eq!(d.dropped, 14);
        assert!(kept.stable_facts.is_empty());
        // CRITERION 5, first half: budget-exemption is not provenance-exemption.
        assert!(kept.active_contradictions.is_empty(), "an unstamped contradiction is still unproven");
    }

    /// The case the rule exposed that I had not seen: a stamp that proves the WRONG thing.
    #[test]
    fn an_operator_hydrated_set_cannot_authorise_a_member_turn() {
        let member = OutputPolicy::for_scope(OutputScope::HouseholdMember);
        let (_, d) = admit_working_set(&member, MinimizationRequest::None, &stamped(Some(operator_read()), 10, 1));
        assert_eq!(
            d.admitted, 0,
            "an operator read is unfiltered by construction; holding a member endpoint does not narrow it"
        );
    }

    /// A properly isolated read IS admitted, capped. Without this the rule is just "deny always",
    /// which would pass every other test here and make the product useless.
    #[test]
    fn an_isolated_member_read_is_admitted_and_capped() {
        let member = OutputPolicy::for_scope(OutputScope::HouseholdMember);
        let set = stamped(Some(isolated_to("asha")), 30, 3);
        let (kept, d) = admit_working_set(&member, MinimizationRequest::None, &set);
        assert_eq!(kept.stable_facts.len(), member.max_evidence_items, "admitted, and capped");
        assert!(d.admitted > 0, "the whole rule must not collapse to deny-always");
        assert_eq!(kept.active_contradictions.len(), 3, "contradictions still exempt from the BUDGET");
        // CRITERION 4: deterministic.
        let (again, _) = admit_working_set(&member, MinimizationRequest::None, &set);
        assert_eq!(kept.stable_facts.len(), again.stable_facts.len());
        assert_eq!(kept.stable_facts[0].id, again.stable_facts[0].id);
    }

    /// CODEX CRITERION 2 — public and audit admit none regardless of how good the provenance is.
    #[test]
    fn public_and_audit_admit_nothing_however_well_proven() {
        for scope in [OutputScope::PublicShare, OutputScope::AuditRedacted] {
            let p = OutputPolicy::for_scope(scope);
            let (kept, d) = admit_working_set(&p, MinimizationRequest::None, &stamped(Some(isolated_to("asha")), 9, 2));
            assert_eq!(d.admitted, 0, "{scope:?} names nothing before anyone asks");
            assert!(kept.active_contradictions.is_empty());
        }
    }

    /// The owner's own surface stays transparent — provenance is not required to read your own life.
    #[test]
    fn the_owner_is_not_asked_to_prove_membership_of_their_own_slice() {
        let owner = OutputPolicy::for_scope(OutputScope::OperatorPrivate);
        let (_, d) = admit_working_set(&owner, MinimizationRequest::None, &stamped(None, 20, 2));
        assert_eq!(d.dropped, 0, "there is no slice to prove membership of when the reader owns all of it");
    }

    /// KILL CRITERION 1 — the gate must be TRANSPARENT on the owner's own surface.
    #[test]
    fn an_operator_private_turn_admits_everything_it_hydrated() {
        let set = ws(40, 3);
        let policy = OutputPolicy::for_scope(OutputScope::OperatorPrivate);
        let (out, d) = admit_working_set(&policy, MinimizationRequest::None, &set);
        assert_eq!(d.dropped, 0, "filtering the owner's own surface is a bug, not caution");
        assert_eq!(d.admitted, d.before);
        assert_eq!(out.stable_facts.len(), 40);
        assert_eq!(out.active_contradictions.len(), 3);
    }

    /// KILL CRITERION 2 — total prohibition takes everything, contradictions included.
    #[test]
    fn a_total_prohibition_admits_nothing_not_even_a_contradiction() {
        let set = ws(12, 2);
        // The live failure's exact shape: operator-private, plus "do not reveal private facts".
        let policy = OutputPolicy::for_scope(OutputScope::OperatorPrivate)
            .tighten(MinimizationRequest::NoPrivateFacts);
        let (out, d) = admit_working_set(&policy, MinimizationRequest::NoPrivateFacts, &set);
        assert_eq!(d.admitted, 0);
        assert_eq!(d.dropped, 14, "all twelve facts and both contradictions");
        assert!(out.stable_facts.is_empty() && out.active_contradictions.is_empty());

        // And a surface that names nothing before anyone asks.
        let public = OutputPolicy::for_scope(OutputScope::PublicShare);
        let (out, d) = admit_working_set(&public, MinimizationRequest::None, &ws(5, 1));
        assert_eq!(d.admitted, 0);
        assert!(out.active_contradictions.is_empty());
    }

    /// KILL CRITERION 3 — the one way this filter could make an answer DISHONEST rather than vaguer.
    ///
    /// A contradiction is not a disclosure; it is "ASK, do not assert either side". If the budget
    /// could evict it while facts survived, a PRIVACY filter would have licensed the mind to assert
    /// a contested fact with no idea it was contested.
    #[test]
    fn the_budget_can_never_evict_a_contradiction_while_facts_survive() {
        let member = OutputPolicy::for_scope(OutputScope::HouseholdMember);
        assert!(member.max_evidence_items < 30, "this test needs the budget to actually bite");

        // STAMPED: this test is about the BUDGET, not about provenance. Before E.SEC10 an
        // unstamped set was admitted by default, so this read as a budget test while quietly
        // relying on admit-by-default. The new rule broke it, which is the rule working.
        let set = stamped(Some(isolated_to("asha")), 30, 4);
        let (out, d) = admit_working_set(&member, MinimizationRequest::None, &set);

        assert_eq!(out.stable_facts.len(), member.max_evidence_items, "facts ARE capped");
        assert!(d.dropped > 0, "and the cap really bit");
        assert_eq!(
            out.active_contradictions.len(),
            4,
            "every contradiction survives a budget that evicted facts"
        );
        assert_eq!(d.contradictions_kept, 4);
    }

    /// KILL CRITERION 4 — telemetry is structural, and the TYPE proves it.
    ///
    /// `Copy` cannot hold a `String`, so a decision record can never carry evidence text. This is a
    /// compile-time proof rather than a promise in a comment, which matters because the promise is
    /// the sort that erodes the first time someone wants "just the one field" for debugging.
    #[test]
    fn a_decision_record_cannot_carry_content() {
        fn assert_copy<T: Copy>() {}
        assert_copy::<EvidenceDecision>();
    }

    #[test]
    fn tightening_can_never_widen() {
        // THE INVARIANT. Everything else in this module is only safe because of it: an imperfect
        // detector that can only narrow costs specificity when it is wrong, and cannot open
        // anything that was shut.
        let scopes = [
            OutputScope::OperatorPrivate,
            OutputScope::HouseholdMember,
            OutputScope::PublicShare,
            OutputScope::AuditRedacted,
        ];
        let requests = [
            MinimizationRequest::None,
            MinimizationRequest::NoExamples,
            MinimizationRequest::NoPrivateFacts,
        ];
        for scope in scopes {
            let base = OutputPolicy::for_scope(scope);
            for req in requests {
                let after = base.tighten(req);
                assert!(!after.examples_allowed || base.examples_allowed, "examples were re-opened");
                assert!(after.max_evidence_items <= base.max_evidence_items, "evidence budget grew");
                for class in &after.entity_classes {
                    assert!(base.entity_classes.contains(class), "{class:?} was not permitted before");
                }
                assert!(after.scope >= base.scope, "scope became more permissive");
            }
        }
    }

    #[test]
    fn a_public_surface_names_nothing_before_anyone_asks() {
        for scope in [OutputScope::PublicShare, OutputScope::AuditRedacted] {
            let p = OutputPolicy::for_scope(scope);
            assert!(!p.examples_allowed);
            assert_eq!(p.max_evidence_items, 0);
            for class in EntityClass::ALL {
                assert!(!p.may_name(*class), "{class:?} must not be nameable on {}", scope.label());
            }
            assert!(scope.fails_closed());
        }
    }

    #[test]
    fn the_operator_surface_is_permissive_but_still_obeys_the_turn() {
        // The live failure: operator-private inference, and an explicit instruction not to reveal
        // private facts. The scope stays operator-private — the owner is entitled to the content —
        // but the turn's own instruction empties what may be named.
        let base = OutputPolicy::for_scope(OutputScope::OperatorPrivate);
        assert!(base.may_name(EntityClass::Task) && base.examples_allowed);

        let asked = base.tighten(MinimizationRequest::NoPrivateFacts);
        assert_eq!(asked.scope, OutputScope::OperatorPrivate, "the SCOPE does not change; the permission does");
        assert!(!asked.examples_allowed);
        assert_eq!(asked.max_evidence_items, 0);
        for class in EntityClass::ALL {
            assert!(!asked.may_name(*class), "{class:?} survived an explicit minimization request");
        }
        // Diagnostic, not fail-closed, on the owner's own surface — Codex's call.
        assert!(!OutputScope::OperatorPrivate.fails_closed());
    }

    #[test]
    fn a_member_sees_household_life_and_not_the_operators_accounts() {
        let p = OutputPolicy::for_scope(OutputScope::HouseholdMember);
        assert!(p.may_name(EntityClass::Person) && p.may_name(EntityClass::Event));
        assert!(!p.may_name(EntityClass::Account), "a member surface must not name accounts");
        assert!(!p.may_name(EntityClass::Purchase));
        assert!(OutputScope::HouseholdMember.fails_closed());
    }

    #[test]
    fn combining_policies_is_order_independent_and_takes_the_stricter() {
        let operator = OutputPolicy::for_scope(OutputScope::OperatorPrivate);
        let public = OutputPolicy::for_scope(OutputScope::PublicShare);
        let a = operator.tighten_to(&public);
        let b = public.tighten_to(&operator);
        assert_eq!(a, b, "combining must not depend on which side you start from");
        assert_eq!(a.scope, OutputScope::PublicShare, "the stricter scope wins");
        assert_eq!(a.max_evidence_items, 0);
        assert!(a.entity_classes.is_empty());
    }

    #[test]
    fn no_examples_is_weaker_than_no_private_facts_and_both_only_narrow() {
        let base = OutputPolicy::for_scope(OutputScope::OperatorPrivate);
        let no_examples = base.tighten(MinimizationRequest::NoExamples);
        let no_private = base.tighten(MinimizationRequest::NoPrivateFacts);

        assert!(!no_examples.examples_allowed);
        assert!(no_examples.may_name(EntityClass::Task), "declining EXAMPLES is not declining to name anything");
        assert!(!no_private.may_name(EntityClass::Task), "declining private facts empties the classes");
        assert!(no_private.max_evidence_items <= no_examples.max_evidence_items);
    }
}
