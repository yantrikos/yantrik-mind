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
const UNSCOPED_ALLOWED: &[(&str, &str)] = &[
    (
        "egress_planning.rs",
        "MUST stay unscoped BY DESIGN: the ARCH-3 egress-clean planner re-authors outbound tool args \
         in a clean room that has never seen private memory. Grounding it would defeat its purpose.",
    ),
    (
        "news.rs",
        "public-web facts — the prompt carries a topic string and fetched headlines, no household memory.",
    ),
    (
        "festivals.rs",
        "public calendar facts (lunar-calendar festival dates); no household memory in the prompt.",
    ),
    (
        "research.rs",
        "public-web research; the prompt carries the research query and fetched sources.",
    ),
    (
        "code.rs",
        "the REMAINING calls carry venture/codebase task text only (PRD drafting, code Q&A over a \
         studied repo). The two that did read the household substrate — work_radar_run (40 verbatim \
         user messages) and paper_ask (belief-store output) — are on chat_grounded as of the \
         2026-07-25 sweep. Any NEW call here that touches self.memory must be grounded.",
    ),
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
                let name = f.file_name().and_then(|x| x.to_str()).unwrap_or("").to_string();
                // tests may call chat() freely — they carry no real household data.
                if name == "tests.rs" || name == "privacy_audit.rs" {
                    continue;
                }
                let Ok(body) = std::fs::read_to_string(&f) else { continue };
                for (i, line) in body.lines().enumerate() {
                    // `chat_grounded(` / `chat_scoped(` also contain "chat(" only via `.chat(` —
                    // match the bare call precisely.
                    if line.contains("inference.chat(")
                        && !UNSCOPED_ALLOWED.iter().any(|(f, _)| *f == name)
                    {
                        offenders.push(format!("{}:{} — {}", name, i + 1, line.trim()));
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
