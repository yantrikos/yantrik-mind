//! sandbox — run untrusted code (shell / python / rust) WITHOUT compromising the host.
//!
//! No container runtime is needed: the box supports unprivileged user namespaces, so the sandbox is
//! `timeout` + `unshare` (user+net+pid+mount+uts+ipc ns) + `prlimit` (cpu/mem/procs/fsize/nofile).
//! Guarantees, verified on the box:
//!  - NO network (`--net` = empty net namespace, no interfaces) → can't exfiltrate or reach the LAN.
//!  - the mind's state dir is masked with a tmpfs (the DB is invisible); secrets file is root:600
//!    (unreadable by the service user anyway).
//!  - hard CPU/memory/process/file-size/fd limits + a wall-clock kill → no fork-bombs, runaways, or
//!    disk-fills. Non-root (mapped root inside the userns only).
//!  - code + inputs live in a throwaway scratch dir passed as FILES (never interpolated into a shell
//!    command line) → no shell-injection via the code itself.
//!
//! If user namespaces aren't available (e.g. local Windows dev), `available()` is false and callers
//! must refuse to run code — never fall back to unsandboxed execution.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone)]
pub struct Limits {
    pub wall_secs: u64,
    pub cpu_secs: u64,
    pub mem_bytes: u64,
    pub max_procs: u64,
    pub fsize_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            wall_secs: 15,
            cpu_secs: 10,
            mem_bytes: 512 * 1024 * 1024,
            max_procs: 64,
            fsize_bytes: 8 * 1024 * 1024,
        }
    }
}

impl Limits {
    /// Heavier limits for compiling Rust (rustc/LLVM need more memory, time, and output size).
    pub fn for_rust() -> Self {
        Self {
            wall_secs: 40,
            cpu_secs: 35,
            mem_bytes: 1536 * 1024 * 1024,
            max_procs: 128,
            fsize_bytes: 128 * 1024 * 1024,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    pub timed_out: bool,
}

impl ExecResult {
    /// A compact rendering for a chat reply (truncated).
    pub fn render(&self) -> String {
        let cap = |s: &str| -> String {
            let t = s.trim_end();
            if t.chars().count() > 4000 {
                format!("{}\n…(truncated)", t.chars().take(4000).collect::<String>())
            } else {
                t.to_string()
            }
        };
        let mut out = String::new();
        let o = cap(&self.stdout);
        if !o.is_empty() {
            out.push_str(&format!("```\n{o}\n```\n"));
        }
        let e = cap(&self.stderr);
        if !e.is_empty() {
            out.push_str(&format!("stderr:\n```\n{e}\n```\n"));
        }
        if self.timed_out {
            out.push_str("⏱ timed out (killed).\n");
        }
        if o.is_empty() && e.is_empty() && !self.timed_out {
            out.push_str("(no output)\n");
        }
        out.push_str(&format!("exit code: {}", self.exit_code));
        out
    }
}

#[derive(Default)]
pub struct Sandbox {
    /// A host dir to mask with a tmpfs inside the sandbox (e.g. the mind's state dir). Optional.
    hidden_dir: Option<String>,
}

impl Sandbox {
    pub fn new() -> Self {
        Self::default()
    }

    /// Mask this host directory with an empty tmpfs inside the sandbox (hide the mind's DB/state).
    pub fn hiding(mut self, dir: impl Into<String>) -> Self {
        self.hidden_dir = Some(dir.into());
        self
    }

    /// Is sandboxed execution actually possible here? (unprivileged userns + the tools present.)
    pub async fn available(&self) -> bool {
        matches!(self.run_shell("echo ok").await, Ok(r) if r.exit_code == 0 && r.stdout.contains("ok"))
    }

    pub async fn run_shell(&self, cmd: &str) -> std::io::Result<ExecResult> {
        self.run(
            Limits::default(),
            vec![("prog.sh", cmd.to_string())],
            "exec /bin/sh prog.sh",
        )
        .await
    }

    pub async fn run_python(&self, code: &str) -> std::io::Result<ExecResult> {
        self.run(
            Limits::default(),
            vec![("prog.py", code.to_string())],
            "exec python3 -I -S -B prog.py",
        )
        .await
    }

    /// Run `code` with ONE extra input file beside it in the scratch dir.
    ///
    /// The header above promises that "code + inputs live in a throwaway scratch dir passed as
    /// FILES", but nothing exposed a way to pass an input, so callers reached for an absolute host
    /// path instead — which the sandbox masks on purpose. The forge's syntax check did exactly
    /// that and therefore FAILED every python file it built, blaming the artifact for its own
    /// plumbing. Inputs go in beside the program now.
    pub async fn run_python_with(
        &self,
        code: &str,
        input_name: &'static str,
        input: &str,
    ) -> std::io::Result<ExecResult> {
        self.run(
            Limits::default(),
            vec![("prog.py", code.to_string()), (input_name, input.to_string())],
            "exec python3 -I -S -B prog.py",
        )
        .await
    }

    pub async fn run_rust(&self, code: &str) -> std::io::Result<ExecResult> {
        // Compile (std-only, offline) then run. Compile errors go to stderr via the captured stream.
        self.run(
            Limits::for_rust(),
            vec![("prog.rs", code.to_string())],
            "rustc -O --edition 2021 prog.rs -o prog || exit 1; exec ./prog",
        )
        .await
    }

    async fn run(
        &self,
        lim: Limits,
        files: Vec<(&'static str, String)>,
        run_sh: &str,
    ) -> std::io::Result<ExecResult> {
        self.run_tree(lim, files.into_iter().map(|(n, c)| (n.to_string(), c)).collect(), run_sh)
            .await
    }

    /// E.SMOKE1: run a whole file TREE (dynamic names, subdirectories allowed) under the same
    /// namespaces and limits. `run_sh` is the driver the sandbox executes; put the tree's own
    /// scripts under a subdirectory so the driver never overwrites them.
    pub async fn run_tree(
        &self,
        lim: Limits,
        files: Vec<(String, String)>,
        run_sh: &str,
    ) -> std::io::Result<ExecResult> {
        self.run_seeded(lim, None, files, run_sh).await
    }

    /// E.RUNG8: run a driver against a COPY of a host directory tree.
    ///
    /// The tree is copied into a SUBDIRECTORY of
    /// the scratch, never its root, because the sandbox writes its own `run.sh` at that root and a
    /// repository carrying a `run.sh` of its own must not be shadowed by the driver that is meant
    /// to invoke it. The copy is the sandbox's, made and destroyed with the scratch: a check may do
    /// anything it likes to it and reach nothing that outlives the run.
    pub async fn run_seeded(
        &self,
        lim: Limits,
        seed: Option<SeedTree>,
        files: Vec<(String, String)>,
        run_sh: &str,
    ) -> std::io::Result<ExecResult> {
        let hidden = self.hidden_dir.clone();
        let run_sh = run_sh.to_string();
        tokio::task::spawn_blocking(move || Self::run_blocking(lim, seed, files, run_sh, hidden))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(format!("join: {e}"))))
    }

    fn run_blocking(
        lim: Limits,
        seed: Option<SeedTree>,
        files: Vec<(String, String)>,
        run_sh: String,
        hidden: Option<String>,
    ) -> std::io::Result<ExecResult> {
        // Throwaway scratch dir (unique without rand: pid + seq + time).
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let scratch: PathBuf =
            std::env::temp_dir().join(format!("ym_sbx_{}_{seq}_{ts}", std::process::id()));
        std::fs::create_dir_all(&scratch)?;
        if let Some(seed) = &seed {
            let rel = std::path::Path::new(&seed.into);
            // One plain path segment. A seed that could name `.` or climb out would put the tree
            // back at the scratch root, which is exactly the collision the subdirectory prevents.
            if rel.is_absolute()
                || rel.components().count() != 1
                || !matches!(rel.components().next(), Some(std::path::Component::Normal(_)))
            {
                let _ = std::fs::remove_dir_all(&scratch);
                return Err(std::io::Error::other(format!(
                    "refusing seed subdir {:?}",
                    seed.into
                )));
            }
            let mut budget = SeedBudget::default();
            if let Err(e) = copy_seed(&seed.root, &scratch.join(rel), 0, &seed.skip, &mut budget) {
                let _ = std::fs::remove_dir_all(&scratch);
                return Err(e);
            }
        }
        for (name, content) in &files {
            // Names are relative and may carry subdirectories (E.SMOKE1 runs whole artifact trees);
            // anything that would escape the scratch dir is refused, not normalised.
            let rel = std::path::Path::new(name);
            if rel.is_absolute()
                || rel.components().any(|c| matches!(c, std::path::Component::ParentDir))
            {
                return Err(std::io::Error::other(format!("refusing path {name:?}")));
            }
            let target = scratch.join(rel);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, content)?;
        }
        std::fs::write(scratch.join("run.sh"), &run_sh)?;

        let scratch_str = scratch.to_string_lossy().to_string();
        let mount_line = match &hidden {
            Some(d) => format!("mount -t tmpfs none {d} 2>/dev/null || true; "),
            None => String::new(),
        };
        // The scratch path is ours (no user input) → safe to interpolate. User code is in files only.
        let outer = format!(
            "{mount_line}cd {scratch_str}; exec prlimit --cpu={cpu} --as={mem} --nproc={procs} --fsize={fsize} --nofile=256 -- /bin/sh run.sh",
            cpu = lim.cpu_secs, mem = lim.mem_bytes, procs = lim.max_procs, fsize = lim.fsize_bytes,
        );

        let output = Command::new("timeout")
            .args([
                "-s",
                "KILL",
                &lim.wall_secs.to_string(),
                "unshare",
                "--user",
                "--map-root-user",
                "--fork",
                "--pid",
                "--mount-proc",
                "--net",
                "--uts",
                "--ipc",
                "/bin/sh",
                "-euc",
                &outer,
            ])
            .output();

        let _ = std::fs::remove_dir_all(&scratch);

        let output = output?;
        let code = output.status.code().unwrap_or(-1);
        // `timeout -s KILL` → 124 (timed out) or 137 (128+SIGKILL).
        let timed_out = code == 124 || code == 137;
        Ok(ExecResult {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            exit_code: code,
            timed_out,
        })
    }
}

/// A host tree to copy into a sandbox scratch, and the top-level entries to leave behind.
///
/// E.RUNG8b: `skip` exists because of a measured exposure, not a hypothetical one. The coder sets
/// `HOME` to its own workdir on purpose, so the agent's home — session logs, tool-result caches,
/// and a `.key` file — lands INSIDE the tree a build is measured on. Copying that into a sandbox
/// that then runs a MODEL-AUTHORED shell command puts the agent's own credentials where that
/// command can read them. The empty network namespace stops them leaving the box; it does not stop
/// a check printing one to stdout, and stdout is the reply.
#[derive(Debug, Clone)]
pub struct SeedTree {
    pub root: PathBuf,
    /// The subdirectory of the scratch the tree is copied into.
    pub into: String,
    /// Top-level entry names never copied. `.git` is always skipped as well.
    pub skip: Vec<String>,
}

/// Caps on a seeded tree. The seed is a whole repository chosen by a caller, and these are the
/// only thing standing between one large checkout and a filled disk.
const SEED_MAX_FILES: usize = 8000;
const SEED_MAX_BYTES: u64 = 128 * 1024 * 1024;
const SEED_MAX_DEPTH: usize = 32;

#[derive(Default)]
struct SeedBudget {
    files: usize,
    bytes: u64,
}

/// Copy a host tree into the sandbox scratch.
///
/// `.git` is left behind: a check judges the WORKING TREE, the history is not part of what was
/// built, and a check that cannot see the history cannot rewrite it either. Symlinks are skipped
/// rather than followed, so no link inside the tree can pull host files into the copy.
fn copy_seed(
    src: &std::path::Path,
    dest: &std::path::Path,
    depth: usize,
    skip: &[String],
    budget: &mut SeedBudget,
) -> std::io::Result<()> {
    if depth > SEED_MAX_DEPTH {
        return Err(std::io::Error::other("seed tree is nested too deeply"));
    }
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        // Only at the top level: a `skip` name is about what the AGENT put beside the tree, not
        // about a file of that name somewhere inside the repository's own source.
        if depth == 0 && name.to_str().is_some_and(|n| skip.iter().any(|s| s == n)) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            copy_seed(&entry.path(), &dest.join(&name), depth + 1, skip, budget)?;
        } else if file_type.is_file() {
            budget.files += 1;
            budget.bytes += entry.metadata().map(|m| m.len()).unwrap_or(0);
            if budget.files > SEED_MAX_FILES || budget.bytes > SEED_MAX_BYTES {
                return Err(std::io::Error::other("seed tree exceeds the sandbox copy cap"));
            }
            std::fs::copy(entry.path(), dest.join(&name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The security half of E.RUNG8b, tested on a real tree rather than asserted.
    ///
    /// A skipped top-level entry is not copied; the SAME name deeper in the tree is, because the
    /// skip is about what the agent left beside the repository, not about the repository's files.
    #[test]
    fn a_skipped_top_level_entry_is_not_copied_but_the_same_name_deeper_is() {
        let root = mind_types::scratch::dir("sandbox-seed-skip");
        let _ = std::fs::remove_dir_all(&root);
        let (src, dest) = (root.join("src"), root.join("dest"));
        std::fs::create_dir_all(src.join(".claude")).unwrap();
        std::fs::create_dir_all(src.join("app").join(".claude")).unwrap();
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join(".claude").join("creds.key"), "SECRET").unwrap();
        std::fs::write(src.join(".claude.json"), "{}").unwrap();
        std::fs::write(src.join("app").join(".claude").join("kept.txt"), "mine").unwrap();
        std::fs::write(src.join(".git").join("HEAD"), "ref: x").unwrap();
        std::fs::write(src.join("start.sh"), "echo hi").unwrap();

        let skip = vec![".claude".to_string(), ".claude.json".to_string()];
        let mut budget = SeedBudget::default();
        copy_seed(&src, &dest, 0, &skip, &mut budget).unwrap();

        assert!(dest.join("start.sh").exists(), "the repository is copied");
        assert!(
            dest.join("app").join(".claude").join("kept.txt").exists(),
            "a .claude INSIDE the source tree is the repository's own and is copied"
        );
        assert!(
            !dest.join(".claude").exists(),
            "the agent's home must not reach the sandbox"
        );
        assert!(!dest.join(".claude.json").exists());
        assert!(!dest.join(".git").exists(), ".git is never copied");
        assert_eq!(budget.files, 2, "only start.sh and the nested kept.txt");
    }

    #[test]
    fn render_shows_output_and_exit() {
        let r = ExecResult {
            stdout: "hello".into(),
            stderr: String::new(),
            exit_code: 0,
            timed_out: false,
        };
        let s = r.render();
        assert!(s.contains("hello") && s.contains("exit code: 0"));
    }

    #[test]
    fn render_flags_timeout() {
        let r = ExecResult {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: 137,
            timed_out: true,
        };
        assert!(r.render().contains("timed out"));
    }

    // Real execution only works on Linux with unprivileged userns — skip elsewhere (e.g. Windows dev).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn python_runs_when_sandbox_available() {
        let sb = Sandbox::new();
        if !sb.available().await {
            eprintln!("sandbox unavailable here — skipping real-exec test");
            return;
        }
        let r = sb.run_python("print(6*7)").await.unwrap();
        assert_eq!(r.exit_code, 0);
        assert!(r.stdout.contains("42"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn network_is_blocked_when_available() {
        let sb = Sandbox::new();
        if !sb.available().await {
            return;
        }
        let r = sb
            .run_python("import socket\ntry:\n socket.create_connection(('1.1.1.1',53),2); print('OPEN')\nexcept Exception: print('BLOCKED')")
            .await
            .unwrap();
        assert!(
            r.stdout.contains("BLOCKED"),
            "network must be unavailable: {}",
            r.stdout
        );
    }
}
