//! E.FILES1 — writing a SET of files as a deliverable.
//!
//! The mind can publish exactly one HTML document. Every point it cannot reach on the frozen
//! benchmark needs more than that: T1 asks for `run.sh` starting a server, a form posting to it and
//! `data/leads.json` with append semantics; T3 asks for `tracker.py` and a passing pytest suite.
//! Both are the same missing capability.
//!
//! THIS MODULE IS WIRED TO NOTHING. No tool exposes it and no recipe calls it — the staging that
//! produced the one large slice today which came through review clean. Validation is a pure
//! function so its rules can be exercised without a filesystem, and the writer is thin enough that
//! the tests can assert the files on disk, which is the only thing that actually matters: the
//! previous slice shipped as a no-op with a correct rule, correct wiring, and no test asking what
//! the file was called.

use std::path::{Path, PathBuf};

/// A file count no honest deliverable exceeds, and beyond which a runaway generation is the more
/// likely explanation than a large project.
pub const MAX_FILES: usize = 32;
/// Per file. A page or a script; not a dataset.
pub const MAX_FILE_BYTES: usize = 512 * 1024;
/// The whole set.
pub const MAX_TOTAL_BYTES: usize = 2 * 1024 * 1024;
/// `a/b/c/d.txt` is four components and is as deep as a deliverable of this size needs.
pub const MAX_DEPTH: usize = 4;

/// Why a file set was refused. The WHOLE set is refused, never part of it: a half-written
/// deliverable is worse than none, because it looks like a finished one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileSetRefusal {
    Empty,
    TooManyFiles {
        count: usize,
        cap: usize,
    },
    FileTooLarge {
        path: String,
        bytes: usize,
        cap: usize,
    },
    TotalTooLarge {
        bytes: usize,
        cap: usize,
    },
    TooDeep {
        path: String,
        depth: usize,
        cap: usize,
    },
    DuplicatePath {
        path: String,
    },
    UnsafePath {
        path: String,
        why: &'static str,
    },
}

impl FileSetRefusal {
    /// An operator has to be able to read this and know which entry to change. A refusal nobody can
    /// act on is a silent failure with extra steps.
    pub fn message(&self) -> String {
        match self {
            Self::Empty => "nothing to write: the file set is empty".to_string(),
            Self::TooManyFiles { count, cap } => {
                format!("{count} files is more than the {cap} a deliverable may contain")
            }
            Self::FileTooLarge { path, bytes, cap } => {
                format!("{path} is {bytes} bytes, over the {cap}-byte limit for one file")
            }
            Self::TotalTooLarge { bytes, cap } => {
                format!("the set is {bytes} bytes, over the {cap}-byte limit for one deliverable")
            }
            Self::TooDeep { path, depth, cap } => {
                format!("{path} is {depth} levels deep, over the limit of {cap}")
            }
            Self::DuplicatePath { path } => {
                format!("{path} appears twice; one path, one file")
            }
            Self::UnsafePath { path, why } => {
                format!("{path} is not a safe relative path: {why}")
            }
        }
    }
}

/// One entry as a caller supplies it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub content: String,
}

/// Is this a path that may be written inside a deliverable?
///
/// Everything here is about what reaches the FILESYSTEM, so it is stated as refusals rather than as
/// a sanitiser: a sanitiser turns a hostile path into a plausible one and writes it, which is the
/// worse failure. `..`, an absolute path, a drive letter, a backslash and a control byte are all
/// refused rather than cleaned.
fn check_path(path: &str) -> Result<String, FileSetRefusal> {
    let bad = |why: &'static str| {
        Err(FileSetRefusal::UnsafePath {
            path: path.to_string(),
            why,
        })
    };
    if path.trim().is_empty() {
        return bad("it is empty");
    }
    if path.chars().any(|c| c.is_control()) {
        return bad("it contains a control character");
    }
    if path.starts_with('/') || path.starts_with('\\') {
        return bad("it is absolute");
    }
    if path.contains('\\') {
        return bad("it contains a backslash");
    }
    // `C:` or any single-letter drive prefix, on any host, because the string may be carried to one.
    if path.len() >= 2 && path.as_bytes()[1] == b':' {
        return bad("it names a drive");
    }
    let parts: Vec<&str> = path.split('/').collect();
    if parts.len() > MAX_DEPTH {
        return Err(FileSetRefusal::TooDeep {
            path: path.to_string(),
            depth: parts.len(),
            cap: MAX_DEPTH,
        });
    }
    for p in &parts {
        if p.is_empty() {
            return bad("it has an empty path component");
        }
        if *p == "." || *p == ".." {
            return bad("it contains a relative component");
        }
        if p.starts_with('.') {
            return bad("a component starts with a dot");
        }
        if p.ends_with('.') || p.ends_with(' ') {
            return bad("a component ends with a dot or a space");
        }
        if !p
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return bad("a component has a character outside [A-Za-z0-9-_.]");
        }
    }
    Ok(parts.join("/"))
}

/// Validate a whole file set. `Ok` means every entry may be written; `Err` means NONE of them is.
///
/// Pure: no filesystem, no clock, no environment. The write below applies exactly this plan.
/// Paths a build stream states more than once, in the order first repeated.
///
/// E.WIN6: the set now keeps the LAST definition rather than refusing everything, so the repeat
/// has to be REPORTED or a wrong winner is silent. Named here beside the parser it uses.
pub fn duplicate_paths(stream: &str) -> Vec<String> {
    let parsed = parse_file_stream(stream);
    let mut seen: Vec<String> = Vec::new();
    let mut dup: Vec<String> = Vec::new();
    for e in &parsed.entries {
        if seen.iter().any(|s| s == &e.path) {
            if !dup.iter().any(|d| d == &e.path) {
                dup.push(e.path.clone());
            }
        } else {
            seen.push(e.path.clone());
        }
    }
    dup
}

/// Plan a set, and report which paths were stated more than once.
///
/// E.WIN6: a repeated path used to refuse the whole set. It now keeps the LAST definition and
/// names the path, so the caller can tell the review round that a file was written twice — a
/// silent pick is how the wrong version wins unnoticed.
pub fn plan_file_set_reporting(
    entries: &[FileEntry],
) -> Result<(Vec<FileEntry>, Vec<String>), FileSetRefusal> {
    let mut duplicated: Vec<String> = Vec::new();
    let planned = plan_inner(entries, &mut duplicated)?;
    Ok((planned, duplicated))
}

pub fn plan_file_set(entries: &[FileEntry]) -> Result<Vec<FileEntry>, FileSetRefusal> {
    let mut ignored = Vec::new();
    plan_inner(entries, &mut ignored)
}

fn plan_inner(
    entries: &[FileEntry],
    duplicated: &mut Vec<String>,
) -> Result<Vec<FileEntry>, FileSetRefusal> {
    if entries.is_empty() {
        return Err(FileSetRefusal::Empty);
    }
    if entries.len() > MAX_FILES {
        return Err(FileSetRefusal::TooManyFiles {
            count: entries.len(),
            cap: MAX_FILES,
        });
    }
    let mut planned: Vec<FileEntry> = Vec::with_capacity(entries.len());
    let mut total = 0usize;
    for e in entries {
        let path = check_path(&e.path)?;
        let bytes = e.content.len();
        if bytes > MAX_FILE_BYTES {
            return Err(FileSetRefusal::FileTooLarge {
                path,
                bytes,
                cap: MAX_FILE_BYTES,
            });
        }
        total += bytes;
        if total > MAX_TOTAL_BYTES {
            return Err(FileSetRefusal::TotalTooLarge {
                bytes: total,
                cap: MAX_TOTAL_BYTES,
            });
        }
        if let Some(prev) = planned.iter().position(|p| p.path == path) {
            // E.WIN6 — a duplicate used to refuse the WHOLE set, and a real `ym delegate` run died
            // on it: the model emitted `run.sh` twice and the build produced nothing at all. The
            // lane already means "later replaces earlier" — the review step's own instruction is
            // that any file it outputs replaces the old one wholesale — so the second statement is
            // the latest intent. Keep it, and let the caller SAY the path repeated, because a
            // silent pick is how the wrong version wins unnoticed.
            duplicated.push(path.clone());
            planned.remove(prev);
        }
        planned.push(FileEntry {
            path,
            content: e.content.clone(),
        });
    }
    Ok(planned)
}

/// Write a validated set under `root`, creating directories as needed. Returns the relative paths
/// written, in the order given.
///
/// Refusal happens BEFORE anything is written, so a rejected set leaves the destination untouched.
/// A failure part-way through a write is a different thing — a full disk, a permission — and is
/// reported as an error rather than pretended away; what this guarantees is that no REFUSED set
/// ever puts a byte on disk.
pub fn write_file_set(root: &Path, entries: &[FileEntry]) -> anyhow::Result<Vec<String>> {
    let planned = plan_file_set(entries).map_err(|r| anyhow::anyhow!("{}", r.message()))?;
    let mut written = Vec::with_capacity(planned.len());
    for e in &planned {
        let full: PathBuf = root.join(&e.path);
        // The path was checked, and this asserts the RESULT of that check rather than trusting it:
        // whatever the components were, the file must land inside the root.
        if !full.starts_with(root) {
            anyhow::bail!("{} would land outside the deliverable", e.path);
        }
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&full, e.content.as_bytes())?;
        written.push(e.path.clone());
    }
    Ok(written)
}

/// The marker that opens each file in a build stream.
pub const FILE_MARKER: &str = "=== FILE:";

/// What a parse recovered, and what it observed about how the stream ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSet {
    pub entries: Vec<FileEntry>,
    /// The last file's path when the stream did NOT end with a newline — an OBSERVATION, not a
    /// verdict, and the file is kept.
    ///
    /// This used to be `truncated`, and the file was DROPPED. That rule cost a real deliverable:
    /// a graded preflight leg produced `tracker.py` and a complete `test_tracker.py`, and the whole
    /// test suite was thrown away and reported as "cut off". Reproducing the same generation showed
    /// `finish_reason: stop` with three complete files and no trailing newline — models routinely
    /// end that way. A missing final newline CANNOT distinguish a truncated file from a finished
    /// one; only the API's finish_reason can, and the parser does not have it. So the parser stops
    /// guessing: it keeps what it recovered and reports what it saw. Dropping a probably-complete
    /// file is a worse error than keeping a possibly-incomplete one, because the checker can judge
    /// a file that exists and can do nothing with one that was deleted.
    pub unterminated: Vec<String>,
    /// Text before the first marker — a preamble the model was told not to write. Kept as evidence
    /// rather than discarded silently, because "it ignored the format" is worth being able to see.
    pub preamble: String,
}

/// Parse a delimited build stream: a line `=== FILE: <path>`, then that file's bytes, until the next
/// marker or the end.
///
/// DELIBERATELY NOT JSON. `extract_html_arg` exists in this codebase because one `publish_page` call
/// that overflowed its token budget produced unparseable JSON, and a set of files is many times
/// larger. This format needs no escaping, so a file's contents cannot break it, and a stream cut off
/// mid-file loses exactly that file instead of the whole deliverable.
///
/// The parser NEVER decides whether the last file is finished. The terminator is the next file's
/// marker, so the final file has none by definition, and a missing trailing newline is equally
/// consistent with a cut-off generation and with a model that ended without one. It is reported as
/// `unterminated` and kept; only the API's `finish_reason` could settle it, and this function does
/// not have it.
/// Strip ONE wrapping markdown fence from a file's contents.
///
/// `publish_page` already carries this lesson and states it plainly: *"A model asked for 'only the
/// HTML' still wraps it in a ```html fence about half the time. Refusing that would fail the chain
/// on a formatting habit, so unwrap it here — the alternative is a prompt that has to win every
/// time."* The file-set lane never got it, and the prompt lost: a completion pass emitted a
/// perfectly good `main.py` wrapped in ```python, the fence was written verbatim, the file did not
/// parse, and the site never came up — 2/11 on a leg whose content was actually correct.
///
/// Deliberately narrow. It strips only when the FIRST non-empty line opens a fence and the LAST
/// non-empty line is a bare closing fence, so a Markdown file that legitimately CONTAINS fenced
/// blocks keeps every one of them: such a file does not both begin and end with one.
fn unfence(content: &str) -> &str {
    let trimmed = content.trim_matches('\n');
    let mut lines = trimmed.lines();
    let (Some(first), Some(last)) = (lines.next(), trimmed.lines().next_back()) else {
        return content;
    };
    if !first.trim_start().starts_with("```") || last.trim() != "```" {
        return content;
    }
    // A fence that opens and closes on one line is not a wrapper.
    if trimmed.lines().count() < 2 {
        return content;
    }
    let start = trimmed.find('\n').map(|i| i + 1).unwrap_or(0);
    let end = trimmed.rfind("```").unwrap_or(trimmed.len());
    trimmed[start..end].trim_end_matches('\n')
}

pub fn parse_file_stream(text: &str) -> ParsedSet {
    let mut entries: Vec<FileEntry> = Vec::new();
    let mut unterminated: Vec<String> = Vec::new();
    let mut preamble = String::new();
    let mut current: Option<(String, String)> = None;
    let mut saw_marker = false;
    let lines: Vec<&str> = text.split_inclusive(char::from(10)).collect();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix(FILE_MARKER) {
            // A new marker ends the previous file, which is therefore complete.
            if let Some((path, body)) = current.take() {
                entries.push(FileEntry {
                    path,
                    content: unfence(&body).to_string(),
                });
            }
            saw_marker = true;
            let path = rest.trim().trim_matches('`').trim().to_string();
            current = Some((path, String::new()));
            continue;
        }
        match current.as_mut() {
            Some((path, body)) => {
                let last = i + 1 == lines.len();
                // A stream that does not end with a newline is NOTED and KEPT. It may be a
                // generation cut off mid-file, or a model that simply ended without one — the text
                // cannot tell them apart, and this used to guess "truncated" and delete the file.
                if last && !line.ends_with(char::from(10)) {
                    unterminated.push(path.clone());
                }
                body.push_str(line);
            }
            None if !saw_marker => preamble.push_str(line),
            None => {}
        }
    }
    if let Some((path, body)) = current.take() {
        // THE FINAL FILE IS UNFENCED TOO. It is built here rather than in the loop above, and
        // patching only the loop left the last file in a stream fenced — which is exactly the case
        // that motivated the fix: a completion pass emitting ONE file makes that file the final
        // one. Found by a mutant that survived because the assertion could not reach this path.
        entries.push(FileEntry {
            path,
            content: unfence(&body).to_string(),
        });
    }
    ParsedSet {
        entries,
        unterminated,
        preamble,
    }
}

