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

/// Deterministic, GROUNDED home-anomaly detection over a state snapshot — the proactive surface's
/// rule set. Returns `(dedup_key, human_message)` per fired alert. No LLM (no confabulation), and
/// CONSERVATIVE: away-alerts fire only when presence is *explicitly* away (someone not_home, nobody
/// home) — never on merely-unknown presence (avoids false nags). This is the cross-domain magic a
/// flat assistant can't do: presence × device, grounded in real state.
pub fn home_alerts(entities: &[HaEntity]) -> Vec<(String, String)> {
    let presence: Vec<&HaEntity> = entities.iter().filter(|e| e.domain == "person" || e.domain == "device_tracker").collect();
    let anyone_home = presence.iter().any(|e| e.state == "home");
    let anyone_away = presence.iter().any(|e| e.state == "not_home" || e.state == "away");
    let away = !anyone_home && anyone_away;

    let mut out: Vec<(String, String)> = Vec::new();
    if away {
        for e in entities.iter().filter(|e| e.domain == "media_player" && matches!(e.state.as_str(), "playing" | "on")) {
            out.push((format!("tv_on_away:{}", e.entity_id), format!("📺 {} is on but nobody's home.", e.name())));
        }
        for e in entities.iter().filter(|e| e.domain == "climate") {
            if let Some(a) = e.attr_str("hvac_action") {
                if a == "heating" || a == "cooling" {
                    out.push((format!("climate_away:{}", e.entity_id), format!("🌡 {} is {} but nobody's home.", e.name(), a)));
                }
            }
        }
        for e in entities.iter().filter(|e| e.domain == "lock" && e.state == "unlocked") {
            out.push((format!("unlocked_away:{}", e.entity_id), format!("🔓 {} is unlocked while you're out.", e.name())));
        }
    }
    // Internet down (a connectivity binary_sensor reading off) — relevant regardless of presence.
    for e in entities.iter().filter(|e| e.domain == "binary_sensor" && e.state == "off") {
        if e.attr_str("device_class").as_deref() == Some("connectivity") {
            out.push((format!("net_down:{}", e.entity_id), format!("📡 {} — the internet looks down.", e.name())));
        }
    }
    // Low printer ink (<15%).
    for e in entities.iter().filter(|e| e.entity_id.contains("ink")) {
        if let Ok(v) = e.state.parse::<f64>() {
            if v < 15.0 {
                out.push((format!("ink_low:{}", e.entity_id), format!("🖨 {} is low ({}%).", e.name(), e.state)));
            }
        }
    }
    out
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

    #[test]
    fn home_alerts_fire_only_when_explicitly_away() {
        let tv = ent("media_player.tv", "playing", "Living Room TV", serde_json::json!({}));
        let net_down = ent("binary_sensor.wan", "off", "WAN", serde_json::json!({ "device_class": "connectivity" }));
        // AWAY (person not_home, nobody home): TV-on-while-away fires, plus the always-on net-down rule
        let away = vec![ent("person.pranab", "not_home", "Pranab", serde_json::json!({})), tv.clone(), net_down.clone()];
        let keys: Vec<String> = home_alerts(&away).into_iter().map(|(k, _)| k).collect();
        assert!(keys.iter().any(|k| k.starts_with("tv_on_away")), "TV-on-while-away fires: {keys:?}");
        assert!(keys.iter().any(|k| k.starts_with("net_down")), "internet-down fires regardless of presence");
        // HOME: the same TV does NOT fire an away-alert (presence × device), but net-down still does
        let home = vec![ent("person.pranab", "home", "Pranab", serde_json::json!({})), tv.clone(), net_down.clone()];
        let hk: Vec<String> = home_alerts(&home).into_iter().map(|(k, _)| k).collect();
        assert!(!hk.iter().any(|k| k.starts_with("tv_on_away")), "no away-alert when home: {hk:?}");
        assert!(hk.iter().any(|k| k.starts_with("net_down")));
        // UNKNOWN presence (no explicit away) → conservative: no away-alerts (no false nags)
        let unknown = vec![ent("person.pranab", "unknown", "Pranab", serde_json::json!({})), tv];
        assert!(home_alerts(&unknown).is_empty(), "unknown presence must not nag");
    }

    #[test]
    fn home_alerts_low_ink() {
        let low = vec![ent("sensor.printer_black_ink", "8", "Printer black ink", serde_json::json!({}))];
        assert!(home_alerts(&low).iter().any(|(k, _)| k.starts_with("ink_low")), "low ink fires");
        let ok = vec![ent("sensor.printer_black_ink", "80", "Printer black ink", serde_json::json!({}))];
        assert!(home_alerts(&ok).is_empty(), "healthy ink is quiet");
    }
}
