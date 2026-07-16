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
    /// When set, run on real Claude via the subscription OAuth token (Max-plan), dropping the MiniMax
    /// base/model override. Falls back to MiniMax (base_url/token/model) when absent or rejected.
    oauth_token: Option<String>,
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
            token: token.into().trim().to_owned(),
            model: model.into(),
            scratch_root: scratch_root.into(),
            timeout_secs: 300,
            oauth_token: None,
        }
    }

    pub fn with_timeout(mut self, secs: u64) -> Self {
        self.timeout_secs = secs;
        self
    }

    /// Run on real Claude via a subscription OAuth token (`claude setup-token`), instead of MiniMax.
    pub fn with_oauth(mut self, token: impl Into<String>) -> Self {
        let t = token.into();
        self.oauth_token = match t.trim() {
            "" => None,
            token => Some(token.to_owned()),
        };
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

    fn command(&self, wd: &str, task: &str, use_oauth: bool) -> Command {
        let mut cmd = Command::new("claude");
        cmd.current_dir(wd)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", wd)
            .env("USER", "yantrikmind");
        if use_oauth {
            cmd.env(
                "CLAUDE_CODE_OAUTH_TOKEN",
                self.oauth_token.as_deref().unwrap_or_default(),
            );
        } else {
            cmd.env("ANTHROPIC_BASE_URL", &self.base_url)
                .env("ANTHROPIC_AUTH_TOKEN", &self.token)
                .env("ANTHROPIC_MODEL", &self.model);
        }
        cmd.arg("-p")
            .arg(task)
            .arg("--permission-mode")
            .arg("acceptEdits")
            .arg("--dangerously-skip-permissions")
            .arg("--output-format")
            .arg("text")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Run an agentic coding task. The agent works in a fresh isolated scratch dir and reports back.
    pub async fn run(&self, task: &str) -> anyhow::Result<CoderResult> {
        let wd = self.fresh_workdir()?;

        let use_oauth = self.oauth_token.is_some();
        let child = self.command(&wd, task, use_oauth).spawn()?;
        let out = match tokio::time::timeout(
            std::time::Duration::from_secs(self.timeout_secs),
            child.wait_with_output(),
        )
        .await
        {
            Ok(r) => r?,
            Err(_) => anyhow::bail!("coder timed out after {}s", self.timeout_secs),
        };

        let auth_error = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let out = if use_oauth && !self.token.is_empty() && is_revoked_oauth_error(&auth_error) {
            let fallback = self.command(&wd, task, false).spawn()?;
            match tokio::time::timeout(
                std::time::Duration::from_secs(self.timeout_secs),
                fallback.wait_with_output(),
            )
            .await
            {
                Ok(r) => r?,
                Err(_) => anyhow::bail!("coder fallback timed out after {}s", self.timeout_secs),
            }
        } else {
            out
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

        Ok(CoderResult {
            ok: out.status.success(),
            summary,
            workdir: wd,
            files,
        })
    }
}

fn is_revoked_oauth_error(output: &str) -> bool {
    let error = output.to_ascii_lowercase();
    error.contains("401")
        && error.contains("oauth")
        && (error.contains("revoked") || error.contains("invalid authentication credentials"))
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
        s.push_str(&format!(
            "\nfiles ({}) in {}: {}",
            r.files.len(),
            r.workdir,
            r.files.join(", ")
        ));
    }
    s.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_token_trims_surrounding_whitespace() {
        let coder = Coder::new("fallback", "model", "https://example.com", "/tmp")
            .with_oauth("  oauth-token\n");

        assert_eq!(coder.oauth_token.as_deref(), Some("oauth-token"));
    }

    #[test]
    fn provider_token_trims_surrounding_whitespace() {
        let coder = Coder::new("  provider-token\n", "model", "https://example.com", "/tmp");

        assert_eq!(coder.token, "provider-token");
    }

    #[test]
    fn recognizes_revoked_oauth_error_for_provider_fallback() {
        assert!(is_revoked_oauth_error(
            "Failed to authenticate. API Error: 401 OAuth access token has been revoked."
        ));
        assert!(!is_revoked_oauth_error(
            "API Error: 429 usage limit exceeded"
        ));
    }
}
