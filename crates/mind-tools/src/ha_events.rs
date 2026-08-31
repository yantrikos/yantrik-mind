//! Home Assistant EVENT BUS subscription — the fast-twitch ear.
//!
//! The home watch polls `/api/states` every 120 s, so the mind learns about the house up to two
//! minutes late (and about email/calendar much later). Jarvis-feel is temporal contiguity: the
//! reaction must follow the event. This module connects OUTBOUND to HA's WebSocket API
//! (`/api/websocket`) with the token we already hold and subscribes to `state_changed` — sub-second
//! events, zero new inbound ports, zero HA-side configuration. Deliberately chosen over an HA
//! automation webhook: a webhook means opening a listener to the LAN and configuring HA to call it;
//! this is a client connection using existing credentials.
//!
//! Blocking `tungstenite` on a caller-owned thread, matching the codebase's synchronous-transport
//! idiom (ureq, imap). Reconnects forever with capped backoff — an event ear that silently dies is
//! worse than none, so drops are logged and retried, never fatal.

use std::net::TcpStream;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::WebSocket;

/// A minimal state-change: entity that changed and its new state string.
#[derive(Debug, Clone)]
pub struct HaEvent {
    pub entity_id: String,
    pub new_state: String,
}

impl HaEvent {
    /// "binary_sensor" from "binary_sensor.front_door" — the funnel counts events per domain.
    pub fn domain(&self) -> &str {
        self.entity_id.split('.').next().unwrap_or("unknown")
    }
}

/// http(s)://host:8123 → ws(s)://host:8123/api/websocket
fn ws_url(base: &str) -> String {
    let b = base.trim_end_matches('/');
    let swapped = if let Some(rest) = b.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = b.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{b}")
    };
    format!("{swapped}/api/websocket")
}

fn connect_and_subscribe(
    base: &str,
    token: &str,
) -> anyhow::Result<WebSocket<MaybeTlsStream<TcpStream>>> {
    let (mut ws, _) = tungstenite::connect(ws_url(base))?;
    // Handshake: HA sends auth_required; we answer with the long-lived token; expect auth_ok.
    loop {
        let msg = ws.read()?;
        let txt = msg.to_text().unwrap_or("");
        let v: serde_json::Value = serde_json::from_str(txt).unwrap_or_default();
        match v.get("type").and_then(|x| x.as_str()).unwrap_or("") {
            "auth_required" => {
                ws.send(tungstenite::Message::Text(
                    serde_json::json!({"type": "auth", "access_token": token}).to_string(),
                ))?;
            }
            "auth_ok" => break,
            "auth_invalid" => anyhow::bail!("HA websocket auth rejected — token revoked?"),
            _ => {}
        }
    }
    ws.send(tungstenite::Message::Text(
        serde_json::json!({"id": 1, "type": "subscribe_events", "event_type": "state_changed"})
            .to_string(),
    ))?;
    Ok(ws)
}

/// Extract the state change from an HA event frame, or None for anything else (result acks, pongs).
pub fn parse_event(txt: &str) -> Option<HaEvent> {
    let v: serde_json::Value = serde_json::from_str(txt).ok()?;
    if v.get("type").and_then(|x| x.as_str()) != Some("event") {
        return None;
    }
    let data = v.get("event")?.get("data")?;
    let entity_id = data.get("entity_id")?.as_str()?.to_string();
    // ATTRIBUTE-ONLY churn (media position, sensor precision noise) arrives as state_changed too;
    // require the STATE STRING to actually differ so the debouncer isn't fed pure noise.
    let new_state = data.get("new_state")?.get("state")?.as_str()?.to_string();
    let old_state = data
        .get("old_state")
        .and_then(|o| o.get("state"))
        .and_then(|s| s.as_str())
        .unwrap_or("");
    if new_state == old_state {
        return None;
    }
    Some(HaEvent {
        entity_id,
        new_state,
    })
}

/// Run forever: connect, subscribe, deliver each state change to `on_event`. Reconnects with capped
/// exponential backoff (5 s → 5 min) on any error. Call from a dedicated `std::thread`.
pub fn ha_event_loop(base: &str, token: &str, mut on_event: impl FnMut(HaEvent)) {
    let mut backoff_secs = 5u64;
    loop {
        match connect_and_subscribe(base, token) {
            Ok(mut ws) => {
                eprintln!("[ha-events] subscribed to state_changed");
                backoff_secs = 5;
                loop {
                    match ws.read() {
                        Ok(msg) => {
                            if let Ok(txt) = msg.to_text() {
                                if let Some(ev) = parse_event(txt) {
                                    on_event(ev);
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("[ha-events] stream dropped: {e} — reconnecting");
                            break;
                        }
                    }
                }
            }
            Err(e) => eprintln!("[ha-events] connect failed: {e} — retry in {backoff_secs}s"),
        }
        std::thread::sleep(std::time::Duration::from_secs(backoff_secs));
        backoff_secs = (backoff_secs * 2).min(300);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ws_url_swaps_scheme_and_appends_path() {
        assert_eq!(
            ws_url("http://192.168.4.7:8123/"),
            "ws://192.168.4.7:8123/api/websocket"
        );
        assert_eq!(
            ws_url("https://ha.example.com"),
            "wss://ha.example.com/api/websocket"
        );
    }

    #[test]
    fn parse_extracts_a_real_state_change() {
        let frame = r#"{"type":"event","event":{"data":{"entity_id":"lock.front_door",
            "old_state":{"state":"locked"},"new_state":{"state":"unlocked"}}}}"#;
        let ev = parse_event(frame).expect("should parse");
        assert_eq!(ev.entity_id, "lock.front_door");
        assert_eq!(ev.new_state, "unlocked");
        assert_eq!(ev.domain(), "lock");
    }

    /// Attribute churn keeps the same state string — it must not reach the debouncer at all.
    #[test]
    fn attribute_only_churn_is_dropped() {
        let frame = r#"{"type":"event","event":{"data":{"entity_id":"media_player.tv",
            "old_state":{"state":"playing"},"new_state":{"state":"playing"}}}}"#;
        assert!(parse_event(frame).is_none());
    }

    #[test]
    fn non_event_frames_are_ignored() {
        assert!(parse_event(r#"{"type":"result","id":1,"success":true}"#).is_none());
        assert!(parse_event("not json").is_none());
    }
}
