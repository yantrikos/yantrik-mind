//! E.WIN3 — the route that serves live numbers must actually read them.
//!
//! `out-p8/mind_T1/server.py` scored **8/11**, losing exactly the three dashboard checks. Its
//! handler is `elif self.path == '/dashboard': self.serve_dashboard()`, and `serve_dashboard`
//! computes `total = len(leads)` from a **module-level global loaded once at startup**. It never
//! re-reads `data/leads.json`. Submissions land; the dashboard keeps reporting the numbers it had
//! at boot.
//!
//! Every other check in this crate is silent on it, and each is right to be: the file parses, its
//! imports resolve, it has an entry point and binds a port, and it repeats nothing. It is a
//! completely healthy-looking program that serves stale data — which is exactly why the model
//! review round read it and approved it.
//!
//! **THE RISK HERE IS INVERTED, and it decides the whole design.** Every other check fires on the
//! PRESENCE of something wrong, so an uncertain scanner can safely fall silent. This one fires on
//! the **ABSENCE of a read**, so a scanner that fails to *find* a read that is really there
//! produces a FALSE ACCUSATION on a healthy file. Two rules follow and are load-bearing:
//!
//! 1. Anything this cannot read confidently — an unterminated string, no locatable route guard —
//!    yields silence rather than a finding.
//! 2. The read vocabulary is at least as generous as the `ast` probe's and never narrower. Adding
//!    a name to `READ_FUNCS` can only ever silence this check, which is the safe direction; leaving
//!    one out is how it accuses a file that reads perfectly well.
//!
//! **Validated against the `ast` probe on real artifacts before this file existed:** 18 artifacts
//! across the benchmark corpus, 16 judged, **15 `ok` and exactly one `SNAPSHOT-SUSPECT` —
//! `out-p8`** — with 2 unparseable abstained. One fire, on the one leg that lost those points.

use crate::fileset::parse_file_stream;

/// The route whose freshness the graded checks care about.
const ROUTE: &str = "/dashboard";

/// Call names that read persistent data.
///
/// Deliberately generous: every addition here can only SILENCE this check, and silence is the safe
/// direction when the finding is triggered by absence.
const READ_FUNCS: &[&str] = &[
    "open",
    "load",
    "loads",
    "read",
    "read_text",
    "read_bytes",
    "readlines",
    "readline",
];

/// A python file split into `(name, body_lines)` for every `def`, by indentation.
fn functions(src: &str) -> Vec<(String, Vec<String>)> {
    let mut out: Vec<(String, Vec<String>)> = Vec::new();
    let mut cur: Option<(String, usize, Vec<String>)> = None;
    for raw in strip_triple_quoted(src) {
        let line = raw.as_str();
        let indent = line.len() - line.trim_start().len();
        let t = line.trim_start();
        let def = t
            .strip_prefix("async def ")
            .or_else(|| t.strip_prefix("def "));
        if let Some(rest) = def {
            if let Some((name, _)) = rest.split_once('(') {
                if let Some((n, i, b)) = cur.take() {
                    out.push((n, b));
                    let _ = (i, &out);
                }
                cur = Some((name.trim().to_string(), indent, Vec::new()));
                continue;
            }
        }
        if let Some((_, i, b)) = cur.as_mut() {
            if !t.is_empty() && indent <= *i {
                let (n, _, b2) = cur.take().expect("checked");
                out.push((n, b2));
            } else {
                b.push(line.to_string());
            }
        }
    }
    if let Some((n, _, b)) = cur.take() {
        out.push((n, b));
    }
    out
}

/// Lines with triple-quoted blocks removed, so a docstring mentioning a route is never a guard.
///
/// Returns `None` for the whole file when a triple quote never closes — the caller treats that as
/// "cannot read", which is silence.
fn strip_triple_quoted(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut open: Option<&str> = None;
    for line in src.lines() {
        if let Some(q) = open {
            if line.contains(q) {
                open = None;
            }
            continue;
        }
        let t = line.trim_start();
        let opener = ["\"\"\"", "'''"].into_iter().find(|q| t.starts_with(q));
        if let Some(q) = opener {
            if t.len() < 6 || !t[3..].contains(q) {
                open = Some(q);
                continue;
            }
        }
        out.push(line.to_string());
    }
    out
}

fn has_unterminated_triple(src: &str) -> bool {
    let mut open: Option<&str> = None;
    for line in src.lines() {
        if let Some(q) = open {
            if line.contains(q) {
                open = None;
            }
            continue;
        }
        let t = line.trim_start();
        if let Some(q) = ["\"\"\"", "'''"].into_iter().find(|q| t.starts_with(q)) {
            if t.len() < 6 || !t[3..].contains(q) {
                open = Some(q);
            }
        }
    }
    open.is_some()
}

/// Every call name on a line: `self.serve_dashboard()` -> `serve_dashboard`, `json.load(f)` ->
/// `load`. The LAST attribute segment is what the `ast` probe compares, so this matches it.
fn call_names(line: &str) -> Vec<String> {
    let b = line.as_bytes();
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < b.len() {
        if b[i] == b'(' {
            // Walk back over an identifier, then over any dotted prefix.
            let mut end = i;
            while end > 0 && (b[end - 1] as char).is_whitespace() {
                end -= 1;
            }
            let mut start = end;
            while start > 0 {
                let c = b[start - 1] as char;
                if c.is_ascii_alphanumeric() || c == '_' {
                    start -= 1;
                } else {
                    break;
                }
            }
            if start < end {
                out.push(line[start..end].to_string());
            }
        }
        i += 1;
    }
    out
}

/// Does a read reach this body, directly or through a local function it calls?
fn reaches_read(body: &[String], fns: &[(String, Vec<String>)], seen: &mut Vec<String>) -> bool {
    for line in body {
        for name in call_names(line) {
            if READ_FUNCS.contains(&name.as_str()) {
                return true;
            }
            if seen.iter().any(|s| *s == name) {
                continue;
            }
            if let Some((_, sub)) = fns.iter().find(|(n, _)| *n == name) {
                seen.push(name.clone());
                if reaches_read(sub, fns, seen) {
                    return true;
                }
            }
        }
    }
    false
}

/// Is this line a route guard for `ROUTE`?
fn guards_route(line: &str) -> bool {
    let t = line.trim_start();
    if !(t.starts_with("if ") || t.starts_with("elif ")) || !t.trim_end().ends_with(':') {
        return false;
    }
    for q in ['"', '\''] {
        let mut parts = t.split(q);
        parts.next();
        for (i, seg) in parts.enumerate() {
            if i % 2 == 0 && seg.trim_end_matches('/') == ROUTE.trim_end_matches('/') {
                return true;
            }
        }
    }
    false
}

/// Keywords that open a block: the colon must be followed by an indented body.
const BLOCK_KEYWORDS: &[&str] = &[
    "if ", "elif ", "else", "for ", "while ", "def ", "class ", "try", "except", "finally", "with ",
    "async def ", "async for ", "async with ",
];

/// A compound-statement header with NO indented body after it — the file cannot compile.
///
/// Added because the corpus test caught this scanner ACCUSING `out-ds-pilot3/mind_T1/server.py`,
/// which python rejects with *"expected an indented block after class definition"* (its generation
/// degenerated into `if __name__ == "__main__":` forty-nine times). The `ast` probe abstained there
/// and this must too: a file that cannot run is not a file that serves stale data, and saying so
/// would be a false accusation of exactly the kind the module header forbids. Found by kill
/// criterion 2 — file-for-file agreement, not "no worse than".
fn has_block_without_body(lines: &[String]) -> bool {
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_end();
        let head = t.trim_start();
        if !t.ends_with(':') || !BLOCK_KEYWORDS.iter().any(|k| head.starts_with(k)) {
            continue;
        }
        let indent = line.len() - head.len();
        // The next line with content must be deeper. Anything else is a missing body.
        match lines.iter().skip(i + 1).find(|l| !l.trim().is_empty()) {
            Some(next) => {
                if next.len() - next.trim_start().len() <= indent {
                    return true;
                }
            }
            None => return true, // header is the last line in the file
        }
    }
    false
}

/// `Some(true)` = the route is served WITHOUT reading; `Some(false)` = it reads; `None` = cannot
/// tell, which every caller treats as silence.
fn serves_a_snapshot(src: &str) -> Option<bool> {
    if has_unterminated_triple(src) {
        return None;
    }
    if has_block_without_body(&strip_triple_quoted(src)) {
        return None;
    }
    let lines = strip_triple_quoted(src);
    let fns = functions(src);

    let mut bodies: Vec<Vec<String>> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if !guards_route(line) {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let mut body = Vec::new();
        for next in lines.iter().skip(i + 1) {
            let t = next.trim_start();
            if t.is_empty() {
                continue;
            }
            if next.len() - t.len() <= indent {
                break;
            }
            body.push(next.clone());
        }
        if !body.is_empty() {
            bodies.push(body);
        }
    }

    if bodies.is_empty() {
        // No inline guard. Fall back to a function whose NAME carries the route, exactly as the
        // probe does — and if there is none, say nothing at all.
        let mut named: Vec<&(String, Vec<String>)> = fns
            .iter()
            .filter(|(n, _)| n.to_ascii_lowercase().contains("dashboard"))
            .collect();
        named.sort_by_key(|(n, _)| n.len());
        let target = named.first()?;
        return Some(!reaches_read(&target.1, &fns, &mut vec![target.0.clone()]));
    }

    let mut fresh = false;
    for body in &bodies {
        if reaches_read(body, &fns, &mut Vec::new()) {
            fresh = true;
        }
    }
    Some(!fresh)
}

/// Findings for routes that render live figures from data they never re-read.
pub(crate) fn stale_route_findings(stream: &str) -> Vec<String> {
    let mut out = Vec::new();
    for e in &parse_file_stream(stream).entries {
        if !e.path.ends_with(".py") {
            continue;
        }
        if serves_a_snapshot(&e.content) == Some(true) {
            out.push(format!(
                "`{}` serves `{ROUTE}` without ever reading the data file on the request. Nothing \
                 on that path calls a read, so the figures come from whatever was loaded when the \
                 process started — every submission after boot is invisible there, and the totals, \
                 the per-day bins and the recent list will all be wrong. Read the data inside the \
                 handler instead of using a value captured at import.",
                e.path
            ));
        }
    }
    out
}
