//! E.REPEAT1 — pinned to the real degenerate artifact and to the 228 healthy ones beside it.

use crate::repetition::degenerate_repetition;

const DEGENERATE: &str =
    include_str!("../fixtures/entrypoint/dspilot3_mind_T1_server_degenerate.py");
const R7_MIND_SERVER: &str = include_str!("../fixtures/entrypoint/r7_mind_T1_server.py");
const R7_HERMES_SERVER: &str = include_str!("../fixtures/entrypoint/r7_hermes_T1_server.py");
const P8_MIND_SERVER: &str = include_str!("../fixtures/entrypoint/p8_mind_T1_server.py");

fn stream(files: &[(&str, &str)]) -> String {
    files
        .iter()
        .map(|(p, c)| format!("=== FILE: {p}\n{c}\n"))
        .collect()
}

/// The artifact this exists for.
///
/// 428 lines, ending with `if __name__ == "__main__":` forty-nine times. It does not parse, and the
/// write step reported `data/leads.json` as the file that might be incomplete.
#[test]
fn fires_on_the_real_degenerate_artifact() {
    let found = degenerate_repetition(&stream(&[("server.py", DEGENERATE)]));
    assert_eq!(found.len(), 1, "expected exactly one finding, got {found:?}");
    assert!(
        found[0].contains("server.py") && found[0].contains("49 times"),
        "the finding must name the file and the count: {}",
        found[0]
    );
    assert!(
        found[0].contains("__main__"),
        "and show the repeated line, which is what makes it actionable: {}",
        found[0]
    );
}

/// Silent on every healthy real artifact already in these fixtures.
#[test]
fn silent_on_healthy_real_artifacts() {
    for (name, src) in [
        ("server.py", R7_MIND_SERVER),
        ("server.py", R7_HERMES_SERVER),
        ("server.py", P8_MIND_SERVER),
    ] {
        assert_eq!(
            degenerate_repetition(&stream(&[(name, src)])),
            Vec::<String>::new(),
            "{name} is healthy and must produce no finding"
        );
    }
}

/// Blank lines break a run, or any file with a few empty lines together would be accused.
#[test]
fn blank_lines_and_ordinary_repetition_are_not_degeneracy() {
    let blanks = "import os\n\n\n\n\n\n\n\nx = 1\n";
    assert!(degenerate_repetition(&stream(&[("a.py", blanks)])).is_empty());
    // Four identical lines is under the threshold and stays silent: the corpus says a healthy file
    // never has even two, so four is already generous.
    let four = "x = 1\nx = 1\nx = 1\nx = 1\ny = 2\n";
    assert!(degenerate_repetition(&stream(&[("a.py", four)])).is_empty());
    // Five is the threshold.
    let five = "x = 1\nx = 1\nx = 1\nx = 1\nx = 1\ny = 2\n";
    assert_eq!(degenerate_repetition(&stream(&[("a.py", five)])).len(), 1);
    // Lines that differ only by indentation are not identical, which is what keeps ordinary
    // block structure from ever counting.
    let indented = "if a:\n    pass\nif a:\n    pass\nif a:\n    pass\n";
    assert!(degenerate_repetition(&stream(&[("a.py", indented)])).is_empty());
}

/// Kill criterion 1 and 2, over the real corpus: exactly one file, and it is the known one.
#[test]
fn fires_on_exactly_one_file_across_the_benchmark_corpus() {
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
            let Ok(c) = std::fs::read_to_string(&p) else {
                continue;
            };
            seen += 1;
            if !degenerate_repetition(&stream(&[("f.py", &c)])).is_empty() {
                fired.push(p.display().to_string());
            }
        }
    }
    assert!(seen > 100, "corpus looked too small: {seen} files");
    assert_eq!(
        fired.len(),
        1,
        "exactly one real artifact is degenerate; found {}: {fired:?}",
        fired.len()
    );
    assert!(
        fired[0].replace('\\', "/").contains("ds-pilot3"),
        "the one fire must be ds-pilot3's server.py, got {}",
        fired[0]
    );
}
