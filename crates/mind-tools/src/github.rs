//! github — the mind's GitHub capability. READ is here (notifications/"what needs my attention",
//! safe like browsing); its output is untrusted (issue/PR titles are attacker-controllable text,
//! wrapped by the caller). WRITE (comment, open issue/PR, merge) is an outward effect and is
//! deliberately NOT here — it must ride the harm-gate + confirmation.
//!
//! `GithubClient` is the injectable seam (real REST API vs scripted-for-tests). The real transport
//! is blocking `ureq` on the blocking pool.

use async_trait::async_trait;

/// One GitHub notification, reduced to what a digest needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubNotification {
    pub repo: String,
    pub kind: String,   // PullRequest / Issue / Release / ...
    pub title: String,
    pub reason: String, // mention / review_requested / assign / ...
}

/// One open issue or PR on a specific repo — what a repo TRACKER needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubItem {
    pub number: u64,
    pub kind: String, // "issue" | "pr"
    pub title: String,
    pub author: String,
    pub url: String,
}

#[async_trait]
pub trait GithubClient: Send + Sync {
    /// Unread notifications (most recent first), capped at `limit`.
    async fn notifications(&self, limit: usize) -> anyhow::Result<Vec<GithubNotification>>;
    /// Open issues + PRs on a specific repo ("owner/name"), newest first. Public repos need no auth;
    /// the token (if present) is sent only to raise the rate limit / reach private repos. Default impl
    /// returns empty so non-API clients don't have to implement it.
    async fn repo_open_items(&self, _repo: &str, _limit: usize) -> anyhow::Result<Vec<GithubItem>> {
        Ok(Vec::new())
    }
}

/// Render notifications as a compact, untrusted digest block.
pub fn render_github_digest(items: &[GithubNotification]) -> String {
    if items.is_empty() {
        return "No unread GitHub notifications.".to_string();
    }
    let mut s = format!("{} unread notification(s):\n", items.len());
    for n in items {
        let title = if n.title.trim().is_empty() { "(no title)" } else { n.title.trim() };
        s.push_str(&format!("- [{}] {} — {} (reason: {})\n", n.kind, n.repo, title, n.reason));
    }
    s
}

/// Deterministic GitHub client for tests/evals.
pub struct ScriptedGithubClient {
    pub items: Vec<GithubNotification>,
}

impl ScriptedGithubClient {
    pub fn new(items: Vec<GithubNotification>) -> Self {
        Self { items }
    }
}

#[async_trait]
impl GithubClient for ScriptedGithubClient {
    async fn notifications(&self, limit: usize) -> anyhow::Result<Vec<GithubNotification>> {
        Ok(self.items.iter().take(limit).cloned().collect())
    }
}

/// Real GitHub REST client (token auth, read-only use here).
pub struct ApiGithubClient {
    token: String,
}

impl ApiGithubClient {
    pub fn new(token: impl Into<String>) -> Self {
        Self { token: token.into() }
    }
}

#[async_trait]
impl GithubClient for ApiGithubClient {
    async fn notifications(&self, limit: usize) -> anyhow::Result<Vec<GithubNotification>> {
        let token = self.token.clone();
        let per_page = limit.clamp(1, 50);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<GithubNotification>> {
            let url = format!("https://api.github.com/notifications?per_page={per_page}");
            let resp = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(20))
                .set("Authorization", &format!("Bearer {token}"))
                .set("Accept", "application/vnd.github+json")
                .set("X-GitHub-Api-Version", "2022-11-28")
                .set("User-Agent", "yantrik-mind")
                .call()?;
            let v: serde_json::Value = resp.into_json()?;
            let arr = v.as_array().cloned().unwrap_or_default();
            let mut out = Vec::new();
            for n in arr {
                let repo = n["repository"]["full_name"].as_str().unwrap_or("").to_string();
                let kind = n["subject"]["type"].as_str().unwrap_or("").to_string();
                let title = n["subject"]["title"].as_str().unwrap_or("").to_string();
                let reason = n["reason"].as_str().unwrap_or("").to_string();
                out.push(GithubNotification { repo, kind, title, reason });
            }
            Ok(out)
        })
        .await?
    }

    async fn repo_open_items(&self, repo: &str, limit: usize) -> anyhow::Result<Vec<GithubItem>> {
        let token = self.token.clone();
        let (repo, per_page) = (repo.to_string(), limit.clamp(1, 50));
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<GithubItem>> {
            // The issues endpoint returns BOTH issues and PRs (a PR carries a "pull_request" key).
            let url = format!(
                "https://api.github.com/repos/{repo}/issues?state=open&sort=created&direction=desc&per_page={per_page}"
            );
            let mut req = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(20))
                .set("Accept", "application/vnd.github+json")
                .set("X-GitHub-Api-Version", "2022-11-28")
                .set("User-Agent", "yantrik-mind");
            if !token.is_empty() {
                req = req.set("Authorization", &format!("Bearer {token}"));
            }
            let v: serde_json::Value = req.call()?.into_json()?;
            let arr = v.as_array().cloned().unwrap_or_default();
            let mut out = Vec::new();
            for it in arr {
                out.push(GithubItem {
                    number: it["number"].as_u64().unwrap_or(0),
                    kind: if it.get("pull_request").is_some() { "pr" } else { "issue" }.to_string(),
                    title: it["title"].as_str().unwrap_or("").to_string(),
                    author: it["user"]["login"].as_str().unwrap_or("").to_string(),
                    url: it["html_url"].as_str().unwrap_or("").to_string(),
                });
            }
            Ok(out)
        })
        .await?
    }
}

// ---------------------------------------------------------------------------------------------
// Writing — an OUTWARD effect (a public comment). Transport only; whether it's allowed/confirmed is
// the harm-gate + ActionRuntime's job.
// ---------------------------------------------------------------------------------------------

#[async_trait]
pub trait GithubWriter: Send + Sync {
    /// Post a comment on an issue/PR (`owner/repo`, number). Returns the new comment's URL.
    async fn comment(&self, repo: &str, number: u64, body: &str) -> anyhow::Result<String>;
}

#[async_trait]
impl GithubWriter for ApiGithubClient {
    async fn comment(&self, repo: &str, number: u64, body: &str) -> anyhow::Result<String> {
        let token = self.token.clone();
        let (repo, body) = (repo.to_string(), body.to_string());
        tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let url = format!("https://api.github.com/repos/{repo}/issues/{number}/comments");
            let resp = ureq::post(&url)
                .timeout(std::time::Duration::from_secs(20))
                .set("Authorization", &format!("Bearer {token}"))
                .set("Accept", "application/vnd.github+json")
                .set("X-GitHub-Api-Version", "2022-11-28")
                .set("User-Agent", "yantrik-mind")
                .send_json(serde_json::json!({ "body": body }))?;
            let v: serde_json::Value = resp.into_json()?;
            Ok(v["html_url"].as_str().unwrap_or("(posted)").to_string())
        })
        .await?
    }
}

/// Records comments instead of posting them — for tests/dry-runs.
#[derive(Default)]
pub struct ScriptedGithubWriter {
    pub posted: std::sync::Mutex<Vec<(String, u64, String)>>,
}

impl ScriptedGithubWriter {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl GithubWriter for ScriptedGithubWriter {
    async fn comment(&self, repo: &str, number: u64, body: &str) -> anyhow::Result<String> {
        self.posted.lock().unwrap().push((repo.into(), number, body.into()));
        Ok(format!("https://github.com/{repo}/issues/{number}#scripted"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n(repo: &str, kind: &str, title: &str) -> GithubNotification {
        GithubNotification { repo: repo.into(), kind: kind.into(), title: title.into(), reason: "mention".into() }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scripted_notifications_respect_limit() {
        let c = ScriptedGithubClient::new(vec![n("a/b", "Issue", "one"), n("c/d", "PullRequest", "two")]);
        assert_eq!(c.notifications(1).await.unwrap().len(), 1);
    }

    #[test]
    fn digest_lists_repo_kind_and_title() {
        let d = render_github_digest(&[n("yantrikos/yantrik-os", "PullRequest", "Add logging")]);
        assert!(d.contains("yantrikos/yantrik-os") && d.contains("Add logging") && d.contains("PullRequest"));
        assert!(render_github_digest(&[]).contains("No unread"));
    }
}
