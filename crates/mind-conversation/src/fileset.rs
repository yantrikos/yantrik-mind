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
pub fn plan_file_set(entries: &[FileEntry]) -> Result<Vec<FileEntry>, FileSetRefusal> {
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
        if planned.iter().any(|p| p.path == path) {
            return Err(FileSetRefusal::DuplicatePath { path });
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
