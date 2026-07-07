//! code — the mind reads the owner's actual repositories. Shallow-clones (or pulls) registered
//! repos into a local workdir on the box and extracts a high-signal DIGEST (README, doc/spec files,
//! recent commit log, top-level structure) that grounds WorkOps scans in the *current* code rather
//! than a stale web snapshot. Read-only in spirit: only `clone`/`fetch`/`reset --hard`/`log`, never
//! a push. Private repos clone via the box's GitHub token, injected into the URL and never logged.

use std::path::{Path, PathBuf};
use std::process::Command;

fn workdir() -> PathBuf {
    PathBuf::from(std::env::var("YM_CODE_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind/repos".into()))
}

/// Short repo name from a git URL: github.com/owner/name(.git) → "name".
pub fn repo_name(git_url: &str) -> String {
    git_url
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or("repo")
        .to_string()
}

/// Build the auth'd URL for a private GitHub clone — token injected, never returned for logging.
fn authed_url(git_url: &str) -> String {
    let token = std::env::var("YM_GITHUB_TOKEN")
        .or_else(|_| std::env::var("YANTRIKDB_ACC_GIT_TOKEN"))
        .unwrap_or_default();
    if token.is_empty() || !git_url.starts_with("https://github.com/") {
        return git_url.to_string();
    }
    git_url.replacen("https://", &format!("https://x-access-token:{token}@"), 1)
}

fn run_git(dir: Option<&Path>, args: &[&str]) -> anyhow::Result<String> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.current_dir(d);
    }
    c.args(args);
    let out = c.output()?;
    if !out.status.success() {
        // Never surface the auth'd URL in an error — strip any token-bearing string.
        let err = String::from_utf8_lossy(&out.stderr);
        let safe: String = err.split_whitespace().filter(|w| !w.contains("x-access-token")).collect::<Vec<_>>().join(" ");
        anyhow::bail!("git {} failed: {}", args.first().copied().unwrap_or(""), safe.chars().take(200).collect::<String>());
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Clone (shallow, depth 60) or fast-forward an already-cloned repo. Returns the local path.
/// Blocking (git subprocess); call via spawn_blocking.
pub fn sync_repo(git_url: &str) -> anyhow::Result<PathBuf> {
    let dir = workdir();
    std::fs::create_dir_all(&dir)?;
    let name = repo_name(git_url);
    let dest = dir.join(&name);
    if dest.join(".git").exists() {
        // refresh — reset any local drift, fetch, hard-reset to the remote default branch
        let _ = run_git(Some(&dest), &["remote", "set-url", "origin", &authed_url(git_url)]);
        run_git(Some(&dest), &["fetch", "--depth", "60", "origin"])?;
        // detect the default branch (origin/HEAD), fall back to main/master
        let head = run_git(Some(&dest), &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
            .unwrap_or_default();
        let branch = head.trim().strip_prefix("origin/").unwrap_or("main").to_string();
        let target = format!("origin/{}", if branch.is_empty() { "main" } else { &branch });
        run_git(Some(&dest), &["reset", "--hard", &target])?;
        // scrub the token back out of the stored remote
        let _ = run_git(Some(&dest), &["remote", "set-url", "origin", git_url]);
    } else {
        run_git(None, &["clone", "--depth", "60", &authed_url(git_url), dest.to_string_lossy().as_ref()])?;
        let _ = run_git(Some(&dest), &["remote", "set-url", "origin", git_url]);
    }
    Ok(dest)
}

fn read_head(path: &Path, max: usize) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.chars().take(max).collect())
        .unwrap_or_default()
}

/// A high-signal grounding digest for one already-synced repo: README + doc/spec filenames +
/// recent commit subjects + top-level layout. Kept compact so it fits a research prompt.
pub fn digest(repo_path: &Path) -> String {
    let name = repo_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let mut out = format!("REPO {name}\n");

    // README (any case/extension)
    if let Ok(entries) = std::fs::read_dir(repo_path) {
        if let Some(readme) = entries
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.file_name().map(|n| n.to_string_lossy().to_lowercase().starts_with("readme")).unwrap_or(false))
        {
            out.push_str("\n--- README (excerpt) ---\n");
            out.push_str(&read_head(&readme, 2400));
            out.push('\n');
        }
    }

    // doc/spec files (names only — the model asks for specifics if it needs them)
    let mut docs: Vec<String> = Vec::new();
    for sub in ["docs", "spec", "specs", "doc"] {
        let d = repo_path.join(sub);
        if let Ok(entries) = std::fs::read_dir(&d) {
            for e in entries.filter_map(|e| e.ok()).take(20) {
                docs.push(format!("{sub}/{}", e.file_name().to_string_lossy()));
            }
        }
    }
    if !docs.is_empty() {
        out.push_str(&format!("\n--- docs ---\n{}\n", docs.join(", ")));
    }

    // recent commits — "what changed lately"
    if let Ok(log) = run_git(Some(repo_path), &["log", "--oneline", "-15", "--no-decorate"]) {
        if !log.trim().is_empty() {
            out.push_str(&format!("\n--- recent commits ---\n{}", log.trim()));
        }
    }
    out.chars().take(4000).collect()
}

/// One-call convenience: sync then digest. `since` = commit subjects in the last N days (for the
/// "what did I change this week" answer). Blocking; call via spawn_blocking.
pub fn sync_and_digest(git_url: &str) -> anyhow::Result<String> {
    let path = sync_repo(git_url)?;
    Ok(digest(&path))
}

/// Recent-commits-only view for a synced repo (name-matched), for a quick "what changed" answer.
pub fn recent_commits(name: &str, days: u32) -> Option<String> {
    let dest = workdir().join(name);
    if !dest.join(".git").exists() {
        return None;
    }
    run_git(Some(&dest), &["log", &format!("--since={days}.days"), "--oneline", "--no-decorate", "-30"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// STUDY reader: walk a synced repo and collect the highest-signal source for comprehension —
/// a chunked bundle of {path + head-of-file} for the most important code/config/doc files, so an
/// LLM can distill an architecture understanding ONCE. Skips vendored/build/binary noise; caps total
/// size so it fits a study prompt. Returns (file_count, bundle). Blocking; call via spawn_blocking.
pub fn study_bundle(git_url: &str, max_files: usize, budget_bytes: usize) -> anyhow::Result<(usize, String)> {
    let root = sync_repo(git_url)?;
    let name = repo_name(git_url);
    let skip_dir = |n: &str| {
        matches!(
            n,
            ".git" | "node_modules" | "target" | "dist" | "build" | ".next" | "vendor"
                | "__pycache__" | ".venv" | "venv" | ".astro" | "coverage" | "out"
        )
    };
    let want_ext = |n: &str| {
        let n = n.to_lowercase();
        [".rs", ".py", ".ts", ".tsx", ".js", ".go", ".java", ".c", ".cpp", ".h", ".rb",
         ".toml", ".json", ".yaml", ".yml", ".md", ".mdx", ".sql", ".proto", ".tex", ".sh"]
            .iter()
            .any(|e| n.ends_with(e))
    };
    // collect candidate files (bounded walk)
    let mut files: Vec<(std::path::PathBuf, u64)> = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let fname = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !skip_dir(&fname) {
                    stack.push(p);
                }
            } else if want_ext(&fname) {
                let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                if sz > 0 && sz < 400_000 {
                    files.push((p, sz));
                }
            }
        }
        if files.len() > 4000 {
            break;
        }
    }
    // rank: entry/config/spec first, then by a "central-ish" heuristic (shallow path, meaty file)
    let score = |p: &std::path::Path| -> i64 {
        let s = p.to_string_lossy().to_lowercase();
        let base = p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let mut v = 0i64;
        if base.starts_with("readme") { v -= 100; }
        if base == "cargo.toml" || base == "package.json" || base == "pyproject.toml" || base == "go.mod" { v -= 90; }
        if base == "lib.rs" || base == "main.rs" || base == "index.ts" || base == "__init__.py" || base == "mod.rs" { v -= 70; }
        if s.contains("/spec") || s.contains("/docs") || base.ends_with(".md") { v -= 40; }
        v += s.matches('/').count() as i64 * 5; // shallower = more central
        v
    };
    files.sort_by_key(|(p, _)| score(p));
    files.truncate(max_files.max(1));

    let mut bundle = format!("REPOSITORY: {name}\n(the code to study — distill an architecture understanding from this)\n");
    let mut used = 0usize;
    let mut n = 0usize;
    for (p, _) in &files {
        if used >= budget_bytes {
            break;
        }
        let rel = p.strip_prefix(&root).unwrap_or(p).to_string_lossy().to_string();
        let Ok(txt) = std::fs::read_to_string(p) else { continue };
        // head of file — signatures/structure carry most of the signal
        let head: String = txt.chars().take(2000).collect();
        let chunk = format!("\n===== {rel} =====\n{head}\n");
        used += chunk.len();
        bundle.push_str(&chunk);
        n += 1;
    }
    Ok((n, bundle.chars().take(budget_bytes + 4000).collect()))
}

/// A study MODULE: a logical unit (a crate under crates/, a top-level source dir, or the root) with
/// its concatenated source, chunked so each piece fits one deep-read LLM pass. Real full contents,
/// not file heads — the point is to actually read the functions.
#[derive(Debug, Clone)]
pub struct StudyModule {
    pub name: String,       // e.g. "mind-memory" or "src"
    pub file_count: usize,
    pub chunks: Vec<String>, // each ~<budget> bytes of "===== path =====\n<full source>"
}

fn is_source(name: &str) -> bool {
    let n = name.to_lowercase();
    [".rs", ".py", ".ts", ".tsx", ".js", ".go", ".java", ".c", ".cpp", ".h", ".rb",
     ".sql", ".proto", ".sh", ".toml", ".yaml", ".yml"]
        .iter()
        .any(|e| n.ends_with(e))
}

fn skip_dir(n: &str) -> bool {
    matches!(
        n,
        ".git" | "node_modules" | "target" | "dist" | "build" | ".next" | "vendor"
            | "__pycache__" | ".venv" | "venv" | ".astro" | "coverage" | "out" | ".turbo"
    )
}

/// Collect source files under a dir (recursive, bounded), returned as (relative-path, full-text).
fn collect_sources(base: &Path, root: &Path, per_file_cap: usize, max_files: usize) -> Vec<(String, String)> {
    let mut files: Vec<(String, String)> = Vec::new();
    let mut stack = vec![base.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            let fname = e.file_name().to_string_lossy().to_string();
            if p.is_dir() {
                if !skip_dir(&fname) {
                    stack.push(p);
                }
            } else if is_source(&fname) {
                let sz = e.metadata().map(|m| m.len()).unwrap_or(0);
                if sz == 0 || sz > 600_000 {
                    continue;
                }
                if let Ok(txt) = std::fs::read_to_string(&p) {
                    let rel = p.strip_prefix(root).unwrap_or(&p).to_string_lossy().to_string();
                    let body: String = txt.chars().take(per_file_cap).collect();
                    files.push((rel, body));
                }
            }
            if files.len() >= max_files {
                return files;
            }
        }
    }
    files
}

/// Chunk (path, full-text) pairs into <=budget-byte study chunks (keeps whole files together when
/// they fit; splits a giant file across chunks with a continuation marker).
fn chunk_files(files: &[(String, String)], budget: usize) -> Vec<String> {
    let mut chunks: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (rel, body) in files {
        let header = format!("\n===== {rel} =====\n");
        if body.len() + header.len() > budget {
            // giant file — flush current, then split it
            if !cur.is_empty() {
                chunks.push(std::mem::take(&mut cur));
            }
            let mut off = 0;
            let chars: Vec<char> = body.chars().collect();
            let mut part = 0;
            while off < chars.len() {
                let end = (off + budget).min(chars.len());
                let piece: String = chars[off..end].iter().collect();
                chunks.push(format!("===== {rel} (part {part}) =====\n{piece}"));
                off = end;
                part += 1;
            }
            continue;
        }
        if cur.len() + header.len() + body.len() > budget && !cur.is_empty() {
            chunks.push(std::mem::take(&mut cur));
        }
        cur.push_str(&header);
        cur.push_str(body);
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// Build the study module map for a synced repo. A Cargo/pnpm-style `crates/`, `packages/`, or
/// `apps/` workspace → one module per member; otherwise group by top-level source dir; the root
/// files are their own module. Each module's source is chunked to `chunk_bytes`.
pub fn study_modules(git_url: &str, chunk_bytes: usize) -> anyhow::Result<(String, Vec<StudyModule>)> {
    let root = sync_repo(git_url)?;
    let name = repo_name(git_url);
    let mut modules: Vec<StudyModule> = Vec::new();

    let workspace_dirs = ["crates", "packages", "apps", "services", "libs"];
    let mut grouped = false;
    for wd in workspace_dirs {
        let wpath = root.join(wd);
        if !wpath.is_dir() {
            continue;
        }
        if let Ok(rd) = std::fs::read_dir(&wpath) {
            for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
                let mname = e.file_name().to_string_lossy().to_string();
                if skip_dir(&mname) {
                    continue;
                }
                let files = collect_sources(&e.path(), &root, 12000, 120);
                if !files.is_empty() {
                    let fc = files.len();
                    modules.push(StudyModule { name: mname, file_count: fc, chunks: chunk_files(&files, chunk_bytes) });
                    grouped = true;
                }
            }
        }
    }

    if !grouped {
        // No workspace layout: group by top-level source directory.
        if let Ok(rd) = std::fs::read_dir(&root) {
            for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
                let dname = e.file_name().to_string_lossy().to_string();
                if skip_dir(&dname) {
                    continue;
                }
                let files = collect_sources(&e.path(), &root, 12000, 120);
                if !files.is_empty() {
                    let fc = files.len();
                    modules.push(StudyModule { name: dname, file_count: fc, chunks: chunk_files(&files, chunk_bytes) });
                }
            }
        }
    }

    // Root-level files (README, top configs, root src files) as their own module.
    let mut root_files: Vec<(String, String)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&root) {
        for e in rd.filter_map(|e| e.ok()).filter(|e| e.path().is_file()) {
            let fname = e.file_name().to_string_lossy().to_string();
            if is_source(&fname) || fname.to_lowercase().starts_with("readme") {
                if let Ok(txt) = std::fs::read_to_string(e.path()) {
                    root_files.push((fname, txt.chars().take(12000).collect()));
                }
            }
        }
    }
    if !root_files.is_empty() {
        let fc = root_files.len();
        modules.insert(0, StudyModule { name: "(root)".into(), file_count: fc, chunks: chunk_files(&root_files, chunk_bytes) });
    }

    Ok((name, modules))
}
