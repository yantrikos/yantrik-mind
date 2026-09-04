//! E.FILES1's kill criteria, and every one of them asserted against the FILES ON DISK where that is
//! what the criterion is about. The previous slice shipped as a no-op with a correct rule and
//! correct wiring because no test asked what actually landed.

use crate::fileset::{
    plan_file_set, write_file_set, FileEntry, FileSetRefusal, MAX_DEPTH, MAX_FILES,
    MAX_FILE_BYTES, MAX_TOTAL_BYTES,
};

fn e(path: &str, content: &str) -> FileEntry {
    FileEntry {
        path: path.to_string(),
        content: content.to_string(),
    }
}

/// A fresh directory under the OS temp dir, removed at the end of the test.
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new(tag: &str) -> Self {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("ym-fileset-{tag}-{n}"));
        std::fs::create_dir_all(&p).expect("scratch dir");
        Self(p)
    }
    fn path(&self) -> &std::path::Path {
        &self.0
    }
    /// Every regular file under the root, as relative slash-separated paths, sorted.
    fn listing(&self) -> Vec<String> {
        fn walk(dir: &std::path::Path, root: &std::path::Path, out: &mut Vec<String>) {
            let Ok(rd) = std::fs::read_dir(dir) else {
                return;
            };
            for entry in rd.flatten() {
                let p = entry.path();
                if p.is_dir() {
                    walk(&p, root, out);
                } else if let Ok(rel) = p.strip_prefix(root) {
                    out.push(rel.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.0, &self.0, &mut out);
        out.sort();
        out
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

// ── the capability itself ────────────────────────────────────────────────────────────────────
#[test]
fn a_deliverable_lands_on_disk_with_its_paths_and_its_bytes() {
    let s = Scratch::new("ok");
    let set = [
        e("index.html", "<!doctype html><html><body>hi</body></html>"),
        e("run.sh", "#!/bin/bash\npython3 server.py\n"),
        e("data/leads.json", "[]"),
        e("static/css/site.css", "body{margin:0}"),
    ];
    let written = write_file_set(s.path(), &set).expect("the set writes");
    assert_eq!(
        written,
        vec![
            "index.html".to_string(),
            "run.sh".to_string(),
            "data/leads.json".to_string(),
            "static/css/site.css".to_string()
        ]
    );
    assert_eq!(
        s.listing(),
        vec![
            "data/leads.json".to_string(),
            "index.html".to_string(),
            "run.sh".to_string(),
            "static/css/site.css".to_string()
        ],
        "the files on disk are the deliverable, and nothing else is"
    );
    // The BYTES, not just the names.
    assert_eq!(
        std::fs::read_to_string(s.path().join("data/leads.json")).unwrap(),
        "[]"
    );
    assert_eq!(
        std::fs::read_to_string(s.path().join("run.sh")).unwrap(),
        "#!/bin/bash\npython3 server.py\n"
    );
}

#[test]
fn writing_the_same_path_twice_overwrites_and_creates_nothing_else() {
    let s = Scratch::new("overwrite");
    write_file_set(s.path(), &[e("index.html", "first")]).unwrap();
    write_file_set(s.path(), &[e("index.html", "second")]).unwrap();
    assert_eq!(s.listing(), vec!["index.html".to_string()]);
    assert_eq!(
        std::fs::read_to_string(s.path().join("index.html")).unwrap(),
        "second"
    );
}

// ── kill criterion 1: a path may never escape, and a refusal is the WHOLE call ───────────────
/// Each refusal reason must be REACHABLE, not merely present. Deleting the `..` check and deleting
/// the backslash check both left the suite green, because the leading-dot rule and the component
/// charset rule already refuse the same inputs — two more guards that could not fire, found the way
/// the others were. They are kept for their clearer diagnostic, and pinned here so they stay live.
#[test]
fn each_refusal_reason_is_reachable_and_says_the_right_thing() {
    let why = |path: &str| match plan_file_set(&[e(path, "x")]) {
        Err(FileSetRefusal::UnsafePath { why, .. }) => why.to_string(),
        other => panic!("{path:?} was not refused as an unsafe path: {other:?}"),
    };
    assert_eq!(why("../evil.html"), "it contains a relative component");
    assert_eq!(why("a/../b.txt"), "it contains a relative component");
    assert_eq!(why("./x.txt"), "it contains a relative component");
    assert_eq!(why(r"sub\dir\x.txt"), "it contains a backslash");
    assert_eq!(why("/etc/passwd"), "it is absolute");
    assert_eq!(why(".hidden"), "a component starts with a dot");
    assert_eq!(why("C:/windows/x.txt"), "it names a drive");
    assert_eq!(why("a//b.txt"), "it has an empty path component");
    assert_eq!(why("bad name.txt"), "a component has a character outside [A-Za-z0-9-_.]");
    assert_eq!(why(""), "it is empty");
}

#[test]
fn no_path_can_escape_the_deliverable() {
    for bad in [
        "../evil.html",
        "a/../../evil.html",
        "/etc/passwd",
        "\\\\server\\share\\x.txt",
        "C:/windows/system32/x.txt",
        "sub\\dir\\x.txt",
        "./x.txt",
        ".hidden",
        "a//b.txt",
        "  ",
        "",
    ] {
        let r = plan_file_set(&[e(bad, "x")]);
        assert!(
            matches!(
                r,
                Err(FileSetRefusal::UnsafePath { .. }) | Err(FileSetRefusal::TooDeep { .. })
            ),
            "accepted {bad:?}: {r:?}"
        );
    }
}

#[test]
fn a_refused_set_leaves_the_destination_untouched() {
    let s = Scratch::new("refused");
    // A perfectly good file FIRST, then a hostile one. Writing the good one and stopping would be
    // a half-finished deliverable that looks finished — worse than writing nothing.
    let set = [
        e("index.html", "<html></html>"),
        e("data/notes.txt", "fine"),
        e("../escape.txt", "no"),
    ];
    let err = write_file_set(s.path(), &set).unwrap_err().to_string();
    assert!(err.contains("escape.txt"), "the message names the entry: {err}");
    assert!(
        s.listing().is_empty(),
        "a refused set wrote something: {:?}",
        s.listing()
    );
}

// ── kill criterion 2: a refusal says which path and why ─────────────────────────────────────
#[test]
fn every_refusal_names_the_entry_and_the_reason() {
    let cases: Vec<(FileSetRefusal, &str)> = vec![
        (plan_file_set(&[]).unwrap_err(), "empty"),
        (
            plan_file_set(&[e("../x.txt", "x")]).unwrap_err(),
            "../x.txt",
        ),

        (
            plan_file_set(&[e("a/b/c/d/e.txt", "x")]).unwrap_err(),
            "a/b/c/d/e.txt",
        ),
    ];
    for (r, needle) in cases {
        let m = r.message();
        assert!(
            m.contains(needle),
            "refusal message {m:?} does not name {needle:?}"
        );
        assert!(m.len() > 10, "refusal message is not actionable: {m:?}");
    }
}

// ── kill criterion 3: the caps ──────────────────────────────────────────────────────────────
#[test]
fn the_caps_are_enforced_at_their_edges() {
    // File count: the cap itself is fine, one more is not.
    let ok: Vec<FileEntry> = (0..MAX_FILES).map(|i| e(&format!("f{i}.txt"), "x")).collect();
    assert!(plan_file_set(&ok).is_ok());
    let over: Vec<FileEntry> = (0..MAX_FILES + 1)
        .map(|i| e(&format!("f{i}.txt"), "x"))
        .collect();
    assert!(matches!(
        plan_file_set(&over),
        Err(FileSetRefusal::TooManyFiles { .. })
    ));

    // Per file.
    let big = "x".repeat(MAX_FILE_BYTES + 1);
    assert!(matches!(
        plan_file_set(&[e("big.txt", &big)]),
        Err(FileSetRefusal::FileTooLarge { .. })
    ));
    let at = "x".repeat(MAX_FILE_BYTES);
    assert!(plan_file_set(&[e("at.txt", &at)]).is_ok());

    // Total, reached by several files that are each individually fine.
    let chunk = "x".repeat(MAX_FILE_BYTES);
    let many: Vec<FileEntry> = (0..(MAX_TOTAL_BYTES / MAX_FILE_BYTES) + 1)
        .map(|i| e(&format!("c{i}.txt"), &chunk))
        .collect();
    assert!(matches!(
        plan_file_set(&many),
        Err(FileSetRefusal::TotalTooLarge { .. })
    ));

    // Depth: MAX_DEPTH components is fine, one more is not.
    let deep_ok = "a/b/c/d.txt";
    assert_eq!(deep_ok.split('/').count(), MAX_DEPTH);
    assert!(plan_file_set(&[e(deep_ok, "x")]).is_ok());
    assert!(matches!(
        plan_file_set(&[e("a/b/c/d/e.txt", "x")]),
        Err(FileSetRefusal::TooDeep { .. })
    ));
}

// ── kill criterion 4: empty, duplicate, and a path that is a directory ───────────────────────
#[test]
fn an_empty_set_a_duplicate_and_a_directory_are_each_refused() {
    assert_eq!(plan_file_set(&[]), Err(FileSetRefusal::Empty));
    // E.WIN6 — a DUPLICATE no longer refuses the set. It used to, and a real `ym delegate` run of
    // the T1 brief died on it: the model emitted `run.sh` twice and the build produced nothing at
    // all. The lane already means "later replaces earlier", so the last definition wins and the
    // repeat is reported to the review instead of costing the whole artifact.
    let planned = plan_file_set(&[e("a.txt", "1"), e("b.txt", "2"), e("a.txt", "3")])
        .expect("a duplicate must no longer destroy the set");
    assert_eq!(planned.len(), 2, "one path, one file still holds on disk");
    assert_eq!(
        planned.iter().find(|x| x.path == "a.txt").map(|x| x.content.as_str()),
        Some("3"),
        "the LAST definition is the model's latest intent"
    );
    assert_eq!(
        crate::fileset::duplicate_paths(
            "=== FILE: a.txt
1
=== FILE: b.txt
2
=== FILE: a.txt
3
"
        ),
        vec!["a.txt".to_string()],
        "and the repeat must be reportable, or a wrong winner is silent"
    );
    // A path whose parent is already a FILE cannot be written; the error is reported, not ignored.
    let s = Scratch::new("dirclash");
    write_file_set(s.path(), &[e("a.txt", "x")]).unwrap();
    let r = write_file_set(s.path(), &[e("a.txt/b.txt", "y")]);
    assert!(r.is_err(), "writing under a file must fail, not be skipped");
    assert_eq!(
        std::fs::read_to_string(s.path().join("a.txt")).unwrap(),
        "x",
        "and it must not have damaged what was there"
    );
}

// ── kill criterion 5: it only creates inside its destination ─────────────────────────────────
#[test]
fn nothing_outside_the_destination_is_touched() {
    let outer = Scratch::new("outer");
    std::fs::write(outer.path().join("precious.txt"), "keep me").unwrap();
    let inner = outer.path().join("deliverable");
    std::fs::create_dir_all(&inner).unwrap();
    // Every hostile shape, one call each; none may reach `precious.txt`.
    for bad in ["../precious.txt", "../../precious.txt", "/precious.txt"] {
        let _ = write_file_set(&inner, &[e(bad, "clobbered")]);
    }
    assert_eq!(
        std::fs::read_to_string(outer.path().join("precious.txt")).unwrap(),
        "keep me"
    );
    // ...and a legitimate write still lands where it should.
    write_file_set(&inner, &[e("index.html", "ok")]).unwrap();
    assert_eq!(
        std::fs::read_to_string(inner.join("index.html")).unwrap(),
        "ok"
    );
}

// ── the module is wired to nothing, and that is a property worth pinning ─────────────────────
#[test]
fn nothing_calls_this_yet() {
    // E.FILES1 is the capability only; the wiring is a separate slice with its own preregistration.
    // If this fails, the wiring landed without that row being written, which is the thing the
    // staging exists to prevent.
    const SELF: &str = include_str!("fileset.rs");
    assert!(SELF.contains("WIRED TO NOTHING"));
    for src in [
        include_str!("delegate.rs"),
        include_str!("capabilities.rs"),
        include_str!("cognitive.rs"),
    ] {
        assert!(
            !src.contains("write_file_set") && !src.contains("plan_file_set"),
            "a caller appeared before the wiring slice was preregistered"
        );
    }
}

// ── the delimited stream ─────────────────────────────────────────────────────────────────────
mod stream {
    use super::*;
    use crate::fileset::{parse_file_stream, FILE_MARKER};

    fn stream(parts: &[(&str, &str)], trailing_newline: bool) -> String {
        let mut out = String::new();
        for (path, body) in parts {
            out.push_str(&format!("{FILE_MARKER} {path}\n{body}"));
        }
        if trailing_newline && !out.ends_with('\u{a}') {
            out.push('\u{a}');
        }
        out
    }

    #[test]
    fn a_well_formed_stream_yields_every_file_with_its_bytes() {
        let text = stream(
            &[
                ("index.html", "<!doctype html>\n<h1>hi</h1>\n"),
                ("run.sh", "#!/bin/bash\npython3 server.py\n"),
                ("data/leads.json", "[]\n"),
            ],
            true,
        );
        let got = parse_file_stream(&text);
        assert!(got.unterminated.is_empty(), "{:?}", got.unterminated);
        assert_eq!(got.preamble, "");
        let paths: Vec<&str> = got.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["index.html", "run.sh", "data/leads.json"]);
        assert_eq!(got.entries[0].content, "<!doctype html>\n<h1>hi</h1>\n");
        assert_eq!(got.entries[2].content, "[]\n");
    }

    #[test]
    fn a_files_contents_can_never_break_the_format() {
        // The reason this is not JSON: a file may contain quotes, backslashes, braces and newlines,
        // and the only thing that ends a file here is the NEXT marker at the start of a line.
        let nasty = "{\"a\": \"b\\\\c\"}\nnot a marker: === FILE: fake.txt\n";
        let text = stream(&[("weird.json", nasty), ("after.txt", "ok\n")], true);
        let got = parse_file_stream(&text);
        assert_eq!(
            got.entries.len(),
            2,
            "content that merely MENTIONS the marker must not split a file: {got:?}"
        );
        assert_eq!(got.entries[0].path, "weird.json");
        assert_eq!(got.entries[0].content, nasty);
        assert_eq!(got.entries[1].path, "after.txt");
    }

    #[test]
    fn the_formats_one_sharp_edge_recorded_rather_than_hidden() {
        // A marker at the START of a line inside a file DOES split it. That is the price of a
        // format with no escaping, it is the only input that can corrupt a set, and it is written
        // down here so nobody discovers it from a broken deliverable. The build prompt tells the
        // model not to begin a line with the marker inside a file's contents.
        let text = format!(
            "{FILE_MARKER} doc.md
how to use the format:
{FILE_MARKER} example.txt
body
"
        );
        let got = parse_file_stream(&text);
        assert_eq!(
            got.entries.len(),
            2,
            "the sharp edge is real: the file was split"
        );
        assert_eq!(got.entries[0].content, "how to use the format:
");
    }

    #[test]
    fn a_stream_that_does_not_end_with_a_newline_keeps_its_last_file_and_says_so() {
        // THIS TEST ASSERTED THE OPPOSITE AND THE OPPOSITE WAS WRONG. It required the last file to
        // be DROPPED as truncated. In a preflight leg that rule threw away a complete
        // `test_tracker.py` — the whole test suite — and reported it as cut off. Reproducing the
        // same generation showed `finish_reason: stop`, three complete files, and no trailing
        // newline, which is simply how models often end.
        //
        // A missing final newline cannot distinguish a cut-off file from a finished one. The parser
        // reports the observation and keeps the work; only `finish_reason` could settle it and the
        // parser does not have it.
        let text = format!(
            "{FILE_MARKER} index.html\n<!doctype html>\n{FILE_MARKER} run.sh\n#!/bin/bash\necho start"
        );
        let got = parse_file_stream(&text);
        assert_eq!(
            got.unterminated,
            vec!["run.sh".to_string()],
            "the observation is reported"
        );
        let paths: Vec<&str> = got.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["index.html", "run.sh"],
            "and NOTHING is thrown away: dropping a probably-complete file is the worse error"
        );
        assert_eq!(got.entries[1].content, "#!/bin/bash\necho start");
    }

    #[test]
    fn a_stream_that_ends_cleanly_reports_nothing_unterminated() {
        // The other side of the same rule, so "unterminated" cannot become a constant.
        let text = format!("{FILE_MARKER} a.txt\nbody\n");
        let got = parse_file_stream(&text);
        assert!(got.unterminated.is_empty(), "{:?}", got.unterminated);
        assert_eq!(got.entries.len(), 1);
    }

    #[test]
    fn a_preamble_is_kept_as_evidence_rather_than_dropped() {
        let text = format!("Sure! Here are the files:\n\n{FILE_MARKER} a.txt\nbody\n");
        let got = parse_file_stream(&text);
        assert_eq!(got.entries.len(), 1);
        assert_eq!(got.entries[0].content, "body\n");
        assert!(
            got.preamble.contains("Sure!"),
            "the model ignoring the format is worth being able to see: {got:?}"
        );
    }

    #[test]
    fn a_stream_with_no_marker_at_all_yields_nothing_and_says_why() {
        let got = parse_file_stream("I could not do that.\n");
        assert!(got.entries.is_empty());
        assert!(got.unterminated.is_empty());
        assert_eq!(got.preamble, "I could not do that.\n");
    }

    #[test]
    fn a_backticked_or_padded_path_is_read_as_the_path() {
        // Models fence things. The marker's tail is a path, not a code span.
        let text = format!("{FILE_MARKER}   `src/main.py`  \nprint(1)\n");
        let got = parse_file_stream(&text);
        assert_eq!(got.entries.len(), 1);
        assert_eq!(got.entries[0].path, "src/main.py");
    }

    #[test]
    fn the_parser_never_decides_whether_a_path_is_safe() {
        // Separation of duties: parsing recovers what was said, `plan_file_set` decides what may be
        // written. A parser that quietly dropped unsafe paths would make a hostile set look like a
        // smaller honest one.
        let text = format!("{FILE_MARKER} ../escape.txt\nx\n");
        let got = parse_file_stream(&text);
        assert_eq!(got.entries.len(), 1);
        assert_eq!(got.entries[0].path, "../escape.txt");
        assert!(matches!(
            plan_file_set(&got.entries),
            Err(FileSetRefusal::UnsafePath { .. })
        ));
    }

    #[test]
    fn an_unterminated_stream_still_writes_every_file_it_recovered() {
        // The end-to-end property from kill criterion 1: parse, then plan, then write. A set whose
        // recovered entries are all valid writes; one containing a bad path writes nothing.
        let s = Scratch::new("stream");
        let good = format!("{FILE_MARKER} index.html\n<h1>hi</h1>\n{FILE_MARKER} run.sh\necho ok\n");
        let parsed = parse_file_stream(&good);
        write_file_set(s.path(), &parsed.entries).expect("a valid recovered set writes");
        assert_eq!(
            s.listing(),
            vec!["index.html".to_string(), "run.sh".to_string()]
        );

        let s2 = Scratch::new("stream-bad");
        let bad = format!("{FILE_MARKER} index.html\n<h1>hi</h1>\n{FILE_MARKER} ../x.txt\nno\n");
        let parsed = parse_file_stream(&bad);
        assert!(write_file_set(s2.path(), &parsed.entries).is_err());
        assert!(
            s2.listing().is_empty(),
            "a set containing one bad path wrote something: {:?}",
            s2.listing()
        );
    }
}

// ── the test that was missing, twice ─────────────────────────────────────────────────────────
mod wiring {
    /// EVERY tool a delegation recipe names must resolve in the dispatch a RECIPE reaches.
    ///
    /// This is the third time the same shape has cost a slice. E.PAGE1 shipped as a no-op because
    /// no test asked what the file on disk was called. E.FILES2 shipped with `write_files`
    /// registered in `capabilities.rs` — the conversation engine's own tool dispatch — while a
    /// recipe reaches `call_tool` in lib.rs, so a graded leg ran, the model produced 2278 tokens of
    /// files, and the chain died on "unknown source 'write_files'" with the deliverable discarded.
    ///
    /// A source scan, deliberately: the dispatch is an `async fn` on a type that needs a whole
    /// engine to construct, and a test that cannot be written without one does not get written.
    /// What it checks is exact — the tool names the recipes emit, against the arms that exist.
    #[test]
    fn every_tool_a_recipe_names_resolves_in_the_dispatch_a_recipe_reaches() {
        const DELEGATE: &str = include_str!("delegate.rs");
        const LIB: &str = include_str!("lib.rs");

        // The dispatch a recipe reaches, isolated so an arm in the OTHER dispatch cannot satisfy
        // this test — which is exactly the mistake being guarded against.
        let start = LIB
            .find("async fn call_tool(&self, tool: &str")
            .expect("the recipe-facing dispatch exists");
        let dispatch = &LIB[start..start + 40_000.min(LIB.len() - start)];

        let mut named: Vec<String> = Vec::new();
        for (i, _) in DELEGATE.match_indices("tool_name: \"") {
            let rest = &DELEGATE[i + "tool_name: \"".len()..];
            if let Some(end) = rest.find('"') {
                let name = rest[..end].to_string();
                if !named.contains(&name) {
                    named.push(name);
                }
            }
        }
        assert!(
            named.len() >= 3,
            "the scan found almost no tool names, so it is not scanning: {named:?}"
        );
        for name in &named {
            assert!(
                dispatch.contains(&format!("\"{name}\" =>"))
                    || dispatch.contains(&format!("| \"{name}\" =>")),
                "the recipes name the tool {name:?} and the dispatch a recipe reaches has no arm                  for it — a chain will die on \"unknown source\" with its work already done"
            );
        }
        // Anti-vacuity: the two that matter must actually be in the list the loop checked.
        assert!(named.iter().any(|n| n == "write_files"), "{named:?}");
        assert!(named.iter().any(|n| n == "publish_page"), "{named:?}");
    }
}

// ── E.FILES3: the review round, and the line it must not cross ───────────────────────────────
mod review {
    use crate::delegate::build_recipe;
    use mind_recipes::{ErrorAction, RecipeStep};

    fn steps() -> Vec<RecipeStep> {
        build_recipe("t", "proj", "build a task tracker with tests", None).steps
    }

    // ── E.CB2-B: truncation becomes a VERDICT instead of a guess ─────────────────────────────
    //
    // `parse_file_stream` refuses to decide whether the last file is finished, and says why: a
    // missing trailing newline is equally consistent with a cut-off generation and a model that
    // ended without one, and "only the API's finish_reason could settle it". It was never absent —
    // `LLMResponse.stop_reason` carries it — it was dropped in the recipe's Think step, one move
    // before the only consumer that needs it. These pin the whole path.

    /// E.CB2-B step 2 — THE RECIPE MUST ACTUALLY USE THE CLAMP, not merely have one available.
    ///
    /// Twice in one day a change was wired and never proven to carry a value: a `CB2_WALL` export
    /// the legs needed, and the `Think` step's stop-reason hand-off. Both compiled, both read
    /// correctly, both broke nothing when deleted. `authoring_budget` existing in mind-inference
    /// says nothing about whether `build_recipe` calls it, so ask the recipe.
    #[test]
    fn every_authoring_step_is_clamped_to_the_providers_deadline() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("YM_PROVIDER_DEADLINE_S").ok();

        // With NIM's measured deadline declared, no authoring step may ask for more than it affords.
        std::env::set_var("YM_PROVIDER_DEADLINE_S", "302");
        let affordable = mind_inference::authoring_budget(16_000);
        let clamped: Vec<usize> = steps()
            .iter()
            .filter_map(|st| match st {
                RecipeStep::Think { max_tokens, .. } => *max_tokens,
                _ => None,
            })
            .collect();
        assert!(!clamped.is_empty(), "the build recipe has no authoring step at all");
        for b in &clamped {
            assert!(
                *b <= affordable,
                "an authoring step asks for {b} tokens where the provider's deadline affords                  {affordable} — the constant is still hard-coded"
            );
        }

        // And with NO deadline declared the recipe is unchanged, which is what makes it safe to
        // ship: the Mind keeps its few-big-calls shape wherever a provider has said nothing.
        std::env::remove_var("YM_PROVIDER_DEADLINE_S");
        let unclamped: Vec<usize> = steps()
            .iter()
            .filter_map(|st| match st {
                RecipeStep::Think { max_tokens, .. } => *max_tokens,
                _ => None,
            })
            .collect();
        assert!(
            unclamped.iter().any(|b| *b > affordable),
            "with no declared deadline the recipe must ask for the full budget, not the clamped one"
        );

        match prev {
            Some(x) => std::env::set_var("YM_PROVIDER_DEADLINE_S", x),
            None => std::env::remove_var("YM_PROVIDER_DEADLINE_S"),
        }
    }

    /// E.CB2-B step 3 — the review emits a DELTA, and only the review may come back empty.
    ///
    /// Re-emitting the complete set made the review as large as the authoring step, which is what
    /// put it over a provider deadline and created the hazard that matters: its `write_files` runs
    /// on whatever parses, so a review cut mid-file overwrites a COMPLETE file with a partial one.
    /// The semantics were always safe — the prompt itself says a file missing from the output keeps
    /// its old contents — the prompt simply asked for everything anyway.
    #[test]
    fn the_review_asks_for_a_delta_and_only_the_review_may_be_empty() {
        let s = steps();
        let thinks: Vec<&str> = s
            .iter()
            .filter_map(|st| match st {
                RecipeStep::Think { prompt, .. } => Some(prompt.as_str()),
                _ => None,
            })
            .collect();
        let review = thinks.last().expect("no review step");
        assert!(
            review.contains("ONLY the files you are CHANGING"),
            "the review still asks for the whole set"
        );
        assert!(
            !review.contains("output the COMPLETE set again"),
            "the review still asks for the whole set"
        );
        // The safety property must survive the rewrite: omitting a file is how you leave it alone.
        assert!(
            review.contains("keeps its old") && review.contains("never how you delete it"),
            "the review lost the rule that omitting a file is not deleting it"
        );

        let writes: Vec<&serde_json::Value> = s
            .iter()
            .filter_map(|st| match st {
                RecipeStep::Tool { tool_name, args, .. } if tool_name == "write_files" => Some(args),
                _ => None,
            })
            .collect();
        assert!(writes.len() >= 2);
        // THE FIRST WRITE IS THE DELIVERABLE. An empty authoring result is a real failure and must
        // stay one; every LATER write is an improvement and may legitimately change nothing.
        assert_ne!(
            writes[0].get("allow_empty").and_then(|v| v.as_bool()),
            Some(true),
            "an empty BUILD must not be excused: that write is the deliverable"
        );
        for (n, w) in writes.iter().enumerate().skip(1) {
            assert_eq!(
                w.get("allow_empty").and_then(|v| v.as_bool()),
                Some(true),
                "write {n} improves an existing set, so changing nothing is the healthy case and                  must not read as a failed build"
            );
        }
    }

    /// E.CB2-B — EVERY large generation budget is either clamped or an EXPLAINED exception.
    ///
    /// `build_recipe` and the venture forge both author a deliverable in one generation and are
    /// clamped. Other budgets are just as large and are deliberately NOT clamped, because
    /// truncating them fails WORSE than the 504 clamping would prevent — so they are listed with
    /// their reasons, and a new one cannot appear silently.
    ///
    /// The registry shape (guarded / exempt-with-a-reason) rather than a remembered file list: a
    /// budget added tomorrow either clamps or lands in front of a reviewer.
    #[test]
    fn every_large_generation_budget_is_clamped_or_an_explained_exception() {
        // What a 302 s deadline affords at the measured 15 tok/s floor. At or above this, a
        // generation cannot complete against a provider that cuts there.
        const AFFORDABLE: usize = 3_171;

        // (file, literal, why it is NOT clamped). Each would fail worse truncated than refused.
        // Keyed by VALUE, not by the literal's spelling: `8000` and `8_000` are the same budget
        // and an exemption that matched only one of them would silently stop covering the site the
        // day someone reformatted it. Caught by this guard failing on exactly that mismatch.
        const EXEMPT: &[(&str, usize, &str)] = &[
            ("delegate.rs", 15_000,
             "the delegated-build CRITIC shares its budget with its own reasoning; measured, a               critique over a 12KB excerpt came back EMPTY at 2000, and an empty critique reads as               approval. A judge that cannot afford to explain itself defaults to shipping, which is               strictly worse than a refused request."),
            ("lib.rs", 8_000,
             "tool-call DISPATCH: a publish_page call inlines a whole HTML page into the tool               arguments, so a small budget yields truncated, unparseable JSON rather than a               shorter answer -- the failure the comment there records having already happened."),
        ];

        let sources: &[(&str, &str)] = &[
            ("delegate.rs", include_str!("delegate.rs")),
            ("code.rs", include_str!("code.rs")),
            // lib.rs is scanned too, despite its size: leaving it out would hide the dispatch
            // budget, which is exactly the "a list is only as complete as its author remembered"
            // failure this registry exists to avoid.
            ("lib.rs", include_str!("lib.rs")),
        ];
        let mut unexplained: Vec<String> = Vec::new();
        for (name, src) in sources {
            for (n, line) in src.lines().enumerate() {
                let Some(rest) = line.split("max_tokens: ").nth(1) else { continue };
                let lit: String = rest.chars().take_while(|c| c.is_ascii_digit() || *c == '_').collect();
                if lit.is_empty() {
                    continue; // already `authoring_budget(..)`
                }
                let value: usize = lit.replace('_', "").parse().unwrap_or(0);
                if value < AFFORDABLE || EXEMPT.iter().any(|(f, v, _)| f == name && *v == value) {
                    continue;
                }
                unexplained.push(format!("{name}:{}: max_tokens: {lit}", n + 1));
            }
        }
        assert!(
            unexplained.is_empty(),
            "a generation budget at or above what a 302s deadline affords ({AFFORDABLE}) is              neither clamped by mind_inference::authoring_budget nor listed as an explained              exception:
  {}",
            unexplained.join("
  ")
        );

        // The registry must not quietly empty out: deleting every exemption would make the check
        // above pass vacuously, so require each to still carry a real reason.
        assert!(!EXEMPT.is_empty(), "the exemption registry was emptied");
        for (f, l, why) in EXEMPT {
            assert!(why.len() > 60, "exemption {f}/{l} has no real reason");
        }
    }

    /// E.CB2-B2 — BOTH lanes that author a file set refuse a cut file, not just the one that was
    /// measured failing.
    ///
    /// The build lane got the truncation verdict because reading 7 showed what happens without it.
    /// The VENTURE forge authors the same way, parses the blocks inline instead of through
    /// `publish_file_set`, and had no verdict at all — and I had made it worse an hour earlier by
    /// clamping its budget, which makes a cut generation more likely. A fix applied only where the
    /// failure was observed is half a fix.
    #[test]
    fn every_lane_that_authors_a_file_set_refuses_a_cut_file() {
        const CODE: &str = include_str!("code.rs");
        // The forge parses `r.text` itself, so it must consult `r.stop_reason` itself.
        assert!(
            CODE.contains("r.stop_reason == \"length\""),
            "the venture forge writes whatever parses, including a file cut mid-content"
        );
        // ...and drop only the LAST block, since every earlier one was terminated by the next
        // marker. Dropping more would delete complete files.
        assert!(
            CODE.contains("blocks.len() - 1"),
            "only the final block can be the cut one; dropping more deletes complete files"
        );
        // The caller must be told, or a short set looks like a model that wrote less.
        assert!(
            CODE.contains("cut at the token limit and NOT written"),
            "a dropped file must be reported, not silently missing"
        );
    }

    /// E.CB2-B2 — "everything was cut" must stay RECOVERABLE, and "wrote prose" must not.
    ///
    /// Found by forcing the failure instead of awaiting it: at a tight deadline the authoring
    /// generation produced one file, it was cut, it was correctly refused — and the write then
    /// failed hard, killing the chain BEFORE the completion pass that exists to finish it. The
    /// recovery path was unreachable exactly when it was needed, and a clean-path gate could never
    /// have shown that.
    ///
    /// The set is empty in both cases; the difference is whether there is anything to recover.
    #[test]
    fn an_all_cut_generation_is_recoverable_but_prose_still_fails() {
        let dir = mind_types::scratch::dir("fset_allcut");
        std::env::set_var("YM_WEB_DIR", dir.path());

        // ONE file, cut. Nothing can be written, but the brief is still buildable.
        let cut = "=== FILE: server.py
print('this file was cut off mid";
        let (url, written, reported) =
            crate::publish_file_set("allcut", cut, true).expect("an all-cut set must NOT be an error");
        assert!(url.is_empty() && written.is_empty(), "nothing may be written from a cut file");
        assert_eq!(reported, vec!["server.py".to_string()], "and the caller must be told what was lost");

        // Prose with no markers is a FORMAT failure, not a budget one — nothing to recover, and it
        // must stay an error so "it ignored the format" is not silently retried forever.
        let prose = "Sure! Here is your web application, I hope you like it.";
        assert!(
            crate::publish_file_set("prose", prose, true).is_err(),
            "a model that ignored the format must still fail, cut or not"
        );
        // ...and the same prose without truncation is equally an error.
        assert!(crate::publish_file_set("prose2", prose, false).is_err());
    }

    /// E.CB2-B2 — a wrapping markdown fence is unwrapped, and a Markdown file keeps its own.
    ///
    /// `publish_page` already carries this lesson: a model asked for "only the HTML" wraps it in
    /// a fence about half the time, and "the alternative is a prompt that has to win every time".
    /// The file-set lane never got it and the prompt lost — a completion pass emitted a correct
    /// `main.py` wrapped in a python fence, the fence was written verbatim, the file did not
    /// parse, and the site never came up: 2/11 on a leg whose content was actually right.
    #[test]
    fn a_wrapping_fence_is_stripped_but_a_markdown_file_keeps_its_own() {
        let q = "```";
        let stream = format!("=== FILE: main.py{n}{q}python{n}print(1){n}{q}{n}=== FILE: notes.md{n}Intro line.{n}{n}{q}bash{n}echo hi{n}{q}{n}{n}Closing line.{n}", n = "\n", q = q);
        let parsed = crate::fileset::parse_file_stream(&stream);
        let by = |path: &str| {
            parsed.entries.iter().find(|e| e.path == path).map(|e| e.content.clone()).unwrap_or_default()
        };
        // The wrapper goes and the CODE survives intact.
        assert_eq!(by("main.py").trim(), "print(1)", "the wrapping fence was not stripped");
        // A Markdown file that merely CONTAINS a fenced block keeps it: it does not both begin
        // and end with one, which is exactly why the rule is narrow.
        let notes = by("notes.md");
        assert!(notes.contains(q), "a markdown file lost its own fenced block: {notes:?}");
        assert!(notes.trim_start().starts_with("Intro line."), "mangled at the front: {notes:?}");
        assert!(notes.trim_end().ends_with("Closing line."), "mangled at the end: {notes:?}");

        // THE ONLY FILE IN A STREAM IS THE FINAL ONE, and the final file is built on a different
        // path from the rest. This case exists because patching only the loop left exactly the
        // motivating scenario -- a completion pass emitting ONE file -- still fenced.
        let solo = format!("=== FILE: solo.py{n}{q}python{n}print(3){n}{q}{n}", n = "\n", q = q);
        let sp = crate::fileset::parse_file_stream(&solo);
        assert_eq!(
            sp.entries[0].content.trim(),
            "print(3)",
            "the ONLY file in a stream is the final one and was left fenced: {:?}",
            sp.entries[0].content
        );

        // Content that ENDS with a fence but does not OPEN with one is not wrapped either.
        let tail = format!("=== FILE: tail.md{n}See below.{n}{q}{n}", n = "\n", q = q);
        let tp = crate::fileset::parse_file_stream(&tail);
        assert!(
            tp.entries[0].content.trim_start().starts_with("See below."),
            "a trailing fence is not a wrapper: {:?}",
            tp.entries[0].content
        );

        // A lone fence line is not a wrapper around anything, and must survive untouched.
        let lone = format!("=== FILE: lone.md{n}{q}{n}", n = "\n", q = q);
        let lp = crate::fileset::parse_file_stream(&lone);
        assert!(
            lp.entries[0].content.contains(q),
            "a lone fence line was eaten: {:?}",
            lp.entries[0].content
        );

        // BOTH ends must be fences for it to be a wrapper. Content that OPENS with one and never
        // closes is not wrapped -- it is a file that happens to start that way, or one that was
        // cut -- and stripping its first line would silently delete real content. Found by a
        // surviving mutant that dropped the closing-fence requirement.
        let half = format!("=== FILE: half.md{n}{q}python{n}print(2){n}", n = "\n", q = q);
        let hp = crate::fileset::parse_file_stream(&half);
        assert!(
            hp.entries[0].content.trim_start().starts_with(q)
                && hp.entries[0].content.contains("print(2)"),
            "an unclosed opening fence is not a wrapper and must be left alone: {:?}",
            hp.entries[0].content
        );
    }

    /// E.LOOP-T1 — A CAPABILITY MUST NOT SILENTLY SHADOW A GUARDED BUILT-IN.
    ///
    /// `run_tool` consults the plugin registry BEFORE the built-in dispatch and returns on a hit,
    /// so a registered capability that answers a tool the built-in also implements makes the
    /// built-in unreachable. That happened to `publish_page`: three guards — fence unwrapping, the
    /// refusal of a document cut mid-generation, and E.PAGE1's `required_filename` precedence —
    /// were live in code that never ran, while the shadowing arm had none of them. Nothing failed;
    /// the guards were simply not in force.
    ///
    /// This is the class, not the instance: any future capability arm for a plugin-declared tool
    /// that the built-in also implements must either DEFER (`=> return None`) or be listed here
    /// with a reason. Silence is what let the first one through.
    #[test]
    fn no_capability_silently_shadows_a_guarded_builtin() {
        const PLUGINS: &str = include_str!("plugins.rs");
        const LIB: &str = include_str!("lib.rs");
        const CAPS: &str = include_str!("capabilities.rs");

        // Tools a capability answers: `    "name" => ...` at an arm's indentation.
        let arm_names = |src: &str| -> Vec<String> {
            src.lines()
                .filter_map(|l| {
                    let t = l.trim();
                    let rest = t.strip_prefix('"')?;
                    let (name, tail) = rest.split_once('"')?;
                    if !tail.trim_start().starts_with("=>") {
                        return None;
                    }
                    if name.is_empty()
                        || !name.chars().all(|c| c.is_ascii_lowercase() || c == '_' || c.is_ascii_digit())
                    {
                        return None;
                    }
                    Some(name.to_string())
                })
                .collect()
        };
        let cap_tools = arm_names(CAPS);
        let builtin_tools = arm_names(LIB);

        // A tool is only routed to a capability if some PluginSpec DECLARES it.
        let declared = |tool: &str| -> bool {
            let needle = format!("\"{tool}\"");
            PLUGINS
                .lines()
                .any(|l| l.contains("PluginSpec::new(") && l.contains(&needle))
        };

        // Intentional shadowing, each with a reason. Empty is fine; unexplained is not.
        const ALLOWED: &[(&str, &str)] = &[];

        let mut offenders: Vec<String> = Vec::new();
        for tool in &cap_tools {
            // DELIBERATELY NOT gated on `declared`. An arm that duplicates a built-in but is not
            // currently routed is not harmless: `write_files` was exactly that, and its second copy
            // of the writer's logic had to be kept in step by hand — it is the copy the next person
            // fixes instead of the live one. It also becomes a real shadow the moment someone adds
            // the tool to a PluginSpec. `declared` is kept only to sharpen the message.
            if !builtin_tools.contains(tool) {
                continue; // the capability is the only implementation — nothing to diverge from
            }
            let _routed_today = declared(tool);
            let defers = CAPS.contains(&format!("\"{tool}\" => return None"));
            if !defers && !ALLOWED.iter().any(|(t, _)| t == tool) {
                offenders.push(tool.clone());
            }
        }
        assert!(
            offenders.is_empty(),
            "these capability arms duplicate a built-in implementation — shadowing it where the              tool is plugin-declared, and drifting from it where it is not; defer with              `=> return None` or list them with a reason: {offenders:?}"
        );

        // The scan must be able to SEE something, or it passes vacuously forever.
        assert!(cap_tools.len() >= 5, "the capability scan found almost nothing: {cap_tools:?}");
        assert!(builtin_tools.len() >= 50, "the built-in scan found almost nothing");
        assert!(declared("publish_page"), "the plugin-declaration scan stopped working");
    }

    /// E.CB2-B3 — the completion pass is SKIPPED when nothing was cut, and its guard points at the
    /// review rather than past it.
    ///
    /// Most builds are not truncated. Spending a model call on every one would trade reading 7's
    /// failure for a permanent tax on the Mind's cheapest column — 2 requests against Hermes's
    /// 3-16 is what reading 6 measured as its advantage. The page lane already guards its repair
    /// for exactly this reason.
    ///
    /// A jump that overshot would silently skip the REVIEW as well, which is the kind of off-by-one
    /// that costs a quality round and shows up as nothing at all.
    #[test]
    fn the_completion_pass_is_skipped_when_nothing_was_cut() {
        let s = steps();
        let jump_ix = s
            .iter()
            .position(|st| matches!(st, RecipeStep::JumpIf { .. }))
            .expect("the completion pass is unguarded — every build pays for it");
        let (cond, target) = match &s[jump_ix] {
            RecipeStep::JumpIf { condition, target_step } => (condition, *target_step),
            _ => unreachable!(),
        };

        // It must key on the FIRST WRITE'S OWN MESSAGE, which is what carries the fact.
        let c = format!("{cond:?}");
        assert!(c.contains("project_url"), "the guard reads the wrong variable: {c}");
        assert!(c.contains("was cut"), "the guard does not key on truncation: {c}");
        assert!(c.contains("Not"), "the guard must SKIP when nothing was cut, not when it was: {c}");

        // The jump must land ON the review's Think, not past it.
        let think_ix: Vec<usize> = s
            .iter()
            .enumerate()
            .filter(|(_, st)| matches!(st, RecipeStep::Think { .. }))
            .map(|(i, _)| i)
            .collect();
        // Four since E.REPAIR3 added a conditional second review; the truncation guard still
        // lands on think_ix[2], the FIRST review, which is the property that matters here.
        assert_eq!(think_ix.len(), 4, "author, completion, review, second review");
        assert!(jump_ix < think_ix[1], "the guard must precede the pass it skips");
        assert_eq!(
            target, think_ix[2],
            "the jump must land on the REVIEW step; overshooting silently drops the quality round"
        );
        assert!(target < s.len(), "the jump target is past the end of the recipe");
    }

    /// E.CB2-P — the author is TOLD its budget, and only when one is actually known.
    ///
    /// Every T1 outcome measured was decided by decomposition: multi-file layouts scored 11/11,
    /// monolithic ones were cut and scored 2/11. The model chose blind — "one big file" and "four
    /// small files" looked equally affordable because it was never told what it had.
    ///
    /// The number must be MEASURED, never asked for. A model's account of its own limits is
    /// remembered, not measured, which is why `myself` is a measurement tool rather than a recall.
    /// And with no declared provider limit there is no number to state, so the prompt must not
    /// invent one.
    #[test]
    fn the_author_is_told_its_budget_only_when_one_is_measured() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let prev = std::env::var("YM_PROVIDER_DEADLINE_S").ok();

        let author_prompt = || -> String {
            steps()
                .iter()
                .find_map(|st| match st {
                    RecipeStep::Think { prompt, .. } => Some(prompt.clone()),
                    _ => None,
                })
                .expect("the build recipe has no authoring step")
        };

        // With NIM's measured deadline declared, the author is told the affordable number.
        std::env::set_var("YM_PROVIDER_DEADLINE_S", "302");
        let told = author_prompt();
        let budget = mind_inference::authoring_budget(16_000);
        assert!(
            told.contains(&budget.to_string()),
            "the author is not told the budget it must fit ({budget})"
        );
        assert!(
            told.contains("Several complete files beat one large one"),
            "the author is told a number but not what to DO with it"
        );

        // With no declared limit there is no measured number, so none is stated. Inventing one
        // would be exactly the self-report this design refuses.
        std::env::remove_var("YM_PROVIDER_DEADLINE_S");
        let untold = author_prompt();
        assert!(
            !untold.contains("YOUR BUDGET FOR THIS RESPONSE"),
            "a budget was asserted with nothing measured behind it"
        );

        match prev {
            Some(x) => std::env::set_var("YM_PROVIDER_DEADLINE_S", x),
            None => std::env::remove_var("YM_PROVIDER_DEADLINE_S"),
        }
    }

    /// E.PAGE1 — a filename the TASK required outranks the page's own title.
    ///
    /// The rule existed, was correct, and was **unreachable**: a capability shadowed the
    /// implementation holding it (E.LOOP-T1), so a brief asking for `index.html` still got a slug
    /// of its `<title>`. It was also untestable, because it lived inline in a match arm. Both are
    /// fixed here — the shadow is gone and the rule is a function that can be asked.
    #[test]
    fn a_required_filename_outranks_the_pages_title() {
        let html = "<html><head><title>My Lovely Page</title></head><body>x</body></html>";

        // THE CASE THAT MATTERED: the brief names the file, and the title must not win.
        assert_eq!(
            crate::page_filename(Some("index.html"), html, Some("whatever")),
            "index.html"
        );
        // With nothing required, the title names the page — the behaviour before E.PAGE1, kept.
        assert_eq!(crate::page_filename(None, html, Some("caller")), "My Lovely Page");
        // With neither, the caller's name; with none of the three, a safe default.
        assert_eq!(crate::page_filename(None, "<p>no title</p>", Some("caller")), "caller");
        assert_eq!(crate::page_filename(None, "<p>no title</p>", None), "page");
        // An EMPTY requirement is not a requirement: it must fall through rather than produce "".
        assert_eq!(crate::page_filename(Some("   "), html, None), "My Lovely Page");
        assert_eq!(crate::page_filename(Some(""), "<p>x</p>", Some("caller")), "caller");

        // AND THE PUBLISHER MUST ACTUALLY ASK IT. Testing the rule proves nothing about the arm
        // that is supposed to obey it -- a mutant that stopped calling `page_filename` and hardcoded
        // a name passed every assertion above. That is the fifth time today a value was declared
        // and never proven to arrive, so it gets the same treatment as the others.
        const LIB: &str = include_str!("lib.rs");
        let arm = LIB
            .split("\"publish_page\" => {")
            .nth(1)
            .expect("the built-in publish_page arm is gone");
        let arm = &arm[..arm.len().min(3_000)];
        // It must BIND THE NAME FROM the rule, not merely mention it: a mutant that called
        // `page_filename` and threw the result away passed a `contains("page_filename(")` check.
        // This is a source assertion and cannot prove behaviour -- a determined refactor defeats
        // it -- but it does catch the realistic regression, someone reintroducing a hardcoded name
        // beside the rule. Saying what a check cannot do is part of the check.
        assert!(
            arm.contains("let name = page_filename("),
            "the publisher does not take its filename FROM the rule it is supposed to obey"
        );
        assert!(
            arm.contains("required_filename"),
            "the publisher never reads the filename the task required"
        );
    }

    #[test]
    fn both_writes_are_told_how_the_generation_ended() {
        let s = steps();
        let writes: Vec<&RecipeStep> = s
            .iter()
            .filter(|st| matches!(st, RecipeStep::Tool { tool_name, .. } if tool_name == "write_files"))
            .collect();
        assert!(writes.len() >= 2, "at least a deliverable write and an improving one");
        // The REVIEW write is the dangerous one: it re-emits the complete set, so a review cut
        // mid-file would otherwise overwrite a complete file with a partial one.
        for (n, w) in writes.iter().enumerate() {
            match w {
                RecipeStep::Tool { args, .. } => {
                    let sr = args.get("stop_reason").and_then(|v| v.as_str()).unwrap_or("");
                    assert!(
                        sr.starts_with("{{") && sr.ends_with("__stop_reason}}"),
                        "write {n} does not receive the generation's stop reason: {args}"
                    );
                    // and it must be THAT step's own reason, not the other one's
                    let stream = args.get("stream").and_then(|v| v.as_str()).unwrap_or("");
                    let var = stream.trim_start_matches("{{").trim_end_matches("}}");
                    assert_eq!(
                        sr,
                        format!("{{{{{var}__stop_reason}}}}"),
                        "write {n} is told about a DIFFERENT step's generation"
                    );
                }
                _ => unreachable!(),
            }
        }
    }

    #[test]
    fn a_cut_generation_drops_the_partial_file_and_a_complete_one_keeps_it() {
        let dir = mind_types::scratch::dir("fset_trunc");
        std::env::set_var("YM_WEB_DIR", dir.path());
        // Two files: the first is terminated by the second's marker and is therefore complete; the
        // second has no terminator, which is exactly the ambiguous case.
        let stream = "=== FILE: a.py
print('complete')
=== FILE: b.py
print('cut off mid";

        // The API said it stopped at the token limit: b.py IS partial. Drop it.
        let (_, written, reported) =
            crate::publish_file_set("trunc-cut", stream, true).expect("write");
        assert_eq!(written, vec!["a.py".to_string()], "the cut file must not be written");
        assert_eq!(reported, vec!["b.py".to_string()], "and the caller must be told WHICH");

        // The API said it finished. The missing newline means nothing on its own — keeping a
        // probably-complete file beats deleting it, which is the rule that once threw away a
        // complete test suite.
        let (_, written2, reported2) =
            crate::publish_file_set("trunc-keep", stream, false).expect("write");
        assert_eq!(
            written2,
            vec!["a.py".to_string(), "b.py".to_string()],
            "without the API's verdict nothing may be dropped"
        );
        assert_eq!(reported2, vec!["b.py".to_string()], "reported as an observation, still written");
    }

    #[test]
    fn the_review_comes_after_a_write_so_a_bad_review_cannot_lose_the_draft() {
        let s = steps();
        let write_ix: Vec<usize> = s
            .iter()
            .enumerate()
            .filter(|(_, st)| matches!(st, RecipeStep::Tool { tool_name, .. } if tool_name == "write_files"))
            .map(|(i, _)| i)
            .collect();
        let think_ix: Vec<usize> = s
            .iter()
            .enumerate()
            .filter(|(_, st)| matches!(st, RecipeStep::Think { .. }))
            .map(|(i, _)| i)
            .collect();
        assert!(write_ix.len() >= 2, "one write to guarantee, at least one to improve");
        assert_eq!(write_ix.len(), think_ix.len(), "every generation is written, or it is lost");
        assert!(
            think_ix[0] < write_ix[0],
            "the first write must follow the authoring generation it writes"
        );
        assert!(
            write_ix[0] < think_ix[1],
            "the review must come AFTER the first write: reviewing first means an unparseable              review loses the whole draft"
        );
        // and EVERY later generation is written after the one before it — no pass may overtake
        // another, which is what keeps "the first write is the guarantee" true as passes are added.
        for i in 1..think_ix.len() {
            assert!(write_ix[i - 1] < think_ix[i] && think_ix[i] < write_ix[i]);
        }
    }

    #[test]
    fn the_second_write_may_only_improve_and_never_destroy() {
        let s = steps();
        let writes: Vec<&RecipeStep> = s
            .iter()
            .filter(|st| matches!(st, RecipeStep::Tool { tool_name, .. } if tool_name == "write_files"))
            .collect();
        // The FIRST write is the guarantee and must fail the chain if it cannot happen.
        match writes[0] {
            RecipeStep::Tool { on_error, .. } => {
                assert!(matches!(on_error, ErrorAction::Fail), "the first write is the deliverable")
            }
            _ => unreachable!(),
        }
        // The SECOND is the improvement: if the review is unparseable, unsafe or empty, the tool
        // errors and the chain must CONTINUE with the first set intact.
        match writes[1] {
            RecipeStep::Tool { on_error, .. } => assert!(
                matches!(on_error, ErrorAction::Skip),
                "a failed review must leave the first set standing, not fail the build"
            ),
            _ => unreachable!(),
        }
    }

    #[test]
    fn at_most_one_review_and_it_costs_at_most_one_extra_call() {
        let s = steps();
        let thinks = s.iter().filter(|st| matches!(st, RecipeStep::Think { .. })).count();
        // RAISED FROM TWO TO THREE, deliberately: author, COMPLETE, review. The ceiling exists to
        // stop an open repair loop, and three fixed calls is still a ceiling — what it is not is a
        // loop that can run until the budget is gone.
        //
        // The third call is not free and was not added for elegance. Reading 7's T1 authoring
        // generation hit the provider deadline clamp, its main file was correctly refused rather
        // than shipped in pieces, and the leg delivered `run.sh` alone: 2/11 against Hermes's
        // 11/11, which was the whole margin of that reading. A cut set with no way to finish is a
        // failed task; one extra call is what a complete set costs.
        //
        // RAISED FROM THREE TO FOUR (E.REPAIR3), on a measurement rather than a hunch. E.REPAIR1
        // and E.REPAIR2 measured the single review round repairing a named defect 45–50% of the
        // time across two independent runs of twenty — so more than half of repairs still shipped
        // broken, and E.VERIFY1 made that visible. The fourth call is CONDITIONAL: a `JumpIf` on
        // FINDINGS_MARKER skips it whenever the first repair cleared everything, so a clean build
        // still spends exactly three. What has not changed is the property this test guards —
        // it is a fixed ceiling, not a loop, and a separate test asserts no step jumps backward.
        assert_eq!(
            thinks, 4,
            "an open repair loop is how a cost ceiling stops existing; the ceiling is four calls, \
             the fourth taken only when findings remain"
        );
    }

    /// KILL CRITERION 1, and the reason it exists rather than being assumed: the execution design
    /// was abandoned because `unshare -rn` is refused inside the benchmark container and generated
    /// code there can reach the run proxy. This asserts the build lane never runs what it wrote.
    #[test]
    fn the_build_lane_executes_nothing_it_generated() {
        const DELEGATE: &str = include_str!("delegate.rs");
        let start = DELEGATE
            .find("pub fn build_recipe")
            .expect("the build recipe exists");
        let end = DELEGATE[start..]
            .find("
}
")
            .map(|e| start + e)
            .unwrap_or(DELEGATE.len());
        let body = &DELEGATE[start..end];
        for forbidden in [
            "Command::new",
            "std::process",
            "spawn_blocking",
            "tokio::process",
        ] {
            assert!(
                !body.contains(forbidden),
                "the build recipe reached for {forbidden:?} — running generated output is a                  separate slice with its own security review, and the sandbox for it was tested                  and refused inside the benchmark container"
            );
        }
        const FILESET: &str = include_str!("fileset.rs");
        for forbidden in ["Command::new", "std::process", "tokio::process"] {
            assert!(
                !FILESET.contains(forbidden),
                "the file-set module reached for {forbidden:?}"
            );
        }
    }
}


/// E.ENTRY1/D2 — the review round must read the FRESHEST write message, not the first one.
///
/// This is the second time in one session a check was wired to a step that could not act on it:
/// `pyimports` and `required_literals` were both reported to a completion pass whose mandate is
/// only "write what is missing", and I stated twice — to Pranab and to a reviewer — that they
/// "reach the review round" before reading the prompt and finding they did not.
///
/// The same defect sat in the dataflow. The completion write stored to `completion_url`, which
/// NOTHING downstream reads; the review interpolates `{{project_url}}`, still holding pass one's
/// message. Reading 7's T1 is what it cost: the review was told "server.py was cut and NOT
/// written" while server.py sat in the deliverable at 2814 bytes, and `RESULT.md` reported that
/// same falsehood to Pranab.
///
/// So this asserts the DATAFLOW, not the presence of a step: whatever variable the review reads
/// must be the one the last write before it stores into. It was watched to FAIL against
/// `completion_url` before the fix, which is the only reason it counts as evidence.
#[test]
fn the_review_round_reads_the_variable_the_completion_write_stores_into() {
    use crate::delegate::build_recipe;
    use mind_recipes::RecipeStep;

    let steps = build_recipe("t", "proj", "build a lead form with a server", None).steps;

    // The review is the Think step whose prompt asks for what the write step OBSERVED.
    let review_prompt = steps
        .iter()
        .find_map(|s| match s {
            RecipeStep::Think { prompt, .. } if prompt.contains("WHAT THE WRITE STEP OBSERVED") => {
                Some(prompt.clone())
            }
            _ => None,
        })
        .expect("build_recipe must have a review step that reads the write step's observations");

    // Every write_files step that runs BEFORE the review must land somewhere the review can see.
    let review_at = steps
        .iter()
        .position(|s| matches!(s, RecipeStep::Think { prompt, .. } if prompt.contains("WHAT THE WRITE STEP OBSERVED")))
        .expect("review step position");

    let writes_before: Vec<String> = steps[..review_at]
        .iter()
        .filter_map(|s| match s {
            RecipeStep::Tool {
                tool_name,
                store_as,
                ..
            } if tool_name == "write_files" => Some(store_as.clone()),
            _ => None,
        })
        .collect();

    assert!(
        writes_before.len() >= 2,
        "expected the authoring write and the completion write before the review, got {writes_before:?}"
    );

    for var in &writes_before {
        assert!(
            review_prompt.contains(&format!("{{{{{var}}}}}")),
            "the review round never reads `{var}`, so anything that write reports — including every \
             mechanical defect found in the files it just wrote — is discarded. Writes before the \
             review: {writes_before:?}"
        );
    }
}

/// The carry-forward that stops the D2 fix being lossy.
///
/// Overwriting `project_url` makes the review truthful; without `prior` it would also make it
/// forget every finding raised on the files pass one wrote. Assert the completion write actually
/// passes the earlier message, because "it compiled" has twice not meant "it carries a value".
#[test]
fn the_completion_write_carries_the_earlier_message_forward() {
    use crate::delegate::build_recipe;
    use mind_recipes::RecipeStep;

    let steps = build_recipe("t", "proj", "build a lead form with a server", None).steps;
    let completion_write = steps
        .iter()
        .filter_map(|s| match s {
            RecipeStep::Tool {
                tool_name, args, ..
            } if tool_name == "write_files" => Some(args.clone()),
            _ => None,
        })
        .find(|a| {
            a.get("stream").and_then(|v| v.as_str()) == Some("{{completion}}")
        })
        .expect("build_recipe must have a completion write");

    assert_eq!(
        completion_write.get("prior").and_then(|v| v.as_str()),
        Some("{{project_url}}"),
        "the completion write must carry pass one's message forward, or overwriting project_url \
         silently drops every finding raised on the files pass one wrote"
    );
}

/// E.VERIFY1 — EVERY write's message must be read by something later. Class, not instance.
///
/// This defect has now appeared twice in one recipe. The completion write stored to
/// `completion_url`, which nothing read; I fixed that this morning and did not look at the step
/// after it, where the REVIEW write stored to `reviewed_url`, which nothing read either. So the
/// mechanical checks ran over the repaired files — imports, entry point, repetition, freshness, and
/// a sandboxed parse — and their verdict was discarded. E.REPAIR1 measured the repair still failing
/// 55% of the time, so in most repairs the mind detected a still-broken artifact and reported
/// "built" regardless.
///
/// Asserting the CLASS is the point. A test pinned to `reviewed_url` would have passed all morning
/// while `completion_url` was broken, and vice versa. A write nobody reads is a check nobody runs.
#[test]
fn every_write_files_message_is_read_by_a_later_step() {
    use crate::delegate::build_recipe;
    use mind_recipes::RecipeStep;

    let steps = build_recipe("t", "proj", "build a lead form with a server", None).steps;

    // Where each write_files step puts its message.
    let writes: Vec<(usize, String)> = steps
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s {
            RecipeStep::Tool {
                tool_name,
                store_as,
                ..
            } if tool_name == "write_files" => Some((i, store_as.clone())),
            _ => None,
        })
        .collect();
    assert!(
        writes.len() >= 3,
        "expected the authoring, completion and review writes, got {writes:?}"
    );

    for (at, var) in &writes {
        let needle = format!("{{{{{var}}}}}");
        let read_later = steps.iter().skip(at + 1).any(|s| match s {
            RecipeStep::Think { prompt, .. } => prompt.contains(&needle),
            RecipeStep::Notify { message } => message.contains(&needle),
            RecipeStep::Tool { args, .. } => serde_json::to_string(args)
                .map(|t| t.contains(&needle))
                .unwrap_or(false),
            _ => false,
        });
        assert!(
            read_later,
            "the write at step {at} stores its message in `{var}`, which NO later step reads. \
             Every mechanical defect that write found — including in files a repair just rewrote — \
             is discarded, and the owner is told the build succeeded. Writes: {writes:?}"
        );
    }
}

/// The review write must carry the earlier message forward, or making the report truthful makes it
/// lossy — the same trap D2 had.
#[test]
fn the_review_write_carries_the_earlier_message_forward() {
    use crate::delegate::build_recipe;
    use mind_recipes::RecipeStep;

    let steps = build_recipe("t", "proj", "build a lead form with a server", None).steps;
    let review_write = steps
        .iter()
        .filter_map(|s| match s {
            RecipeStep::Tool {
                tool_name, args, ..
            } if tool_name == "write_files" => Some(args.clone()),
            _ => None,
        })
        .find(|a| a.get("stream").and_then(|v| v.as_str()) == Some("{{reviewed}}"))
        .expect("build_recipe must have a review write");

    assert_eq!(
        review_write.get("prior").and_then(|v| v.as_str()),
        Some("{{project_url}}"),
        "the review write must carry the earlier message forward, or a truthful final report \
         silently drops every finding raised before the review"
    );
}

/// E.REPAIR3 — the second repair round exists, is CONDITIONAL, and is BOUNDED.
///
/// Three properties, each asserted on the recipe's structure rather than assumed:
/// 1. A clean build pays nothing: the JumpIf tests `project_url` for the findings marker and, when
///    absent, jumps straight to the Notify — the same steps as before this slice.
/// 2. The marker comes from ONE constant, shared with the write step's header. Two copies of a
///    string drift, and a drifted copy here would silently disable the round forever.
/// 3. Exactly one extra round, and no step may jump BACKWARD — a backward jump would let a defect
///    the model cannot fix burn model calls without limit.
#[test]
fn the_second_repair_round_is_conditional_bounded_and_shares_its_marker() {
    use crate::delegate::build_recipe;
    use mind_recipes::{Condition, RecipeStep};

    let steps = build_recipe("t", "proj", "build a lead form with a server", None).steps;
    let notify_at = steps
        .iter()
        .rposition(|s| matches!(s, RecipeStep::Notify { .. }))
        .expect("build_recipe ends in a Notify");

    // Find the JumpIf that guards the second round: it tests project_url for the marker.
    let guards: Vec<(usize, usize)> = steps
        .iter()
        .enumerate()
        .filter_map(|(i, s)| match s {
            RecipeStep::JumpIf {
                condition:
                    Condition::Not { inner },
                target_step,
            } => match inner.as_ref() {
                Condition::VarContains { var, substring }
                    if var == "project_url" && substring == crate::FINDINGS_MARKER =>
                {
                    Some((i, *target_step))
                }
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        guards.len(),
        1,
        "exactly one JumpIf must guard the second round on FINDINGS_MARKER; found {guards:?}"
    );
    let (guard_at, target) = guards[0];

    // Property 1 + 5: absent findings, the jump lands ON the Notify — not before it (which would
    // double-run a step) and not past it (which would skip the report).
    assert_eq!(
        target, notify_at,
        "the guard must jump to the Notify at index {notify_at}, not {target}"
    );

    // Property 3: exactly one Think and one write_files between the guard and the Notify.
    let between = &steps[guard_at + 1..notify_at];
    let thinks = between.iter().filter(|s| matches!(s, RecipeStep::Think { .. })).count();
    let writes = between
        .iter()
        .filter(|s| matches!(s, RecipeStep::Tool { tool_name, .. } if tool_name == "write_files"))
        .count();
    assert_eq!((thinks, writes), (1, 1), "the second round is exactly one review and one write");

    // Property 3, the other half: no JumpIf anywhere in the recipe targets its own index or earlier.
    for (i, s) in steps.iter().enumerate() {
        if let RecipeStep::JumpIf { target_step, .. } = s {
            assert!(
                *target_step > i,
                "step {i} jumps backward/self to {target_step}; a repair loop must be bounded"
            );
        }
    }
}

/// The write step's header and the recipe's guard must be the SAME string — asserted on the
/// source, mirroring the TRUNCATION_MARKER test, because a literal copy in either place is how the
/// two sides drift and the round goes dark.
#[test]
fn the_findings_marker_is_not_duplicated_as_a_literal() {
    let lib = include_str!("lib.rs");
    let del = include_str!("delegate.rs");
    let header_uses_constant = lib.contains("{FINDINGS_MARKER} IN WHAT YOU JUST WROTE");
    assert!(header_uses_constant, "the findings header must interpolate FINDINGS_MARKER");
    // The literal may appear exactly once in lib.rs: the constant's own definition.
    assert_eq!(
        lib.matches("\"DEFECTS FOUND MECHANICALLY\"").count(),
        1,
        "the literal must exist only as the constant's definition in lib.rs"
    );
    assert_eq!(
        del.matches("DEFECTS FOUND MECHANICALLY").count(),
        0,
        "delegate.rs must reference crate::FINDINGS_MARKER, never the literal"
    );
}
