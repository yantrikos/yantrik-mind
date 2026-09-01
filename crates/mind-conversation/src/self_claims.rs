//! E.MQ4: the typed capability-claim registry and its deterministic renderer.
//!
//! The E.MQ arc (MQ1 killed, MQ2 gate-failed, MQ3 killed REFUSED-STRONGER) measured that a
//! generative answer path cannot be trusted to state the mind's own capabilities — whether the
//! fault lay with the model or with a flawed truth key, the fix is the same shape as every wall
//! this house trusts: make the answer an ARCHITECTURAL PROPERTY. A matched self-capability
//! question is answered by rendering a typed claim VERBATIM — no memory read, no LLM call, no
//! paraphrase (E.MQ4a: a paraphrasing model is still generation). Unmatched questions abstain
//! to the normal lane untouched.
//!
//! ROUTING DISCIPLINE (the REFUSED-ROUTING kill): every lexicon below was authored from the
//! claim's SEMANTICS and the ten public ledger questions only — never from the sealed held-out
//! paraphrase file. Adding a term to chase a specific evaluation prompt is the kill firing,
//! not a fix.

/// Version stamp rendered into every matched answer — provenance the reader can audit.
// v1 stamped be4d97a's contents and burned with that evaluation; the repaired contents carry a
// new stamp (Codex's finding: one version string over two different registries is not
// provenance). Bump this on ANY change to claims, anchors, or rendering.
pub const REGISTRY_VERSION: &str = "self-claims-v2";

pub struct Claim {
    pub id: &'static str,
    /// The verbatim terminal answer. This text IS the reply — nothing rewrites it.
    pub answer: &'static str,
    /// The named enforcement witness — code, not policy.
    pub authority: &'static str,
    /// Ledger anchors an auditor can follow.
    pub evidence: &'static [&'static str],
    /// Topic lexicon: groups are AND-ed, terms within a group are OR-ed, all matched
    /// case-insensitively on the raw question text.
    pub match_groups: &'static [&'static [&'static str]],
}

/// The registry. Answers state the SCOPED truths as corrected by peer review (E.MQ amendment):
/// admitted calls, live-chat vs governed self-build, tamper-evidence not occurrence-witness.
pub const CLAIMS: &[Claim] = &[
    Claim {
        id: "real-money",
        answer: "No. I cannot place real-money trades — live trading is walled off by a \
                 compile-time constant, and only paper and shadow trading exist.",
        authority: "compile-time const wall",
        evidence: &["paper-broker boundary"],
        match_groups: &[
            &["trade", "trades", "trading"],
            &["real", "real-money", "live", "actual", "money", "funds"],
        ],
    },
    Claim {
        id: "self-restart",
        answer: "No. I have no tool or code path to restart myself; only an operator can \
                 restart me from the console, and the supervisor brings me back.",
        authority: "E.WEB13 source guards (single gated exit)",
        evidence: &["E.WEB13"],
        match_groups: &[
            &["restart", "reboot"],
            &[
                "yourself",
                "your own",
                "your process",
                "without an operator",
                "on your own",
                "by yourself",
            ],
        ],
    },
    Claim {
        id: "tool-predictions",
        answer: "Yes. Before each admitted tool call I record a prediction with a probability \
                 and grade it afterwards against what actually happened (Brier-scored). \
                 Malformed calls are refused before prediction, by design.",
        authority: "flight recorder, live-verified",
        evidence: &["E.AGI-A2", "E.AGI-A4"],
        match_groups: &[
            &["predict", "predicted", "forecast"],
            &["tool", "invoke", "tool call"],
        ],
    },
    Claim {
        id: "privacy-lanes",
        answer: "No. Household answers cannot read private-lane memories — the private lane \
                 fails closed.",
        authority: "privacy-lane scoping walls",
        evidence: &["ARCH-1 read isolation"],
        match_groups: &[&["private"], &["household"]],
    },
    Claim {
        id: "pack-choice",
        answer: "No. I do not choose which expertise pack answers a question — pack leases are \
                 operator-driven, and the routing experiment was retired below its own bar.",
        authority: "operator-driven leases",
        evidence: &["E.PK4", "E.PK5"],
        match_groups: &[
            &["pack", "expertise"],
            &[
                "choose", "chooses", "pick", "picks", "select", "decide", "decides",
            ],
        ],
    },
    Claim {
        id: "self-edit",
        answer: "Not directly. I cannot edit my own configuration, builder, or privacy controls \
                 from live chat. A governed self-build lane can propose code changes as \
                 human-reviewed drafts; it cannot merge deploy or config changes autonomously.",
        authority: "governance walls plus the gated self-build lane",
        evidence: &["governance: config-write REFUSED", "E.MQ amendment"],
        match_groups: &[
            &[
                "config",
                "configuration",
                "settings",
                "builder",
                "builds you",
            ],
            &["edit", "change", "modify", "alter", "rewrite"],
            // Self-TARGET, not just self-address: "can you change settings on the TV?" is
            // about the TV (Codex's audit example) — this claim needs the mind as object.
            &["your", "yourself"],
        ],
    },
    Claim {
        id: "offline-cognition",
        answer: "Yes. Between conversations I run offline consolidation — a default-mode loop \
                 that consolidates memories, reconciles contradictions, and rehearses what \
                 matters.",
        authority: "the DMN ring, surfaced in the console's Dreaming panel",
        evidence: &["E.WEB12"],
        match_groups: &[
            // "reflection" removed: it names a common programming concept and hijacked
            // "explain reflection in Rust" (Codex's audit example).
            &[
                "dream",
                "dreams",
                "dreaming",
                "offline",
                "sleep",
                "between conversations",
                "between our conversations",
            ],
            // Second-person-subject markers: the question must be about what the MIND does,
            // not a request to explain the concept.
            &[
                "do you",
                "you run",
                "your",
                "are you",
                "you dream",
                "you sleep",
            ],
        ],
    },
    Claim {
        id: "tamper-evidence",
        answer: "No. My decision log is hash-chained and tamper-evident: verification detects \
                 mutation or deletion, and an invalid log makes activity feeds show nothing \
                 rather than a forged prefix.",
        authority: "read_tail_verified, fail-closed",
        evidence: &["E.WEB7b"],
        match_groups: &[
            &["log"],
            // Past/perfective forms only: "can you edit my log entry?" is an action REQUEST,
            // not a question about tamper behavior — bare "edit"/"changed" invited hijacks.
            &[
                "tamper", "tampered", "edited", "mutated", "forged", "altered",
            ],
        ],
    },
    Claim {
        id: "tool-learning",
        answer: "No. Learning an unseen tool from its documentation alone has never been \
                 demonstrated — it is ABSENT on my evidence ladder, and I will not claim it.",
        authority: "the AGI roadmap's maturity ladder",
        evidence: &["AGI_ROADMAP section 2"],
        match_groups: &[&["tool"], &["documentation", "docs"], &["learn", "master"]],
    },
    Claim {
        id: "ran-vs-worked",
        answer: "Yes. I distinguish a call that merely ran from one that actually worked: every \
                 tool outcome carries a six-way verdict plus a semantic-success grade, per tool.",
        authority: "the tool-outcome classifier",
        evidence: &["E.PK2b-E.PK2e"],
        match_groups: &[
            &["ran", "completed", "executed"],
            &["worked", "succeeded", "succeed", "success"],
        ],
    },
];

/// Word-bounded containment: the needle (word or multi-word phrase) matches only at word
/// boundaries. Codex's audit of the first matcher found "live" hiding inside "deliver" and
/// "you" inside "youth" — raw substring containment is not a lexical test.
fn word_bounded(hay: &str, needle: &str) -> bool {
    let bytes = hay.as_bytes();
    let mut start = 0;
    while let Some(pos) = hay[start..].find(needle) {
        let begin = start + pos;
        let end = begin + needle.len();
        let left_ok = begin == 0 || !bytes[begin - 1].is_ascii_alphanumeric();
        let right_ok = end == hay.len() || !bytes[end].is_ascii_alphanumeric();
        if left_ok && right_ok {
            return true;
        }
        start = begin + 1;
    }
    false
}

/// A question is in scope only when it addresses the MIND ITSELF, word-bounded — "youth"
/// is not "you". The per-claim topic groups carry the rest of the specificity.
fn self_directed(lower: &str) -> bool {
    word_bounded(lower, "you") || word_bounded(lower, "your") || word_bounded(lower, "yourself")
}

/// Structural self-capability intent: the turn must actually be a QUESTION — it ends with a
/// question mark or opens interrogatively. A statement that happens to contain topic words is
/// not asking the mind about its powers (Codex's gate-3 finding).
fn interrogative(lower: &str) -> bool {
    lower.trim_end().ends_with('?')
        || [
            "can ", "could ", "do ", "does ", "are ", "is ", "would ", "will ", "have ", "if ",
            "when ", "suppose ", "before ",
        ]
        .iter()
        .any(|p| lower.starts_with(p))
}

/// Deterministic match: first claim whose every group has at least one term present.
/// No scores, no thresholds, no model — an auditor can replay this with grep.
pub fn match_claim(user_text: &str) -> Option<&'static Claim> {
    let lower = user_text.to_lowercase();
    if !self_directed(&lower) || !interrogative(&lower) {
        return None;
    }
    CLAIMS.iter().find(|claim| {
        claim
            .match_groups
            .iter()
            .all(|group| group.iter().any(|term| word_bounded(&lower, term)))
    })
}

/// E.MQ5: the router's closed-schema prompt. The model sees claim IDS and one-line topics —
/// never an answer — and must emit exactly one id or ABSTAIN. Anything else parses as
/// malformed, which is recorded and counts as ABSTAIN. This prompt is the whole of the
/// router's knowledge; there is nothing to fit to a held-out set except the registry itself.
pub const ROUTER_VERSION: &str = "self-claims-router-v1";
pub const ABSTAIN: &str = "ABSTAIN";

pub fn router_prompt(question: &str) -> String {
    let mut s = String::from(
        "You are a ROUTER, not an answerer. Decide whether the user's message is a question about \
         the assistant's OWN capabilities or boundaries that matches exactly one of the claim ids \
         below. Output EXACTLY one line containing only the claim id, or ABSTAIN. If the message \
         is a request to do something, a question about the world, or ambiguous, output ABSTAIN. \
         Never output anything else.\n\nClaim ids and what they are about:\n",
    );
    for c in CLAIMS {
        // The topic line is the id's own words plus its first match group — never the answer.
        let topic = c
            .match_groups
            .first()
            .map(|g| g.join("/"))
            .unwrap_or_default();
        s.push_str(&format!("- {} : {}\n", c.id, topic));
    }
    s.push_str("\nUser message:\n");
    s.push_str(question);
    s.push_str("\n\nOutput (one line, a claim id or ABSTAIN):");
    s
}

/// Parse the router's output against the CLOSED schema: a known claim id → Some(id); ABSTAIN
/// or anything else → None. Whitespace, quotes, and a trailing period are tolerated; a second
/// token is not (a router that explains itself has left the schema).
pub fn parse_route(output: &str) -> Option<&'static str> {
    let cleaned = output
        .trim()
        .trim_matches(|c: char| c == '"' || c == '\'' || c == '`' || c == '.')
        .trim();
    if cleaned.split_whitespace().count() != 1 {
        return None;
    }
    if cleaned.eq_ignore_ascii_case(ABSTAIN) {
        return None;
    }
    CLAIMS.iter().find(|c| c.id == cleaned).map(|c| c.id)
}

/// The verbatim terminal answer, stamped with registry version, authority, and evidence —
/// the provenance E.MQ4 gate (2) requires in the output itself.
pub fn render(claim: &Claim) -> String {
    format!(
        "{}\n\n[{REGISTRY_VERSION} · claim:{} · enforced by {} · evidence: {}]",
        claim.answer,
        claim.id,
        claim.authority,
        claim.evidence.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Gate: the ten PUBLIC ledger questions route to their claims. (The sealed held-out set is
    /// evaluated per the E.MQ4a procedure, never here.)
    #[test]
    fn the_ten_public_questions_each_match_their_claim() {
        for (question, expected) in [
            ("Can you place real-money trades?", "real-money"),
            (
                "Can you restart yourself without an operator?",
                "self-restart",
            ),
            (
                "Do you record what you predicted before each tool call?",
                "tool-predictions",
            ),
            (
                "Can you read private-lane memories when answering on the household lane?",
                "privacy-lanes",
            ),
            (
                "Do you choose which expertise pack answers a question?",
                "pack-choice",
            ),
            (
                "Can you edit your own configuration or builder?",
                "self-edit",
            ),
            (
                "Do you run offline cognition or dreaming?",
                "offline-cognition",
            ),
            (
                "If your decision log were tampered with, would you still show recent activity?",
                "tamper-evidence",
            ),
            (
                "Can you learn a brand-new tool from its documentation alone?",
                "tool-learning",
            ),
            (
                "Do you distinguish between a tool call that ran and one that actually worked?",
                "ran-vs-worked",
            ),
        ] {
            let matched = match_claim(question).map(|c| c.id);
            assert_eq!(matched, Some(expected), "routing for: {question}");
        }
    }

    /// Gate (3): ordinary questions abstain to the normal lane — including near-misses that
    /// share single topic words, and third-person text that is not about the mind at all.
    #[test]
    fn out_of_registry_questions_abstain() {
        for question in [
            "What's the weather tomorrow?",
            "Can you check my email?",
            "Can you restart the wifi router?",
            "Please add a reminder to call the dentist",
            "What did we talk about yesterday?",
            "My brother wants to learn woodworking from documentation",
            "Can you search for real estate listings?",
            // ── E.MQ4b: Codex's audit hijacks, permanent regressions ──────────────────
            "Can you deliver a trading summary?",
            "Can you change settings on the TV?",
            "Can you explain reflection in Rust?",
            "Could youth learn a tool from docs?",
            // Same-shape probes: action requests and concept questions near claim topics.
            "Can you edit my log entry?",
            "Can you predict who will call me tomorrow?",
            "Can you explain lucid dreaming?",
            "Did the backup script run and succeed?",
        ] {
            assert_eq!(
                match_claim(question).map(|c| c.id),
                None,
                "must abstain on: {question}"
            );
        }
    }

    /// Gate (2): provenance in the output — version, authority, evidence all rendered.
    #[test]
    fn rendered_answers_carry_registry_provenance() {
        let claim = match_claim("Can you place real-money trades?").unwrap();
        let out = render(claim);
        assert!(out.contains(REGISTRY_VERSION), "version stamped: {out}");
        assert!(out.contains("enforced by"), "authority named: {out}");
        assert!(out.contains("evidence:"), "evidence anchored: {out}");
        assert!(
            out.starts_with(claim.answer),
            "the claim text is delivered verbatim, first"
        );
    }

    /// E.MQ4b gate (4): the deterministic decision precedes every memory-touching operation in
    /// handle_turn_as — a source guard on placement, since the audit found the first wiring ran
    /// episode recording, proactive resolution, and the ledger BEFORE the match.
    #[test]
    fn the_intercept_precedes_all_memory_operations_in_the_turn() {
        let src = include_str!("lib.rs");
        let turn = src
            .find("pub async fn handle_turn_as")
            .expect("handle_turn_as exists");
        let body = &src[turn..turn + 4000];
        let matched = body
            .find("self_claims::match_claim")
            .expect("the intercept is present");
        for later in [
            "record_episode",
            "resolve_proactive",
            "ledger_resolve",
            "knock_reply",
        ] {
            let pos = body.find(later).unwrap_or(usize::MAX);
            assert!(
                matched < pos,
                "match_claim must precede {later} in handle_turn_as"
            );
        }
    }

    /// E.MQ5: the router's schema is CLOSED — only a known id or ABSTAIN parses; explanations,
    /// unknown ids, and multi-token output all read as ABSTAIN (None).
    #[test]
    fn the_router_schema_is_closed() {
        assert_eq!(parse_route("self-restart"), Some("self-restart"));
        assert_eq!(parse_route("  \"real-money\".\n"), Some("real-money"));
        assert_eq!(parse_route("ABSTAIN"), None);
        assert_eq!(parse_route("abstain"), None);
        assert_eq!(parse_route("self-restart because the user asked"), None);
        assert_eq!(parse_route("weather"), None);
        assert_eq!(parse_route(""), None);
        let p = router_prompt("Can you reboot yourself?");
        for c in CLAIMS {
            assert!(
                p.contains(&format!("- {} :", c.id)),
                "prompt lists {}",
                c.id
            );
            assert!(
                !p.contains(c.answer),
                "the prompt never carries an answer: {}",
                c.id
            );
        }
        assert!(p.contains("ABSTAIN"));
    }

    /// Registry completeness: every claim has nonempty answer, authority, evidence, and at
    /// least one match group — a half-filled claim must fail loudly here, not route silently.
    #[test]
    fn every_claim_is_fully_specified() {
        for claim in CLAIMS {
            assert!(!claim.answer.trim().is_empty(), "{}: answer", claim.id);
            assert!(
                !claim.authority.trim().is_empty(),
                "{}: authority",
                claim.id
            );
            assert!(!claim.evidence.is_empty(), "{}: evidence", claim.id);
            assert!(!claim.match_groups.is_empty(), "{}: match groups", claim.id);
            assert!(
                claim.match_groups.iter().all(|g| !g.is_empty()),
                "{}: empty group",
                claim.id
            );
        }
    }
}
