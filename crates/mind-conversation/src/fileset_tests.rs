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
            plan_file_set(&[e("a.txt", "x"), e("a.txt", "y")]).unwrap_err(),
            "a.txt",
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
    assert!(matches!(
        plan_file_set(&[e("a.txt", "1"), e("b.txt", "2"), e("a.txt", "3")]),
        Err(FileSetRefusal::DuplicatePath { .. })
    ));
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

