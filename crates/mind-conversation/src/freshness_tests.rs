//! E.WIN3 — pinned to the artifact that lost the points, and to the healthy legs beside it.

use crate::freshness::stale_route_findings;

const P8_SNAPSHOT: &str = include_str!("../fixtures/entrypoint/p8_mind_T1_server.py");
const P1_FRESH: &str = include_str!("../fixtures/entrypoint/p1_mind_T1_server.py");
const R7_MIND: &str = include_str!("../fixtures/entrypoint/r7_mind_T1_server.py");
const R7_HERMES: &str = include_str!("../fixtures/entrypoint/r7_hermes_T1_server.py");

fn stream(files: &[(&str, &str)]) -> String {
    files
        .iter()
        .map(|(p, c)| format!("=== FILE: {p}\n{c}\n"))
        .collect()
}

/// The 8/11 leg: `elif self.path == '/dashboard': self.serve_dashboard()`, and that method reads a
/// module-level global captured at import.
#[test]
fn fires_on_the_real_p8_artifact_that_served_a_snapshot() {
    let found = stale_route_findings(&stream(&[("server.py", P8_SNAPSHOT)]));
    assert_eq!(found.len(), 1, "expected exactly one finding, got {found:?}");
    assert!(
        found[0].contains("/dashboard") && found[0].contains("loaded when the"),
        "the finding must name the route and the cause: {}",
        found[0]
    );
}

/// An 11/11 leg that serves the same route correctly. A check that fires on both is worthless.
#[test]
fn silent_on_the_real_eleven_of_eleven_leg() {
    assert_eq!(
        stale_route_findings(&stream(&[("server.py", P1_FRESH)])),
        Vec::<String>::new(),
        "p1 reads on request and must produce no finding"
    );
}

/// Other real artifacts, including the opponent's. None of these lost dashboard points.
#[test]
fn silent_on_other_real_artifacts() {
    for (n, s) in [("server.py", R7_MIND), ("server.py", R7_HERMES)] {
        assert_eq!(
            stale_route_findings(&stream(&[(n, s)])),
            Vec::<String>::new(),
            "no finding expected for this artifact"
        );
    }
}

/// THE INVERTED RISK. The finding fires on ABSENCE of a read, so anything unreadable, and any file
/// with no locatable route, must be SILENT rather than accused.
#[test]
fn silence_wherever_it_cannot_read_confidently() {
    // Unterminated triple quote.
    let torn = "import os\n\"\"\"never closes\nif self.path == '/dashboard':\n    pass\n";
    assert!(stale_route_findings(&stream(&[("a.py", torn)])).is_empty());
    // No dashboard route at all.
    let none = "import os\n\n\ndef go():\n    return 1\n";
    assert!(stale_route_findings(&stream(&[("a.py", none)])).is_empty());
    // The route named only inside a docstring is not a guard.
    let doc = "\"\"\"serves /dashboard eventually\"\"\"\nimport os\n\n\ndef go():\n    return 1\n";
    assert!(stale_route_findings(&stream(&[("a.py", doc)])).is_empty());
    // Not a python file.
    let html = "if self.path == '/dashboard':\n    pass\n";
    assert!(stale_route_findings(&stream(&[("index.html", html)])).is_empty());
}

/// A read reached through a helper counts, and so does one inline.
#[test]
fn a_read_anywhere_on_the_route_silences_it() {
    let inline = "class H:\n    def do_GET(self):\n        if self.path == '/dashboard':\n            with open('data/leads.json') as f:\n                pass\n";
    assert!(stale_route_findings(&stream(&[("a.py", inline)])).is_empty());
    let helper = "import json\n\n\ndef load_leads():\n    with open('data/leads.json') as f:\n        return json.load(f)\n\n\nclass H:\n    def do_GET(self):\n        if self.path == '/dashboard':\n            rows = load_leads()\n";
    assert!(stale_route_findings(&stream(&[("a.py", helper)])).is_empty());
    // And the snapshot shape DOES fire: the handler uses a global, nothing reads.
    let snap = "import json\n\nLEADS = []\n\n\nclass H:\n    def render(self):\n        return len(LEADS)\n\n    def do_GET(self):\n        if self.path == '/dashboard':\n            self.render()\n";
    assert_eq!(stale_route_findings(&stream(&[("a.py", snap)])).len(), 1);
}

/// Kill criteria 1-3: exact agreement with the `ast` probe over the corpus.
#[test]
fn agrees_with_the_ast_probe_across_the_corpus() {
    let Ok(root) = std::env::var("YM_ENTRY_CORPUS") else {
        return;
    };
    let mut fired: Vec<String> = Vec::new();
    let mut seen = 0usize;
    let mut walk = vec![std::path::PathBuf::from(&root)];
    while let Some(dir) = walk.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk.push(p);
                continue;
            }
            if p.extension().and_then(|x| x.to_str()) != Some("py") {
                continue;
            }
            let Ok(c) = std::fs::read_to_string(&p) else {
                continue;
            };
            seen += 1;
            if !stale_route_findings(&stream(&[("server.py", &c)])).is_empty() {
                fired.push(p.display().to_string());
            }
        }
    }
    assert!(seen > 10, "corpus looked too small: {seen} python files");
    assert_eq!(
        fired.len(),
        1,
        "the ast probe found exactly one snapshot server; this found {}: {fired:?}",
        fired.len()
    );
    assert!(
        fired[0].replace('\\', "/").contains("out-p8"),
        "the one fire must be out-p8, got {}",
        fired[0]
    );
}
