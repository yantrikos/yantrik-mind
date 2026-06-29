//! homeassistant — the mind's smart-home capability. READ is here (entity states: climate, presence,
//! sensors, weather), safe like browsing; its output is UNTRUSTED (friendly-names are user-editable
//! text, wrapped by the caller). CONTROL (set thermostat, toggle a device) is an outward effect and is
//! deliberately NOT here — it must ride the harm-gate + confirmation when we add it.
//!
//! `HomeAssistantClient` is the injectable seam (real REST API vs scripted-for-tests). The real
//! transport is blocking `ureq` on the blocking pool, talking to HA's `/api/states` with a token.

use async_trait::async_trait;

/// One Home Assistant entity, reduced to what the world-model + a digest need.
#[derive(Debug, Clone, PartialEq)]
pub struct HaEntity {
    pub entity_id: String,
    pub domain: String, // climate / person / device_tracker / sensor / weather / light / ...
    pub state: String,
    pub friendly_name: String,
    pub attributes: serde_json::Value,
}

impl HaEntity {
    fn attr_str(&self, k: &str) -> Option<String> {
        self.attributes.get(k).and_then(|v| match v {
            serde_json::Value::String(s) => Some(s.clone()),
            serde_json::Value::Number(n) => Some(n.to_string()),
            serde_json::Value::Bool(b) => Some(b.to_string()),
            _ => None,
        })
    }
    fn name(&self) -> &str {
        if self.friendly_name.trim().is_empty() { &self.entity_id } else { self.friendly_name.trim() }
    }
}

#[async_trait]
pub trait HomeAssistantClient: Send + Sync {
    /// All entity states (the world snapshot). Caller treats the result as untrusted reference data.
    async fn states(&self) -> anyhow::Result<Vec<HaEntity>>;
}

/// Render the home snapshot as a compact, untrusted digest — the signals that actually matter
/// (who's home, climate, weather, anything notably "on"), not the dozens of internal sensors.
pub fn render_home_digest(entities: &[HaEntity]) -> String {
    if entities.is_empty() {
        return "Home Assistant returned no entities (nothing set up yet?).".to_string();
    }
    let mut lines: Vec<String> = Vec::new();

    // Presence: who's home.
    let presence: Vec<String> = entities
        .iter()
        .filter(|e| e.domain == "person" || e.domain == "device_tracker")
        .map(|e| format!("{}: {}", e.name(), e.state))
        .collect();
    if !presence.is_empty() {
        lines.push(format!("Presence — {}", presence.join(", ")));
    }

    // Climate: current vs target + what it's doing.
    for e in entities.iter().filter(|e| e.domain == "climate") {
        let cur = e.attr_str("current_temperature").map(|t| format!("{t}°")).unwrap_or_default();
        let target = e.attr_str("temperature").map(|t| format!(" → {t}°")).unwrap_or_default();
        let action = e.attr_str("hvac_action").map(|a| format!(" ({a})")).unwrap_or_default();
        lines.push(format!("{} — {}{}{} [{}]", e.name(), cur, target, action, e.state));
    }

    // Weather.
    for e in entities.iter().filter(|e| e.domain == "weather") {
        let temp = e.attr_str("temperature").map(|t| format!(", {t}°")).unwrap_or_default();
        lines.push(format!("{} — {}{}", e.name(), e.state, temp));
    }

    // Anything notably ON (lights, switches, on/open binary sensors, unlocked locks).
    let on: Vec<String> = entities
        .iter()
        .filter(|e| {
            matches!(e.domain.as_str(), "light" | "switch" | "fan" | "media_player")
                && matches!(e.state.as_str(), "on" | "playing")
                || (e.domain == "binary_sensor" && e.state == "on")
                || (e.domain == "lock" && e.state == "unlocked")
                || (e.domain == "cover" && e.state == "open")
        })
        .map(|e| format!("{} ({})", e.name(), e.state))
        .collect();
    if !on.is_empty() {
        lines.push(format!("Active/open — {}", on.join(", ")));
    }

    if lines.is_empty() {
        return format!("Home looks quiet — nothing notable right now ({} entities).", entities.len());
    }
    format!("{}\n({} entities total)", lines.join("\n"), entities.len())
}

/// Deterministic HA client for tests/evals.
pub struct ScriptedHomeAssistantClient {
    pub entities: Vec<HaEntity>,
}

impl ScriptedHomeAssistantClient {
    pub fn new(entities: Vec<HaEntity>) -> Self {
        Self { entities }
    }
}

#[async_trait]
impl HomeAssistantClient for ScriptedHomeAssistantClient {
    async fn states(&self) -> anyhow::Result<Vec<HaEntity>> {
        Ok(self.entities.clone())
    }
}

/// Real Home Assistant REST client (long-lived-token auth, read-only here).
pub struct ApiHomeAssistantClient {
    base_url: String,
    token: String,
}

impl ApiHomeAssistantClient {
    /// `base_url` like "http://192.168.4.97:8123"; `token` = a long-lived access token.
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Self {
        Self { base_url: base_url.into(), token: token.into() }
    }
}

#[async_trait]
impl HomeAssistantClient for ApiHomeAssistantClient {
    async fn states(&self) -> anyhow::Result<Vec<HaEntity>> {
        let (base, token) = (self.base_url.trim_end_matches('/').to_string(), self.token.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<HaEntity>> {
            let url = format!("{base}/api/states");
            let resp = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(15))
                .set("Authorization", &format!("Bearer {token}"))
                .set("Content-Type", "application/json")
                .call()?;
            let v: serde_json::Value = resp.into_json()?;
            let arr = v.as_array().cloned().unwrap_or_default();
            let mut out = Vec::with_capacity(arr.len());
            for e in arr {
                let entity_id = e["entity_id"].as_str().unwrap_or("").to_string();
                if entity_id.is_empty() {
                    continue;
                }
                let domain = entity_id.split('.').next().unwrap_or("").to_string();
                let state = e["state"].as_str().unwrap_or("").to_string();
                let friendly_name = e["attributes"]["friendly_name"].as_str().unwrap_or("").to_string();
                let attributes = e.get("attributes").cloned().unwrap_or_else(|| serde_json::json!({}));
                out.push(HaEntity { entity_id, domain, state, friendly_name, attributes });
            }
            Ok(out)
        })
        .await?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ent(id: &str, state: &str, name: &str, attrs: serde_json::Value) -> HaEntity {
        HaEntity {
            entity_id: id.to_string(),
            domain: id.split('.').next().unwrap().to_string(),
            state: state.to_string(),
            friendly_name: name.to_string(),
            attributes: attrs,
        }
    }

    #[test]
    fn digest_surfaces_the_signals_that_matter() {
        let ents = vec![
            ent("person.pranab", "home", "Pranab", serde_json::json!({})),
            ent("climate.living_room", "heat", "Living Room", serde_json::json!({"current_temperature": 19.5, "temperature": 22, "hvac_action": "heating"})),
            ent("weather.forecast_home", "clear-night", "Forecast Home", serde_json::json!({"temperature": 14})),
            ent("light.porch", "on", "Porch Light", serde_json::json!({})),
            ent("sensor.cpu", "3.1", "CPU", serde_json::json!({})), // noise — must NOT clutter
        ];
        let d = render_home_digest(&ents);
        assert!(d.contains("Pranab: home"), "presence: {d}");
        assert!(d.contains("Living Room") && d.contains("19.5°") && d.contains("22°") && d.contains("heating"), "climate: {d}");
        assert!(d.contains("clear-night") && d.contains("14°"), "weather: {d}");
        assert!(d.contains("Porch Light"), "active light: {d}");
        assert!(!d.contains("CPU"), "internal sensor noise must be filtered: {d}");
        assert!(d.contains("5 entities total"));
    }

    #[test]
    fn empty_and_quiet_homes_are_handled() {
        assert!(render_home_digest(&[]).contains("no entities"));
        let quiet = vec![ent("sensor.cpu", "3.1", "CPU", serde_json::json!({}))];
        assert!(render_home_digest(&quiet).contains("quiet"));
    }
}
