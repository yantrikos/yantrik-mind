//! Smart home as a dispatchable capability — the Home Assistant digest, routed by the registry.

/// Home as a capability: `ym home` + the home/home_status agent tools. The unconfigured CLI case
/// returns None so the legacy fallback answers exactly as it did before the port.
pub struct HomeCapability;

#[async_trait::async_trait]
impl crate::plugins::CapabilityHandler for HomeCapability {
    fn id(&self) -> &str {
        "home"
    }

    async fn handle_command(
        &self,
        host: &super::ConversationEngine,
        cmd: &str,
        _rest: &str,
    ) -> Option<String> {
        match cmd {
            // same guard the old arm had: with no Home Assistant configured, fall through
            "home" | "house" if host.home.is_some() => {
                Some(host.run_agent_tool("home", &serde_json::json!({})).await)
            }
            _ => None,
        }
    }

    async fn handle_tool(
        &self,
        host: &super::ConversationEngine,
        tool: &str,
        _args: &serde_json::Value,
    ) -> Option<String> {
        Some(match tool {
            "home" | "home_status" | "house" | "smart_home" => match &host.home {
                Some(h) => match h.states().await {
                    Ok(ents) => mind_tools::render_home_digest(&ents),
                    Err(e) => format!("(couldn't reach Home Assistant: {e})"),
                },
                None => "(smart home not configured — set YM_HA_URL + YM_HA_TOKEN)".to_string(),
            },
            _ => return None,
        })
    }
}
