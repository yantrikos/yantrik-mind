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
