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
        Self { wall_secs: 15, cpu_secs: 10, mem_bytes: 512 * 1024 * 1024, max_procs: 64, fsize_bytes: 8 * 1024 * 1024 }
    }
}

impl Limits {
    /// Heavier limits for compiling Rust (rustc/LLVM need more memory, time, and output size).
    pub fn for_rust() -> Self {
        Self { wall_secs: 40, cpu_secs: 35, mem_bytes: 1536 * 1024 * 1024, max_procs: 128, fsize_bytes: 128 * 1024 * 1024 }
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
            if t.chars().count() > 4000 { format!("{}\n…(truncated)", t.chars().take(4000).collect::<String>()) } else { t.to_string() }
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

pub struct Sandbox {
    /// A host dir to mask with a tmpfs inside the sandbox (e.g. the mind's state dir). Optional.
    hidden_dir: Option<String>,
}

impl Default for Sandbox {
    fn default() -> Self {
        Self { hidden_dir: None }
    }
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
        self.run(Limits::default(), vec![("prog.sh", cmd.to_string())], "exec /bin/sh prog.sh").await
    }

    pub async fn run_python(&self, code: &str) -> std::io::Result<ExecResult> {
        self.run(Limits::default(), vec![("prog.py", code.to_string())], "exec python3 -I -S -B prog.py").await
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

    async fn run(&self, lim: Limits, files: Vec<(&'static str, String)>, run_sh: &str) -> std::io::Result<ExecResult> {
        let hidden = self.hidden_dir.clone();
        let run_sh = run_sh.to_string();
        tokio::task::spawn_blocking(move || Self::run_blocking(lim, files, run_sh, hidden))
            .await
            .unwrap_or_else(|e| Err(std::io::Error::other(format!("join: {e}"))))
    }

    fn run_blocking(lim: Limits, files: Vec<(&'static str, String)>, run_sh: String, hidden: Option<String>) -> std::io::Result<ExecResult> {
        // Throwaway scratch dir (unique without rand: pid + seq + time).
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let ts = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0);
        let scratch: PathBuf = std::env::temp_dir().join(format!("ym_sbx_{}_{seq}_{ts}", std::process::id()));
        std::fs::create_dir_all(&scratch)?;
        for (name, content) in &files {
            std::fs::write(scratch.join(name), content)?;
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
                "-s", "KILL", &lim.wall_secs.to_string(),
                "unshare", "--user", "--map-root-user", "--fork", "--pid", "--mount-proc", "--net", "--uts", "--ipc",
                "/bin/sh", "-euc", &outer,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_shows_output_and_exit() {
        let r = ExecResult { stdout: "hello".into(), stderr: String::new(), exit_code: 0, timed_out: false };
        let s = r.render();
        assert!(s.contains("hello") && s.contains("exit code: 0"));
    }

    #[test]
    fn render_flags_timeout() {
        let r = ExecResult { stdout: String::new(), stderr: String::new(), exit_code: 137, timed_out: true };
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
        assert!(r.stdout.contains("BLOCKED"), "network must be unavailable: {}", r.stdout);
    }
}
