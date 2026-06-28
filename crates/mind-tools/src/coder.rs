//! coder — an agentic coding capability: Claude Code (the `claude` CLI) driven by a third-party
//! model (MiniMax-M2 via MiniMax's Anthropic-compatible endpoint), so it runs on the MiniMax
//! subscription with zero Anthropic cost. This is the `code` role's real engine: not code-text
//! generation but a tool-using agent that writes + runs files in a scratch workdir.
//!
//! Containment (the mind's security ethos — an autonomous file/exec agent is the highest-capability
//! thing here):
//! - **Secret-stripped env**: the child gets `env_clear()` + ONLY the MiniMax endpoint/token/model
//!   (+ PATH/HOME/USER). It never inherits the mind's other keys (NANOGPT/github/gmail/telegram),
//!   so a prompt-injected task can't read or exfiltrate them.
//! - **Isolated scratch**: a fresh per-run dir under the service user's own home (not the state dir
//!   that holds the cognitive DB); `HOME` points there too.
//! - **Bounded**: wall-clock timeout; output captured, not streamed to a shell.
//! - **Generate-only**: the agent produces files in its scratch; the mind surfaces the result.
//!   Applying/committing them is a separate, harm-gated step (not done here).
//!
//! `--dangerously-skip-permissions` is what makes it non-interactive; `claude` itself refuses that
//! flag as root, so the service MUST run as a non-root user (it runs as `yantrikmind`).

use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::process::Command;

/// An agentic coder backed by Claude Code on a custom (Anthropic-compatible) provider.
pub struct Coder {
    base_url: String,
    token: String,
    model: String,
    scratch_root: String,
    timeout_secs: u64,
}

/// The result of one coder run.
pub struct CoderResult {
    pub ok: bool,
    /// The agent's final text (its own summary of what it did).
    pub summary: String,
    /// Absolute path of the scratch workdir holding any files it produced.
    pub workdir: String,
    /// Non-hidden files the agent created/left in the workdir.
    pub files: Vec<String>,
}

impl Coder {
    /// `token` is the provider key (e.g. MINIMAX_API_KEY); `base_url` its Anthropic-compat endpoint.
    pub fn new(
        token: impl Into<String>,
        model: impl Into<String>,
        base_url: impl Into<String>,
        scratch_root: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            token: token.into(),
            model: model.into(),
            scratch_root: scratch_root.into(),
            timeout_secs: 300,
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Is the `claude` CLI installed?
    pub fn available() -> bool {
        std::process::Command::new("claude")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn fresh_workdir(&self) -> std::io::Result<String> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let wd = format!("{}/run-{nanos}", self.scratch_root.trim_end_matches('/'));
        std::fs::create_dir_all(&wd)?;
        Ok(wd)
    }

    /// Run an agentic coding task. The agent works in a fresh isolated scratch dir and reports back.
    pub async fn run(&self, task: &str) -> anyhow::Result<CoderResult> {
        let wd = self.fresh_workdir()?;

        let mut cmd = Command::new("claude");
        cmd.current_dir(&wd)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", &wd)
            .env("USER", "yantrikmind")
            .env("ANTHROPIC_BASE_URL", &self.base_url)
            .env("ANTHROPIC_AUTH_TOKEN", &self.token)
            .env("ANTHROPIC_MODEL", &self.model)
            .arg("-p")
            .arg(task)
            .arg("--permission-mode")
            .arg("acceptEdits")
            .arg("--dangerously-skip-permissions")
            .arg("--output-format")
            .arg("text")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let child = cmd.spawn()?;
        let out = match tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            child.wait_with_output(),
        )
        .await
        {
            Ok(r) => r?,
            Err(_) => anyhow::bail!("coder timed out after {}s", self.timeout_secs),
        };

        let mut summary = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if summary.is_empty() {
            summary = String::from_utf8_lossy(&out.stderr).trim().to_string();
        }
        let files = std::fs::read_dir(&wd)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| !n.starts_with('.'))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        Ok(CoderResult { ok: out.status.success(), summary, workdir: wd, files })
    }
}

/// Render a coder result for the chat.
pub fn render_coder(r: &CoderResult) -> String {
    let mut s = String::new();
    if !r.ok {
        s.push_str("⚠ coder run did not complete cleanly\n");
    }
    if !r.summary.is_empty() {
        s.push_str(&r.summary);
        s.push('\n');
    }
    if !r.files.is_empty() {
        s.push_str(&format!("\nfiles ({}) in {}: {}", r.files.len(), r.workdir, r.files.join(", ")));
    }
    s.trim().to_string()
}
