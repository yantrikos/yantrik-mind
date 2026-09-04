//! E.WIN2 — a placeholder the code substitutes that no template actually contains.
//!
//! Measured, and the measurement killed my first idea. Re-checking 32 real T1 artifacts found one
//! leg at **7/11** whose four lost checks all hang off a missing `<script id="cb2-dashboard">`. My
//! first hypothesis was that the artifact never mentioned the element; checking the real files
//! showed it mentions it exactly as the 11/11 legs do. The failure is one step further in:
//!
//! ```text
//! p1 (11/11)  template: <script id="cb2-dashboard" ...>{{DASHBOARD_JSON}}</script>
//!             code:     page.replace('{{DASHBOARD_JSON}}', json)          -> matches, rendered
//!
//! p3 ( 7/11)  template: <!-- DYNAMIC_SCRIPT -->
//!             code:     render_template('dashboard.html', {'DYNAMIC_SCRIPT': tag})
//!                       ...where render_template looks for {{KEY}}        -> NO match, dropped
//! ```
//!
//! The model wrote the substitution key one way and the template another, and nothing between them
//! noticed. That is a real bug class in any templating code — it has nothing to do with this
//! benchmark, and a check that special-cased `cb2-dashboard` would raise a score without improving
//! the mind.
//!
//! **Two guards against crying wolf**, because a false accusation is worse than a miss: the
//! identifier must look like a placeholder (`UPPER_SNAKE`), and it must appear in **at least two
//! different files** — a constant used twice inside one file is just a constant.

/// The marker that opens each file in a build stream, mirrored from `fileset`.
const FILE_MARKER: &str = "=== FILE:";

fn is_placeholder_name(tok: &str) -> bool {
    // The underscore is load-bearing, not cosmetic. Without it `POST` qualifies — an HTTP verb that
    // appears quoted in the server and again in the form's method, in two different files, on a leg
    // that scored 11/11. Real artifacts caught that before it shipped; a checker that accuses a
    // passing build of a defect is worse than one that stays quiet.
    tok.contains('_')
        && tok.len() >= 4
        && tok.len() <= 48
        && tok.starts_with(|c: char| c.is_ascii_uppercase())
        && tok
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
        && tok.chars().any(|c| c.is_ascii_alphabetic())
}

/// Quoted `UPPER_SNAKE` tokens — the shape a substitution key takes when passed as a dict key or
/// a replace() argument. A token already written as `{{NAME}}` is not collected: the braces mean
/// the author is using the template form, which is the thing that works.
fn quoted_placeholder_keys(src: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for quote in ['"', '\''] {
        let mut rest = src;
        while let Some(a) = rest.find(quote) {
            let after = &rest[a + 1..];
            let Some(b) = after.find(quote) else { break };
            let tok = &after[..b];
            if is_placeholder_name(tok) && !out.iter().any(|e| e == tok) {
                out.push(tok.to_string());
            }
            rest = &after[b + 1..];
        }
    }
    out
}

/// Substitution keys the build uses that no file renders as `{{KEY}}`.
///
/// Empty for a build whose code and templates agree, which keeps the write tool's message
/// byte-identical in the ordinary case.
pub(crate) fn placeholder_mismatches(stream: &str) -> Vec<String> {
    let files: Vec<&str> = stream.split(FILE_MARKER).skip(1).collect();
    if files.len() < 2 {
        return Vec::new(); // a single file cannot have a cross-file mismatch
    }
    let mut out = Vec::new();
    for key in quoted_placeholder_keys(stream) {
        // The template form exists somewhere: the author is doing it correctly.
        if stream.contains(&format!("{{{{{key}}}}}")) {
            continue;
        }
        // It must read as a placeholder shared between files, not a constant used inside one.
        let carrying = files.iter().filter(|f| f.contains(key.as_str())).count();
        if carrying >= 2 {
            out.push(format!(
                "`{key}` is substituted as a placeholder but no file contains `{{{{{key}}}}}` — the \
                 replacement will not match, and whatever it was meant to insert is silently dropped"
            ));
        }
    }
    out
}
