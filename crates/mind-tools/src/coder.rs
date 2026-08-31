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

use sha2::{Digest, Sha256};
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

/// What one round actually cost. Absent when the CLI did not return usable JSON — an UNMEASURED
/// round must be distinguishable from a free one, so this is an Option and never a zeroed struct.
/// (A spend meter that reports 0 when it failed is worse than no meter: it reads as "nothing
/// happened yet" while the money leaves.)
#[derive(Debug, Clone)]
pub struct RoundSpend {
    pub model: String,
    pub input: u64,
    pub cache_write: u64,
    pub cache_read: u64,
    pub output: u64,
    pub usd: f64,
}

impl RoundSpend {
    pub fn total_tokens(&self) -> u64 {
        self.input + self.cache_write + self.cache_read + self.output
    }

    /// The ledger line format already written by `ym-record-spend` and already parsed by the
    /// `tokens` verb. Matching it byte-for-byte (only the lane label differs) means delegation
    /// spend shows up in the existing report with no reader change — one ledger, one truth. The
    /// 08-05 control-center research put "never two sources of truth for spend" at #2 by user pain,
    /// after an ecosystem shipped a $12.3M header against $10-15 of real cost.
    pub fn ledger_line(&self, lane: &str, when: &str) -> String {
        format!(
            "{when} | {lane} | {} | tokens={} (in={} cache_w={} cache_r={} out={}) | usd={:.4}",
            self.model,
            self.total_tokens(),
            self.input,
            self.cache_write,
            self.cache_read,
            self.output,
            self.usd
        )
    }
}

/// Pull the prose and the spend out of a `claude --output-format json` blob. Returns the raw text
/// unchanged (and no spend) when it is not that shape — the CLI emits plain text on some error
/// paths, and losing the error message to a parse failure would be a bad trade.
fn parse_cli_json(raw: &str) -> (String, Option<RoundSpend>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (raw.trim().to_string(), None);
    };
    let prose = v
        .get("result")
        .and_then(|r| r.as_str())
        .map_or_else(|| raw.trim().to_string(), |s| s.trim().to_string());
    let u = v.get("usage");
    let n = |k: &str| {
        u.and_then(|u| u.get(k))
            .and_then(|x| x.as_u64())
            .unwrap_or(0)
    };
    // No usage block means the run reported no spend it can vouch for — say unmeasured, not zero.
    let spend = u.map(|_| RoundSpend {
        model: v
            .get("modelUsage")
            .and_then(|m| m.as_object())
            .and_then(|m| m.keys().next().cloned())
            .unwrap_or_else(|| "unknown".to_string()),
        input: n("input_tokens"),
        cache_write: n("cache_creation_input_tokens"),
        cache_read: n("cache_read_input_tokens"),
        output: n("output_tokens"),
        usd: v
            .get("total_cost_usd")
            .and_then(|c| c.as_f64())
            .unwrap_or(0.0),
    });
    (prose, spend)
}

/// The result of one coder run.
pub struct CoderResult {
    pub ok: bool,
    /// The agent's final text (its own summary of what it did).
    pub summary: String,
    /// What the round cost, when the CLI reported it. `None` means UNMEASURED, not free.
    pub spend: Option<RoundSpend>,
    /// Absolute path of the scratch workdir holding any files it produced.
    pub workdir: String,
    /// Non-hidden files the agent created/left in the workdir.
    pub files: Vec<String>,
    /// The wall clock expired before the agent finished. The workdir still holds everything it
    /// wrote up to the cutoff — a timed-out run once turned out to be a COMPLETE, gate-passing
    /// redesign that was thrown away as "failed", so callers must treat this as a partial result
    /// to salvage, never as an absence of work.
    pub timed_out: bool,
    /// Snapshot of the artifact tree immediately before this refinement round. `None` on the
    /// first round because there was no prior artifact to preserve.
    pub checkpoint: Option<String>,
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

    /// Resolve an existing run directory and prove it is a child of this coder's scratch root.
    /// Ledger paths are data, not authority: no rollback operation may escape into an arbitrary
    /// directory even if a row is malformed or tampered with.
    fn checked_workdir(&self, wd: &str) -> anyhow::Result<(std::path::PathBuf, String)> {
        let root = std::path::Path::new(&self.scratch_root).canonicalize()?;
        let workdir = std::path::Path::new(wd).canonicalize()?;
        if workdir == root || !workdir.starts_with(&root) {
            anyhow::bail!("workdir is outside the coder scratch root");
        }
        let run = workdir
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| !n.is_empty())
            .ok_or_else(|| anyhow::anyhow!("workdir has no safe run name"))?
            .to_string();
        Ok((workdir, run))
    }

    /// Stable SHA-256 of the visible artifact tree. Hidden state is outside the deliverable and is
    /// deliberately excluded, matching checkpoint copy/restore semantics.
    pub fn artifact_sha256(&self, wd: &str) -> anyhow::Result<String> {
        let (workdir, _) = self.checked_workdir(wd)?;
        Ok(digest_visible_tree(&workdir)?)
    }

    pub fn create_checkpoint(&self, wd: &str) -> anyhow::Result<String> {
        let (workdir, run) = self.checked_workdir(wd)?;
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let id = format!("cp-{nanos}");
        let target = std::path::Path::new(&self.scratch_root)
            .join(".checkpoints")
            .join(run)
            .join(&id);
        std::fs::create_dir_all(&target)?;
        if let Err(error) = copy_visible_tree(&workdir, &target) {
            let _ = std::fs::remove_dir_all(&target);
            return Err(error.into());
        }
        Ok(id)
    }

    /// Restore one pre-round snapshot. This is intentionally synchronous and operator-invoked;
    /// callers must refuse while a job is running so no builder can race the restore.
    pub fn restore_checkpoint(&self, wd: &str, id: &str) -> anyhow::Result<Vec<String>> {
        if !id
            .strip_prefix("cp-")
            .is_some_and(|tail| !tail.is_empty() && tail.chars().all(|c| c.is_ascii_digit()))
        {
            anyhow::bail!("invalid checkpoint id");
        }
        let (workdir, run) = self.checked_workdir(wd)?;
        let checkpoint_root = std::path::Path::new(&self.scratch_root)
            .join(".checkpoints")
            .join(run);
        let source = checkpoint_root.join(id).canonicalize()?;
        let checkpoint_root = checkpoint_root.canonicalize()?;
        if !source.starts_with(&checkpoint_root) || !source.is_dir() {
            anyhow::bail!("checkpoint is outside the run checkpoint root");
        }
        let expected_sha256 = digest_visible_tree(&source)?;
        for entry in std::fs::read_dir(&workdir)? {
            let entry = entry?;
            let name = entry.file_name();
            if name.to_string_lossy().starts_with('.') {
                continue;
            }
            let ty = entry.file_type()?;
            if ty.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        copy_visible_tree(&source, &workdir)?;
        let restored_sha256 = digest_visible_tree(&workdir)?;
        if restored_sha256 != expected_sha256 {
            anyhow::bail!(
                "checkpoint restore digest mismatch: expected {expected_sha256}, observed {restored_sha256}"
            );
        }
        Ok(list_files(wd))
    }

    fn command(&self, wd: &str, task: &str, use_oauth: bool, resume: bool) -> Command {
        let mut cmd = Command::new("claude");
        cmd.current_dir(wd)
            .env_clear()
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("HOME", wd)
            .env("USER", "yantrikmind")
            // Timeout must KILL, not abandon: dropping the wait future without this leaves the
            // agent editing files with no supervisor — one kept working for six minutes after the
            // mind had already reported the run failed. Containment requires the wall clock to end
            // the process, not just our interest in it.
            .kill_on_drop(true);
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
        // Opt-in only, and OFF by default — see delegate.rs's YM_DELEGATE_RESUME. Resuming makes
        // round N re-send rounds 1..N-1's transcript on every one of its tool calls, so cost grows
        // with the SQUARE of total turns; the caller now hands down a written history instead.
        if resume {
            cmd.arg("--continue");
        }
        // A CEILING ON THE ROUND, because turns are the cost axis and an uncapped round can spiral
        // by itself: each turn re-sends the whole conversation so far, so one 400-turn round costs
        // far more than ten 40-turn rounds doing the same work. That shape is how a week's token
        // quota went in a day. Being stopped at the ceiling is a pause, not a loss — the round
        // structure is already the checkpoint, and the critic judges whatever is on disk.
        //
        // CAVEAT: the CLI prices the run from its own model table, so on the qwen/MiniMax path
        // (ANTHROPIC_BASE_URL override) it may not recognise the model and the cap may not bind.
        // It is a real ceiling on the OAuth path and a best-effort one elsewhere; the wall clock
        // (timeout_secs) remains the backstop that always holds.
        let max_usd = std::env::var("YM_CODER_MAX_USD")
            .ok()
            .and_then(|v| v.trim().parse::<f64>().ok())
            .filter(|n| *n > 0.0)
            .unwrap_or(5.0);
        cmd.arg("--max-budget-usd")
            .arg(format!("{max_usd}"))
            .arg("-p")
            .arg(task)
            .arg("--permission-mode")
            .arg("acceptEdits")
            .arg("--dangerously-skip-permissions")
            .arg("--output-format")
            .arg("json")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        cmd
    }

    /// Run an agentic coding task. The agent works in a fresh isolated scratch dir and reports back.
    pub async fn run(&self, task: &str) -> anyhow::Result<CoderResult> {
        let wd = self.fresh_workdir()?;
        self.run_round(task, wd, false, None).await
    }

    /// Run in an EXISTING workdir — the iterate-until-good loop's primitive. Round N+1 continues
    /// where round N left its files, so a critique can say "fix the contrast on index.html" and the
    /// builder actually has an index.html to fix.
    pub async fn run_in(&self, task: &str, wd: String) -> anyhow::Result<CoderResult> {
        let checkpoint = self.create_checkpoint(&wd)?;
        self.run_round(task, wd, false, Some(checkpoint)).await
    }

    /// Like `run_in`, but RESUMES the previous round's session in this workdir instead of starting
    /// a cold one. The files alone carry WHAT was done; only the transcript carries WHY, and a
    /// critic's "fix X" lands very differently on a builder that remembers choosing X.
    pub async fn continue_in(&self, task: &str, wd: String) -> anyhow::Result<CoderResult> {
        let checkpoint = self.create_checkpoint(&wd)?;
        self.run_round(task, wd, true, Some(checkpoint)).await
    }

    async fn run_round(
        &self,
        task: &str,
        wd: String,
        resume: bool,
        checkpoint: Option<String>,
    ) -> anyhow::Result<CoderResult> {
        let use_oauth = self.oauth_token.is_some();
        let child = self.command(&wd, task, use_oauth, resume).spawn()?;
        let timeout = std::time::Duration::from_secs(self.timeout_secs);
        // A timeout is NOT an error: kill_on_drop reaps the child, and whatever it wrote up to the
        // cutoff is real work sitting in the workdir. Bailing here once discarded a complete,
        // gate-passing redesign; instead the caller gets the partial with `timed_out` set and
        // decides whether it is worth critiquing.
        let out = match tokio::time::timeout(timeout, child.wait_with_output()).await {
            Ok(r) => r?,
            Err(_) => return Ok(self.salvage(wd, "", resume, checkpoint)),
        };

        let auth_error = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let out = if use_oauth && !self.token.is_empty() && is_revoked_oauth_error(&auth_error) {
            let fallback = self.command(&wd, task, false, resume).spawn()?;
            match tokio::time::timeout(timeout, fallback.wait_with_output()).await {
                Ok(r) => r?,
                Err(_) => {
                    return Ok(self.salvage(
                        wd,
                        " (during the provider fallback)",
                        resume,
                        checkpoint,
                    ))
                }
            }
        } else {
            out
        };

        // stdout is a JSON envelope now (see --output-format json): the prose lives at .result and
        // the spend at .usage/.total_cost_usd. Falls back to the raw text when it is not JSON,
        // because the CLI prints bare errors on some paths and that message is worth more than a
        // clean parse failure.
        let (mut summary, spend) = parse_cli_json(&String::from_utf8_lossy(&out.stdout));
        if summary.is_empty() {
            summary = String::from_utf8_lossy(&out.stderr).trim().to_string();
        }
        Ok(CoderResult {
            ok: out.status.success(),
            summary,
            spend,
            files: list_files(&wd),
            workdir: wd,
            timed_out: false,
            checkpoint,
        })
    }

    /// What a timed-out round yields: the on-disk state, honestly labelled. `ok` stays false —
    /// the agent never got to confirm its own work — but the files are there to judge.
    fn salvage(
        &self,
        wd: String,
        ctx: &str,
        resumed: bool,
        checkpoint: Option<String>,
    ) -> CoderResult {
        CoderResult {
            ok: false,
            summary: format!(
                "(wall clock expired after {}s{ctx} — the agent was stopped mid-run{}; the files listed are its work up to the cutoff)",
                self.timeout_secs,
                if resumed { " while refining an earlier round" } else { "" },
            ),
            // A killed child never printed its JSON envelope, so the round's cost is genuinely
            // unknown — not zero. The ledger will show it as UNMEASURED rather than quietly
            // under-reporting the total, which is the failure this whole meter exists to prevent.
            spend: None,
            files: list_files(&wd),
            workdir: wd,
            timed_out: true,
            checkpoint,
        }
    }
}

/// Copy artifact files without following symlinks or importing hidden state such as `.env`, `.git`,
/// or an agent transcript. A checkpoint is the deliverable tree, not a second secret store.
fn copy_visible_tree(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let ty = entry.file_type()?;
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_visible_tree(&entry.path(), &target)?;
        } else if ty.is_file() {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn digest_visible_tree(root: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read as _;

    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        hasher: &mut Sha256,
    ) -> std::io::Result<()> {
        let mut entries = std::fs::read_dir(dir)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "artifact path is not valid UTF-8",
                )
            })?;
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            let relative = path.strip_prefix(root).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "artifact path escaped its root",
                )
            })?;
            let relative = relative
                .components()
                .map(|part| part.as_os_str().to_str())
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "artifact path is not valid UTF-8",
                    )
                })?
                .join("/");
            let ty = entry.file_type()?;
            if ty.is_dir() {
                hasher.update(b"\0directory\0");
                hasher.update((relative.len() as u64).to_le_bytes());
                hasher.update(relative.as_bytes());
                walk(root, &path, hasher)?;
            } else if ty.is_file() {
                let metadata = entry.metadata()?;
                hasher.update(b"\0file\0");
                hasher.update((relative.len() as u64).to_le_bytes());
                hasher.update(relative.as_bytes());
                hasher.update(metadata.len().to_le_bytes());
                let mut file = std::fs::File::open(&path)?;
                let mut buffer = [0u8; 8192];
                loop {
                    let read = file.read(&mut buffer)?;
                    if read == 0 {
                        break;
                    }
                    hasher.update(&buffer[..read]);
                }
            }
        }
        Ok(())
    }

    let mut hasher = Sha256::new();
    hasher.update(b"yantrik-visible-artifact-v1");
    walk(root, root, &mut hasher)?;
    Ok(format!("{:x}", hasher.finalize()))
}

/// Non-hidden files currently in a workdir.
fn list_files(wd: &str) -> Vec<String> {
    std::fs::read_dir(wd)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| !n.starts_with('.'))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
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

    /// The envelope the CLI actually returns under `--output-format json`, shaped exactly like the
    /// blobs `ym-record-spend` has been parsing on the box.
    #[test]
    fn cli_json_yields_both_the_prose_and_the_spend() {
        let raw = r#"{
            "result": "  Added tests/mail-form.test.ts (67 lines).  ",
            "total_cost_usd": 0.0769,
            "modelUsage": {"claude-haiku-4-5-20251001": {"anything": 1}},
            "usage": {"input_tokens": 5, "cache_creation_input_tokens": 7898,
                      "cache_read_input_tokens": 61965, "output_tokens": 667}
        }"#;
        let (prose, spend) = parse_cli_json(raw);
        assert_eq!(
            prose, "Added tests/mail-form.test.ts (67 lines).",
            "the prose is .result, trimmed — not the whole envelope"
        );
        let s = spend.expect("a usage block means the round is measured");
        assert_eq!(
            s.total_tokens(),
            70_535,
            "total must include cache reads — they are the whole story of this loop's cost"
        );
        assert_eq!(s.model, "claude-haiku-4-5-20251001");
        assert!((s.usd - 0.0769).abs() < 1e-9);
    }

    /// The line must match what `ym-record-spend` already writes, because the `tokens` verb parses
    /// that shape. Only the lane label differs. Drift here silently splits the ledger in two.
    #[test]
    fn ledger_line_matches_the_existing_recorder_format() {
        let s = RoundSpend {
            model: "claude-haiku-4-5-20251001".into(),
            input: 5,
            cache_write: 7898,
            cache_read: 61965,
            output: 667,
            usd: 0.0769,
        };
        assert_eq!(
            s.ledger_line("delegate:a1b2c3#2", "2026-08-13T03:17:19Z"),
            "2026-08-13T03:17:19Z | delegate:a1b2c3#2 | claude-haiku-4-5-20251001 | tokens=70535 (in=5 cache_w=7898 cache_r=61965 out=667) | usd=0.0769"
        );
    }

    /// Unmeasured must never masquerade as free. A CLI error prints bare text, and a JSON envelope
    /// can arrive with no usage block; in both cases the answer is None, and the caller records
    /// UNMEASURED. Reporting 0.0 here is what makes an expensive path look idle.
    #[test]
    fn a_round_without_usage_is_unmeasured_not_zero() {
        let (prose, spend) = parse_cli_json("API Error: Request rejected (429) · quota exhausted");
        assert!(
            prose.contains("429"),
            "a bare CLI error is worth more than a clean parse failure — keep the text"
        );
        assert!(spend.is_none(), "no envelope means no measurement");

        let (_, spend) = parse_cli_json(r#"{"result": "done", "total_cost_usd": 0.0}"#);
        assert!(
            spend.is_none(),
            "an envelope with no usage block is unmeasured, not a free round"
        );
    }

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

    #[test]
    fn checkpoint_restore_reproduces_pre_resume_hash_without_hidden_state() {
        let root = mind_types::scratch::dir("coder-checkpoint");
        let _ = std::fs::remove_dir_all(&root);
        let run = root.join("run-test");
        std::fs::create_dir_all(run.join("src")).unwrap();
        std::fs::write(run.join("src/app.rs"), "before").unwrap();
        std::fs::write(run.join("README.md"), "before-readme").unwrap();
        std::fs::write(run.join(".env"), "must-not-enter-checkpoint").unwrap();
        let coder = Coder::new(
            "token",
            "model",
            "https://example.com",
            root.to_string_lossy(),
        );

        let pre_resume_sha256 = coder.artifact_sha256(&run.to_string_lossy()).unwrap();
        let id = coder.create_checkpoint(&run.to_string_lossy()).unwrap();
        std::fs::write(run.join("src/app.rs"), "after").unwrap();
        std::fs::remove_file(run.join("README.md")).unwrap();
        std::fs::write(run.join("NEW.txt"), "new").unwrap();
        std::fs::write(run.join(".env"), "new-hidden-value").unwrap();
        assert_ne!(
            coder.artifact_sha256(&run.to_string_lossy()).unwrap(),
            pre_resume_sha256,
            "the resumed artifact must differ before rollback"
        );

        let files = coder
            .restore_checkpoint(&run.to_string_lossy(), &id)
            .unwrap();
        assert_eq!(
            coder.artifact_sha256(&run.to_string_lossy()).unwrap(),
            pre_resume_sha256,
            "verified rollback must reproduce the recorded pre-resume artifact hash"
        );
        assert_eq!(
            std::fs::read_to_string(run.join("src/app.rs")).unwrap(),
            "before"
        );
        assert_eq!(
            std::fs::read_to_string(run.join("README.md")).unwrap(),
            "before-readme"
        );
        assert!(!run.join("NEW.txt").exists());
        assert_eq!(
            std::fs::read_to_string(run.join(".env")).unwrap(),
            "new-hidden-value",
            "hidden state is neither snapshotted nor overwritten"
        );
        std::fs::write(run.join(".env"), "another-hidden-value").unwrap();
        assert_eq!(
            coder.artifact_sha256(&run.to_string_lossy()).unwrap(),
            pre_resume_sha256,
            "hidden state must not perturb the deliverable receipt"
        );
        assert!(files.contains(&"README.md".to_string()));
        assert!(files.contains(&"src".to_string()));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn checkpoint_restore_rejects_path_and_id_escape_attempts() {
        let root = mind_types::scratch::dir("coder-checkpoint-escape");
        let outside = mind_types::scratch::dir("coder-checkpoint-outside");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
        std::fs::create_dir_all(root.join("run-test")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        let coder = Coder::new(
            "token",
            "model",
            "https://example.com",
            root.to_string_lossy(),
        );
        assert!(coder
            .restore_checkpoint(&outside.to_string_lossy(), "cp-123")
            .is_err());
        assert!(coder
            .restore_checkpoint(&root.join("run-test").to_string_lossy(), "../escape")
            .is_err());
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }
}
