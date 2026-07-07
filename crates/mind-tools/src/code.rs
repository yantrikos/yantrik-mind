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
    [".rs", ".py", ".ts", ".tsx", ".js", ".jsx", ".mjs", ".go", ".java", ".kt", ".kts",
     ".swift", ".cs", ".php", ".rb", ".ex", ".exs", ".erl", ".hs", ".lua", ".pl", ".pm",
     ".r", ".jl", ".zig", ".dart", ".scala", ".clj", ".cljs", ".ml", ".mli", ".fs",
     ".groovy", ".vue", ".svelte", ".c", ".cc", ".cxx", ".cpp", ".h", ".hh", ".hpp",
     ".m", ".mm", ".nim", ".cr", ".d", ".sql", ".proto", ".sh", ".ps1", ".toml",
     ".yaml", ".yml", ".gradle"]
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
                let files = collect_sources(&e.path(), &root, 200_000, 160);
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
                let files = collect_sources(&e.path(), &root, 200_000, 160);
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
                    root_files.push((fname, txt.chars().take(200_000).collect()));
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


/// Deterministic study output: structural facts parsed straight from source (no LLM), plus a
/// compact per-module skeleton to feed a single interpretive synthesis pass.
#[derive(Debug, Clone, Default)]
pub struct DetStudy {
    pub facts: Vec<(String, String)>, // (module, fact) — tag with [det] at save time
    pub skeleton: String,             // "## module\ndoc\nfns…\ntypes…\ndeps…" for the synth prompt
    pub module_count: usize,
    pub file_count: usize,
}

fn ident_after<'a>(line: &'a str, kw: &str) -> Option<&'a str> {
    let l = line.trim_start();
    let rest = l.strip_prefix(kw)?;
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !(c.is_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let id = &rest[..end];
    if id.is_empty() { None } else { Some(id) }
}

/// Strip leading visibility/qualifier keywords so `public static final class Foo` matches `class Foo`.
fn strip_modifiers(mut l: &str) -> &str {
    const MODS: [&str; 22] = [
        "pub ", "public ", "private ", "protected ", "internal ", "static ", "final ",
        "abstract ", "sealed ", "export ", "default ", "async ", "unsafe ", "extern ",
        "virtual ", "override ", "open ", "inline ", "const ", "partial ", "data ", "case ",
    ];
    loop {
        let before = l;
        for m in MODS {
            if let Some(rest) = l.strip_prefix(m) {
                l = rest.trim_start();
            }
        }
        if l == before {
            return l;
        }
    }
}

/// Language-agnostic definition/dependency extraction for files with no precise fast-path.
/// Keyword tables cover ~25 languages; a C-style `Type name(args) …{` heuristic catches
/// keyword-less definitions (C functions, Java methods). Deterministic, never invents.
fn universal_symbols(body: &str) -> (Vec<String>, Vec<String>, Vec<String>, Option<String>, Vec<String>) {
    const FN_KWS: [&str; 10] = ["fn ", "func ", "function ", "def ", "defp ", "fun ", "sub ", "proc ", "method ", "task "];
    const TYPE_KWS: [&str; 8] = ["class ", "struct ", "enum ", "record ", "union ", "object ", "module ", "type "];
    const TRAIT_KWS: [&str; 4] = ["trait ", "interface ", "protocol ", "typeclass "];
    const CTRL: [&str; 12] = ["if", "for", "while", "switch", "return", "else", "catch", "do", "match", "when", "case", "try"];
    let mut fns = Vec::new();
    let mut types = Vec::new();
    let mut traits = Vec::new();
    let mut doc: Option<String> = None;
    let mut deps: Vec<String> = Vec::new();
    for (li, raw) in body.lines().enumerate() {
        let t = raw.trim_start();
        // leading doc: first comment line with substance near the top of the file
        if doc.is_none() && li < 12 {
            for lead in ["//!", "///", "//", "#", "/*", "*", "--", ";;", "%", "\"\"\""] {
                if let Some(d) = t.strip_prefix(lead) {
                    let d = d.trim_start_matches(['*', '!', '/', '-']).trim();
                    if d.len() > 8 && !d.starts_with('=') {
                        doc = Some(d.to_string());
                    }
                    break;
                }
            }
        }
        // dependencies
        for kw in ["import ", "use ", "require ", "require(", "include ", "#include ", "from ", "using ", "open "] {
            if let Some(rest) = t.strip_prefix(kw) {
                let rest = rest.trim_start_matches(['<', '"', '\'', '(']);
                let seg: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if seg.len() > 2 {
                    deps.push(seg);
                }
                break;
            }
        }
        let stripped = strip_modifiers(t);
        let mut matched = false;
        for kw in TRAIT_KWS {
            if let Some(id) = ident_after(stripped, kw) {
                traits.push(id.to_string());
                matched = true;
                break;
            }
        }
        if !matched {
            for kw in TYPE_KWS {
                if let Some(id) = ident_after(stripped, kw) {
                    types.push(id.to_string());
                    matched = true;
                    break;
                }
            }
        }
        if !matched {
            for kw in FN_KWS {
                if let Some(id) = ident_after(stripped, kw) {
                    fns.push(id.to_string());
                    matched = true;
                    break;
                }
            }
        }
        // C-style keyword-less definition: `ReturnType name(args…` at shallow indent, `{`-terminated
        // region, first word not a control keyword.
        if !matched && raw.len() - t.len() <= 4 && (t.ends_with('{') || t.ends_with('(') || t.ends_with(')')) {
            let words: Vec<&str> = stripped.split_whitespace().collect();
            if words.len() >= 2 && !CTRL.contains(&words[0].trim_end_matches('*')) {
                if let Some(p) = stripped.find('(') {
                    let before = &stripped[..p];
                    if let Some(name) = before.rsplit(|c: char| !(c.is_alphanumeric() || c == '_')).next() {
                        if name.len() > 2
                            && name.chars().next().map(|c| c.is_alphabetic() || c == '_').unwrap_or(false)
                            && before.trim_end().len() > name.len()
                        {
                            fns.push(name.to_string());
                        }
                    }
                }
            }
        }
    }
    (fns, types, traits, doc, deps)
}

/// Parse one source file for its public surface. Returns (fns, types, traits, doc_line, dep_tokens).
fn parse_symbols(rel: &str, body: &str) -> (Vec<String>, Vec<String>, Vec<String>, Option<String>, Vec<String>) {
    let mut fns = Vec::new();
    let mut types = Vec::new();
    let mut traits = Vec::new();
    let mut doc: Option<String> = None;
    let mut deps: Vec<String> = Vec::new();
    let rust = rel.ends_with(".rs");
    let py = rel.ends_with(".py");
    let ts = rel.ends_with(".ts") || rel.ends_with(".tsx") || rel.ends_with(".js");
    let go = rel.ends_with(".go");
    if !(rust || py || ts || go) {
        // No precise fast-path for this language — the universal extractor covers it.
        return universal_symbols(body);
    }
    for line in body.lines() {
        let t = line.trim_start();
        if rust {
            if doc.is_none() {
                if let Some(d) = t.strip_prefix("//!") {
                    let d = d.trim();
                    if d.len() > 8 { doc = Some(d.to_string()); }
                }
            }
            for kw in ["pub fn ", "pub async fn ", "pub(crate) fn ", "pub(crate) async fn "] {
                if let Some(id) = ident_after(t, kw) { fns.push(id.to_string()); break; }
            }
            if let Some(id) = ident_after(t, "pub struct ") { types.push(id.to_string()); }
            if let Some(id) = ident_after(t, "pub enum ") { types.push(id.to_string()); }
            if let Some(id) = ident_after(t, "pub trait ") { traits.push(id.to_string()); }
            if let Some(rest) = t.strip_prefix("use crate::").or_else(|| t.strip_prefix("use ")) {
                let seg: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if seg.len() > 2 { deps.push(seg); }
            }
        } else if py {
            if let Some(id) = ident_after(t, "def ") { if !line.starts_with(' ') { fns.push(id.to_string()); } }
            if let Some(id) = ident_after(t, "class ") { types.push(id.to_string()); }
            if let Some(rest) = t.strip_prefix("from ") {
                let seg: String = rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                if seg.len() > 2 { deps.push(seg); }
            }
        } else if ts {
            for kw in ["export function ", "export async function ", "export default function "] {
                if let Some(id) = ident_after(t, kw) { fns.push(id.to_string()); break; }
            }
            if let Some(id) = ident_after(t, "export class ") { types.push(id.to_string()); }
            if let Some(id) = ident_after(t, "export interface ") { types.push(id.to_string()); }
            if let Some(id) = ident_after(t, "export type ") { types.push(id.to_string()); }
        } else if go {
            if let Some(id) = ident_after(t, "func ") {
                if id.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) { fns.push(id.to_string()); }
            }
            if let Some(id) = ident_after(t, "type ") {
                if id.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) { types.push(id.to_string()); }
            }
        }
    }
    (fns, types, traits, doc, deps)
}

fn dedup_keep_order(v: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    v.into_iter().filter(|x| seen.insert(x.clone())).collect()
}

/// DETERMINISTIC study: clone + parse. Produces structural facts (public API, types, module docs,
/// internal deps) with ZERO LLM calls, plus a skeleton for one interpretive synthesis pass.
pub fn deterministic_study(git_url: &str) -> anyhow::Result<(String, DetStudy)> {
    let (name, modules) = study_modules(git_url, 40000)?;
    let mod_names: std::collections::HashSet<String> =
        modules.iter().map(|m| m.name.to_lowercase()).collect();
    let mut det = DetStudy::default();
    det.module_count = modules.len();
    let mut skeleton = format!("# {name} — parsed structure ({} modules)\n", modules.len());

    // LANGUAGE CENSUS + CONCEPT MINING — fully language-agnostic: extension counts, and identifier
    // frequency across files (the identifiers threading the most files ARE the domain concepts).
    let mut ext_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut ident_files: std::collections::HashMap<String, std::collections::HashSet<u64>> =
        std::collections::HashMap::new();
    let mut file_id: u64 = 0;
    const STOP: [&str; 34] = [
        "return", "public", "private", "static", "string", "import", "package", "class",
        "struct", "function", "const", "async", "await", "self", "this", "true", "false",
        "None", "null", "void", "match", "while", "break", "continue", "export", "default",
        "extends", "implements", "interface", "println", "printf", "include", "define", "endif",
    ];
    for m in &modules {
        for chunk in &m.chunks {
            for line in chunk.lines() {
                if let Some(h) = line.strip_prefix("===== ") {
                    file_id += 1;
                    let p = h.trim_end_matches(" =====").trim();
                    if let Some(ext) = p.rsplit('.').next().filter(|e| e.len() <= 6 && !e.contains('/')) {
                        *ext_counts.entry(ext.to_lowercase()).or_default() += 1;
                    }
                    continue;
                }
                let mut cur = String::new();
                for c in line.chars().chain(std::iter::once(' ')) {
                    if c.is_alphanumeric() || c == '_' {
                        cur.push(c);
                    } else if !cur.is_empty() {
                        if cur.len() >= 5
                            && cur.chars().next().map(|c| c.is_alphabetic()).unwrap_or(false)
                            && !STOP.contains(&cur.to_lowercase().as_str())
                        {
                            ident_files.entry(std::mem::take(&mut cur)).or_default().insert(file_id);
                        } else {
                            cur.clear();
                        }
                    }
                }
            }
        }
    }
    if !ext_counts.is_empty() {
        let mut census: Vec<(String, usize)> = ext_counts.into_iter().collect();
        census.sort_by(|a, b| b.1.cmp(&a.1));
        let line = census.iter().take(6).map(|(e, n)| format!(".{e} ({n} files)")).collect::<Vec<_>>().join(", ");
        det.facts.push(("(root)".into(), format!("The `{name}` codebase is composed of: {line}.")));
        skeleton.push_str(&format!("languages: {line}\n"));
    }
    {
        let min_spread = 3usize.max(file_id as usize / 12);
        let mut concepts: Vec<(String, usize)> = ident_files
            .into_iter()
            .map(|(k, v)| (k, v.len()))
            .filter(|(_, n)| *n >= min_spread)
            .collect();
        concepts.sort_by(|a, b| b.1.cmp(&a.1));
        if !concepts.is_empty() {
            let line = concepts.iter().take(18).map(|(k, n)| format!("{k} ({n} files)")).collect::<Vec<_>>().join(", ");
            det.facts.push(("(root)".into(), format!(
                "Core identifiers threading the `{name}` codebase (by cross-file spread): {line}."
            )));
            skeleton.push_str(&format!("core concepts: {line}\n"));
        }
    }

    // def name → defining module; ambiguous names (defined in 2+ modules) are dropped from the
    // call-graph scan rather than guessed.
    let mut defs: std::collections::HashMap<String, Option<String>> = std::collections::HashMap::new();

    for m in &modules {
        det.file_count += m.file_count;
        let mut fns = Vec::new();
        let mut types = Vec::new();
        let mut traits = Vec::new();
        let mut docs: Vec<String> = Vec::new();
        let mut deps: Vec<String> = Vec::new();
        for chunk in &m.chunks {
            let mut cur_rel = String::new();
            let mut cur_body = String::new();
            for line in chunk.lines() {
                if let Some(h) = line.strip_prefix("===== ") {
                    if !cur_rel.is_empty() && !cur_body.is_empty() {
                        let (f, ty, tr, doc, dp) = parse_symbols(&cur_rel, &cur_body);
                        fns.extend(f); types.extend(ty); traits.extend(tr);
                        if let Some(d) = doc { docs.push(d); }
                        deps.extend(dp);
                    }
                    cur_rel = h.trim_end_matches(" =====").trim().to_string();
                    cur_body.clear();
                } else {
                    cur_body.push_str(line);
                    cur_body.push('\n');
                }
            }
            if !cur_rel.is_empty() && !cur_body.is_empty() {
                let (f, ty, tr, doc, dp) = parse_symbols(&cur_rel, &cur_body);
                fns.extend(f); types.extend(ty); traits.extend(tr);
                if let Some(d) = doc { docs.push(d); }
                deps.extend(dp);
            }
        }
        let fns = dedup_keep_order(fns);
        let types = dedup_keep_order(types);
        let traits = dedup_keep_order(traits);
        let deps: Vec<String> = dedup_keep_order(deps)
            .into_iter()
            .filter(|d| mod_names.contains(&d.to_lowercase()) && d.to_lowercase() != m.name.to_lowercase())
            .collect();

        if let Some(doc) = docs.first() {
            det.facts.push((m.name.clone(), format!("Module `{}` is documented as: {}", m.name, doc)));
        }
        if !traits.is_empty() {
            det.facts.push((m.name.clone(), format!(
                "Module `{}` defines traits: {}.", m.name, traits.join(", "))));
        }
        if !types.is_empty() {
            let shown: Vec<_> = types.iter().take(24).cloned().collect();
            det.facts.push((m.name.clone(), format!(
                "Module `{}` defines {} public type(s): {}{}.",
                m.name, types.len(), shown.join(", "),
                if types.len() > shown.len() { ", …" } else { "" })));
        }
        if !fns.is_empty() {
            let shown: Vec<_> = fns.iter().take(30).cloned().collect();
            det.facts.push((m.name.clone(), format!(
                "Module `{}` exposes {} public function(s): {}{}.",
                m.name, fns.len(), shown.join(", "),
                if fns.len() > shown.len() { ", …" } else { "" })));
        }
        if !deps.is_empty() {
            det.facts.push((m.name.clone(), format!(
                "Module `{}` depends internally on: {}.", m.name, deps.join(", "))));
        }

        for sym in fns.iter().chain(types.iter()).chain(traits.iter()) {
            if sym.len() >= 5 {
                defs.entry(sym.clone())
                    .and_modify(|e| {
                        if e.as_deref() != Some(m.name.as_str()) {
                            *e = None; // ambiguous — defined in more than one module
                        }
                    })
                    .or_insert_with(|| Some(m.name.clone()));
            }
        }

        skeleton.push_str(&format!("\n## {} ({} files)\n", m.name, m.file_count));
        if let Some(doc) = docs.first() { skeleton.push_str(&format!("doc: {doc}\n")); }
        if !traits.is_empty() { skeleton.push_str(&format!("traits: {}\n", traits.join(", "))); }
        if !types.is_empty() {
            let shown: Vec<_> = types.iter().take(30).cloned().collect();
            skeleton.push_str(&format!("types: {}\n", shown.join(", ")));
        }
        if !fns.is_empty() {
            let shown: Vec<_> = fns.iter().take(40).cloned().collect();
            skeleton.push_str(&format!("fns: {}\n", shown.join(", ")));
        }
        if !deps.is_empty() { skeleton.push_str(&format!("deps: {}\n", deps.join(", "))); }
    }

    // CALL-GRAPH SCAN (deterministic): count references to each unambiguous def from every OTHER
    // module. Yields "A calls into B via x, y, z" edges and a repo-wide most-referenced list —
    // the relational layer that lets code_ask reason about impact and flow, not just inventory.
    {
        let mut edge: std::collections::HashMap<(String, String), std::collections::HashMap<String, usize>> =
            std::collections::HashMap::new();
        let mut hot: std::collections::HashMap<String, (usize, std::collections::HashSet<String>)> =
            std::collections::HashMap::new();
        for m in &modules {
            for chunk in &m.chunks {
                for line in chunk.lines() {
                    if line.starts_with("===== ") {
                        continue;
                    }
                    let mut cur = String::new();
                    for c in line.chars().chain(std::iter::once(' ')) {
                        if c.is_alphanumeric() || c == '_' {
                            cur.push(c);
                        } else if !cur.is_empty() {
                            let ident = std::mem::take(&mut cur);
                            // ubiquitous method names give false "hot symbol" signal: a bare-ident
                            // scan can't tell MyType::is_empty from Vec::is_empty.
                            const HOT_STOP: [&str; 20] = [
                                "default", "is_empty", "from_str", "to_string", "clone", "deref",
                                "as_str", "as_ref", "unwrap_or", "to_owned", "into_iter", "chars",
                                "lines", "trim", "split", "collect", "filter", "insert", "remove", "plain",
                            ];
                            if ident.len() >= 5 && !HOT_STOP.contains(&ident.as_str()) {
                                if let Some(Some(home)) = defs.get(&ident) {
                                    if home != &m.name {
                                        *edge
                                            .entry((m.name.clone(), home.clone()))
                                            .or_default()
                                            .entry(ident.clone())
                                            .or_default() += 1;
                                        let h = hot.entry(ident).or_default();
                                        h.0 += 1;
                                        h.1.insert(m.name.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        let mut hot_v: Vec<(String, usize, usize)> =
            hot.into_iter().map(|(k, (n, mods))| (k, n, mods.len())).collect();
        hot_v.sort_by(|a, b| (b.2, b.1).cmp(&(a.2, a.1)));
        if !hot_v.is_empty() {
            let line = hot_v.iter().take(12)
                .map(|(k, n, mc)| format!("{k} ({n} refs from {mc} modules)"))
                .collect::<Vec<_>>().join(", ");
            det.facts.push(("(root)".into(), format!(
                "Most-referenced symbols across `{name}` module boundaries (change these carefully): {line}."
            )));
            skeleton.push_str(&format!("\nhot symbols: {line}\n"));
        }
        let mut edges: Vec<((String, String), std::collections::HashMap<String, usize>)> =
            edge.into_iter().collect();
        edges.sort_by_key(|((_, _), syms)| std::cmp::Reverse(syms.values().sum::<usize>()));
        for ((from, to), syms) in edges.into_iter().take(14) {
            let mut sv: Vec<(String, usize)> = syms.into_iter().collect();
            sv.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
            let names = sv.iter().take(6).map(|(k, _)| k.clone()).collect::<Vec<_>>().join(", ");
            det.facts.push((from.clone(), format!(
                "Module `{from}` calls into `{to}` mainly via: {names}."
            )));
            skeleton.push_str(&format!("edge: {from} -> {to}: {names}\n"));
        }
    }

    det.skeleton = skeleton;
    Ok((name, det))
}

/// Targeted definition lookup in an already-synced repo — the ACTIVE-LEARNING primitive. For each
/// identifier, grep source files for its definition-ish line and return that line plus context, so
/// code_ask can fill a knowledge gap with one focused excerpt instead of a re-study. Deterministic.
pub fn lookup_symbols(repo: &str, idents: &[String], ctx_lines: usize) -> Vec<(String, String)> {
    let root = workdir().join(repo);
    if !root.is_dir() || idents.is_empty() {
        return vec![];
    }
    let files = collect_sources(&root, &root, 200_000, 400);
    let mut out: Vec<(String, String)> = Vec::new();
    for ident in idents.iter().take(3) {
        let mut best: Option<(usize, String, usize, bool)> = None; // (score, file, line_idx, is_def)
        for (rel, body) in &files {
            for (li, line) in body.lines().enumerate() {
                if !line.contains(ident.as_str()) {
                    continue;
                }
                // whole-word check
                let ok = line
                    .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .any(|w| w == ident);
                if !ok {
                    continue;
                }
                let t = strip_modifiers(line.trim_start());
                let is_def = ["fn ", "func ", "function ", "def ", "class ", "struct ", "enum ",
                              "trait ", "interface ", "type ", "impl ", "module ", "protocol "]
                    .iter()
                    .any(|kw| ident_after(t, kw).map(|id| id == ident).unwrap_or(false))
                    || (t.contains('(') && (t.ends_with('{') || t.ends_with(')')) && t.contains(ident.as_str()));
                let score = if is_def { 2 } else { 1 };
                if best.as_ref().map(|(s, _, _, _)| score > *s).unwrap_or(true) {
                    best = Some((score, rel.clone(), li, is_def));
                    if is_def {
                        break;
                    }
                }
            }
            if best.as_ref().map(|(_, _, _, d)| *d).unwrap_or(false) {
                break;
            }
        }
        if let Some((_, rel, li, _)) = best {
            if let Some((_, body)) = files.iter().find(|(r, _)| r == &rel) {
                let lines: Vec<&str> = body.lines().collect();
                let end = (li + ctx_lines).min(lines.len());
                let excerpt = lines[li..end].join("\n");
                out.push((format!("{rel}:{}", li + 1), excerpt));
            }
        }
    }
    out
}

#[cfg(test)]
mod study_tests {
    use super::*;

    #[test]
    fn universal_extractor_reads_java() {
        let src = "// Handles order checkout and payment capture.\nimport java.util.List;\n\npublic final class CheckoutService {\n    public static PaymentResult capturePayment(Order order) {\n        return gateway.charge(order);\n    }\n}\npublic interface PaymentGateway {\n}\n";
        let (fns, types, traits, doc, deps) = parse_symbols("CheckoutService.java", src);
        assert!(types.contains(&"CheckoutService".to_string()), "types: {types:?}");
        assert!(traits.contains(&"PaymentGateway".to_string()), "traits: {traits:?}");
        assert!(fns.contains(&"capturePayment".to_string()), "fns: {fns:?}");
        assert!(doc.unwrap().contains("checkout"));
        assert!(deps.iter().any(|d| d == "java"), "deps: {deps:?}");
    }

    #[test]
    fn universal_extractor_reads_c() {
        let src = "/* editor row operations */\n#include <stdio.h>\n\nstruct erow {\n    int size;\n};\n\nvoid editorInsertRow(int at, char *s, size_t len) {\n    if (at < 0) return;\n}\n\nint editorRowCxToRx(erow *row, int cx) {\n    return 0;\n}\n";
        let (fns, types, _traits, doc, deps) = parse_symbols("kilo.c", src);
        assert!(types.contains(&"erow".to_string()), "types: {types:?}");
        assert!(fns.contains(&"editorInsertRow".to_string()), "fns: {fns:?}");
        assert!(fns.contains(&"editorRowCxToRx".to_string()), "fns: {fns:?}");
        assert!(doc.unwrap().contains("row operations"));
        assert!(deps.iter().any(|d| d == "stdio"), "deps: {deps:?}");
        // control-flow lines must NOT be misread as definitions
        assert!(!fns.iter().any(|f| f == "if" || f == "return"), "fns: {fns:?}");
    }

    #[test]
    fn lookup_symbols_finds_definitions() {
        let dir = std::env::temp_dir().join("ym_lookup_test_repo");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("rows.c"), "/* rows */
struct erow {
    int size;
    char *chars;
};

void editorInsertRow(int at) {
}
").unwrap();
        std::env::set_var("YM_CODE_DIR", std::env::temp_dir());
        let hits = lookup_symbols("ym_lookup_test_repo", &["erow".to_string()], 5);
        assert_eq!(hits.len(), 1, "hits: {hits:?}");
        assert!(hits[0].0.starts_with("rows.c:"), "loc: {}", hits[0].0);
        assert!(hits[0].1.contains("chars"), "excerpt: {}", hits[0].1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rust_fast_path_still_precise() {
        let src = "//! Belief store.\npub struct Belief { pub id: u64 }\npub fn assert_belief(b: Belief) {}\nfn private_helper() {}\n";
        let (fns, types, _tr, doc, _deps) = parse_symbols("lib.rs", src);
        assert!(types.contains(&"Belief".to_string()));
        assert!(fns.contains(&"assert_belief".to_string()));
        // Rust path stays pub-only precision: private items excluded
        assert!(!fns.contains(&"private_helper".to_string()));
        assert!(doc.unwrap().contains("Belief store"));
    }
}
