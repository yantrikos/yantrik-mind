//! E.SYNTAX1 — a written `.py` that does not parse, caught with the machinery the forge already has.
//!
//! The Mind on `gpt-oss-backup:20b` produced an `app.py` whose line 89 is a JavaScript template
//! literal inside Python — `${Object.entries(data.per_day).map(([d,c])=>...)}`. `SyntaxError`. The
//! server never started and every request returned `000`. **All four existing mechanical checks
//! stayed silent and each was right to**: the imports resolve, the entry script names a file that
//! exists, that file has a `__main__` guard so it is not a dead entry point, nothing repeats, and
//! the dashboard is not a snapshot. Every check answered its own question correctly and the
//! deliverable was worthless, because none of them asks whether the file is valid Python.
//!
//! **This needs no new dependency and runs nothing.** `ast.parse` PARSES; it never executes. The
//! forge has done exactly this since E.FORGE1 (`code.rs`), inside the same sandbox, on the same
//! boxes — so `python3` is a dependency the product already has and relies on. `delegate.rs`
//! declines to *run* the artifact, which is correct and is a different question; this slice does
//! not touch it.
//!
//! **THE RISK IS INVERTED, exactly as in `freshness.rs`, and it decides the design.** A finding here
//! accuses a file of being broken, so a sandbox that merely fails to answer must never be read as
//! "the file is bad". Hence the rule: **only an explicit `SYNTAX:` marker in the output produces a
//! finding.** An `Err`, a non-zero exit, a missing marker, an unavailable sandbox — all mean
//! *could not check*, and all yield silence. That is why `available()` is never called: an
//! unavailable sandbox simply produces no marker, and paying for a probe would only add a spawn.
//!
//! **Cost is bounded.** No `.py` in the set costs nothing at all. When the sandbox is not working,
//! the first inconclusive answer stops the loop, so an unusable environment costs one attempt for
//! the whole build rather than one per file. Inside the cb2 benchmark container the sandbox is
//! refused (no unprivileged userns), so graded legs are byte-identical — this improves the real
//! product and cannot move a reading.

use crate::fileset::parse_file_stream;

/// The fixed program that does the checking. It is a literal: the model's output is DATA that this
/// reads, never code that runs. `-I -S -B` (set by the sandbox) keep it off site-packages too.
const CHECK: &str = "import ast\nsrc = open('target.py').read()\ntry:\n    ast.parse(src)\n    print('OK')\nexcept SyntaxError as e:\n    print('SYNTAX:', e)";

/// The mind's state directory, masked inside the sandbox so the check cannot read it.
/// Derived exactly as `mind-core` derives it for the engine's own sandbox.
fn state_dir() -> String {
    std::env::var("YM_DB")
        .ok()
        .and_then(|p| {
            std::path::Path::new(&p)
                .parent()
                .map(|d| d.to_string_lossy().to_string())
        })
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/var/lib/yantrik-mind".to_string())
}

/// What one sandbox answer means. Three outcomes, and only one of them convicts.
enum Verdict {
    Parses,
    Broken(String),
    /// The sandbox could not answer — unavailable, killed, or output we do not recognise. NEVER an
    /// accusation, and the signal to stop trying for this build.
    Unknown,
}

/// Read a rendered sandbox result. Pure, so the three-way rule is testable without a sandbox.
fn read_verdict(rendered: &str) -> Verdict {
    if let Some(at) = rendered.find("SYNTAX:") {
        let msg: String = rendered[at + "SYNTAX:".len()..]
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(120)
            .collect();
        return Verdict::Broken(if msg.is_empty() {
            "it is not valid Python".to_string()
        } else {
            msg
        });
    }
    if rendered.contains("OK") {
        return Verdict::Parses;
    }
    Verdict::Unknown
}

/// E.SYNTAX2 — when the failing line is visibly JavaScript-inside-Python, say so.
///
/// Measured (E.REPAIR5, n=20 per arm, prediction recorded first): a finding that names only the
/// SYMPTOM repaired 8/20; one that names this CAUSE repaired 14/20. The failure classes explained
/// the gap — 11 of 12 symptom-arm failures re-committed the same `${…}`-in-an-f-string
/// misconception, while the cause arm's few failures were unrelated slips. A model cannot act on a
/// symptom whose cause it does not recognise as a cause.
///
/// Bounded on purpose: this reads ONLY the line Python reported, and speaks ONLY when that line
/// contains `${`. Every other syntax error keeps the symptom wording byte-for-byte, so the finding
/// is never broadened past what was measured. A missing or malformed line number yields `None`,
/// never a guess.
fn cause_hint(src: &str, why: &str) -> Option<&'static str> {
    // Python's message ends "(<unknown>, line N)". Take the last "line N" it mentions.
    let at = why.rfind("line ")?;
    let n: usize = why[at + "line ".len()..]
        .trim_end_matches(')')
        .trim()
        .parse()
        .ok()?;
    let line = src.lines().nth(n.checked_sub(1)?)?;
    if line.contains("${") {
        Some(
            " THE CAUSE: that line contains JavaScript template-literal syntax `${...}` inside a              Python string. Python does not understand it — inside an f-string `{` begins a Python              expression and `$` is not valid there, so the JavaScript is being parsed as Python and              fails. Build the JavaScript WITHOUT an f-string (a plain non-f string for the whole              <script> block, substituting values with str.replace), or double every brace that              belongs to the JavaScript ({{ and }}).",
        )
    } else {
        None
    }
}

/// Findings for python files in this set that do not parse.
///
/// Empty for every healthy build, which keeps the write step's message byte-identical.
pub(crate) async fn unparseable_python(stream: &str) -> Vec<String> {
    let entries = parse_file_stream(stream).entries;
    let pys: Vec<_> = entries.iter().filter(|e| e.path.ends_with(".py")).collect();
    if pys.is_empty() {
        return Vec::new(); // a build with no python pays nothing, not even a spawn
    }
    let sb = mind_tools::Sandbox::new().hiding(state_dir());
    let mut out = Vec::new();
    for e in pys {
        // ONE run per file. An earlier draft called the sandbox twice — once to classify and once
        // to extract the message — which doubled the cost of the check for no gain.
        let rendered = match sb.run_python_with(CHECK, "target.py", &e.content).await {
            Ok(r) => r.render(),
            Err(_) => return out, // sandbox unusable: stop, accuse nothing
        };
        match read_verdict(&rendered) {
            Verdict::Parses => {}
            Verdict::Broken(why) => out.push(format!(
                "`{}` is not valid Python — {why}.{} Nothing that runs it can start, so the whole                  deliverable is dead however good the rest of it is. Rewrite that file; a stray                  fragment of another language (a JavaScript template literal, a shell line) is the                  usual cause.",
                e.path,
                cause_hint(&e.content, &why).unwrap_or("")
            )),
            // Inconclusive answers mean the environment cannot check, not that the file is bad.
            // Stop for this build so an unusable sandbox costs one attempt, not one per file.
            Verdict::Unknown => return out,
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact bytes the real sandbox returned for the artifact that shipped broken.
    /// Captured on staging rather than invented, because invented cases share the author's
    /// misconception -- eighteen of them once passed while the first real artifact failed.
    const REAL_BROKEN: &str =
        "SYNTAX: expression cannot contain assignment, perhaps you meant \"==\"? (<unknown>, line 89)\n";
    const REAL_OK: &str = "OK\n";

    #[test]
    fn convicts_on_the_real_sandbox_output_for_the_artifact_that_shipped_broken() {
        match read_verdict(REAL_BROKEN) {
            Verdict::Broken(why) => {
                assert!(why.contains("line 89"), "must carry the location: {why}");
                assert!(
                    why.contains("expression cannot contain assignment"),
                    "must carry python's own words: {why}"
                );
            }
            _ => panic!("the real broken output must convict"),
        }
    }

    #[test]
    fn acquits_on_the_real_sandbox_output_for_a_healthy_file() {
        assert!(matches!(read_verdict(REAL_OK), Verdict::Parses));
    }

    /// THE INVERTED-RISK RULE, and the reason this check cannot accuse a healthy file.
    ///
    /// A finding here says "your file is broken". So anything that is not an explicit verdict must
    /// be UNKNOWN -- an unavailable sandbox, a kill, a permission error, empty output. Reading any
    /// of these as a conviction is exactly the failure `freshness.rs` was designed against, and the
    /// one the corpus test caught there.
    #[test]
    fn anything_inconclusive_is_unknown_never_a_conviction() {
        for s in [
            "",
            "unshare: unshare failed: Operation not permitted",
            "Killed",
            "prlimit: cannot execute: Permission denied",
            "bash: python3: command not found",
            "some unrelated chatter",
        ] {
            assert!(
                matches!(read_verdict(s), Verdict::Unknown),
                "inconclusive output must never convict: {s:?}"
            );
        }
    }

    /// A conviction needs the marker, not merely the word "syntax" somewhere.
    #[test]
    fn only_the_explicit_marker_convicts() {
        assert!(matches!(read_verdict("checking syntax now"), Verdict::Unknown));
        assert!(matches!(read_verdict("SyntaxError somewhere"), Verdict::Unknown));
        assert!(matches!(read_verdict("SYNTAX: bad"), Verdict::Broken(_)));
    }

    /// `OK` and a syntax error cannot both be true; the marker wins, so a file whose own text
    /// contains "OK" is still convicted when python rejected it.
    #[test]
    fn the_syntax_marker_beats_a_stray_ok() {
        assert!(matches!(
            read_verdict("OK\nSYNTAX: invalid syntax (line 3)"),
            Verdict::Broken(_)
        ));
    }

    /// Kill criteria 1 and 2, and they can only run where the sandbox works.
    ///
    /// This dev machine has no unprivileged user namespaces, so the check is inert here and this
    /// test would pass vacuously -- which is why it asserts `judged > 0` first. Point
    /// `YM_SYNTAX_CORPUS` at a tree of benchmark artifacts ON A LINUX BOX and run it there.
    #[tokio::test]
    async fn fires_on_exactly_the_unparseable_files_in_a_real_corpus() {
        let Ok(root) = std::env::var("YM_SYNTAX_CORPUS") else {
            return;
        };
        let mut judged = 0usize;
        let mut fired: Vec<String> = Vec::new();
        let mut walk = vec![std::path::PathBuf::from(&root)];
        while let Some(dir) = walk.pop() {
            let Ok(rd) = std::fs::read_dir(&dir) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk.push(p);
                    continue;
                }
                if p.extension().and_then(|x| x.to_str()) != Some("py") {
                    continue;
                }
                let Ok(c) = std::fs::read_to_string(&p) else { continue };
                let stream = format!("=== FILE: t.py
{c}
");
                judged += 1;
                if !unparseable_python(&stream).await.is_empty() {
                    fired.push(p.display().to_string());
                }
            }
        }
        assert!(judged > 5, "corpus too small or unreadable: {judged} python files");
        // The reference is python's own `ast.parse` over the same tree, run separately.
        // Every fire must be a file python also rejects -- printed so a mismatch is legible.
        eprintln!("judged={judged} fired={}: {fired:#?}", fired.len());
        for f in &fired {
            let out = std::process::Command::new("python3")
                .arg("-c")
                .arg("import ast,sys; ast.parse(open(sys.argv[1],encoding='utf-8',errors='replace').read())")
                .arg(f)
                .output()
                .expect("python3 must be present where the sandbox works");
            assert!(
                !out.status.success(),
                "accused a file python accepts -- a FALSE ACCUSATION, the one failure this check                  must never make: {f}"
            );
        }
    }

    // ── E.SYNTAX2 ──────────────────────────────────────────────────────────────────────────────

    /// The real artifact: line 89 holds a JavaScript template literal. The cause must be named.
    #[test]
    fn names_the_cause_on_the_real_artifact_whose_failing_line_has_a_template_literal() {
        let src = include_str!("../fixtures/entrypoint/oss20_app_jsinpython.py");
        let why = "expression cannot contain assignment, perhaps you meant \"==\"? (<unknown>, line 89)";
        let hint = cause_hint(src, why).expect("line 89 contains ${ and must yield the cause");
        assert!(hint.contains("template-literal"), "{hint}");
        assert!(src.lines().nth(88).unwrap().contains("${"), "the fixture's line 89 must contain ${{");
    }

    /// Kill criterion 2: a syntax error whose failing line has no `${` gets NO hint, so the
    /// finding stays byte-identical to today's for every other class of error.
    #[test]
    fn stays_silent_when_the_failing_line_has_no_template_literal() {
        let src = "import os

def f(:
    pass
";
        let why = "invalid syntax (<unknown>, line 3)";
        assert_eq!(cause_hint(src, why), None);
    }

    /// Kill criterion 3: a malformed or missing line number never panics and never invents a cause.
    #[test]
    fn a_bad_line_number_yields_nothing_rather_than_a_guess() {
        let src = "x = `${a}`
";
        for why in ["invalid syntax", "line", "line abc", "line 0", "line 99", "(<unknown>, line )"] {
            assert_eq!(cause_hint(src, why), None, "{why:?} must not produce a cause");
        }
    }

    /// Kill criterion 6: a build with no python costs nothing -- not even a sandbox spawn.
    /// Asserted through the public entry point, which returns before constructing one.
    #[tokio::test]
    async fn a_set_with_no_python_costs_nothing() {
        let stream = "=== FILE: index.html\n<h1>hi</h1>\n=== FILE: run.sh\n#!/bin/sh\necho hi\n";
        assert_eq!(unparseable_python(stream).await, Vec::<String>::new());
    }
}
