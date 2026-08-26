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
const ALLOWED: &[(&str, &str)] = &[];

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::{Path, PathBuf};

    fn crates_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
    }

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
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

    /// Split a file into rough function bodies. Crude on purpose: the point is to stop a binding in
    /// one function being paired with a slice in another, which is the only false positive this
    /// check has ever produced.
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

    /// The variable bound to a lowered copy, and the expression it was lowered FROM.
    fn lowered_binding(line: &str) -> Option<(&str, &str)> {
        let (lhs, rhs) = line.split_once('=')?;
        let rhs = rhs.trim();
        let src = rhs.strip_suffix(".to_lowercase();")?;
        let name = lhs.trim().strip_prefix("let ")?.trim().strip_prefix("mut ").unwrap_or_else(|| {
            lhs.trim().strip_prefix("let ").unwrap_or("").trim()
        });
        let name = name.split(':').next()?.trim();
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        // The ROOT of what was lowered — and ONLY when the expression is a plain variable wearing
        // identity-preserving calls. `toks.iter().map(..).join(" ").to_lowercase()` lowercases the
        // JOINED STRING, not `toks`, so pairing it with a `toks[i]` Vec index is nonsense; a looser
        // rule reported sixteen of those and buried the real hits in them.
        let mut rest = src.trim_start_matches(['&', '*']).trim();
        let root = rest.split(['.', ' ']).next()?.trim();
        if root.is_empty() || !root.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        rest = rest[root.len()..].trim();
        // Only these may sit between the variable and the lowering. `trim` is INCLUDED on purpose:
        // it shifts offsets too, which is the research.rs defect.
        for call in [".trim()", ".trim_start()", ".trim_end()", ".as_str()", ""] {
            if call.is_empty() {
                break;
            }
            while let Some(r) = rest.strip_prefix(call) {
                rest = r.trim();
            }
        }
        if !rest.is_empty() {
            return None;
        }
        Some((name, root))
    }

    /// Does this line index `root` — as a WHOLE NAME, not as the tail of a longer one?
    ///
    /// Matching the bare substring `t[` finds it inside `next["id"]`, and `s[` inside `parts[0]`.
    /// Both were reported as findings by the first version of this check. It is the same
    /// substring-for-token mistake that made `contains("save")` fire on "saved" — this time in the
    /// instrument rather than in the code it audits, which is the more embarrassing place for it.
    fn slices(line: &str, root: &str) -> bool {
        let pat = format!("{root}[");
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(&pat) {
            let at = from + rel;
            let boundary = line[..at].chars().next_back().is_none_or(|c| !c.is_alphanumeric() && c != '_');
            if boundary {
                return true;
            }
            from = at + pat.len();
        }
        false
    }

    /// An offset taken from a lowered copy must never index a string that is not byte-identical to
    /// it. `to_lowercase` is not length-preserving, and neither is `trim`.
    #[test]
    fn no_offset_from_a_lowered_copy_indexes_an_original() {
        let mut offenders: Vec<String> = Vec::new();
        let mut files = Vec::new();
        rs_files(&crates_dir(), &mut files);

        for f in files {
            let name = f.file_name().and_then(|x| x.to_str()).unwrap_or("").to_string();
            if ALLOWED.iter().any(|(a, _)| *a == name) {
                continue;
            }
            let Ok(body) = std::fs::read_to_string(&f) else { continue };
            for (start, lines) in functions(&body) {
                // Tests build adversarial strings on purpose; they are not the production path.
                if lines.iter().any(|l| l.contains("#[test]") || l.contains("#[cfg(test)]")) {
                    continue;
                }
                for line in &lines {
                    let Some((low, root)) = lowered_binding(line) else { continue };
                    // Does this function slice the ORIGINAL, or the lowered copy's source?
                    let slices_original =
                        lines.iter().any(|l| !l.contains(".to_lowercase()") && slices(l, root));
                    if slices_original {
                        offenders.push(format!(
                            "{name}: fn at line {start} binds `{low}` from `{root}.to_lowercase()` and slices `{root}[..]`"
                        ));
                    }
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "An offset from a LOWERED COPY is being used to index an ORIGINAL string.\n\n{}\n\n\
             `to_lowercase()` is NOT length-preserving — U+0130 is 2 bytes and its lowercase is 3 — \
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
        let bad = "fn f(text: &str) {\n    let l = text.to_lowercase();\n    let x = &text[l.find(\"a\").unwrap()..];\n}";
        let found = functions(bad).iter().any(|(_, lines)| {
            lines.iter().any(|line| {
                lowered_binding(line).is_some_and(|(_, root)| {
                    lines.iter().any(|l| !l.contains(".to_lowercase()") && slices(l, root))
                })
            })
        });
        assert!(found, "the check must fire on the shape it exists to catch");

        // And it must NOT fire on the safe forms, or it will be silenced rather than heeded.
        let ascii = "fn f(text: &str) {\n    let l = text.to_ascii_lowercase();\n    let x = &text[l.find(\"a\").unwrap()..];\n}";
        assert!(functions(ascii).iter().all(|(_, lines)| lines.iter().all(|l| lowered_binding(l).is_none())));

        let compare_only = "fn f(text: &str) {\n    let t = text.trim().to_lowercase();\n    let _ = t == \"yes\";\n}";
        let fired = functions(compare_only).iter().any(|(_, lines)| {
            lines.iter().any(|line| {
                lowered_binding(line).is_some_and(|(_, root)| {
                    lines.iter().any(|l| !l.contains(".to_lowercase()") && slices(l, root))
                })
            })
        });
        assert!(!fired, "a lowered copy that is only COMPARED is not this defect");
    }
}
