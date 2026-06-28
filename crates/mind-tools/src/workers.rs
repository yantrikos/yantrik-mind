//! workers — a remote worker pool: the mind dispatches commands to the LXCs transferred from JARVIS
//! (yantrik-mind-worker-1/2/3) over SSH, using the `yantrikmind` service key. This is how the
//! companion gets real parallelism (fan out coder/research/sandbox) and offloads heavy work off its
//! main box.
//!
//! Config (env): `YM_WORKERS="root@192.168.4.88,root@192.168.4.61,root@192.168.4.94"` and
//! `YM_WORKER_KEY=/opt/yantrik-mind/.ssh/id_ed25519`. Absent → no pool (the mind runs everything
//! locally, as before).
//!
//! Security: this runs arbitrary commands on the workers as root, so anything the MIND (vs the
//! operator) dispatches here must be governed by the harm-gate/sandbox at the call site — the pool
//! itself is just transport.

use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::io::AsyncWriteExt;
use tokio::process::Command;

/// A round-robin pool of SSH-reachable workers.
pub struct WorkerPool {
    hosts: Vec<String>,
    key: String,
    next: AtomicUsize,
}

impl WorkerPool {
    pub fn new(hosts: Vec<String>, key: impl Into<String>) -> Self {
        Self { hosts, key: key.into(), next: AtomicUsize::new(0) }
    }

    /// Build from env (`YM_WORKERS` comma-separated `user@host`, `YM_WORKER_KEY`). `None` if unset.
    pub fn from_env() -> Option<Self> {
        let raw = std::env::var("YM_WORKERS").ok()?;
        let hosts: Vec<String> = raw.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if hosts.is_empty() {
            return None;
        }
        let key = std::env::var("YM_WORKER_KEY").unwrap_or_else(|_| "/opt/yantrik-mind/.ssh/id_ed25519".to_string());
        Some(Self::new(hosts, key))
    }

    pub fn len(&self) -> usize {
        self.hosts.len()
    }
    pub fn is_empty(&self) -> bool {
        self.hosts.is_empty()
    }
    pub fn hosts(&self) -> &[String] {
        &self.hosts
    }

    /// Next host in round-robin order.
    pub fn pick(&self) -> Option<&str> {
        if self.hosts.is_empty() {
            return None;
        }
        let i = self.next.fetch_add(1, Ordering::Relaxed) % self.hosts.len();
        Some(&self.hosts[i])
    }

    /// Run a command on a specific worker; returns stdout (trimmed) or an error.
    pub async fn run_on(&self, host: &str, cmd: &str, timeout_secs: u64) -> anyhow::Result<String> {
        run_ssh(&self.key, host, cmd, timeout_secs).await
    }

    /// Run the same command on ALL workers in parallel (one tokio task each). Returns (host, result).
    pub async fn map(&self, cmd: &str, timeout_secs: u64) -> Vec<(String, anyhow::Result<String>)> {
        let mut set = tokio::task::JoinSet::new();
        for h in &self.hosts {
            let (host, key, cmd) = (h.clone(), self.key.clone(), cmd.to_string());
            set.spawn(async move {
                let r = run_ssh(&key, &host, &cmd, timeout_secs).await;
                (host, r)
            });
        }
        let mut out = Vec::new();
        while let Some(res) = set.join_next().await {
            if let Ok(pair) = res {
                out.push(pair);
            }
        }
        out
    }

    /// Liveness check across the pool.
    pub async fn health(&self) -> Vec<(String, bool)> {
        self.map("echo ok", 8).await.into_iter().map(|(h, r)| (h, matches!(r, Ok(ref s) if s == "ok"))).collect()
    }

    /// Run code in an ISOLATED sandbox ON A WORKER (unprivileged userns, no network, temp-file masked,
    /// time-bounded) — offloads execution off the main companion box. Code is piped over SSH stdin
    /// (no shell-quoting hazard). `python` and `shell` only (workers have python3 + util-linux; rust
    /// would need rustc installed there). Picks the next worker round-robin → concurrent calls spread.
    pub async fn run_sandboxed(&self, lang: &str, code: &str, timeout_secs: u64) -> anyhow::Result<String> {
        let host = self.pick().ok_or_else(|| anyhow::anyhow!("no workers in pool"))?.to_string();
        let runner = match lang {
            "python" => "python3 -I -S -B",
            "shell" => "sh",
            other => anyhow::bail!("remote sandbox supports python|shell, not {other}"),
        };
        let inner = timeout_secs.saturating_sub(3).max(5);
        // mktemp -> read code from stdin -> run under unshare (no net) -> clean up. `2>&1` folds the
        // program's stderr into stdout so the caller sees errors too.
        let cmd = format!(
            "f=$(mktemp /tmp/ymcode.XXXXXX) && cat > \"$f\" && timeout {inner} \
             unshare --user --map-root-user --fork --pid --mount-proc --net --uts --ipc {runner} \"$f\" 2>&1; \
             rc=$?; rm -f \"$f\"; exit $rc"
        );
        let out = run_ssh_stdin(&self.key, &host, &cmd, code, timeout_secs).await?;
        Ok(format!("[ran on {host} — isolated, no network]\n{out}"))
    }
}

impl WorkerPool {
    /// CODER FAN-OUT: run an agentic coding task with Claude Code (on MiniMax) ON A WORKER, off the
    /// main box. Write-only (Write/Edit/Read — a code *generator*; running generated code stays in the
    /// sandbox), so it runs headless as root without `--dangerously-skip-permissions`. The MiniMax
    /// endpoint+token come from the worker's root:600 `/root/.ym-coder.env` (not passed per-call); the
    /// task prompt arrives via stdin (no quoting hazard). Returns claude's summary + the files it wrote.
    /// Picks the next worker round-robin → concurrent `code:` requests spread across machines.
    pub async fn run_coder(&self, task: &str, model: &str, timeout_secs: u64) -> anyhow::Result<String> {
        let host = self.pick().ok_or_else(|| anyhow::anyhow!("no workers in pool"))?.to_string();
        let inner = timeout_secs.saturating_sub(5).max(30);
        let cmd = format!(
            "d=$(mktemp -d /tmp/ymcoder.XXXXXX) && cd \"$d\" && export HOME=\"$d\" && \
             set -a && . /root/.ym-coder.env && set +a && export ANTHROPIC_MODEL={model} && \
             task=$(cat) && \
             out=$(timeout {inner} claude -p \"$task\" --permission-mode acceptEdits --allowedTools 'Write Edit Read' --output-format text 2>&1); \
             echo \"$out\"; for f in \"$d\"/*; do [ -f \"$f\" ] && printf '\\n=== %s ===\\n' \"$(basename \"$f\")\" && cat \"$f\"; done; cd / && rm -rf \"$d\""
        );
        let out = run_ssh_stdin(&self.key, &host, &cmd, task, timeout_secs).await?;
        Ok(format!("[coded on {host} — Claude Code on {model}, write-only]\n{out}"))
    }
}

/// SSH to `host`, pipe `stdin_data` into the remote `cmd`, return its stdout (trimmed). For the
/// remote sandbox: the code is the stdin, so no quoting/encoding of the program is needed.
async fn run_ssh_stdin(key: &str, host: &str, cmd: &str, stdin_data: &str, timeout_secs: u64) -> anyhow::Result<String> {
    let mut child = Command::new("ssh")
        .arg("-i").arg(key)
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("StrictHostKeyChecking=accept-new")
        .arg("-o").arg(format!("ConnectTimeout={}", timeout_secs.min(20)))
        .arg(host)
        .arg(cmd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut si) = child.stdin.take() {
        si.write_all(stdin_data.as_bytes()).await?;
        si.shutdown().await?;
    }
    let out = match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(r) => r?,
        Err(_) => anyhow::bail!("worker {host} timed out after {timeout_secs}s"),
    };
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run `cmd` on `host` over SSH with `key`, bounded by a timeout. Free fn so the parallel `map` can
/// own its inputs (spawned tasks must be `'static`).
async fn run_ssh(key: &str, host: &str, cmd: &str, timeout_secs: u64) -> anyhow::Result<String> {
    let mut c = Command::new("ssh");
    c.arg("-i").arg(key)
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("StrictHostKeyChecking=accept-new")
        .arg("-o").arg(format!("ConnectTimeout={}", timeout_secs.min(20)))
        .arg(host)
        .arg(cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = c.spawn()?;
    let out = match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), child.wait_with_output()).await {
        Ok(r) => r?,
        Err(_) => anyhow::bail!("worker {host} timed out after {timeout_secs}s"),
    };
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        anyhow::bail!("worker {host}: {}", String::from_utf8_lossy(&out.stderr).trim())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_env_and_round_robin() {
        let p = WorkerPool::new(vec!["root@a".into(), "root@b".into(), "root@c".into()], "/k");
        assert_eq!(p.len(), 3);
        // round-robin cycles through all hosts
        let picks: Vec<String> = (0..6).filter_map(|_| p.pick().map(|s| s.to_string())).collect();
        assert_eq!(picks, vec!["root@a", "root@b", "root@c", "root@a", "root@b", "root@c"]);
        let empty = WorkerPool::new(vec![], "/k");
        assert!(empty.pick().is_none());
    }
}
