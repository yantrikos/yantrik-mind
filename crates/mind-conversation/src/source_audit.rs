//! source_audit — a standing guard on the "transformed copy" defect class.
//!
//! Four separate defects in one day were the same mistake: REASONING ABOUT A TRANSFORMED COPY OF A
//! STRING AS THOUGH IT WERE THE ORIGINAL.
//!
//!   1. `mind_types::first_sensitive` — offsets found in `text.to_lowercase()` sliced `text`.
//!      `to_lowercase` is not length-preserving (`İ` U+0130 is 2 bytes, its lowercase 3), so
//!      `first_sensitive("İpassword日本")` panicked mid-character. Reachable from `gate_write`,
//!      which scans arbitrary user text on every memory write (E.SEC1b).
//!   2. The skill command parsers — the same, twice, plus silent corruption:
//!      `parse_run_skill("İ run skill csv-sum")` returned `Some(("sv-sum", ""))`, so the user was
//!      told their skill did not exist (E.SEC3).
//!   3. `research.rs` — offsets from `text.trim()` slicing the UNTRIMMED `text`, so a leading space
//!      turned "research quantum computing" into the topic "h quantum computing", silently
//!      (E.SEC4).
//!   4. `privacy_audit` itself matched source text one line at a time while rustfmt wrapped the
//!      chain, hiding 19 unscoped cloud calls (E.SEC2). Different transform, same mistake.
//!
//! A one-off sweep found these. A sweep is a thing someone has to remember to run, and I did run
//! one and then moved on while it still had hits. This is the same sweep as a TEST, so a new
//! occurrence fails the build instead of waiting for the next person who thinks to look.
//!
//! It scans the whole workspace, function by function. Function scoping is what keeps it precise:
//! a ±N-line window crosses into neighbouring functions and reports bindings that never touch each
//! other, which is exactly how the first version of this hid two real hits behind noise.

/// Files permitted to derive an offset from a lowered copy and slice an original.
///
/// Empty, and it should stay that way. An entry here is a claim that the transform is
/// length-preserving for every input the function can receive — state why.
///
/// Keyed by CRATE-RELATIVE PATH even while empty. `privacy_audit` was keyed by basename and a
/// single `lib.rs` entry silenced three crates' worth of files; an empty list keyed the same way
/// is not a bug today and is a bug the first time someone adds a row (E.SEC6, Codex's note).
const ALLOWED: &[(&str, &str)] = &[];

/// Remove `/* block */` and `// line` comments, replacing each with a single space.
///
/// A space, not nothing: deleting a comment outright would glue the tokens on either side of it
/// into one, which invents matches. Both audits use this, because both squash whitespace before
/// matching and a comment left in place splits the very pattern they look for — Codex's note that
/// `inference /* c */ .chat(...)` could hide from a matcher that only understood `//` (E.SEC6).
#[cfg(test)]
pub(crate) fn strip_comments(src: &str) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let (mut i, mut in_block, mut in_line) = (0usize, false, false);
    while i < b.len() {
        if in_block {
            if b[i] == b'*' && i + 1 < b.len() && b[i + 1] == b'/' {
                in_block = false;
                out.push(' ');
                i += 2;
                continue;
            }
        } else if in_line {
            if b[i] == b'\n' {
                in_line = false;
                out.push('\n');
            }
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            in_block = true;
            i += 2;
            continue;
        } else if b[i] == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            in_line = true;
            out.push(' ');
            i += 2;
            continue;
        } else {
            out.push(b[i] as char);
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn crates_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                if p.file_name().and_then(|x| x.to_str()) == Some("target") {
                    continue;
                }
                rs_files(&p, out);
            } else if p.extension().and_then(|x| x.to_str()) == Some("rs") {
                out.push(p);
            }
        }
    }

    /// Split a file into rough function bodies, so a binding in one function is never paired with
    /// a slice in another.
    fn functions(body: &str) -> Vec<(usize, Vec<&str>)> {
        let mut out: Vec<(usize, Vec<&str>)> = Vec::new();
        for (i, line) in body.lines().enumerate() {
            let t = line.trim_start();
            let starts_fn = t.starts_with("fn ")
                || t.starts_with("pub fn ")
                || t.starts_with("pub(crate) fn ")
                || t.starts_with("async fn ")
                || t.starts_with("pub async fn ")
                || t.starts_with("pub(crate) async fn ");
            if starts_fn || out.is_empty() {
                out.push((i + 1, Vec::new()));
            }
            out.last_mut().unwrap().1.push(line);
        }
        out
    }

    /// A function body as LOGICAL STATEMENTS, not physical lines.
    ///
    /// CODEX FOUND WHY THIS IS NECESSARY. The first version matched a single line shaped like
    /// `let x = y.to_lowercase();`, so rustfmt's own output defeated it: a binding wrapped as
    /// `let low = text` / `.trim()` / `.to_lowercase();` has no line carrying both the assignment
    /// and the lowering, and nothing fired. That is the EXACT class this guard was written to catch
    /// after `privacy_audit` fell to it — reproduced inside the replacement. Comments are stripped,
    /// whitespace collapsed, and the space rustfmt inserts before a wrapped `.method()` removed, so
    /// a chain reads as one statement whatever shape it was formatted into (E.SEC5).
    fn statements(lines: &[&str]) -> Vec<String> {
        let joined = super::strip_comments(&lines.join("\n")).replace('\n', " ");
        let mut flat = String::with_capacity(joined.len());
        let mut prev_space = false;
        for c in joined.chars() {
            if c.is_whitespace() {
                if !prev_space {
                    flat.push(' ');
                }
                prev_space = true;
            } else {
                flat.push(c);
                prev_space = false;
            }
        }
        let flat = flat.replace(" .", ".");
        flat.split(';')
            .map(|st| format!("{};", st.trim()))
            .filter(|st| st.len() > 1)
            .collect()
    }

    /// The variable bound to a lowered copy, and the expression it was lowered FROM.
    fn lowered_binding(stmt: &str) -> Option<(String, String)> {
        let (lhs, rhs) = stmt.split_once('=')?;
        let src = rhs.trim().strip_suffix(".to_lowercase();")?;
        // The LAST `let` in the left-hand side. Joining physical lines into statements glues a
        // function signature or an opening brace onto the first one, so `lhs` no longer starts
        // with `let` even when the statement plainly is a binding.
        let let_at = lhs.rfind("let ")?;
        let name = lhs[let_at + "let ".len()..].trim();
        let name = name.strip_prefix("mut ").unwrap_or(name);
        let name = name.split(':').next()?.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        // Only a plain variable wearing identity-preserving calls. `toks.iter().map(..).join(" ")`
        // lowercases the JOINED STRING, not `toks`, so pairing it with a `toks[i]` index is nonsense.
        let mut rest = src.trim_start_matches(['&', '*']).trim();
        let root = rest.split(['.', ' ']).next()?.trim().to_string();
        if root.is_empty() || !root.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        rest = rest[root.len()..].trim();
        // `trim` is INCLUDED on purpose: it shifts offsets too, which is the research.rs defect.
        loop {
            let before = rest.len();
            for call in [".trim()", ".trim_start()", ".trim_end()", ".as_str()"] {
                rest = rest.strip_prefix(call).unwrap_or(rest).trim();
            }
            if rest.len() == before {
                break;
            }
        }
        if !rest.is_empty() {
            return None;
        }
        Some((name.to_string(), root))
    }

    /// Does this statement index `root` — as a WHOLE NAME, not the tail of a longer one?
    ///
    /// Matching the bare substring `t[` finds it inside `next["id"]`, and `s[` inside `parts[0]`.
    /// Both were reported as findings by the first version — the same substring-for-token mistake
    /// this codebase keeps making, that time in the instrument rather than the code it audits.
    fn slices(stmt: &str, root: &str) -> bool {
        let pat = format!("{root}[");
        let mut from = 0usize;
        while let Some(rel) = stmt[from..].find(&pat) {
            let at = from + rel;
            let boundary = stmt[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if boundary {
                return true;
            }
            from = at + pat.len();
        }
        false
    }

    /// A lowered binding whose source is sliced elsewhere in the same function.
    fn offenders_in(lines: &[&str]) -> Vec<(String, String)> {
        let stmts = statements(lines);
        let mut out = Vec::new();
        for (i, stmt) in stmts.iter().enumerate() {
            let Some((low, root)) = lowered_binding(stmt) else {
                continue;
            };
            let sliced = stmts.iter().enumerate().any(|(j, other)| {
                j != i && !other.contains(".to_lowercase()") && slices(other, &root)
            });
            if sliced {
                out.push((low, root));
            }
        }
        out
    }

    /// An offset taken from a lowered copy must never index a string that is not byte-identical to
    /// it. `to_lowercase` is not length-preserving, and neither is `trim`.
    #[test]
    fn no_offset_from_a_lowered_copy_indexes_an_original() {
        let mut offenders: Vec<String> = Vec::new();
        let mut files = Vec::new();
        rs_files(&crates_dir(), &mut files);

        for f in files {
            // CRATE-RELATIVE, never a basename — see ALLOWED's own note.
            let name = f
                .strip_prefix(crates_dir())
                .map(|r| r.to_string_lossy().replace(std::path::MAIN_SEPARATOR, "/"))
                .unwrap_or_else(|_| {
                    f.file_name()
                        .and_then(|x| x.to_str())
                        .unwrap_or("")
                        .to_string()
                });
            if ALLOWED.iter().any(|(a, _)| *a == name) {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&f) else {
                continue;
            };
            for (start, lines) in functions(&body) {
                // Tests build adversarial strings on purpose; they are not the production path.
                if lines
                    .iter()
                    .any(|l| l.contains("#[test]") || l.contains("#[cfg(test)]"))
                {
                    continue;
                }
                for (low, root) in offenders_in(&lines) {
                    offenders.push(format!(
                        "{name}: fn at line {start} binds `{low}` from `{root}.to_lowercase()` and slices `{root}[..]`"
                    ));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "An offset from a LOWERED COPY is being used to index an ORIGINAL string.\n\n{}\n\n\
             `to_lowercase()` is NOT length-preserving - U+0130 is 2 bytes and its lowercase is 3 - \
             so every offset after such a character is shifted. Best case the parse silently changes \
             what the user asked for; worst case the shifted offset lands mid-character and panics.\n\
             Fix one of two ways:\n  \
             (a) the patterns are ASCII (they almost always are) -> use `to_ascii_lowercase()`, which \
             is length- and boundary-preserving by construction\n  \
             (b) they genuinely are not -> carry an index map, and add the file to ALLOWED here WITH \
             the reason the transform is safe for every input.\n\
             The same rule covers `trim()`: slice the string the offsets came FROM, not the one it \
             was derived from.",
            offenders.join("\n")
        );
    }

    /// The guard must be able to see the defect it exists for, or it is decoration.
    #[test]
    fn the_check_can_actually_fire() {
        let one_line = [
            "fn f(text: &str) {",
            "    let l = text.to_lowercase();",
            "    let x = &text[l.find(\"a\").unwrap()..];",
            "}",
        ];
        assert_eq!(
            offenders_in(&one_line).len(),
            1,
            "the plain shape must fire"
        );

        // CODEX'S CASE: rustfmt wrapped the binding itself. The first version saw nothing here -
        // the same line-wrapping blindness this guard was written to replace.
        let wrapped = [
            "fn f(text: &str) {",
            "    let low = text",
            "        .trim()",
            "        .to_lowercase();",
            "    let x = &text[low.find(\"a\").unwrap()..];",
            "}",
        ];
        assert_eq!(
            offenders_in(&wrapped).len(),
            1,
            "a wrapped binding must fire too"
        );

        // And it must NOT fire on the safe forms, or it gets silenced rather than heeded.
        let ascii = [
            "fn f(text: &str) {",
            "    let l = text.to_ascii_lowercase();",
            "    let x = &text[l.find(\"a\").unwrap()..];",
            "}",
        ];
        assert!(
            offenders_in(&ascii).is_empty(),
            "to_ascii_lowercase is the fix, not the defect"
        );

        let wrapped_ascii = [
            "fn f(text: &str) {",
            "    let l = text",
            "        .to_ascii_lowercase();",
            "    let x = &text[l.find(\"a\").unwrap()..];",
            "}",
        ];
        assert!(
            offenders_in(&wrapped_ascii).is_empty(),
            "and wrapped, still the fix"
        );

        let compare_only = [
            "fn f(text: &str) {",
            "    let t = text.trim().to_lowercase();",
            "    let _ = t == \"yes\";",
            "}",
        ];
        assert!(
            offenders_in(&compare_only).is_empty(),
            "a lowered copy that is only COMPARED is not this defect"
        );

        let joined = [
            "fn f(toks: &[&str]) {",
            "    let c = toks.iter().map(|t| *t).collect::<Vec<_>>().join(\" \").to_lowercase();",
            "    let _ = toks[0];",
            "}",
        ];
        assert!(
            offenders_in(&joined).is_empty(),
            "the JOINED string is not `toks`"
        );

        let substring = [
            "fn f(next: &serde_json::Value) {",
            "    let t = next.to_string().to_lowercase();",
            "    let _ = next[\"id\"];",
            "}",
        ];
        assert!(
            offenders_in(&substring).is_empty(),
            "`t[` must not match inside `next[\"id\"]`"
        );
    }
}
