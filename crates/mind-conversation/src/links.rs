//! E.LINKS1: an internal link to a file the set never wrote.
//!
//! Reading 9's Mind T2 scored 5/6. The one failed check was `relative_links_resolve`: `index.html`
//! offered six project and writing pages, and the build wrote one of them. Nothing else in the set
//! was wrong, and no other check could see it — the imports resolve, the entry point is live, the
//! files parse, the server starts. A link is a promise about a file, and the write step is the one
//! place that knows every file there is.
//!
//! Inverted risk, as its four neighbours: only a reference that cannot be anything but broken
//! convicts. An external scheme, a fragment, a root-absolute path, a directory index, any extension
//! we do not judge, anything resolving outside the deliverable — all silence, because each has a
//! reading in which it is correct and the write step cannot tell which reading holds.

use crate::fileset::parse_file_stream;
use std::collections::BTreeSet;

/// The extensions a written set is expected to satisfy itself. Everything else — `.json`, `.png`,
/// an extensionless route — is routinely produced at run time or served by a handler, so its
/// absence from the set proves nothing.
const JUDGED: [&str; 4] = ["html", "htm", "css", "js"];

/// At most this many missing targets are named before the finding says "and N more".
const NAMED: usize = 8;

fn is_html(path: &str) -> bool {
    let p = path.to_ascii_lowercase();
    p.ends_with(".html") || p.ends_with(".htm")
}

/// Every `href=` / `src=` value in the document.
///
/// The attribute name must stand on its own — preceded by whitespace or `<`, followed by `=` — so
/// `srcset` (a comma-separated list with descriptors, not a path) is never read as one reference,
/// and an unquoted value is left alone rather than guessed at.
fn references(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut out = Vec::new();
    for attr in ["href", "src"] {
        let mut from = 0usize;
        while let Some(rel) = lower[from..].find(attr) {
            let at = from + rel;
            from = at + attr.len();
            let before_ok = at == 0
                || lower[..at]
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_whitespace() || c == '<');
            if !before_ok {
                continue;
            }
            let mut rest = lower[from..].chars();
            let mut consumed = 0usize;
            // optional spaces, then '='
            let eq = loop {
                match rest.next() {
                    Some(c) if c.is_whitespace() => consumed += c.len_utf8(),
                    Some('=') => {
                        consumed += 1;
                        break true;
                    }
                    _ => break false,
                }
            };
            if !eq {
                continue;
            }
            // optional spaces, then a quote
            let quote = loop {
                match rest.next() {
                    Some(c) if c.is_whitespace() => consumed += c.len_utf8(),
                    Some(q @ ('"' | '\'')) => {
                        consumed += 1;
                        break Some(q);
                    }
                    _ => break None,
                }
            };
            let Some(q) = quote else { continue };
            let start = from + consumed;
            let Some(end) = lower[start..].find(q).map(|e| start + e) else {
                continue;
            };
            // Byte offsets from an ASCII-lowercased copy address the same boundaries in the
            // original, so the value is returned with its own case intact.
            out.push(html[start..end].to_string());
            from = end;
        }
    }
    out
}

/// The path this reference points at, or `None` when the reference is not one we may judge.
fn judgeable_target(raw: &str) -> Option<&str> {
    let v = raw.trim();
    let v = v.split('#').next().unwrap_or("");
    let v = v.split('?').next().unwrap_or("");
    if v.is_empty() {
        return None; // a pure fragment or query: this page
    }
    if v.starts_with('/') {
        return None; // root-absolute: where the site is served decides, and we do not know
    }
    if v.ends_with('/') {
        return None; // a directory index
    }
    if let Some(colon) = v.find(':') {
        let scheme = &v[..colon];
        let looks_like_scheme = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'));
        if looks_like_scheme {
            return None; // http:, mailto:, tel:, data:, javascript:
        }
    }
    let last = v.rsplit('/').next().unwrap_or("");
    let ext = last.rsplit_once('.')?.1.to_ascii_lowercase();
    if !JUDGED.contains(&ext.as_str()) {
        return None;
    }
    Some(v)
}

/// `target` resolved against the directory of `from`, as a path from the deliverable root.
/// `None` when it climbs out of the root, which we do not judge.
fn resolve(from: &str, target: &str) -> Option<String> {
    let mut segs: Vec<&str> = Vec::new();
    if let Some((dir, _)) = from.rsplit_once('/') {
        segs.extend(dir.split('/').filter(|s| !s.is_empty()));
    }
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                segs.pop()?;
            }
            p => segs.push(p),
        }
    }
    if segs.is_empty() {
        None
    } else {
        Some(segs.join("/"))
    }
}

fn normal(path: &str) -> String {
    resolve("", path).unwrap_or_else(|| path.to_string())
}

/// Findings for links that point at files neither this set nor the deliverable contains.
///
/// `present` is the deliverable as it already stands — the files an earlier pass wrote — so a
/// two-pass build is never convicted for a link its own second pass satisfies.
pub(crate) fn dangling_link_findings(stream: &str, present: &[String]) -> Vec<String> {
    let entries = parse_file_stream(stream).entries;
    let mut have: BTreeSet<String> = entries.iter().map(|e| normal(&e.path)).collect();
    have.extend(present.iter().map(|p| normal(p)));

    let mut out = Vec::new();
    let mut html_seen = 0usize;
    for e in &entries {
        if !is_html(&e.path) {
            continue;
        }
        html_seen += 1;
        let mut missing: Vec<String> = Vec::new();
        for raw in references(&e.content) {
            let Some(target) = judgeable_target(&raw) else {
                continue;
            };
            let Some(resolved) = resolve(&e.path, target) else {
                continue;
            };
            if !have.contains(&resolved) && !missing.contains(&resolved) {
                missing.push(resolved);
            }
        }
        if missing.is_empty() {
            continue;
        }
        let shown: Vec<String> = missing.iter().take(NAMED).cloned().collect();
        let more = missing.len().saturating_sub(shown.len());
        out.push(format!(
            "`{}` links {} file(s) that this build never wrote: {}{}. A link is a promise about a file — write those pages, or drop the links that lead nowhere.",
            e.path,
            missing.len(),
            shown.join(", "),
            if more > 0 { format!(", and {more} more") } else { String::new() }
        ));
    }
    // The one trace a witness on the box can look for. Silent for a set with no html in it, so
    // nothing changes for the builds this check does not judge.
    if html_seen > 0 {
        eprintln!(
            "[links] checked {html_seen} html file(s) against {} known path(s): {} finding(s)",
            have.len(),
            out.len()
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stream(files: &[(&str, &str)]) -> String {
        files
            .iter()
            .map(|(n, c)| format!("=== FILE: {n}\n{c}\n"))
            .collect()
    }

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(
            concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/links/").to_string() + name,
        )
        .expect("fixture present")
    }

    /// The real artifact that cost reading 9's Mind T2 its sixth check: `index.html` offering six
    /// pages the build never wrote. Every other link in it resolves, and none of them may appear.
    #[test]
    fn the_real_reading_9_artifact_names_its_six_missing_pages_and_nothing_else() {
        let index = fixture("r9_mind_T2_index.html");
        let ferry = fixture("r9_mind_T2_projects_ferry.html");
        let f = dangling_link_findings(
            &stream(&[
                ("index.html", &index),
                ("css/style.css", "body{}"),
                ("projects/ferry.html", &ferry),
                ("RESULT.md", "# notes"),
            ]),
            &[],
        );
        assert_eq!(f.len(), 1, "one finding, for the one file that links: {f:?}");
        for missing in [
            "projects/ledgerline.html",
            "projects/patchwork.html",
            "projects/tidepool.html",
            "writing/backpressure.html",
            "writing/reviewing-migrations.html",
            "writing/sqlite-at-scale.html",
        ] {
            assert!(f[0].contains(missing), "must name {missing}: {}", f[0]);
        }
        assert!(f[0].contains("links 6 file"), "{}", f[0]);
        for resolves in ["ferry.html\"", "style.css\"", "mailto", "#contact"] {
            assert!(!f[0].contains(resolves), "must not accuse {resolves}: {}", f[0]);
        }
    }

    #[test]
    fn a_set_whose_links_all_resolve_is_silent() {
        let index = "<a href=\"about.html\">a</a><link href=\"css/s.css\"><script src=\"./app.js\"></script>";
        let about = "<a href=\"../index.html\">home</a><img src=\"../pic.png\">";
        assert!(dangling_link_findings(
            &stream(&[
                ("index.html", index),
                ("about.html", "<p>hi</p>"),
                ("css/s.css", "body{}"),
                ("app.js", "1"),
            ]),
            &[]
        )
        .is_empty());
        // A page in a subdirectory resolves against its own directory, not the root.
        assert!(dangling_link_findings(
            &stream(&[("index.html", "<p>x</p>"), ("sub/about.html", about)]),
            &[]
        )
        .is_empty());
    }

    #[test]
    fn every_uncertain_reference_stays_silent() {
        for (name, body) in [
            ("external", "<a href=\"https://example.com/a.html\">x</a>"),
            ("protocol relative", "<a href=\"//cdn.example.com/a.css\">x</a>"),
            ("mailto", "<a href=\"mailto:a@b.dev\">x</a>"),
            ("tel", "<a href=\"tel:+15551234\">x</a>"),
            ("data uri", "<img src=\"data:image/png;base64,AA\">"),
            ("javascript", "<a href=\"javascript:void(0)\">x</a>"),
            ("fragment", "<a href=\"#contact\">x</a>"),
            ("root absolute", "<a href=\"/index.html\">x</a>"),
            ("directory", "<a href=\"projects/\">x</a>"),
            ("unjudged extension", "<a href=\"data/leads.json\">x</a>"),
            ("no extension", "<a href=\"api/status\">x</a>"),
            ("climbs out", "<a href=\"../../elsewhere.html\">x</a>"),
            ("srcset is not src", "<img srcset=\"a.html 1x, b.html 2x\">"),
            ("unquoted", "<a href=missing.html>x</a>"),
            ("empty", "<a href=\"\">x</a>"),
        ] {
            let f = dangling_link_findings(&stream(&[("index.html", body)]), &[]);
            assert!(f.is_empty(), "{name} must stay silent: {f:?}");
        }
    }

    #[test]
    fn a_link_satisfied_by_an_earlier_pass_is_silent_and_one_still_missing_is_not() {
        let index = "<a href=\"round1.html\">a</a><a href=\"never.html\">b</a>";
        let f = dangling_link_findings(
            &stream(&[("index.html", index)]),
            &["round1.html".to_string()],
        );
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].contains("never.html") && !f[0].contains("round1.html"), "{}", f[0]);
        // Both present: nothing to say.
        assert!(dangling_link_findings(
            &stream(&[("index.html", index)]),
            &["round1.html".to_string(), "never.html".to_string()]
        )
        .is_empty());
    }

    #[test]
    fn the_fragment_and_query_are_stripped_before_the_path_is_judged() {
        // Same file, three ways: only the missing one is named, once.
        let body = "<a href=\"page.html#top\">1</a><a href=\"page.html?v=2\">2</a><a href=\"page.html\">3</a>";
        let f = dangling_link_findings(&stream(&[("index.html", body)]), &[]);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].contains("links 1 file"), "named once, not three times: {}", f[0]);
        assert!(dangling_link_findings(
            &stream(&[("index.html", body), ("page.html", "<p>x</p>")]),
            &[]
        )
        .is_empty());
    }

    #[test]
    fn single_quotes_and_odd_spacing_are_read_the_same_way() {
        let body = "<a HREF = 'missing.html'>x</a>";
        let f = dangling_link_findings(&stream(&[("index.html", body)]), &[]);
        assert_eq!(f.len(), 1, "{f:?}");
        assert!(f[0].contains("missing.html"), "{}", f[0]);
    }

    /// Corpus: no false positives anywhere. Every path a finding names must genuinely be absent
    /// from that artifact on disk. The count of firing artifacts is REPORTED, not pinned at zero —
    /// the corpus now contains reading 9's genuinely broken site, and a rule that fires on it is
    /// working.
    #[test]
    fn no_finding_over_the_whole_corpus_names_a_file_that_exists() {
        let Ok(root) = std::env::var("YM_ENTRY_CORPUS") else {
            eprintln!("skipped: YM_ENTRY_CORPUS unset");
            return;
        };
        let mut judged = 0usize;
        let mut fired: Vec<String> = Vec::new();
        let mut wrong: Vec<String> = Vec::new();
        for dir in artifact_dirs(&root) {
            let files = relative_files(&dir);
            if !files.iter().any(|(n, _)| is_html(n)) {
                continue;
            }
            judged += 1;
            let refs: Vec<(&str, &str)> =
                files.iter().map(|(n, c)| (n.as_str(), c.as_str())).collect();
            let f = dangling_link_findings(&stream(&refs), &[]);
            if f.is_empty() {
                continue;
            }
            fired.push(dir.display().to_string());
            for named in f.iter().flat_map(|m| {
                m.split(": ")
                    .nth(1)
                    .unwrap_or("")
                    .split(". A link")
                    .next()
                    .unwrap_or("")
                    .split(", ")
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            }) {
                let p = dir.join(named.trim());
                if p.exists() {
                    wrong.push(p.display().to_string());
                }
            }
        }
        assert!(judged > 0, "YM_ENTRY_CORPUS pointed at nothing with html in it");
        assert!(wrong.is_empty(), "named files that DO exist: {wrong:?}");
        eprintln!("corpus: {judged} artifacts with html, {} fired: {fired:?}", fired.len());
    }

    fn artifact_dirs(root: &str) -> Vec<std::path::PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![std::path::PathBuf::from(root)];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            let mut has_file = false;
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    has_file = true;
                }
            }
            // Only a real artifact directory: the benchmark writes them as
            // `out-*/artifacts/<system>_<task>/`. A parent directory would aggregate several
            // artifacts into one imaginary set whose relative paths belong to no build.
            if has_file && d.parent().and_then(|p| p.file_name()) == Some(std::ffi::OsStr::new("artifacts")) {
                out.push(d);
            }
        }
        out
    }

    /// Files under `dir`, keyed by their path RELATIVE to it — subdirectories included, because a
    /// link resolves against directories and a flattened name cannot be judged.
    fn relative_files(dir: &std::path::Path) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else { continue };
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if let Ok(c) = std::fs::read_to_string(&p) {
                    if let Ok(rel) = p.strip_prefix(dir) {
                        out.push((rel.to_string_lossy().replace('\\', "/"), c));
                    }
                }
            }
        }
        out
    }
}
