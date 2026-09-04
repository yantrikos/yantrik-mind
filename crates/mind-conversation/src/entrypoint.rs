//! E.ENTRY1 — the entry point a deliverable NAMES must actually do something when it is run.
//!
//! Reading 7's T1 died on `ERR_CONNECTION_REFUSED`, and the cause was none of the things the loop
//! already checks. `server.py` was present. It parsed cleanly — so did all ten python artifacts in
//! that run, which is why a syntax check was probed and killed rather than built. Its imports all
//! resolved. What it did not have was a single executable statement at module level: no
//! `serve_forever`, no `__main__` guard, nothing but imports, defs and constants. `run.sh` ran
//! `python3 server.py`, python defined a handler class, and the process exited. Nothing ever bound
//! a port.
//!
//! The build recipe's review round already asks the model a question of this shape — *"Does
//! anything reference a file that is not in the set?"* — and asking a model to check its own work
//! is the weakest instrument available. This answers a sharper version of it exactly.
//!
//! **Scope is the whole safety argument.** A test module is supposed to be nothing but defs. So is
//! a library. Judging one would be a false accusation, and this check therefore judges a file ONLY
//! when an entry script in the same set actually invokes it — and reads that mapping out of the
//! script's text rather than inferring it from a filename.
//!
//! **Every uncertainty resolves toward silence.** A line this cannot confidently classify counts as
//! executable, which yields no finding. That is the same direction `pyimports` chose and for the
//! same measured reason: a checker that cries wolf gets turned off, so the failure mode to prefer
//! is missing a defect, never inventing one. Concretely, that is why the constant test below
//! accepts only literals it is sure of: `app = Flask(__name__)` is a call and must read as
//! behaviour, and anything unrecognised reads as behaviour too.
//!
//! **Validated on real artifacts before this file existed.** An `ast`-based probe of the identical
//! rule ran over every artifact directory of ~24 benchmark runs on the staging box: it fired
//! exactly once, on `out-r7/mind_T1/server.py` — the leg that failed — and produced zero false
//! positives across sixteen judged files, including `hermes_T1` on four separate runs and the whole
//! p1–p8 corpus, which contains both 11/11 and 2/11 legs. The tests pin this port against that
//! probe's verdicts rather than against cases invented here, because eighteen invented cases once
//! passed while the first real artifact failed.

use crate::fileset::parse_file_stream;

/// Literals a module-level name may be bound to without that being *behaviour*.
///
/// Deliberately short. Anything not recognised here is treated as executable, which produces no
/// finding — the safe direction.
fn is_plain_literal(rhs: &str) -> bool {
    let r = rhs.trim();
    if r.is_empty() {
        return false;
    }
    if matches!(r, "True" | "False" | "None" | "[]" | "{}" | "()") {
        return true;
    }
    // A quoted string, and not one built by a call.
    let quoted = (r.starts_with('"') && r.ends_with('"'))
        || (r.starts_with('\'') && r.ends_with('\''));
    if quoted && r.len() >= 2 {
        return !r.contains('(');
    }
    // A number, possibly negative.
    let n = r.strip_prefix('-').unwrap_or(r);
    !n.is_empty() && n.chars().all(|c| c.is_ascii_digit() || c == '.' || c == '_')
}

/// Is this top-level line a DEFINITION rather than something that runs?
fn is_definition(line: &str) -> bool {
    for kw in ["import ", "from ", "def ", "class ", "async def ", "@"] {
        if line.starts_with(kw) {
            return true;
        }
    }
    // `NAME = <literal>` — a module constant. `==` is a comparison, not a binding.
    if let Some((lhs, rhs)) = line.split_once('=') {
        if !lhs.ends_with(['=', '!', '<', '>']) && !rhs.starts_with('=') {
            let name = lhs.trim();
            let is_name = !name.is_empty()
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ',' || c == ' ');
            if is_name && is_plain_literal(rhs) {
                return true;
            }
        }
    }
    false
}

/// Does running this source as a script perform NO action?
///
/// `None` means "cannot tell" — an unterminated string, unbalanced brackets — and every caller
/// treats that as "it runs", so an unreadable file is never accused.
fn runs_nothing(src: &str) -> Option<bool> {
    let mut triple: Option<&str> = None;
    let mut depth: i32 = 0;
    let mut continued = false;
    let mut saw_executable = false;

    for raw in src.lines() {
        let line = raw.trim_end();
        // Inside a triple-quoted block nothing on the line is code except its terminator.
        if let Some(q) = triple {
            if line.contains(q) {
                triple = None;
            }
            continue;
        }
        let trimmed = line.trim_start();
        let top_level = depth == 0 && !continued && !line.starts_with([' ', '\t']);

        if top_level && !trimmed.is_empty() && !trimmed.starts_with('#') {
            // A module docstring opens a triple quote and is not behaviour.
            let opens = ["\"\"\"", "'''"].into_iter().find(|q| trimmed.starts_with(q));
            if let Some(q) = opens {
                // The same line may close it again.
                if trimmed.len() < 6 || !trimmed[3..].contains(q) {
                    triple = Some(q);
                }
            } else if !is_definition(trimmed) {
                saw_executable = true;
            }
        }

        // Track bracket depth and continuations so a wrapped expression is not read as a new
        // statement. Quotes inside a line are not tracked; a line whose brackets do not balance for
        // that reason simply keeps depth non-zero, which SUPPRESSES findings rather than creating
        // them — again the safe direction.
        for c in line.chars() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth -= 1,
                _ => {}
            }
        }
        if depth < 0 {
            return None; // unbalanced: refuse to judge
        }
        continued = line.ends_with('\\');
    }

    if triple.is_some() {
        return None; // unterminated string: refuse to judge
    }
    Some(!saw_executable)
}

/// The python files an entry script actually invokes, read out of the script's own text.
fn invoked_python(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in script.lines() {
        let l = line.trim();
        if l.starts_with('#') {
            continue;
        }
        let mut toks = l.split_whitespace().peekable();
        while let Some(t) = toks.next() {
            let base = t.rsplit('/').next().unwrap_or(t);
            if base != "python" && base != "python3" {
                continue;
            }
            // Skip flags, then take the first thing that looks like a script.
            for next in toks.by_ref() {
                if next.starts_with('-') {
                    continue;
                }
                if next.ends_with(".py") {
                    let p = next.trim_matches(|c| c == '"' || c == '\'').to_string();
                    if !out.contains(&p) {
                        out.push(p);
                    }
                }
                break;
            }
        }
    }
    out
}

/// Findings for entry points that run nothing. Empty for every set measured so far but one, which
/// keeps the write step's message byte-identical on a healthy build.
pub(crate) fn dead_entry_points(stream: &str) -> Vec<String> {
    let parsed = parse_file_stream(stream);
    let mut out = Vec::new();
    for entry in &parsed.entries {
        if !entry.path.ends_with(".sh") {
            continue;
        }
        for target in invoked_python(&entry.content) {
            let want = target.rsplit('/').next().unwrap_or(&target);
            // Match on the path as written, or on its basename — a script may say `./server.py`.
            let found = parsed
                .entries
                .iter()
                .find(|e| e.path == target || e.path.rsplit('/').next() == Some(want));
            let Some(py) = found else {
                out.push(format!(
                    "`{}` runs `{target}`, which is NOT in the file set — the deliverable cannot start",
                    entry.path
                ));
                continue;
            };
            if runs_nothing(&py.content) == Some(true) {
                out.push(format!(
                    "`{}` runs `{}`, but `{}` has NO code at module level — only imports, \
                     definitions and constants. Running it defines things and exits immediately, so \
                     nothing starts, nothing listens and nothing is printed. It needs the call that \
                     actually does the work (for a server, the bind-and-serve), usually under an \
                     `if __name__ == \"__main__\":` guard.",
                    entry.path, py.path, py.path
                ));
            }
        }
    }
    out
}
