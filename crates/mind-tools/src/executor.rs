//! The `ActionExecutor` that performs gated outward effects. It is dumb on purpose: it only runs an
//! action AFTER the harm-gate + `ActionRuntime` have approved it (the runtime re-checks the gate
//! right before calling this). It never decides policy — it just does the thing and reports.

use std::sync::Arc;

use async_trait::async_trait;
use mind_types::{ActionExecutor, ActionRequest, MindError, Result};

use crate::github::GithubWriter;
use crate::mail::MailSender;

/// Dispatches an `ActionRequest` to the right transport by `intent.kind`. Capabilities the mind
/// hasn't been given a transport for simply error (and an action with no executor never "succeeds").
#[derive(Default)]
pub struct ToolActionExecutor {
    mail: Option<Arc<dyn MailSender>>,
    github: Option<Arc<dyn GithubWriter>>,
}

impl ToolActionExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mail_sender(mut self, sender: Arc<dyn MailSender>) -> Self {
        self.mail = Some(sender);
        self
    }

    pub fn with_github_writer(mut self, writer: Arc<dyn GithubWriter>) -> Self {
        self.github = Some(writer);
        self
    }
}

/// Parse a `owner/repo#123` action target into (repo, number).
fn parse_repo_target(target: &str) -> Option<(String, u64)> {
    let (repo, num) = target.rsplit_once('#')?;
    let number: u64 = num.trim().parse().ok()?;
    let repo = repo.trim();
    (repo.contains('/') && !repo.is_empty()).then(|| (repo.to_string(), number))
}

#[async_trait]
impl ActionExecutor for ToolActionExecutor {
    async fn perform(&self, req: &ActionRequest) -> Result<String> {
        match req.intent.kind.as_str() {
            "send_email" => {
                let sender = self
                    .mail
                    .as_ref()
                    .ok_or_else(|| MindError::Other("no mail sender configured".into()))?;
                let to = &req.intent.target;
                let subject = &req.intent.summary;
                let body = req.intent.payload.as_deref().unwrap_or("");
                sender
                    .send(to, subject, body)
                    .await
                    .map_err(|e| MindError::Other(e.to_string()))?;
                Ok(format!("email sent to {to}"))
            }
            "github_comment" => {
                let writer = self
                    .github
                    .as_ref()
                    .ok_or_else(|| MindError::Other("no github writer configured".into()))?;
                let (repo, number) = parse_repo_target(&req.intent.target)
                    .ok_or_else(|| MindError::Other(format!("bad github target '{}' (want owner/repo#N)", req.intent.target)))?;
                let body = req.intent.payload.as_deref().unwrap_or("");
                let url = writer
                    .comment(&repo, number, body)
                    .await
                    .map_err(|e| MindError::Other(e.to_string()))?;
                Ok(format!("comment posted on {repo}#{number}: {url}"))
            }
            other => Err(MindError::Other(format!("no executor for action kind '{other}'"))),
        }
    }
}
