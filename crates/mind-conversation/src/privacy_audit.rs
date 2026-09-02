//! privacy_audit — a source-level guard on the private-inference boundary.
//!
//! The privacy kernel has a hole no counter can see. `chat_grounded` routes a turn to the owned
//! private lane and, when a lane exists but fails, FAILS CLOSED rather than escalating private text
//! to cloud. An unscoped `inference.chat(...)` does none of that: it silently takes the Household
//! (cloud) lane and — critically — never touches `PRIVACY_ESCALATED`. So a private-carrying call
//! written as a bare `chat()` leaks *and reads as clean on the dashboard*. That is exactly how the
//! DMN leak survived for months, and how `work_radar_run` shipped 40 verbatim user messages to a
//! cloud provider on a daily timer while the counter read zero.
//!
//! Counters measure the paths that ASK for privacy. This test measures the paths that don't.
//!
//! It is deliberately a SOURCE scan rather than a type-level ban: making `chat()` unavailable would
//! break the legitimately-public callers, and a `#[deny]` lint can't express "this prompt carries
//! household memory". A pinned allowlist forces the question to be answered — and re-answered in
//! review — every time a new bare call appears.

/// Files permitted to call the unscoped `inference.chat(...)`, each with the reason it is NOT
/// private-grounded. Adding a file here is a PRIVACY DECISION: state why the prompt cannot carry
/// household memory. If it can, use `chat_grounded` instead.
/// Keyed by CRATE-RELATIVE PATH, never by basename. Codex found the hole: `SCANNED` covers three
/// crates and `lib.rs` exists in nineteen, so a bare `"lib.rs"` entry silenced
/// `mind-agents/src/lib.rs` and `mind-recipes/src/lib.rs` along with the one it was written for —
/// and `mind-agents/src/lib.rs` is exactly where E.SEC2 grounded the sub-agent synthesis call. The
/// guard had gone blind to the very file it was protecting (E.SEC5).
/// Individual call sites that are not inference calls at all, identified by PATH + CALL SHAPE +
/// a hash of their surrounding lines.
///
/// Per-FILE allowlisting is too coarse here: `mind-agents/src/lib.rs` holds a test-double backend
/// AND the real sub-agent synthesis call that E.SEC2 grounded. Listing the file would silently
/// re-open the very call that audit closed.
///
/// # This is SITE-SPECIFIC permission, not SEMANTIC permission
///
/// An entry says "this exact call, in this exact place, is not an inference call". It does NOT say
/// "calls that look like this are fine". Codex's challenge to the first version was that exact
/// squashed source is brittle AND spoofable: failing closed on formatting drift is not the same as
/// being hard to imitate, and a second `self.chat(messages, config, tools)?` pasted anywhere in the
/// same file would have inherited the allow for free.
///
/// So the identity is three things — crate-relative PATH, the normalized CALL SHAPE, and a hash of
/// the lines AROUND it. A copy of the same call elsewhere has different neighbours and is not
/// allowed; an edit to the call or its context stops the match and the guard fires. A test asserts
/// the first of those properties directly, because "surely a hash would differ" is a guess.
const ALLOWED_SITES: &[AllowedSite] = &[AllowedSite {
    file: "mind-agents/src/lib.rs",
    shape: "letr=self.chat(messages,config,tools)?;",
    context: 0x6703_1dad_646d_819b,
    why: "a #[cfg(test)] LLMBackend double that pops canned responses from a queue — it reaches no \
          provider and carries no household data. Its `chat` is the trait method, not an inference call.",
}];

pub(crate) struct AllowedSite {
    pub file: &'static str,
    pub shape: &'static str,
    pub context: u64,
    pub why: &'static str,
}

/// Does this squashed source ride the HOUSEHOLD lane under a name that is not `chat(`?
///
/// Extracted so it can be tested against synthetic source rather than by editing real files. The
/// by-hand mutation that first proved this works is not a test — it lives in a transcript, not in
/// CI, and Codex asked for the permanent version.
pub(crate) fn is_household_lane_call(squashed: &str) -> bool {
    squashed.contains("PrivacyScope::Household)") || squashed.contains("chat_streaming_sink(")
}

/// Whitespace-stripped source, the form every comparison here uses.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// A hash of the lines AROUND a call — what makes two identical calls in one file distinguishable.
///
/// Deliberately excludes the call line itself: that is `shape`, and folding it in would mean an
/// entry could not say which of the two identities failed to match.
pub(crate) fn context_hash(lines: &[&str], idx: usize) -> u64 {
    use std::hash::{Hash, Hasher};
    const SPAN: usize = 3;
    let lo = idx.saturating_sub(SPAN);
    let hi = (idx + SPAN + 1).min(lines.len());
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (n, l) in lines[lo..hi].iter().enumerate() {
        if lo + n == idx {
            continue;
        }
        squash(l).hash(&mut h);
    }
    h.finish()
}

/// Is this specific call site allowlisted? PATH, SHAPE and CONTEXT must all agree.
pub(crate) fn site_is_allowed(file: &str, lines: &[&str], idx: usize) -> bool {
    let shape = squash(lines[idx]);
    ALLOWED_SITES
        .iter()
        .any(|a| a.file == file && shape.contains(a.shape) && context_hash(lines, idx) == a.context)
}

/// Deliberate Household callers that now use `chat_household_attributed`, each with the reason the
/// prompt is eligible for that lane. Unlike the old per-file `UNSCOPED_ALLOWED`, these files are
/// NOT skipped by `no_new_unscoped_inference_calls`: adding a bare `.chat(` beside an attributed
/// call is still a build failure. A separate test below proves every attributed call supplies a
/// code-authored `module_path!()` identity and that no unlisted file can introduce the API.
const ATTRIBUTED_HOUSEHOLD: &[(&str, &str)] = &[
    (
        "mind-conversation/src/egress_planning.rs",
        "MUST stay unscoped BY DESIGN: the ARCH-3 egress-clean planner re-authors outbound tool args \
         in a clean room that has never seen private memory. Grounding it would defeat its purpose.",
    ),
    (
        "mind-conversation/src/news.rs",
        "public-web facts — the prompt carries a topic string and fetched headlines, no household memory.",
    ),
    (
        "mind-conversation/src/festivals.rs",
        "public calendar facts (lunar-calendar festival dates); no household memory in the prompt.",
    ),
    (
        "mind-conversation/src/research.rs",
        "public-web research; the prompt carries the research query and fetched sources.",
    ),
    (
        "mind-conversation/src/code.rs",
        "the REMAINING calls carry venture/codebase task text only (PRD drafting, code Q&A over a \
         studied repo). The two that did read the household substrate — work_radar_run (40 verbatim \
         user messages) and paper_ask (belief-store output) — are on chat_grounded as of the \
         2026-07-25 sweep. Any NEW call here that touches self.memory must be grounded.",
    ),
];

/// Kept as a distinct, intentionally empty category so a future genuinely-unattributed exception
/// is conspicuous in review. Prefer `chat_household_attributed` + `ATTRIBUTED_HOUSEHOLD`.
const UNSCOPED_ALLOWED: &[(&str, &str)] = &[];

/// Files with unscoped calls that the WRAPPING-PROOF scan found on 2026-08-26, whose disposition is
/// NOT YET DECIDED (E.SEC2).
///
/// These are NOT allowlisted. `UNSCOPED_ALLOWED` means "we looked, and this prompt cannot carry
/// household memory". This list means "we looked, and it probably CAN — a human has to choose".
/// Keeping them apart matters: folding them into the allowlist would record a privacy decision
/// nobody made, which is worse than the original blindness because it would look settled.
///
/// Why they were invisible: the guard matched `inference.chat(` one line at a time, and rustfmt
/// wraps the chain as `.inference` / `.chat(...)`. Every call below has been taking the household
/// (cloud) lane silently, uncounted, for as long as this guard has existed.
///
/// Why they are not simply converted: this box HAS a private lane (`YM_PRIVATE_PROVIDERS=
/// ollama-local`, qwen3.8:27b), so `chat_grounded` would move them off the cloud model AND fail
/// closed when the local endpoint is down. That is a capability trade on features in daily use —
/// the owner's call, not the auditor's.
/// Files that DECLARE `PrivacyScope::Household` deliberately, and whose declaration is now an open
/// question rather than an accident (E.SEC12).
///
/// A different class from `UNSCOPED_PENDING`, which held calls that defaulted to Household by
/// SAYING NOTHING. Every entry here is someone choosing Household on purpose. The audit could not
/// see any of them for as long as it has existed, because its pattern excludes `chat_scoped(` —
/// correct for `chat_grounded(`, the private lane, and wrong for its Household sibling.
///
/// Listing a file here is NOT approval. It records that the declaration exists, that it has been
/// read, and what the unresolved question is. The sweep is E.SEC12's second half and needs
/// decisions that are not mine to make alone — compose in particular is on the live path for every
/// turn, and `chat_grounded` fails closed, so re-laning it trades a conditional cloud failover for
/// a hard outage whenever the owned cluster is down. Pranab has ruled on exactly that trade once
/// already, for the main turn.
/// One reviewed lane decision, machine-readable (Codex, E.SEC16).
///
/// Deliberately a small const record and NOT a runtime type: a one-entry inventory does not justify
/// production abstraction, and Codex said so when I offered to build one. The value here is
/// reviewability — every field a reviewer would otherwise have to reconstruct from prose is a
/// column, and the `test` field means a claim without a test is visibly a claim without a test.
pub(crate) struct LaneDecision {
    pub file: &'static str,
    /// The call shape that made the audit notice it.
    pub call_shape: &'static str,
    /// What lane it rides now.
    pub lane: &'static str,
    /// WHY that lane is correct — the invariant, not a vibe.
    pub invariant: &'static str,
    /// What happens when that lane is unavailable.
    pub fallback: &'static str,
    /// The test that holds it.
    pub test: &'static str,
}

/// Files the audit still flags, each with a reviewed decision behind it.
const HOUSEHOLD_DECLARED: &[LaneDecision] = &[LaneDecision {
    file: "mind-conversation/src/lib.rs",
    call_shape: "chat_streaming_sink(messages, cfg, tok_tx, scope)",
    lane: "Private (constant)",
    invariant:
        "compose's inputs are a SUPERSET of the loop dispatch's, and dispatch already ran on \
                the private lane via chat_grounded_tools — so a weaker compose lane would send \
                material this same turn already treated as private. Listed only because the helper \
                is matched BY NAME: it gates internally, so no call-site rule can read its scope.",
    fallback: "COMPOSE_LANE_UNAVAILABLE, a const status line; never a Household retry",
    test: "compose_can_never_ride_a_weaker_lane_than_the_dispatch_that_preceded_it",
}];

const UNSCOPED_PENDING: &[(&str, &str)] = &[
    // EMPTY, and that is the finding rather than the absence of one.
    //
    // mail, finance, calendar and briefing were struck first; E.SEC9 struck foresight, onboarding,
    // studio, plugins_mod, skills and every lib.rs site including `handle_turn_as`'s main
    // composition -- the largest of them, grounded once Pranab chose fail-closed-but-honest over
    // both a hard outage and a silent cloud failover.
    //
    // An empty list means the guard now defends the whole crate with no deferrals: any new
    // `inference.chat(` must either be grounded or justify itself in UNSCOPED_ALLOWED. Keep it
    // empty. A name added back here is a deferral, and deferrals are how the four originals
    // survived as long as they did.
];

/// Crates whose sources are scanned. These are the ones that hold an `InferencePool` and can reach
/// household memory; the inference crate itself defines the primitives and is exempt.
const SCANNED: &[&str] = &["mind-conversation", "mind-recipes", "mind-agents"];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn crates_dir() -> PathBuf {
        // CARGO_MANIFEST_DIR = <repo>/crates/mind-conversation
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }

    /// Every inventory entry must carry a real decision, not a placeholder (E.SEC16).
    ///
    /// The prose version of this list drifted twice tonight — an entry that said "deliberate future
    /// sweep" outlived the sweep. Fields make that visible: an entry with no invariant or no test
    /// is a deferral wearing a decision's clothes.
    #[test]
    fn every_lane_decision_names_its_invariant_and_its_test() {
        for d in HOUSEHOLD_DECLARED {
            assert!(
                d.file.contains("/src/"),
                "{}: a crate-relative path",
                d.file
            );
            assert!(!d.call_shape.is_empty(), "{}: name the call shape", d.file);
            assert!(
                d.lane.contains("Private")
                    || d.lane.contains("Household")
                    || d.lane.contains("Public"),
                "{}: state the lane",
                d.file
            );
            assert!(
                d.invariant.len() > 60,
                "{}: an invariant, not a shrug",
                d.file
            );
            assert!(
                !d.fallback.is_empty(),
                "{}: say what happens when the lane is gone",
                d.file
            );
            assert!(d.test.len() > 10, "{}: name the test that holds it", d.file);
        }
    }

    /// An exception without a rationale is permission that cannot be reviewed. Keep the reason
    /// machine-checked so future allowlist entries cannot silence the guard with an empty label.
    #[test]
    fn every_allowed_site_explains_why_it_is_safe() {
        for site in ALLOWED_SITES {
            assert!(
                site.file.contains("/src/"),
                "{}: use a crate-relative source path",
                site.file
            );
            assert!(
                !site.shape.is_empty(),
                "{}: identify the exact call shape",
                site.file
            );
            assert!(
                site.why.len() > 60,
                "{}: explain the safety invariant, not merely that the site is allowed",
                site.file
            );
        }
    }

    fn has_channel_consumer(squashed_source: &str, variant: &str) -> bool {
        ["admits", "grounding.push", "messages.evidence"]
            .iter()
            .any(|boundary| {
                squashed_source.contains(&format!("{boundary}(Channel::{variant}"))
                    || squashed_source
                        .contains(&format!("{boundary}(mind_types::Channel::{variant}"))
            })
    }

    /// The audit must CATCH Household-by-another-name, and must not catch its siblings (E.SEC14).
    ///
    /// Codex asked for this as a permanent mutation test rather than the by-hand one I ran once.
    /// Synthetic source, so it proves the RULE rather than the current state of a file — and it
    /// keeps proving it after someone edits `lib.rs`.
    #[test]
    fn the_audit_catches_household_under_its_other_names() {
        let squash = |s: &str| -> String { s.chars().filter(|c| !c.is_whitespace()).collect() };

        // CAUGHT: the two spellings that hid ten call sites for as long as the guard existed.
        for caught in [
            "self.inference.chat_scoped(msgs, cfg, mind_inference::PrivacyScope::Household)",
            ".chat_scoped(vec![ChatMessage::user(&p)], cfg, PrivacyScope::Household)",
            "self.inference.chat_streaming_sink(messages, cfg, tok_tx, scope)",
        ] {
            assert!(
                is_household_lane_call(&squash(caught)),
                "must be caught: {caught}"
            );
        }

        // NOT CAUGHT: the private lane, which is the whole point of distinguishing them.
        for safe in [
            "self.inference.chat_grounded(messages, cfg)",
            "self.inference.chat_grounded_tools(messages, cfg, schemas)",
            "self.inference.chat_scoped(m, c, PrivacyScope::Private)",
        ] {
            assert!(
                !is_household_lane_call(&squash(safe)),
                "must NOT be caught: {safe}"
            );
        }

        // NOT CAUGHT: Public. Declaring content public is a different claim from letting household
        // content ride the household lane, and `emissary.rs` makes both kinds of call.
        assert!(
            !is_household_lane_call(&squash(
                "chat_scoped(m, c, mind_inference::PrivacyScope::Public)"
            )),
            "a Public declaration is not a Household leak"
        );
    }

    /// A COPY of an allowlisted call, elsewhere, must NOT inherit the allow (Codex, E.SEC9).
    ///
    /// The first version keyed a site on its exact squashed source alone. That fails closed on
    /// formatting drift — any edit stops the match — but Codex's point was that failing closed is
    /// not the same as being hard to IMITATE: a second `self.chat(messages, config, tools)?`
    /// pasted anywhere in the same file would have inherited the permission for free, which is
    /// semantic permission wearing a site-specific label.
    ///
    /// Identity is now path + call shape + a hash of the surrounding lines, and this asserts the
    /// part that actually does the work rather than assuming "surely a hash would differ".
    #[test]
    fn an_identical_call_with_different_neighbours_is_a_different_site() {
        let allowed: Vec<&str> = vec![
            "impl LLMBackend for Fake {",
            "    fn chat_streaming(&self, m: &[ChatMessage]) -> Result<LLMResponse> {",
            "        // the test double",
            "        let r = self.chat(messages, config, tools)?;",
            "        on_token(&r.text);",
            "        Ok(r)",
            "    }",
        ];
        // Same line, byte for byte. Different company.
        let impostor: Vec<&str> = vec![
            "async fn synthesise(&self, task: &str) -> String {",
            "    let messages = self.build_household_prompt(task);",
            "    // looks identical, lives somewhere else entirely",
            "        let r = self.chat(messages, config, tools)?;",
            "    plain_prose(&r.text)",
            "}",
            "",
        ];
        assert_eq!(
            allowed[3], impostor[3],
            "the test is only meaningful if the LINES are identical"
        );
        assert_ne!(
            context_hash(&allowed, 3),
            context_hash(&impostor, 3),
            "a copy of an allowlisted call must not inherit its permission"
        );
    }

    /// ...and the identity must be STABLE, or the allowlist would need rewriting on every build.
    #[test]
    fn the_same_site_hashes_the_same_way_twice() {
        let lines: Vec<&str> = vec![
            "a();",
            "b();",
            "let r = self.chat(m, c, t)?;",
            "d();",
            "e();",
        ];
        assert_eq!(context_hash(&lines, 2), context_hash(&lines, 2));
        // Indentation and spacing are normalised away: reformatting is not a revocation, but any
        // change to WHAT the neighbours are is.
        let reindented: Vec<&str> = vec![
            "  a();",
            "\tb();",
            "let r = self.chat(m, c, t)?;",
            " d();",
            "  e();",
        ];
        assert_eq!(
            context_hash(&lines, 2),
            context_hash(&reindented, 2),
            "whitespace is not identity"
        );
        let moved: Vec<&str> = vec![
            "a();",
            "b();",
            "let r = self.chat(m, c, t)?;",
            "CHANGED();",
            "e();",
        ];
        assert_ne!(
            context_hash(&lines, 2),
            context_hash(&moved, 2),
            "but the neighbours are"
        );
    }

    /// EVERY declared channel must have a consumer, and no gate may be open-coded (E.CTX2).
    ///
    /// Codex blocked E.CTX1 on b3eb32f7 and was right on every count. Two findings live here.
    ///
    /// FIRST: five variants had ZERO consumer calls. A declared channel nothing routes through is
    /// decorative — the enum claimed coverage the code did not have, and the exhaustive match made
    /// that look rigorous. An inventory test is the only thing that catches a variant nobody uses.
    ///
    /// SECOND: my previous guard banned exactly ONE spelling, `policy.entity_classes.is_empty()`,
    /// and two gates survived it by computing the policy inline and breaking the line differently.
    /// That is the fourteenth instance of this session's one error — matching a spelling and calling
    /// it coverage — and it happened INSIDE the guard written to end that very pattern. So the
    /// check is now on the squashed, comment-stripped source, which no line break can defeat.
    #[test]
    fn every_channel_has_a_consumer_and_no_gate_is_open_coded() {
        let types_src = std::fs::read_to_string(
            crates_dir()
                .join("mind-types")
                .join("src")
                .join("output_scope.rs"),
        )
        .expect("output_scope.rs must be readable");
        let conv_dir = crates_dir().join("mind-conversation").join("src");
        let mut conv_files = Vec::new();
        rs_files(&conv_dir, &mut conv_files);
        // `tests.rs` and this audit module are not prompt producers; letting their test-only
        // references satisfy the inventory would turn the guard decorative again (and this module
        // necessarily contains the banned needles it searches for). Inline comments are stripped
        // for the same reason. All prompt-capable source modules are included so a new builder
        // outside lib.rs cannot sit beyond the open-coding scan.
        let conv_src = conv_files
            .into_iter()
            .filter(|path| {
                !matches!(
                    path.file_name().and_then(|n| n.to_str()),
                    Some("tests.rs" | "privacy_audit.rs")
                )
            })
            .map(|path| {
                let body = std::fs::read_to_string(&path)
                    .unwrap_or_else(|e| panic!("{} must be readable: {e}", path.display()));
                crate::source_audit::strip_comments(&body)
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Every variant declared between `pub enum Channel {` and its closing brace.
        let body = types_src
            .split_once("pub enum Channel {")
            .expect("Channel enum must exist")
            .1;
        let body = body.split_once("\n}").expect("enum must close").0;
        let variants: Vec<String> = body
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty() && !l.starts_with("//") && l.ends_with(','))
            .map(|l| l.trim_end_matches(',').to_string())
            .filter(|l| l.chars().next().is_some_and(|c| c.is_ascii_uppercase()))
            .collect();
        assert!(
            variants.len() >= 13,
            "expected the full channel inventory, found {variants:?}"
        );

        let squashed: String = conv_src.chars().filter(|c| !c.is_whitespace()).collect();
        for v in &variants {
            assert!(
                has_channel_consumer(&squashed, v),
                "Channel::{v} is declared but no production call routes it through an approved \
                 typed insertion boundary. A bare enum reference is not a consumer: it can live in \
                 an unrelated tuple or match and make the inventory look complete. Wire an admits, \
                 GatedGrounding::push, or GatedPrompt::evidence boundary at its insertion site, or \
                 delete the variant."
            );
        }

        // No open-coded gate, checked on SQUASHED source so a line break cannot hide one — which is
        // exactly how two survived the previous version of this test.
        for banned in [
            "letnames_anything=",
            "letprivate_channels=",
            ".entity_classes.is_empty()",
        ] {
            assert!(
                !squashed.contains(banned),
                "an open-coded channel gate is back ({banned}). Every channel decision goes through \
                 `OutputPolicy::admits(Channel::…)` so the inventory test can see it; a local \
                 boolean or an inline policy build is invisible to both the compiler and this check."
            );
        }
    }

    #[test]
    fn a_bare_channel_reference_cannot_fake_an_inventory_consumer() {
        let variant = "SavedSkills";
        assert!(!has_channel_consumer(
            "letinventory=[mind_types::Channel::SavedSkills];",
            variant
        ));
        assert!(has_channel_consumer(
            "policy.admits(mind_types::Channel::SavedSkills)",
            variant
        ));
        assert!(has_channel_consumer(
            "policy.admits(Channel::SavedSkills)",
            variant
        ));
        assert!(has_channel_consumer(
            "grounding.push(mind_types::Channel::SavedSkills,\"x\")",
            variant
        ));
        assert!(has_channel_consumer(
            "messages.evidence(Channel::SavedSkills,message)",
            variant
        ));
    }

    #[test]
    fn the_agent_grounding_buffer_enforces_the_channel_at_insertion() {
        let policy = mind_types::OutputPolicy::for_scope(mind_types::OutputScope::AuditRedacted);
        let mut grounding = crate::GatedGrounding::new(&policy);
        grounding.push(mind_types::Channel::Grounding, "private");
        grounding.push(mind_types::Channel::MetacogNote, "safe");
        assert_eq!(grounding.finish(), "safe");
    }

    #[test]
    fn a_withheld_voice_transcript_cannot_steer_a_followup_lookup() {
        let policy = mind_types::OutputPolicy::for_scope(mind_types::OutputScope::AuditRedacted);
        let mut transcript = crate::GatedGrounding::new(&policy);
        transcript.push(
            mind_types::Channel::Transcript,
            "assistant: Want me to pull the Nifty 50 to compare?",
        );
        let admitted = transcript.finish();
        let resolver_context: Vec<String> = admitted.lines().map(str::to_string).collect();

        assert!(
            admitted.is_empty(),
            "the transcript itself must be withheld"
        );
        assert!(
            mind_tools::asked::symbols_with_context("yes please", &resolver_context).is_empty(),
            "withheld transcript still steered a deterministic follow-up lookup"
        );
    }

    #[test]
    fn turn_grounding_has_no_untyped_append_escape_hatch() {
        let lib = std::fs::read_to_string(
            crates_dir()
                .join("mind-conversation")
                .join("src")
                .join("lib.rs"),
        )
        .expect("lib.rs must be readable");
        let body = lib
            .split_once("async fn turn_grounding(")
            .expect("turn_grounding must exist")
            .1
            .split_once("#[deny(unreachable_code)]")
            .expect("agent_loop boundary must remain visible")
            .0;
        let squashed: String = crate::source_audit::strip_comments(body)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            squashed.contains("letmutgrounding=GatedGrounding::new(&policy);"),
            "turn_grounding must build through the typed insertion boundary"
        );
        for escape in ["grounding.push_str(", "grounding.rendered"] {
            assert!(
                !squashed.contains(escape),
                "turn_grounding bypasses its typed insertion boundary via {escape}"
            );
        }
    }

    #[test]
    fn fast_reply_has_no_untyped_grounding_escape_hatch() {
        let lib = std::fs::read_to_string(
            crates_dir()
                .join("mind-conversation")
                .join("src")
                .join("lib.rs"),
        )
        .expect("lib.rs must be readable");
        let body = lib
            .split_once("pub async fn fast_reply(")
            .expect("fast_reply must exist")
            .1
            .split_once("// ESCALATE RATHER THAN REFUSE.")
            .expect("fast_reply generation boundary must remain visible")
            .0;
        let squashed: String = crate::source_audit::strip_comments(body)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            squashed.contains("letmutgrounding=GatedGrounding::new(&policy);"),
            "fast_reply must build voice evidence through the typed insertion boundary"
        );
        for escape in ["grounding.push_str(", "grounding.rendered"] {
            assert!(
                !squashed.contains(escape),
                "fast_reply bypasses its typed insertion boundary via {escape}"
            );
        }
    }

    #[test]
    fn the_legacy_prompt_buffer_gates_evidence_but_keeps_trusted_context() {
        let policy = mind_types::OutputPolicy::for_scope(mind_types::OutputScope::AuditRedacted);
        let mut prompt = crate::GatedPrompt::new(&policy, "persona");
        prompt.trusted_system("policy");
        prompt.evidence(
            mind_types::Channel::Grounding,
            yantrik_ml::ChatMessage::system("private"),
        );
        prompt.evidence(
            mind_types::Channel::Grounding,
            yantrik_ml::ChatMessage::user("private-user-role"),
        );
        prompt.evidence(
            mind_types::Channel::Grounding,
            yantrik_ml::ChatMessage::assistant("private-assistant-role"),
        );
        prompt.evidence(
            mind_types::Channel::WebPage,
            yantrik_ml::ChatMessage::system("public"),
        );
        let messages = prompt.finish("request");
        let contents: Vec<&str> = messages.iter().map(|m| m.content.as_str()).collect();
        let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(contents, ["persona", "policy", "public", "request"]);
        assert_eq!(
            roles,
            ["system", "system", "system", "user"],
            "trusted context and admitted evidence must remain upstream of the current user turn"
        );
    }

    #[test]
    fn build_prompt_has_no_untyped_message_escape_hatch() {
        let lib = std::fs::read_to_string(
            crates_dir()
                .join("mind-conversation")
                .join("src")
                .join("lib.rs"),
        )
        .expect("lib.rs must be readable");
        let body = lib
            .split_once("fn build_prompt(")
            .expect("build_prompt must exist")
            .1
            .split_once("/// Pull an explicitly-taught fact")
            .expect("build_prompt boundary must remain visible")
            .0;
        let squashed: String = crate::source_audit::strip_comments(body)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            squashed.contains("letmutmessages=GatedPrompt::new(policy,&self.persona);"),
            "build_prompt must build through the typed message boundary"
        );
        for escape in ["messages.push(", "messages.messages"] {
            assert!(
                !squashed.contains(escape),
                "build_prompt bypasses its typed message boundary via {escape}"
            );
        }
    }

    #[test]
    fn member_turn_has_no_untyped_message_escape_hatch() {
        let members = std::fs::read_to_string(
            crates_dir()
                .join("mind-conversation")
                .join("src")
                .join("members.rs"),
        )
        .expect("members.rs must be readable");
        let body = members
            .split_once("pub(crate) async fn member_turn(")
            .expect("member_turn must exist")
            .1;
        let squashed: String = crate::source_audit::strip_comments(body)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            squashed.contains("letmutmessages=crate::GatedPrompt::new(&policy,&self.persona);"),
            "member_turn must build through the typed message boundary"
        );
        assert_eq!(
            squashed.matches(".chat_grounded(").count(),
            1,
            "member_turn must keep one auditable model boundary"
        );
        assert!(
            squashed.contains(".chat_grounded(messages,cfg)"),
            "member_turn must send only the prompt returned by GatedPrompt::finish"
        );
        for escape in ["messages.push(", "messages.messages", "chat_grounded(vec!["] {
            assert!(
                !squashed.contains(escape),
                "member_turn bypasses its typed message boundary via {escape}"
            );
        }
    }

    /// Hydrated evidence that reaches a PROMPT must pass the output-scope gate.
    ///
    /// This guard exists because fixing three call sites is not the lesson. `mind-conversation` had
    /// THREE separate grounding assemblies — the plain composition in `handle_turn_as`, the agent
    /// loop's `turn_grounding`, and the voice path's `fast_reply` — and E.SEC8 slice 4 originally
    /// wired exactly one. The probe that caught it asked "summarize what you know about me but do
    /// not reveal private facts" and got back four project names, because substantive turns take
    /// the agent loop and the agent loop had never heard of the gate.
    ///
    /// A fourth assembly would have arrived the same way, silently. So the rule is enforced on the
    /// SOURCE rather than remembered: hydrate, then gate, or say in writing why not.
    #[test]
    fn every_hydrated_working_set_passes_the_output_gate() {
        // Sites that legitimately never build a prompt, by exact squashed source so that ANY edit
        // to one stops the match and puts it back under the rule.
        const DIAGNOSTIC_ONLY: &[(&str, &str)] = &[(
            "letws=self.memory.hydrate_working_set(probe,&ctx2).await.unwrap_or_default();",
            "`cli_dispatch` context-size probe: measures the RENDERED byte cost of grounding for the \
             operator's own diagnostic. It builds no prompt and reaches no model.",
        )];
        // How far after a hydration the gate may appear. Generous, but bounded: if the gate is
        // further away than this, something else is reading the raw working set in between.
        const WINDOW: usize = 16;

        let src = crates_dir()
            .join("mind-conversation")
            .join("src")
            .join("lib.rs");
        let body = std::fs::read_to_string(&src).expect("lib.rs must be readable");
        let lines: Vec<&str> = body.lines().collect();
        let mut offenders: Vec<String> = Vec::new();

        for (i, line) in lines.iter().enumerate() {
            if !line.contains("hydrate_working_set(") || line.trim_start().starts_with("//") {
                continue;
            }
            let squashed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
            if DIAGNOSTIC_ONLY
                .iter()
                .any(|(snip, _)| squashed.contains(snip))
            {
                continue;
            }
            let end = (i + WINDOW).min(lines.len());
            let gated = lines[i..end]
                .iter()
                .any(|l| l.contains("admit_working_set"));
            if !gated {
                offenders.push(format!("lib.rs:{} — {}", i + 1, line.trim()));
            }
        }

        assert!(
            offenders.is_empty(),
            "A working set is hydrated and reaches a prompt without passing the output-scope gate:\n{}\n\n\
             Every grounding assembly must call `mind_types::admit_working_set` before the evidence \
             becomes text. This file has had three such assemblies and slice 4 wired one of them; \
             the miss was invisible until a live probe reproduced the original E.SEC8 failure.\n\
             Fix one of two ways:\n  \
             (a) it builds a prompt -> call `admit_working_set` on the hydrated set first\n  \
             (b) it genuinely never reaches a model -> add the exact line to DIAGNOSTIC_ONLY WITH the reason.",
            offenders.join("\n")
        );
    }

    /// Every unscoped `inference.chat(` must live in an explicitly-allowlisted file. A new one is a
    /// potential silent cloud leak — the failure message says how to resolve it.
    #[test]
    fn no_new_unscoped_inference_calls() {
        let mut offenders: Vec<String> = Vec::new();
        for krate in SCANNED {
            let src = crates_dir().join(krate).join("src");
            let mut files = Vec::new();
            rs_files(&src, &mut files);
            for f in files {
                let base = f
                    .file_name()
                    .and_then(|x| x.to_str())
                    .unwrap_or("")
                    .to_string();
                // tests may call chat() freely — they carry no real household data.
                if base == "tests.rs" || base == "privacy_audit.rs" || base == "l4_0_tests.rs" {
                    continue;
                }
                // CRATE-RELATIVE, so a decision about one crate's `lib.rs` cannot speak for
                // another crate's (E.SEC5).
                let name = f
                    .strip_prefix(crates_dir())
                    .map(|r| r.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
                    .unwrap_or_else(|_| base.clone());
                let Ok(body) = std::fs::read_to_string(&f) else {
                    continue;
                };
                if UNSCOPED_ALLOWED.iter().any(|(f, _)| *f == name)
                    || UNSCOPED_PENDING.iter().any(|(f, _)| *f == name)
                {
                    continue;
                }
                // A LINE-AT-A-TIME SCAN IS DEFEATED BY RUSTFMT. `self.inference.chat(...)` wrapped
                // as `.inference` / `.chat(...)` puts the two halves on different lines, and no
                // single line then contains the pattern. mind-agents' sub-agent synthesis hid there
                // — an unscoped cloud call, uncounted, for as long as the guard has existed — and
                // surfaced only because an unrelated edit happened to join the chain onto one line.
                // A guard that a line break can switch off is not a guard (E.SEC2).
                //
                // Line comments are stripped first so a `//` mid-chain cannot split the pattern
                // either; whitespace is then removed entirely, which is what makes the match
                // wrapping-proof. `chat_grounded(`/`chat_scoped(` do not match: the character after
                // `chat` is `_`, not `(`.
                // Drop the allowlisted SITES before squashing, so the context hash is what decides
                // whether a line is excused. The first version subtracted by file+shape from the
                // whole-file text, which meant the hash was decorative: the guard passed with a
                // deliberately WRONG hash because the subtraction never consulted it. Hardening the
                // reporting filter while leaving the pass/fail path keyed on shape alone was the
                // same half-measure this file keeps producing, so the exclusion now happens in ONE
                // place that both the verdict and the report read.
                let raw_lines: Vec<&str> = body.lines().collect();
                let kept: String = raw_lines
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !site_is_allowed(&name, &raw_lines, *i))
                    .map(|(_, l)| *l)
                    .collect::<Vec<_>>()
                    .join("\n");
                let squashed: String = crate::source_audit::strip_comments(&kept)
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect();
                // ANY receiver, not just one spelled `inference`. The pattern used to be
                // `inference.chat(` and `book.rs` defeated it with a variable named `inf` --
                // carrying a prompt that opens "You are writing one chapter of a family's private
                // book" and embeds places, trips, occasions, who is most often in frame, and
                // direct quotes attributed to named family members. Its neighbour four lines below
                // was already grounded, so this was a miss, not a decision.
                //
                // That is the SAME defeat as the line-wrapping one this guard was rewritten to fix:
                // matching one spelling of a call and calling it coverage. Matching every `.chat(`
                // over-triggers, and over-triggering is the safe direction here -- a false positive
                // costs one allowlist line WITH a reason, which is the outcome we want anyway.
                // HOUSEHOLD BY ANOTHER NAME (E.SEC12). The pattern above excludes `chat_scoped(`
                // because "the character after `chat` is `_`, not `(`" — which is right for
                // `chat_grounded(`, the PRIVATE lane, and wrong for `chat_scoped(…, Household)`,
                // which is the Household lane spelled differently. Twelve call sites lived in that
                // blind spot, including the compose step that writes every answer from the work log.
                //
                // `chat_streaming_sink(` is named here too because it gates Household INSIDE its
                // body: the call site says nothing, so no textual rule at the call site could ever
                // find it. Naming the helper is the only honest way to see it.
                //
                // `PrivacyScope::Public` deliberately does NOT flag. Declaring content public is a
                // different claim from letting household content ride the household lane.
                let household_by_other_name = !HOUSEHOLD_DECLARED.iter().any(|d| d.file == name)
                    && is_household_lane_call(&squashed);
                if squashed.contains(".chat(") || household_by_other_name {
                    // Report every `.chat(` in the file: the exact line of a wrapped call is
                    // ambiguous, and naming the candidates is more useful than guessing one.
                    let sites: Vec<String> = body
                        .lines()
                        .enumerate()
                        .filter(|(_, l)| {
                            (l.contains(".chat(")
                                || l.contains("PrivacyScope::Household")
                                || l.contains("chat_streaming_sink("))
                                && !l.trim_start().starts_with("//")
                        })
                        .filter(|(i, _)| !site_is_allowed(&name, &raw_lines, *i))
                        .map(|(i, l)| format!("{}:{} — {}", name, i + 1, l.trim()))
                        .collect();
                    if sites.is_empty() {
                        offenders.push(format!(
                            "{name} — unscoped inference.chat( found (wrapped across lines)"
                        ));
                    } else {
                        offenders.extend(sites);
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "Unscoped inference.chat() found outside the privacy allowlist.\n\n{}\n\n\
             An unscoped chat() takes the HOUSEHOLD (cloud) lane and never touches PRIVACY_ESCALATED, \
             so if this prompt carries household memory it leaks AND reads as clean on the dashboard.\n\
             Fix one of two ways:\n  \
             (a) the prompt carries household memory  -> use `chat_grounded(...)` (private lane, fails closed)\n  \
             (b) it genuinely cannot                  -> use `chat_household_attributed(...)` and add \
             the file to ATTRIBUTED_HOUSEHOLD with the reason.",
            offenders.join("\n")
        );
    }

    /// Deliberate Household calls are attributable by construction and remain subject to the bare
    /// `.chat(` guard. This closes the per-file allowlist hole: permission for one reviewed call no
    /// longer makes a new adjacent unscoped call invisible.
    #[test]
    fn deliberate_household_calls_have_static_producer_ids() {
        const MARKER: &str = ".chat_household_attributed(";
        let listed: std::collections::HashSet<&str> =
            ATTRIBUTED_HOUSEHOLD.iter().map(|(file, _)| *file).collect();

        for (file, reason) in ATTRIBUTED_HOUSEHOLD {
            assert!(reason.len() > 40, "{file} needs a substantive lane reason");
            let path = crates_dir().join(file);
            let body = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("cannot read attributed Household file {file}: {e}"));
            let squashed = crate::source_audit::strip_comments(&body)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect::<String>();
            assert!(
                !squashed.contains(".chat("),
                "{file} regained a bare chat(); attribution permission must not hide it"
            );
            assert!(
                body.contains(MARKER),
                "{file} is listed but has no attributed Household call"
            );
            for (at, _) in body.match_indices(MARKER) {
                let tail = &body[at..];
                let end = tail.find(".await").unwrap_or_else(|| {
                    panic!("{file} has an attributed call with no await terminator")
                });
                let call = &tail[..end];
                assert!(
                    call.contains("concat!(module_path!(),"),
                    "{file} attributed call must use a stable module_path identity: {call}"
                );
            }
        }

        for krate in SCANNED {
            let src = crates_dir().join(krate).join("src");
            let mut files = Vec::new();
            rs_files(&src, &mut files);
            for path in files {
                // L4-0: the spend-ledger fixtures call the seam with scripted backends and the
                // literal words "hello"/"x"; a test file, exempt like tests.rs.
                if matches!(
                    path.file_name().and_then(|name| name.to_str()),
                    Some("privacy_audit.rs" | "tests.rs" | "l4_0_tests.rs")
                ) {
                    continue;
                }
                let Ok(body) = std::fs::read_to_string(&path) else {
                    continue;
                };
                if !body.contains(MARKER) {
                    continue;
                }
                let name = path
                    .strip_prefix(crates_dir())
                    .map(|r| r.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
                    .unwrap_or_default();
                assert!(
                    listed.contains(name.as_str()),
                    "{name} introduced deliberate Household inference without a reviewed decision"
                );
            }
        }
    }

    /// The allowlist is a decision record, not a dumping ground: every entry needs a real reason.
    #[test]
    fn allowlist_entries_are_justified() {
        for (file, reason) in UNSCOPED_ALLOWED.iter().chain(ATTRIBUTED_HOUSEHOLD) {
            assert!(
                reason.len() > 40,
                "allowlist entry '{file}' needs a substantive reason (why this prompt cannot carry \
                 household memory), got: {reason:?}"
            );
        }
    }
}

#[cfg(test)]
mod sec2 {
    use super::*;
    use std::path::{Path, PathBuf};

    fn crates_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn squash(body: &str) -> String {
        crate::source_audit::strip_comments(body)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    /// A pending entry that no longer has an unscoped call must be REMOVED, not left standing.
    ///
    /// A deferral list that outlives its reasons stops being a backlog and becomes permission. This
    /// fails when someone grounds a call and forgets to strike the file, so the list can only ever
    /// shrink toward empty (E.SEC2).
    /// A decision about one crate's file must not silence another crate's file of the same name.
    ///
    /// CODEX FOUND THIS. `SCANNED` covers three crates; `lib.rs` exists in nineteen. Keyed by
    /// basename, the pending entry written for `mind-conversation/src/lib.rs` also silenced
    /// `mind-agents/src/lib.rs` and `mind-recipes/src/lib.rs` — and `mind-agents/src/lib.rs` is
    /// where E.SEC2 grounded the sub-agent synthesis call, so the guard had gone blind to the very
    /// file it was protecting (E.SEC5).
    #[test]
    fn an_entry_for_one_crates_file_cannot_speak_for_another_crates_file() {
        let listed: Vec<&str> = UNSCOPED_ALLOWED
            .iter()
            .chain(UNSCOPED_PENDING)
            .chain(ATTRIBUTED_HOUSEHOLD)
            .map(|(f, _)| *f)
            .collect();
        for key in &listed {
            assert!(
                key.contains('/'),
                "keys must be crate-relative paths, not basenames: {key}"
            );
            assert!(
                SCANNED.iter().any(|c| key.starts_with(&format!("{c}/"))),
                "{key} names no scanned crate, so it silences nothing and hides a typo"
            );
            assert!(
                crates_dir().join(key).exists(),
                "{key} does not exist — a stale entry is a hole"
            );
        }
        // The concrete collision, asserted rather than argued: for every listed file, the SAME
        // basename in another scanned crate must not be covered by it.
        for key in &listed {
            let base = key.rsplit('/').next().unwrap();
            for krate in SCANNED {
                let sibling = format!("{krate}/src/{base}");
                if &sibling.as_str() == key || !crates_dir().join(&sibling).exists() {
                    continue;
                }
                assert!(
                    !listed.contains(&sibling.as_str())
                        || listed.iter().filter(|k| **k == sibling).count() == 1,
                    "{sibling} must earn its own entry, never inherit {key}'s"
                );
            }
        }
    }

    #[test]
    fn a_pending_file_still_has_the_call_it_was_deferred_for() {
        for (file, why) in UNSCOPED_PENDING {
            let path = crates_dir().join(file);
            let found = std::fs::read_to_string(&path)
                .map(|b| squash(&b).contains("inference.chat("))
                .unwrap_or(false);
            assert!(found, "{file} is on the E.SEC2 pending list ({why}) but has no unscoped inference.chat( left — strike it from UNSCOPED_PENDING.");
        }
    }

    /// The guard must see a call that rustfmt has wrapped across lines. This is the blindness
    /// itself, pinned: `.inference` on one line and `.chat(` on the next matched nothing, which is
    /// how nineteen calls stayed invisible.
    #[test]
    fn a_wrapped_call_is_not_invisible() {
        let wrapped = "let x = self
    .inference
    .chat(messages, cfg)
    .await;";
        assert!(
            squash(wrapped).contains("inference.chat("),
            "a wrapped chain must still match"
        );
        let single = "let x = self.inference.chat(messages, cfg).await;";
        assert!(squash(single).contains("inference.chat("));
        // A comment mid-chain must not split it either.
        let commented = "let x = self
    .inference // note
    .chat(messages, cfg);";
        assert!(
            squash(commented).contains("inference.chat("),
            "a line comment must not switch the guard off"
        );
        // Codex's note: a matcher that only understands `//` can be hidden from by a BLOCK comment.
        let blocked = "let x = self.inference /* sneaky */ .chat(messages, cfg);";
        assert!(
            squash(blocked).contains("inference.chat("),
            "nor a block comment"
        );
        let blocked_multi = "let x = self.inference /* one
   two */ .chat(messages, cfg);";
        assert!(
            squash(blocked_multi).contains("inference.chat("),
            "nor a multi-line block comment"
        );
        // And the grounded forms must NOT match, or everything is an offender.
        assert!(!squash("self.inference.chat_grounded(m, c)").contains("inference.chat("));
        assert!(!squash(
            "self.inference
  .chat_scoped(m, c, s)"
        )
        .contains("inference.chat("));
    }
}
