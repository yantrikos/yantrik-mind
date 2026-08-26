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
const UNSCOPED_ALLOWED: &[(&str, &str)] = &[
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
const UNSCOPED_PENDING: &[(&str, &str)] = &[
    // mail.rs, finance.rs, calendar.rs and briefing.rs were HERE and are now grounded — the four
    // whose prompts were unambiguously household. Struck rather than left standing, which the
    // companion test enforces.
    ("mind-conversation/src/foresight.rs", "persona + a prompt built from household signals."),
    ("mind-conversation/src/onboarding.rs", "two calls carrying persona + user answers during setup."),
    ("mind-conversation/src/studio.rs", "three calls; prompt provenance not yet traced."),
    ("mind-conversation/src/plugins_mod.rs", "persona + plugin prompt; provenance not yet traced."),
    ("mind-conversation/src/skills.rs", "the verifier-written skill summary — the prompt carries the user's own code."),
    ("mind-conversation/src/lib.rs", "five calls in the conversation core; each needs its own reading."),
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
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                out.push(p);
            }
        }
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
                let base = f.file_name().and_then(|x| x.to_str()).unwrap_or("").to_string();
                // tests may call chat() freely — they carry no real household data.
                if base == "tests.rs" || base == "privacy_audit.rs" {
                    continue;
                }
                // CRATE-RELATIVE, so a decision about one crate's `lib.rs` cannot speak for
                // another crate's (E.SEC5).
                let name = f
                    .strip_prefix(crates_dir())
                    .map(|r| r.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
                    .unwrap_or_else(|_| base.clone());
                let Ok(body) = std::fs::read_to_string(&f) else { continue };
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
                let decommented = body
                    .lines()
                    .map(|l| l.split("//").next().unwrap_or(""))
                    .collect::<Vec<_>>()
                    .join("
");
                let squashed: String = decommented.chars().filter(|c| !c.is_whitespace()).collect();
                if squashed.contains("inference.chat(") {
                    // Report every `.chat(` in the file: the exact line of a wrapped call is
                    // ambiguous, and naming the candidates is more useful than guessing one.
                    let sites: Vec<String> = body
                        .lines()
                        .enumerate()
                        .filter(|(_, l)| l.contains(".chat(") && !l.trim_start().starts_with("//"))
                        .map(|(i, l)| format!("{}:{} — {}", name, i + 1, l.trim()))
                        .collect();
                    if sites.is_empty() {
                        offenders.push(format!("{name} — unscoped inference.chat( found (wrapped across lines)"));
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
             (b) it genuinely cannot                  -> add the file to UNSCOPED_ALLOWED in privacy_audit.rs \
             WITH the reason.",
            offenders.join("\n")
        );
    }

    /// The allowlist is a decision record, not a dumping ground: every entry needs a real reason.
    #[test]
    fn allowlist_entries_are_justified() {
        for (file, reason) in UNSCOPED_ALLOWED {
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
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    }

    fn squash(body: &str) -> String {
        let decommented = body.lines().map(|l| l.split("//").next().unwrap_or("")).collect::<Vec<_>>().join("
");
        decommented.chars().filter(|c| !c.is_whitespace()).collect()
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
        let listed: Vec<&str> = UNSCOPED_ALLOWED.iter().chain(UNSCOPED_PENDING).map(|(f, _)| *f).collect();
        for key in &listed {
            assert!(key.contains('/'), "keys must be crate-relative paths, not basenames: {key}");
            assert!(
                SCANNED.iter().any(|c| key.starts_with(&format!("{c}/"))),
                "{key} names no scanned crate, so it silences nothing and hides a typo"
            );
            assert!(crates_dir().join(key).exists(), "{key} does not exist — a stale entry is a hole");
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
                    !listed.contains(&sibling.as_str()) || listed.iter().filter(|k| **k == sibling).count() == 1,
                    "{sibling} must earn its own entry, never inherit {key}'s"
                );
            }
        }
    }

    #[test]
    fn a_pending_file_still_has_the_call_it_was_deferred_for() {
        for (file, why) in UNSCOPED_PENDING {
            let path = crates_dir().join(file);
            let found = std::fs::read_to_string(&path).map(|b| squash(&b).contains("inference.chat(")).unwrap_or(false);
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
        assert!(squash(wrapped).contains("inference.chat("), "a wrapped chain must still match");
        let single = "let x = self.inference.chat(messages, cfg).await;";
        assert!(squash(single).contains("inference.chat("));
        // A comment mid-chain must not split it either.
        let commented = "let x = self
    .inference // note
    .chat(messages, cfg);";
        assert!(squash(commented).contains("inference.chat("), "a comment must not switch the guard off");
        // And the grounded forms must NOT match, or everything is an offender.
        assert!(!squash("self.inference.chat_grounded(m, c)").contains("inference.chat("));
        assert!(!squash("self.inference
  .chat_scoped(m, c, s)").contains("inference.chat("));
    }
}
