//! E.REPEAT1 — a generation that degenerated into repetition must not be reported as built.
//!
//! `out-ds-pilot3/mind_T1/server.py` ends with `if __name__ == "__main__":` repeated **49 times**
//! and does not parse. The write step reported *"(3 files: run.sh, server.py, data/leads.json) —
//! NOTE: the stream ended without a newline, so data/leads.json may be incomplete"*: it named the
//! LAST file as possibly incomplete and said nothing whatsoever about `server.py`, the file that
//! was destroyed.
//!
//! Nothing else covers it, and each silence is correct on its own terms. `pyimports` resolves the
//! imports it can see and they are fine. `entrypoint` abstains, because the file will not parse and
//! its uncertainty rule sends every unreadable file to silence. The duplicate-path check has
//! nothing to say about one file. So the deliverable was written, reported as built, and cannot
//! run — the same shape as E.ENTRY1, reached by a different road.
//!
//! **The threshold is measured rather than chosen.** Across all 229 artifacts of the benchmark
//! corpus the longest run of consecutive identical non-blank lines is **1 for 228 files** and 49
//! for the one degenerate file. There is nothing in between: no healthy artifact has even two
//! consecutive identical non-blank lines, and the per-extension maximum is 1 for `.py`, `.json`,
//! `.html`, `.js`, `.md`, `.sh` and `.css` alike. `MIN_RUN` therefore sits four above anything ever
//! observed in a good file and at a tenth of the bad one, which is as much headroom in both
//! directions as a rule of this kind is ever likely to get.

use crate::fileset::parse_file_stream;

/// Consecutive identical non-blank lines that mean the generator looped rather than wrote.
///
/// See the module header for the measurement. Raising this costs nothing until a file has five
/// identical lines in a row; lowering it toward 2 would start judging files the corpus says are
/// healthy.
const MIN_RUN: usize = 5;

/// The longest run of consecutive identical non-blank lines, and what that line was.
fn longest_run(content: &str) -> (usize, String) {
    let lines: Vec<&str> = content.split('\n').collect();
    let mut best = (0usize, String::new());
    let mut run = 0usize;
    for i in 0..lines.len() {
        // A blank line breaks a run: whitespace between blocks is ordinary formatting, and
        // counting it would flag any file with a few empty lines together.
        if lines[i].trim().is_empty() {
            run = 0;
            continue;
        }
        if i > 0 && lines[i] == lines[i - 1] {
            run += 1;
            if run + 1 > best.0 {
                best = (run + 1, lines[i].trim().to_string());
            }
        } else {
            run = 0;
        }
    }
    best
}

/// Findings for files whose content repeats a line far past anything a real deliverable does.
///
/// Empty for all 228 healthy artifacts in the corpus, which keeps the write step's message
/// byte-identical on a good build.
pub(crate) fn degenerate_repetition(stream: &str) -> Vec<String> {
    let mut out = Vec::new();
    for e in &parse_file_stream(stream).entries {
        let (n, line) = longest_run(&e.content);
        if n >= MIN_RUN {
            // Show the repeated line, trimmed: it is the single most useful thing for whoever has
            // to rewrite the file, and it is what makes the finding actionable rather than a
            // complaint about statistics.
            let shown: String = line.chars().take(60).collect();
            out.push(format!(
                "`{}` repeats the same line {n} times in a row (`{shown}`). That is a generation \
                 that looped rather than finished — the file is almost certainly broken and needs \
                 to be written again from the brief, not patched.",
                e.path
            ));
        }
    }
    out
}
