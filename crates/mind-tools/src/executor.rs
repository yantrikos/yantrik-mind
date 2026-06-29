//! The `ActionExecutor` that performs gated outward effects. It is dumb on purpose: it only runs an
//! action AFTER the harm-gate + `ActionRuntime` have approved it (the runtime re-checks the gate
//! right before calling this). It never decides policy — it just does the thing and reports.

use std::sync::Arc;

use async_trait::async_trait;
use mind_types::{ActionExecutor, ActionRequest, MindError, Result};

use crate::github::GithubWriter;
use crate::mail::MailSender;
use crate::mcp::McpHub;

/// Dispatches an `ActionRequest` to the right transport by `intent.kind`. Capabilities the mind
/// hasn't been given a transport for simply error (and an action with no executor never "succeeds").
#[derive(Default)]
pub struct ToolActionExecutor {
    mail: Option<Arc<dyn MailSender>>,
    github: Option<Arc<dyn GithubWriter>>,
    mcp: Option<Arc<McpHub>>,
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

    /// Wire the MCP hub so a confirmed `mcp_call` action can run a mutating integration tool.
    pub fn with_mcp_hub(mut self, hub: Arc<McpHub>) -> Self {
        self.mcp = Some(hub);
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
            // A confirmed MCP write: target = the qualified tool id (`mcp.<server>.<tool>`), payload =
            // the JSON arguments. The harm-gate has already approved (and `execute` re-checks it). The
            // blocking JSON-RPC call runs on the blocking pool.
            "mcp_call" => {
                let hub = self.mcp.as_ref().ok_or_else(|| MindError::Other("no MCP hub configured".into()))?.clone();
                let qualified = req.intent.target.clone();
                let args: serde_json::Value =
                    req.intent.payload.as_deref().and_then(|p| serde_json::from_str(p).ok()).unwrap_or(serde_json::json!({}));
                tokio::task::spawn_blocking(move || hub.call_blocking(&qualified, &args))
                    .await
                    .map_err(|e| MindError::Other(e.to_string()))?
                    .map_err(|e| MindError::Other(e.to_string()))
            }
            other => Err(MindError::Other(format!("no executor for action kind '{other}'"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mind_types::{ActionIntent, Capability, RiskLevel};

    fn mcp_req(target: &str, args: &str) -> ActionRequest {
        ActionRequest {
            id: "r1".into(),
            actor: "mind".into(),
            intent: ActionIntent {
                kind: "mcp_call".into(),
                target: target.into(),
                summary: "run a tool".into(),
                payload: Some(args.into()),
                capabilities: vec![Capability::Network],
                risk: RiskLevel::Medium,
                reversible: false,
            },
            justification: "test".into(),
            created_ms: 0,
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mcp_call_without_a_hub_errors_cleanly() {
        // No hub configured → the action fails with a clear error (never a silent "success").
        let exec = ToolActionExecutor::new();
        let err = exec.perform(&mcp_req("mcp.github.create_issue", "{}")).await.unwrap_err();
        assert!(err.to_string().contains("no MCP hub"), "got: {err}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unknown_kind_has_no_executor() {
        let exec = ToolActionExecutor::new();
        let mut bad = mcp_req("x", "{}");
        bad.intent.kind = "teleport".into();
        let err = exec.perform(&bad).await.unwrap_err();
        assert!(err.to_string().contains("no executor for action kind 'teleport'"));
    }
}
