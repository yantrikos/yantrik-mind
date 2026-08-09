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
    home: Option<Arc<dyn crate::HomeWriter>>,
}

/// Domains a HAND may NEVER touch through `ha_call`, allowlist or not: physical security and
/// safety. Unlocking a door because a model was confident is not a failure mode this system will
/// ever have; those stay human-only until someone designs a much stronger ceremony than an
/// allowlist.
const HA_DENY_DOMAINS: [&str; 5] = ["lock", "cover", "alarm_control_panel", "camera", "siren"];

/// entity_id against the operator's allowlist (comma-separated globs: `light.*`, `switch.porch`,
/// `media_player.living_*`). No allowlist configured = NOTHING allowed — the hand is opt-in per
/// entity class, fail-closed by construction.
pub(crate) fn ha_entity_allowed(entity: &str, allowlist: &str) -> bool {
    allowlist.split(',').map(str::trim).filter(|p| !p.is_empty()).any(|pat| {
        match pat.strip_suffix('*') {
            Some(prefix) => entity.starts_with(prefix),
            None => entity == pat,
        }
    })
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

    /// Grant the HOME hand. The executor still enforces the deny-domains and the entity allowlist
    /// on every call — the grant is a transport, not a permission.
    pub fn with_home_writer(mut self, w: Arc<dyn crate::HomeWriter>) -> Self {
        self.home = Some(w);
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
            // A confirmed home-control action: target = "domain.service entity_id" (e.g.
            // "light.turn_off light.porch"). POLICY ENFORCED HERE, at execution time, regardless
            // of what any upstream layer believed: security domains are denied outright, and the
            // entity must match the operator's allowlist — which, unset, allows NOTHING.
            "ha_call" => {
                let home = self.home.as_ref().ok_or_else(|| MindError::Other("no home writer configured".into()))?;
                let (svc, entity) = req
                    .intent
                    .target
                    .split_once(' ')
                    .ok_or_else(|| MindError::Other(format!("bad ha target '{}' (want 'domain.service entity_id')", req.intent.target)))?;
                let (domain, service) = svc
                    .split_once('.')
                    .ok_or_else(|| MindError::Other(format!("bad service '{svc}' (want domain.service)")))?;
                let entity = entity.trim();
                let entity_domain = entity.split('.').next().unwrap_or("");
                if HA_DENY_DOMAINS.contains(&domain) || HA_DENY_DOMAINS.contains(&entity_domain) {
                    return Err(MindError::Other(format!(
                        "'{entity_domain}' is a security domain — I don't operate locks, covers, alarms, cameras or sirens. Ever."
                    )));
                }
                let allow = std::env::var("YM_HA_ACTIONS_ALLOW").unwrap_or_default();
                if !ha_entity_allowed(entity, &allow) {
                    return Err(MindError::Other(format!(
                        "'{entity}' is not on the home-control allowlist (YM_HA_ACTIONS_ALLOW). The hand is opt-in per entity."
                    )));
                }
                let receipt = home
                    .call_service(domain, service, entity)
                    .await
                    .map_err(|e| MindError::Other(e.to_string()))?;
                Ok(format!("done: {receipt}"))
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

#[cfg(test)]
mod ha_policy_tests {
    use super::*;
    use mind_types::{ActionIntent, Capability, RiskLevel};

    struct RecordingHome(std::sync::Mutex<Vec<String>>);
    #[async_trait]
    impl crate::HomeWriter for RecordingHome {
        async fn call_service(&self, d: &str, s: &str, e: &str) -> anyhow::Result<String> {
            self.0.lock().unwrap().push(format!("{d}.{s} {e}"));
            Ok(format!("{d}.{s} → {e}"))
        }
    }

    fn ha_req(target: &str) -> ActionRequest {
        ActionRequest {
            id: "h1".into(),
            actor: "mind".into(),
            justification: "test".into(),
            created_ms: 0,
            intent: ActionIntent {
                kind: "ha_call".into(),
                target: target.into(),
                summary: "home".into(),
                payload: None,
                capabilities: vec![Capability::Network],
                risk: RiskLevel::Medium,
                reversible: true,
            },
        }
    }

    /// Env vars are process-global; these tests must not interleave.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn exec_with_home() -> (ToolActionExecutor, Arc<RecordingHome>) {
        let home = Arc::new(RecordingHome(std::sync::Mutex::new(Vec::new())));
        (ToolActionExecutor::new().with_home_writer(home.clone()), home)
    }

    /// THE line that must never move: security domains are refused regardless of any allowlist.
    #[tokio::test]
    async fn security_domains_are_denied_even_when_allowlisted() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("YM_HA_ACTIONS_ALLOW", "lock.*,light.*,cover.*");
        let (ex, home) = exec_with_home();
        for t in ["lock.unlock lock.front_door", "cover.open_cover cover.garage", "light.turn_on lock.front_door"] {
            let r = ex.perform(&ha_req(t)).await;
            assert!(r.is_err(), "{t} must be refused");
            assert!(r.unwrap_err().to_string().contains("security domain"), "{t}: wrong refusal reason");
        }
        assert!(home.0.lock().unwrap().is_empty(), "nothing may reach the transport");
        std::env::remove_var("YM_HA_ACTIONS_ALLOW");
    }

    /// No allowlist = NOTHING allowed. The hand is opt-in per entity, fail-closed by construction.
    #[tokio::test]
    async fn unset_allowlist_allows_nothing() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("YM_HA_ACTIONS_ALLOW");
        let (ex, home) = exec_with_home();
        let r = ex.perform(&ha_req("light.turn_off light.porch")).await;
        assert!(r.is_err() && r.unwrap_err().to_string().contains("allowlist"));
        assert!(home.0.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn allowlisted_entity_executes_and_glob_scopes_it() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::set_var("YM_HA_ACTIONS_ALLOW", "light.*, switch.porch");
        let (ex, home) = exec_with_home();
        assert!(ex.perform(&ha_req("light.turn_off light.kitchen")).await.is_ok());
        assert!(ex.perform(&ha_req("switch.turn_on switch.porch")).await.is_ok());
        let r = ex.perform(&ha_req("switch.turn_on switch.heater")).await;
        assert!(r.is_err(), "glob must not leak past its prefix");
        assert_eq!(home.0.lock().unwrap().len(), 2);
        std::env::remove_var("YM_HA_ACTIONS_ALLOW");
    }

    #[test]
    fn glob_matching_is_prefix_only_and_exact_otherwise() {
        assert!(ha_entity_allowed("light.kitchen", "light.*"));
        assert!(ha_entity_allowed("switch.porch", "light.*, switch.porch"));
        assert!(!ha_entity_allowed("switch.porch_heater", "switch.porch"));
        assert!(!ha_entity_allowed("light.kitchen", ""));
    }
}
