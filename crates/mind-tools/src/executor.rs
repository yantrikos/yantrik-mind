//! The `ActionExecutor` that performs gated outward effects. It is dumb on purpose: it only runs an
//! action AFTER the harm-gate + `ActionRuntime` have approved it (the runtime re-checks the gate
//! right before calling this). It never decides policy — it just does the thing and reports.

use std::sync::Arc;

use async_trait::async_trait;
use mind_types::{ActionExecutor, ActionRequest, MindError, Result};

use crate::mail::MailSender;

/// Dispatches an `ActionRequest` to the right transport by `intent.kind`. Capabilities the mind
/// hasn't been given a transport for simply error (and an action with no executor never "succeeds").
pub struct ToolActionExecutor {
    mail: Option<Arc<dyn MailSender>>,
}

impl Default for ToolActionExecutor {
    fn default() -> Self {
        Self { mail: None }
    }
}

impl ToolActionExecutor {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_mail_sender(mut self, sender: Arc<dyn MailSender>) -> Self {
        self.mail = Some(sender);
        self
    }
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
            other => Err(MindError::Other(format!("no executor for action kind '{other}'"))),
        }
    }
}
