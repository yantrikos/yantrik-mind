//! E.ENTRY1 — pinned against REAL artifacts, not invented ones.
//!
//! The reason is recorded and was expensive: a scorer written for this benchmark passed eighteen
//! cases invented alongside it and then failed the first three real verdicts it saw, because the
//! cases and the code shared one misconception. So the decisive fixtures here are the actual bytes
//! of the graded run — `out-r7/artifacts/mind_T1/server.py`, which failed with
//! `ERR_CONNECTION_REFUSED`, and `out-r7/artifacts/hermes_T1/server.py`, which scored 11/11 — and
//! the assertions are exact counts rather than `>=`.

use super::entrypoint::dead_entry_points;

const R7_MIND_SERVER: &str = include_str!("../fixtures/entrypoint/r7_mind_T1_server.py");
const R7_MIND_RUN: &str = include_str!("../fixtures/entrypoint/r7_mind_T1_run.sh");
const R7_HERMES_SERVER: &str = include_str!("../fixtures/entrypoint/r7_hermes_T1_server.py");
const R7_HERMES_RUN: &str = include_str!("../fixtures/entrypoint/r7_hermes_T1_run.sh");
const P8_MIND_SERVER: &str = include_str!("../fixtures/entrypoint/p8_mind_T1_server.py");

fn stream(files: &[(&str, &str)]) -> String {
    files
        .iter()
        .map(|(p, c)| format!("=== FILE: {p}\n{c}\n"))
        .collect()
}

/// The measurement this whole slice exists for.
///
/// This artifact parses cleanly, every import resolves, and the file is present — so `pyimports`,
/// the duplicate-path check and a syntax check all stay silent on it, and the model review round
/// read it and approved it. Its module body is imports, defs and constants, so `python3 server.py`
/// defines a handler class and exits.
#[test]
fn fires_on_the_real_r7_artifact_that_died_on_connection_refused() {
    let s = stream(&[("run.sh", R7_MIND_RUN), ("server.py", R7_MIND_SERVER)]);
    let found = dead_entry_points(&s);
    assert_eq!(
        found.len(),
        1,
        "the r7 T1 artifact must raise exactly one finding, got: {found:?}"
    );
    assert!(
        found[0].contains("server.py") && found[0].contains("NO code at module level"),
        "the finding must name the file and say what is wrong: {}",
        found[0]
    );
}

/// The other half of the same graded task, from the agent that won it.
///
/// A check that fires on both is worthless, and this is the artifact that would expose it.
#[test]
fn silent_on_the_real_hermes_artifact_that_scored_eleven_of_eleven() {
    let s = stream(&[("run.sh", R7_HERMES_RUN), ("server.py", R7_HERMES_SERVER)]);
    assert_eq!(
        dead_entry_points(&s),
        Vec::<String>::new(),
        "the winning artifact must produce no finding"
    );
}

/// A Mind artifact from a passing leg, so "silent on Hermes" is not just a house-style difference.
#[test]
fn silent_on_a_passing_mind_artifact() {
    let s = stream(&[("run.sh", R7_MIND_RUN), ("server.py", P8_MIND_SERVER)]);
    assert_eq!(
        dead_entry_points(&s),
        Vec::<String>::new(),
        "p8's server, which served fresh data, must produce no finding"
    );
}

/// SCOPE is the safety argument: a module nobody runs is not judged.
///
/// A test suite and a library are SUPPOSED to be nothing but definitions. Judging one would be a
/// false accusation, and a checker that cries wolf gets turned off.
#[test]
fn never_judges_a_module_no_entry_script_invokes() {
    let lib = "import json\n\n\ndef helper(x):\n    return json.dumps(x)\n";
    let tests = "import tracker\n\n\ndef test_add():\n    assert tracker.add('a') is not None\n";
    // No entry script at all.
    assert!(dead_entry_points(&stream(&[("lib.py", lib), ("test_x.py", tests)])).is_empty());
    // An entry script that runs something ELSE leaves both alone.
    let s = stream(&[
        ("run.sh", "#!/usr/bin/env bash\npython3 main.py\n"),
        ("main.py", "import lib\n\nlib.helper({})\n"),
        ("lib.py", lib),
        ("test_x.py", tests),
    ]);
    assert_eq!(dead_entry_points(&s), Vec::<String>::new());
}

/// An entry script naming a file that was never written is its own defect, and a distinct message.
#[test]
fn reports_an_entry_script_whose_target_is_missing() {
    let s = stream(&[("run.sh", "#!/usr/bin/env bash\npython3 server.py\n")]);
    let found = dead_entry_points(&s);
    assert_eq!(found.len(), 1, "got: {found:?}");
    assert!(found[0].contains("NOT in the file set"), "{}", found[0]);
}

/// A `__main__` guard is behaviour, and so is a bare top-level call.
#[test]
fn a_module_that_actually_runs_something_is_silent() {
    let run = "#!/usr/bin/env bash\npython3 server.py\n";
    let guarded = "import sys\n\n\ndef main():\n    print('hi')\n\n\nif __name__ == '__main__':\n    main()\n";
    assert!(dead_entry_points(&stream(&[("run.sh", run), ("server.py", guarded)])).is_empty());
    let bare = "import sys\n\n\ndef main():\n    print('hi')\n\n\nmain()\n";
    assert!(dead_entry_points(&stream(&[("run.sh", run), ("server.py", bare)])).is_empty());
}

/// Constants, docstrings and wrapped expressions are not behaviour — and getting any of these
/// wrong invents a finding on a healthy file, which is the failure mode that matters.
#[test]
fn setup_that_is_not_behaviour_still_reads_as_dead() {
    let run = "#!/usr/bin/env bash\npython3 server.py\n";
    let src = "\"\"\"A module docstring\nspanning lines, with def and import words in it.\n\"\"\"\nimport os\nfrom json import dumps\n\nPORT = 8123\nHOST = '0.0.0.0'\nDEBUG = False\nEMPTY = []\nNEG = -1.5\n\n\n@decorator\nclass Handler:\n    def go(self):\n        return dumps({})\n\n\nasync def later():\n    return None\n";
    let found = dead_entry_points(&stream(&[("run.sh", run), ("server.py", src)]));
    assert_eq!(found.len(), 1, "all of this is setup, so it is dead: {found:?}");
}

/// Anything the classifier is unsure of must read as behaviour, which yields SILENCE.
///
/// Three real artifacts in the corpus do not parse at all; the AST probe abstained on them and so
/// must this. Accusing a file it cannot read is exactly the wrong direction.
#[test]
fn refuses_to_judge_what_it_cannot_read() {
    let run = "#!/usr/bin/env bash\npython3 server.py\n";
    // Unterminated triple quote.
    let torn = "import os\n\"\"\"this never closes\ndef x():\n    pass\n";
    assert!(dead_entry_points(&stream(&[("run.sh", run), ("server.py", torn)])).is_empty());
    // Brackets that do not balance.
    let unbalanced = "import os\nX = )\n";
    assert!(dead_entry_points(&stream(&[("run.sh", run), ("server.py", unbalanced)])).is_empty());
    // A call bound to a name is behaviour, not a constant.
    let call = "import flask\napp = flask.Flask(__name__)\n";
    assert!(dead_entry_points(&stream(&[("run.sh", run), ("server.py", call)])).is_empty());
}

/// Agreement with the `ast` probe over the whole benchmark corpus.
///
/// Kill criterion 1 of the prereg: the Rust port and the probe must agree FILE FOR FILE, not
/// "no worse than". Run it by pointing `YM_ENTRY_CORPUS` at a tree of `out-*/artifacts/*/`
/// directories; it was run against all 23 judged artifacts of ~24 benchmark runs before this
/// slice shipped, and the expected set is one directory.
#[test]
fn agrees_with_the_ast_probe_across_the_benchmark_corpus() {
    let Ok(root) = std::env::var("YM_ENTRY_CORPUS") else {
        return; // corpus not present on this machine
    };
    let mut fired: Vec<String> = Vec::new();
    let mut judged = 0usize;
    let mut walk = vec![std::path::PathBuf::from(&root)];
    while let Some(dir) = walk.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut files: Vec<(String, String)> = Vec::new();
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk.push(p);
            } else if let Ok(c) = std::fs::read_to_string(&p) {
                if let Some(n) = p.file_name().and_then(|n| n.to_str()) {
                    files.push((n.to_string(), c));
                }
            }
        }
        if !files.iter().any(|(n, _)| n.ends_with(".sh")) {
            continue;
        }
        judged += 1;
        let refs: Vec<(&str, &str)> =
            files.iter().map(|(n, c)| (n.as_str(), c.as_str())).collect();
        if !dead_entry_points(&stream(&refs)).is_empty() {
            fired.push(dir.display().to_string());
        }
    }
    assert!(judged > 0, "YM_ENTRY_CORPUS pointed at nothing judgeable");
    assert_eq!(
        fired.len(),
        1,
        "the probe found exactly one dead entry point in this corpus; the port found {}: {fired:?}",
        fired.len()
    );
    assert!(
        fired[0].replace('\\', "/").contains("out-r7"),
        "the one fire must be r7's mind_T1, got {}",
        fired[0]
    );
}

// ── E.FENCE1 ──────────────────────────────────────────────────────────────────────────────────
//
// This started as a proposed new fence-stripper and was KILLED: `parse_file_stream` already calls
// `unfence`, and the artifact I found (`out-v3/mind_T1/main.py`, which begins ```python and ends
// ```) is the PRE-FIX evidence that motivated it, not an open defect. What survived is smaller and
// real — `unfence` had no Markdown exclusion, and the comment justifying that was a false premise.

const V3_FENCED_MAIN: &str = include_str!("../fixtures/entrypoint/v3_mind_T1_main_fenced.py");

/// The existing fix, pinned to the real artifact that motivated it.
///
/// `out-v3/artifacts/mind_T1/main.py` was written with its ```python wrapper intact; python died on
/// line 1 and the deliverable was a total loss. Nothing in the test suite held that artifact, so
/// this pins the behaviour against the actual bytes rather than a paraphrase of them.
#[test]
fn the_parser_unfences_the_real_v3_artifact() {
    let s = format!("=== FILE: main.py\n{V3_FENCED_MAIN}\n");
    let parsed = crate::fileset::parse_file_stream(&s);
    assert_eq!(parsed.entries.len(), 1);
    assert!(
        parsed.entries[0].content.starts_with("#!/usr/bin/env python3"),
        "the shebang must be first after unfencing, got: {:?}",
        &parsed.entries[0].content[..parsed.entries[0].content.len().min(40)]
    );
    assert!(
        !parsed.entries[0].content.contains("```"),
        "no fence may reach disk"
    );
}

/// E.FENCE1 — a README that opens AND closes with a fenced block must survive intact.
///
/// The comment on `unfence` used to argue Markdown needed no special case, because "such a file
/// does not both begin and end with one". An install command at the top and a config block at the
/// bottom is an ordinary README, and on it the rule stripped the OUTER pair and left the inner
/// fences orphaned. The safety argument was a premise nobody had tested; this is the test.
#[test]
fn a_readme_that_opens_and_closes_with_a_fence_is_not_mangled() {
    let readme = "```bash\npip install thing\n```\n\nThen configure it:\n\n```json\n{\"key\": \"value\"}\n```";
    let s = format!("=== FILE: README.md\n{readme}\n");
    let parsed = crate::fileset::parse_file_stream(&s);
    assert_eq!(parsed.entries.len(), 1);
    let got = parsed.entries[0].content.trim_end();
    assert_eq!(
        got, readme,
        "a Markdown file must reach disk byte-identical; unfencing it orphans its inner fences"
    );
    // The same bytes in a PROGRAM are still a wrapper, so the exclusion is by file type, not by
    // giving up on the rule.
    let prog = format!("=== FILE: main.py\n{readme}\n");
    let p2 = crate::fileset::parse_file_stream(&prog);
    assert!(
        !p2.entries[0].content.starts_with("```bash"),
        "a .py wrapped in a fence must still be unwrapped"
    );
}
