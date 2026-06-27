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

#[async_trait]
pub trait GithubClient: Send + Sync {
    /// Unread notifications (most recent first), capped at `limit`.
    async fn notifications(&self, limit: usize) -> anyhow::Result<Vec<GithubNotification>>;
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
