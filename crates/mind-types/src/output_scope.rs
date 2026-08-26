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

/// What the user asked for, in this turn, about disclosure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MinimizationRequest {
    None,
    /// "answer without examples from my life"
    NoExamples,
    /// "do not reveal private facts", "summarize without naming current tasks"
    NoPrivateFacts,
}

#[cfg(test)]
mod tests {
    use super::*;

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
