//! Purpose Gate v1 (docs/VISION_ONE_MIND_2026-08-17.md) — purpose sits at the READ boundary,
//! not only the egress boundary. Every wall before this one answered "who can see this?"
//! (`Scope`) and "can this leave?" (`EgressBroker`). Neither answered "may this be used for
//! THIS?" — a private fact used internally can be a violation without ever leaving owned
//! hardware: Alice's fact optimizing Bob's convenience, health facts seasoning gift smalltalk.
//!
//! The design, as jointly ratified:
//! - Every read DECLARES a purpose (audit always). The declaration is carried inside
//!   `AccessContext`, so the compiler forces every call site to say what the read serves.
//! - Read-denial is scoped to OWNER crossing and SENSITIVITY crossing — not total purpose
//!   permission. The CONNECT behavior (the birthday answer carrying the gift plan) is the
//!   product and it lives: facts the primary stored are the primary's own memory of the
//!   world, so same-owner ordinary reads stay default-permissive.
//! - Cross-owner use (a fact confided by X used in work serving Y ≠ X) requires a standing
//!   grant — default deny. Sensitive classes carry purpose policies — default deny outside
//!   their allowed activities. Household/shared is its own subject class.
//! - Grants are explicit, expiring, revocable, and never widen a `Principal`'s viewing
//!   scope: viewer isolation stays supreme; grants only open the operator's background
//!   lanes (proactive/dream/research/…), which are otherwise owner-locked to who they serve.
//!
//! Like `Scope::visible_to`, the policy here is pure and deterministic — no LLM in the loop,
//! monotonic toward denial (an activity outside the allowed set can never widen access).

use serde::{Deserialize, Serialize};

use crate::memory::PRIMARY;

/// A subject in the purpose economy: whose facts (data owner) or whose benefit
/// (beneficiary of the work). The household itself is a first-class subject —
/// a shared fact belongs to the household, never forced to pretend it is one
/// person's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Subject {
    /// A household member, by member id (see `PRIMARY`).
    Member(String),
    /// The household's shared subject class.
    Household,
}

impl Subject {
    /// The primary member (the companion's owner).
    pub fn primary() -> Subject {
        Subject::Member(PRIMARY.to_string())
    }
    /// Storage form: "household" or "member:<id>".
    pub fn as_tag(&self) -> String {
        match self {
            Subject::Household => "household".into(),
            Subject::Member(m) => format!("member:{m}"),
        }
    }
    pub fn parse(tag: &str) -> Subject {
        match tag.strip_prefix("member:") {
            Some(m) => Subject::Member(m.to_string()),
            None => Subject::Household,
        }
    }
    /// The data OWNER of a stored item, derived from its visibility scope tag
    /// ("shared" | "private:<owner>" | untagged/legacy). Ownership here is
    /// scope-ownership: a fact that entered through X's private channel is X's;
    /// what X shared with the household is the household's; what the primary
    /// stored is the primary's own memory of the world (which is why the
    /// CONNECT behavior survives the gate).
    pub fn owner_of_scope_tag(stored: Option<&str>) -> Subject {
        match stored {
            None => Subject::primary(), // legacy/untagged → the primary's own memory
            Some(tag) => match tag.strip_prefix("private:") {
                Some(o) => Subject::Member(o.to_string()),
                None => Subject::Household,
            },
        }
    }
    /// The visibility scope a beneficiary could see for themselves — how a
    /// declared purpose downgrades an operator-lane transcript read to "what
    /// the person this work serves could read".
    pub fn as_viewer_scope(&self) -> crate::memory::Scope {
        match self {
            Subject::Member(m) => crate::memory::Scope::Private(m.clone()),
            Subject::Household => crate::memory::Scope::Shared,
        }
    }
}

/// What kind of work is reading. Declared at the call site, never inferred.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Activity {
    /// Replying to the current speaker in a live turn (incl. the voice path and
    /// in-turn helpers like calendar/support enrichment).
    Conversation,
    /// Self-initiated scans and outreach: proactive digests, anticipation, emissary.
    Proactive,
    /// Research passes (live web research, night research).
    Research,
    /// Nightly dreaming/ideation.
    Dream,
    /// Foresight/prediction work.
    Foresight,
    /// The coder and self-build pipeline.
    CodeWork,
    /// The recipe / sub-agent host.
    Recipe,
    /// The operator's own console, evals, export, verification — full
    /// visibility, always receipted.
    Audit,
    /// Substrate hygiene: consolidation, dedup, migration. Reads the store to
    /// maintain it, not to use facts for a beneficiary.
    Maintenance,
}

impl Activity {
    pub fn as_tag(&self) -> &'static str {
        match self {
            Activity::Conversation => "conversation",
            Activity::Proactive => "proactive",
            Activity::Research => "research",
            Activity::Dream => "dream",
            Activity::Foresight => "foresight",
            Activity::CodeWork => "code",
            Activity::Recipe => "recipe",
            Activity::Audit => "audit",
            Activity::Maintenance => "maintenance",
        }
    }
    pub fn parse(tag: &str) -> Option<Activity> {
        Some(match tag {
            "conversation" => Activity::Conversation,
            "proactive" => Activity::Proactive,
            "research" => Activity::Research,
            "dream" => Activity::Dream,
            "foresight" => Activity::Foresight,
            "code" => Activity::CodeWork,
            "recipe" => Activity::Recipe,
            "audit" => Activity::Audit,
            "maintenance" => Activity::Maintenance,
            _ => return None,
        })
    }
}

/// Sensitivity classes carrying purpose policies — default deny outside their
/// allowed activities, whoever the fact belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sensitivity {
    Ordinary,
    Health,
    Finance,
    /// Codes, keys, combinations. The write-gate keeps marker-shaped secrets out
    /// of the moat entirely; this class catches the human-shaped ones ("the
    /// garage code is…") that pass it. Answers its owner in direct conversation
    /// (that's the product), never seasons background work, and a wildcard-class
    /// grant deliberately does NOT cover it — opening credentials takes an
    /// explicit credentials grant.
    Credentials,
}

impl Sensitivity {
    pub fn as_tag(&self) -> &'static str {
        match self {
            Sensitivity::Ordinary => "ordinary",
            Sensitivity::Health => "health",
            Sensitivity::Finance => "finance",
            Sensitivity::Credentials => "credentials",
        }
    }
    pub fn parse(tag: &str) -> Sensitivity {
        match tag {
            "health" => Sensitivity::Health,
            "finance" => Sensitivity::Finance,
            "credentials" => Sensitivity::Credentials,
            _ => Sensitivity::Ordinary,
        }
    }

    /// May this class hydrate WITHOUT a standing grant, for work serving the
    /// fact's own owner? (Audit/Maintenance short-circuit before this is asked.)
    /// Sensitive classes answer the person directly but never season background
    /// work — health facts must not flavor gift smalltalk, and a code must not
    /// ride into a research prompt.
    pub fn allowed_without_grant(&self, activity: Activity) -> bool {
        match self {
            Sensitivity::Ordinary => true,
            Sensitivity::Health | Sensitivity::Finance | Sensitivity::Credentials => {
                matches!(activity, Activity::Conversation)
            }
        }
    }

    /// Deterministic write-time classifier (read-time fallback for legacy rows).
    /// Conservative by design: over-classifying only narrows background lanes
    /// (fail-closed); it can never widen access. Stems match anywhere in a word;
    /// short ambiguous words match on word boundaries only.
    pub fn classify(text: &str) -> Sensitivity {
        let lower = text.to_lowercase();
        // Word-boundary form: non-alphanumerics collapsed to single spaces, padded.
        let mut bounded = String::with_capacity(lower.len() + 2);
        bounded.push(' ');
        let mut prev_space = false;
        for c in lower.chars() {
            if c.is_alphanumeric() {
                bounded.push(c);
                prev_space = false;
            } else if !prev_space {
                bounded.push(' ');
                prev_space = true;
            }
        }
        if !bounded.ends_with(' ') {
            bounded.push(' ');
        }
        let has_word = |w: &str| bounded.contains(&format!(" {w} "));
        let has_stem = |s: &str| lower.contains(s);

        const CREDENTIAL_STEMS: &[&str] = &["password", "passcode", "passphrase", "credential", "ssh key", "private key", "api key", "access code", "door code", "gate code", "garage code", "safe combination", "security code", "one-time code", "recovery code"];
        const CREDENTIAL_WORDS: &[&str] = &["pin", "otp", "2fa", "cvv"];
        if CREDENTIAL_STEMS.iter().any(|s| has_stem(s)) || CREDENTIAL_WORDS.iter().any(|w| has_word(w)) {
            return Sensitivity::Credentials;
        }
        const HEALTH_STEMS: &[&str] = &["diagnos", "oncolog", "cancer", "chemo", "surgery", "medicat", "prescri", "symptom", "hospital", "illness", "disease", "depress", "anxiety", "pregnan", "allerg", "diabet", "asthma", "migraine", "therap", "psychiatr"];
        const HEALTH_WORDS: &[&str] = &["doctor", "clinic"];
        if HEALTH_STEMS.iter().any(|s| has_stem(s)) || HEALTH_WORDS.iter().any(|w| has_word(w)) {
            return Sensitivity::Health;
        }
        const FINANCE_STEMS: &[&str] = &["salary", "mortgage", "paycheck", "net worth", "bank account", "account balance", "credit card", "invest", "savings", "401k", "iban"];
        const FINANCE_WORDS: &[&str] = &["loan", "debt", "tax", "taxes", "income"];
        if FINANCE_STEMS.iter().any(|s| has_stem(s)) || FINANCE_WORDS.iter().any(|w| has_word(w)) {
            return Sensitivity::Finance;
        }
        Sensitivity::Ordinary
    }
}

/// The declared purpose of a read: which subject the work SERVES, through what
/// activity. Carried inside `AccessContext`, so no read happens without one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Purpose {
    pub serves: Subject,
    pub activity: Activity,
}

impl Purpose {
    pub fn new(serves: Subject, activity: Activity) -> Purpose {
        Purpose { serves, activity }
    }
    /// A live turn serving the speaking member.
    pub fn conversation(member_id: &str) -> Purpose {
        Purpose::new(Subject::Member(member_id.to_string()), Activity::Conversation)
    }
    /// The operator's console/evals/verification lane.
    pub fn audit() -> Purpose {
        Purpose::new(Subject::primary(), Activity::Audit)
    }
    /// Substrate hygiene (consolidation/dedup/migration).
    pub fn maintenance() -> Purpose {
        Purpose::new(Subject::primary(), Activity::Maintenance)
    }
    /// A background lane serving the primary (proactive/dream/research/…).
    pub fn serving_primary(activity: Activity) -> Purpose {
        Purpose::new(Subject::primary(), activity)
    }
    /// Receipt label, e.g. "proactive→member:primary".
    pub fn label(&self) -> String {
        format!("{}→{}", self.activity.as_tag(), self.serves.as_tag())
    }
    /// True for the lanes that see everything (and are always receipted).
    pub fn is_unrestricted_lane(&self) -> bool {
        matches!(self.activity, Activity::Audit | Activity::Maintenance)
    }
}

/// A standing purpose grant — the ONLY way a cross-owner or out-of-policy
/// sensitive-class read opens. Explicit, expiring, revocable, auditable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurposeGrant {
    pub id: i64,
    /// Whose facts this grant releases.
    pub owner: Subject,
    /// Whose work may use them.
    pub beneficiary: Subject,
    /// Which sensitivity class it covers; None = any class.
    pub class: Option<Sensitivity>,
    /// Which activity it covers; None = any activity.
    pub activity: Option<Activity>,
    /// Hard expiry (unix ms). A grant without an end is a policy, not a grant.
    pub expires_ms: u64,
    /// Why it exists — the audit story ("gift planning for Alice's birthday").
    pub note: String,
    pub revoked: bool,
    pub created_ms: u64,
}

/// What a caller supplies to create a grant (id/created/revoked are the store's).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurposeGrantSpec {
    pub owner: Subject,
    pub beneficiary: Subject,
    pub class: Option<Sensitivity>,
    pub activity: Option<Activity>,
    pub expires_ms: u64,
    pub note: String,
}

impl PurposeGrant {
    /// Does this grant cover reading a fact of (`owner`, `sensitivity`) for `purpose`, now?
    /// A wildcard class (None) covers everything EXCEPT credentials — opening a
    /// credential fact takes a grant that names the class explicitly.
    pub fn covers(&self, purpose: &Purpose, owner: &Subject, sensitivity: Sensitivity, now_ms: u64) -> bool {
        !self.revoked
            && now_ms < self.expires_ms
            && self.owner == *owner
            && self.beneficiary == purpose.serves
            && self.class.map_or(sensitivity != Sensitivity::Credentials, |c| c == sensitivity)
            && self.activity.map_or(true, |a| a == purpose.activity)
    }
}

/// THE purpose predicate: may an item owned by `owner` with class `sensitivity`
/// hydrate into work declared as `purpose`? `granted` = some unexpired,
/// unrevoked grant covers this exact crossing (see `PurposeGrant::covers`).
///
/// Monotonic toward denial: outside Audit/Maintenance, nothing about the
/// activity or the query text can widen access — only a standing grant can.
pub fn purpose_allows(purpose: &Purpose, owner: &Subject, sensitivity: Sensitivity, granted: bool) -> bool {
    if purpose.is_unrestricted_lane() {
        return true; // the operator's own audit + hygiene lanes (always receipted)
    }
    if granted {
        return true;
    }
    let same_owner = match owner {
        Subject::Household => true, // household facts serve any household member's work
        m => *m == purpose.serves,
    };
    same_owner && sensitivity.allowed_without_grant(purpose.activity)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(m: &str) -> Subject {
        Subject::Member(m.to_string())
    }

    #[test]
    fn same_owner_ordinary_is_default_permissive() {
        // The CONNECT behavior lives: the primary's own memory serves the primary's work, every lane.
        for activity in [Activity::Conversation, Activity::Proactive, Activity::Research, Activity::Dream, Activity::Foresight, Activity::CodeWork, Activity::Recipe] {
            assert!(purpose_allows(&Purpose::serving_primary(activity), &Subject::primary(), Sensitivity::Ordinary, false), "{activity:?}");
        }
    }

    #[test]
    fn cross_owner_is_default_deny_every_activity() {
        let alice_fact = member("alice");
        for activity in [Activity::Conversation, Activity::Proactive, Activity::Research, Activity::Dream, Activity::Foresight, Activity::CodeWork, Activity::Recipe] {
            let p = Purpose::new(Subject::primary(), activity);
            assert!(!purpose_allows(&p, &alice_fact, Sensitivity::Ordinary, false), "Alice's fact must not serve primary's {activity:?} without a grant");
        }
        // …and the mirror image: primary's fact must not serve Alice's work.
        let p = Purpose::conversation("alice");
        assert!(!purpose_allows(&p, &Subject::primary(), Sensitivity::Ordinary, false));
    }

    #[test]
    fn household_facts_serve_household_work() {
        assert!(purpose_allows(&Purpose::conversation("alice"), &Subject::Household, Sensitivity::Ordinary, false));
        assert!(purpose_allows(&Purpose::serving_primary(Activity::Proactive), &Subject::Household, Sensitivity::Ordinary, false));
        // …but a member's private fact does not serve "the household's" work by default.
        let p = Purpose::new(Subject::Household, Activity::Recipe);
        assert!(!purpose_allows(&p, &member("alice"), Sensitivity::Ordinary, false));
    }

    #[test]
    fn sensitive_classes_answer_directly_but_never_season_background_work() {
        // Sensitive facts: allowed in direct conversation with their owner ("what's my
        // garage code?" is the product)…
        for class in [Sensitivity::Health, Sensitivity::Finance, Sensitivity::Credentials] {
            assert!(purpose_allows(&Purpose::conversation(PRIMARY), &Subject::primary(), class, false), "{class:?}");
        }
        // …denied for gift smalltalk / proactive / dream — the vision's own example.
        for activity in [Activity::Proactive, Activity::Dream, Activity::Research, Activity::Foresight, Activity::CodeWork, Activity::Recipe] {
            for class in [Sensitivity::Health, Sensitivity::Finance, Sensitivity::Credentials] {
                assert!(!purpose_allows(&Purpose::serving_primary(activity), &Subject::primary(), class, false), "{activity:?}/{class:?}");
            }
        }
    }

    #[test]
    fn wildcard_grants_never_cover_credentials() {
        let blanket = PurposeGrant {
            id: 7,
            owner: member("alice"),
            beneficiary: Subject::primary(),
            class: None, // "anything of Alice's" — deliberately NOT credentials
            activity: None,
            expires_ms: u64::MAX,
            note: "blanket".into(),
            revoked: false,
            created_ms: 0,
        };
        let p = Purpose::serving_primary(Activity::Proactive);
        assert!(blanket.covers(&p, &member("alice"), Sensitivity::Health, 1));
        assert!(!blanket.covers(&p, &member("alice"), Sensitivity::Credentials, 1));
        // Naming the class explicitly does open it.
        let explicit = PurposeGrant { class: Some(Sensitivity::Credentials), ..blanket };
        assert!(explicit.covers(&p, &member("alice"), Sensitivity::Credentials, 1));
    }

    #[test]
    fn audit_and_maintenance_see_everything() {
        for purpose in [Purpose::audit(), Purpose::maintenance()] {
            assert!(purpose_allows(&purpose, &member("alice"), Sensitivity::Credentials, false));
            assert!(purpose_allows(&purpose, &Subject::Household, Sensitivity::Health, false));
        }
    }

    #[test]
    fn grants_open_exactly_their_crossing_and_expire() {
        let grant = PurposeGrant {
            id: 1,
            owner: member("alice"),
            beneficiary: Subject::primary(),
            class: None,
            activity: Some(Activity::Proactive),
            expires_ms: 1_000,
            note: "gift planning".into(),
            revoked: false,
            created_ms: 0,
        };
        let p = Purpose::serving_primary(Activity::Proactive);
        assert!(grant.covers(&p, &member("alice"), Sensitivity::Ordinary, 999));
        assert!(purpose_allows(&p, &member("alice"), Sensitivity::Ordinary, true));
        // Expiry is a hard edge; revocation wins; other activities/owners stay closed.
        assert!(!grant.covers(&p, &member("alice"), Sensitivity::Ordinary, 1_000));
        assert!(!grant.covers(&Purpose::serving_primary(Activity::Dream), &member("alice"), Sensitivity::Ordinary, 999));
        assert!(!grant.covers(&p, &member("bob"), Sensitivity::Ordinary, 999));
        let revoked = PurposeGrant { revoked: true, ..grant };
        assert!(!revoked.covers(&p, &member("alice"), Sensitivity::Ordinary, 999));
    }

    #[test]
    fn classifier_is_conservative_and_deterministic() {
        assert_eq!(Sensitivity::classify("Pranab's oncology appointment is on July 18"), Sensitivity::Health);
        assert_eq!(Sensitivity::classify("the garage code is 4921"), Sensitivity::Credentials);
        assert_eq!(Sensitivity::classify("her PIN is 0042"), Sensitivity::Credentials);
        assert_eq!(Sensitivity::classify("monthly mortgage payment went up"), Sensitivity::Finance);
        assert_eq!(Sensitivity::classify("prefers terse replies in the morning"), Sensitivity::Ordinary);
        // Boundary words do not fire inside other words: "pinned"/"taxonomy" are ordinary.
        assert_eq!(Sensitivity::classify("pinned the tab about taxonomy"), Sensitivity::Ordinary);
    }

    #[test]
    fn owner_derivation_matches_scope_semantics() {
        assert_eq!(Subject::owner_of_scope_tag(None), Subject::primary());
        assert_eq!(Subject::owner_of_scope_tag(Some("shared")), Subject::Household);
        assert_eq!(Subject::owner_of_scope_tag(Some("private:alice")), member("alice"));
        assert_eq!(Subject::parse(&member("alice").as_tag()), member("alice"));
        assert_eq!(Subject::parse(&Subject::Household.as_tag()), Subject::Household);
    }
}
