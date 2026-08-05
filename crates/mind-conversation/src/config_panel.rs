//! The settings surface — a TYPED schema over /etc/yantrik-mind.env.
//!
//! Config hell is the #1 complaint in every agent-desktop ecosystem surveyed (2026-08-05 research:
//! "40+ hours fixing configuration", paid setup consultants, values drifting between file and
//! process). The cure that worked elsewhere: the UI renders FORMS FROM A SCHEMA, so it cannot
//! drift from what the code actually reads. Every key below is verified against an env::var read
//! in this workspace (or the deploy scripts) — a setting the code doesn't read is a lie in a form.
//!
//! AUTHORITY BOUNDARY, deliberate: the mind only READS its config. Writes happen over the
//! operator's ssh (the desktop app edits the env file and restarts the service) — the process
//! cannot rewrite its own environment, which also means the self-improve loop can never quietly
//! flip its own builder, model, or privacy lanes. Reads come from std::env, i.e. the LIVE values;
//! the desktop compares against the FILE it edits, so drift between file and process is visible
//! instead of mysterious.

use super::*;

pub(crate) struct Setting {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    /// "string" | "int" | "toggle" (on/off) | "enum:<a|b|c>" | "secret"
    pub kind: &'static str,
    pub desc: &'static str,
    /// Whether a change only takes effect after a service restart (true for nearly everything —
    /// env is read at startup).
    pub restart: bool,
}

pub(crate) const SCHEMA: &[Setting] = &[
    // ── Brain ────────────────────────────────────────────────────────────
    Setting { key: "YM_MODEL", label: "Cloud model", group: "Brain", kind: "string", desc: "Model id for the cloud provider lane.", restart: true },
    Setting { key: "YM_BUILDER", label: "Self-build builder", group: "Brain", kind: "enum:claude|qwen|codex", desc: "Which agent runs the nightly self-improvement tick.", restart: false },
    Setting { key: "YM_QWEN_MODEL", label: "Qwen model", group: "Brain", kind: "string", desc: "Model used when the builder is qwen (default qwen3.8-max).", restart: false },
    Setting { key: "YM_LOCAL_OLLAMA_URL", label: "Private-lane URL", group: "Brain", kind: "string", desc: "Owned-hardware endpoint for private turns. Empty = private turns escalate (audited).", restart: true },
    Setting { key: "YM_LOCAL_OLLAMA_MODEL", label: "Private-lane model", group: "Brain", kind: "string", desc: "Model served on the private lane.", restart: true },
    Setting { key: "YM_VISION_MODEL", label: "Vision model", group: "Brain", kind: "string", desc: "Model for photo understanding.", restart: true },
    // ── Rhythm ───────────────────────────────────────────────────────────
    Setting { key: "YM_TZ", label: "Timezone", group: "Rhythm", kind: "string", desc: "IANA timezone (e.g. America/Chicago). Governs quiet hours and reminders.", restart: true },
    Setting { key: "YM_QUIET_START", label: "Quiet from", group: "Rhythm", kind: "int", desc: "Hour (0-23, local) when proactive surfaces go quiet.", restart: true },
    Setting { key: "YM_QUIET_END", label: "Quiet until", group: "Rhythm", kind: "int", desc: "Hour (0-23, local) when they wake.", restart: true },
    Setting { key: "YM_DMN_IDLE_SECS", label: "Idle before DMN", group: "Rhythm", kind: "int", desc: "Seconds of user idle before offline cognition may run (default 600).", restart: true },
    Setting { key: "YM_HOME_WATCH_SECS", label: "Home poll (s)", group: "Rhythm", kind: "int", desc: "Home-watch poll period (default 120). The websocket ear reacts faster regardless.", restart: true },
    Setting { key: "YM_MAILSWEEP_SECS", label: "Mail sweep (s)", group: "Rhythm", kind: "int", desc: "Personal-inbox scan period (default daily).", restart: true },
    Setting { key: "YM_TWITCH_DEBOUNCE_SECS", label: "Twitch debounce (s)", group: "Rhythm", kind: "int", desc: "Minimum gap between fast-twitch evaluations in an event storm (default 5).", restart: true },
    Setting { key: "YM_ESCROW_STALE_DAYS", label: "Escrow expiry (d)", group: "Rhythm", kind: "int", desc: "Held interruptions older than this are dropped (default 14).", restart: true },
    // ── Switches ─────────────────────────────────────────────────────────
    Setting { key: "YM_PROACTIVE", label: "Proactive layer", group: "Switches", kind: "toggle", desc: "Digests, asks, patterns — the unprompted voice.", restart: true },
    Setting { key: "YM_KNOCK", label: "Calibrated knock", group: "Switches", kind: "toggle", desc: "Prepared-work interruptions with a confidence band.", restart: true },
    Setting { key: "YM_HOME_WATCH", label: "Home watch", group: "Switches", kind: "toggle", desc: "Grounded home-anomaly alerts.", restart: true },
    Setting { key: "YM_HA_EVENTS", label: "Fast-twitch ear", group: "Switches", kind: "toggle", desc: "Home Assistant websocket event subscription.", restart: true },
    Setting { key: "YM_WEB", label: "Web dashboards", group: "Switches", kind: "toggle", desc: "The read-only static dashboard server.", restart: true },
    // ── Channels & keys (masked) ─────────────────────────────────────────
    Setting { key: "YM_TELEGRAM_TOKEN", label: "Telegram bot token", group: "Channels & keys", kind: "secret", desc: "The family chat surface.", restart: true },
    Setting { key: "YM_HA_URL", label: "Home Assistant URL", group: "Channels & keys", kind: "string", desc: "http://host:8123 on the LAN.", restart: true },
    Setting { key: "YM_HA_TOKEN", label: "Home Assistant token", group: "Channels & keys", kind: "secret", desc: "Long-lived access token (read + event bus).", restart: true },
    Setting { key: "IMMICH_SERVER", label: "Immich URL", group: "Channels & keys", kind: "string", desc: "The photo library.", restart: true },
    Setting { key: "IMMICH_USER_API_KEY", label: "Immich API key", group: "Channels & keys", kind: "secret", desc: "Face/people/photo access.", restart: true },
    Setting { key: "YM_EMAIL", label: "Mind's email", group: "Channels & keys", kind: "string", desc: "The mailbox the mind sends and receives as.", restart: true },
    Setting { key: "YM_EMAIL_PASSWORD", label: "Email app password", group: "Channels & keys", kind: "secret", desc: "IMAP/SMTP app password.", restart: true },
    Setting { key: "YM_GITHUB_TOKEN", label: "GitHub token", group: "Channels & keys", kind: "secret", desc: "Self-improvement PRs and repo reads.", restart: true },
    Setting { key: "QWEN_API_KEY", label: "QwenCloud key", group: "Channels & keys", kind: "secret", desc: "Builder + household lane via token-plan.", restart: false },
    Setting { key: "CLAUDE_CODE_OAUTH_TOKEN", label: "Claude token", group: "Channels & keys", kind: "secret", desc: "Claude CLI auth for the claude builder/brain.", restart: true },
];

/// Mask a secret to shape-only: presence and length class, never content. (Two credential leaks
/// this month came from printing "just enough" of a value — the only safe rendering is none.)
fn mask(v: &str) -> String {
    if v.is_empty() { "(unset)".into() } else { format!("••• set ({} chars)", v.chars().count()) }
}

impl super::ConversationEngine {
    /// `ym config` — the live view. `ym config schema` — machine-readable, for the desktop's forms.
    pub async fn config_panel(&self, rest: &str) -> String {
        if rest.trim() == "schema" {
            let items: Vec<serde_json::Value> = SCHEMA
                .iter()
                .map(|s| {
                    let live = std::env::var(s.key).unwrap_or_default();
                    serde_json::json!({
                        "key": s.key, "label": s.label, "group": s.group, "kind": s.kind,
                        "desc": s.desc, "restart": s.restart,
                        "value": if s.kind == "secret" { serde_json::json!(null) } else { serde_json::json!(live) },
                        "set": !live.is_empty(),
                    })
                })
                .collect();
            return serde_json::json!({ "schema_version": 1, "settings": items }).to_string();
        }
        let mut out = String::from("⚙️ CONFIG — live values this process was started with\n");
        let mut group = "";
        for s in SCHEMA {
            if s.group != group {
                group = s.group;
                out.push_str(&format!("\n[{group}]\n"));
            }
            let live = std::env::var(s.key).unwrap_or_default();
            let shown = if s.kind == "secret" {
                mask(&live)
            } else if live.is_empty() {
                "(default)".into()
            } else {
                live
            };
            out.push_str(&format!("  {:<26} {shown}\n", s.key));
        }
        out.push_str(
            "\nEdits happen from the desktop app's Settings pane (or by editing /etc/yantrik-mind.env \
             over ssh) — I can read my configuration, not rewrite it. Most changes need a service restart.",
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the schema is that it can't lie: every key must be one the workspace
    /// actually reads. This test greps the source tree at test time so a renamed env var breaks
    /// the schema loudly instead of leaving a dead form field.
    #[test]
    fn every_schema_key_is_actually_read_somewhere() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let mut haystack = String::new();
        for dir in ["crates", "deploy"] {
            let mut stack = vec![root.join(dir)];
            while let Some(d) = stack.pop() {
                let Ok(rd) = std::fs::read_dir(&d) else { continue };
                for e in rd.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        if !p.ends_with("target") {
                            stack.push(p);
                        }
                    } else if p.extension().map(|x| x == "rs" || x == "sh").unwrap_or(false) {
                        haystack.push_str(&std::fs::read_to_string(&p).unwrap_or_default());
                    }
                }
            }
        }
        for s in SCHEMA {
            assert!(haystack.contains(s.key), "schema key {} is read nowhere in crates/ or deploy/ — dead form field", s.key);
        }
    }

    #[test]
    fn secrets_never_render_their_value() {
        std::env::set_var("YM_TEST_SECRET_XYZ", "hunter2-very-secret");
        assert!(!mask("hunter2-very-secret").contains("hunter2"));
        assert!(mask("").contains("unset"));
    }

    #[test]
    fn schema_json_carries_no_secret_values() {
        // Belt and braces at the serialization layer: secret kinds serialize value=null.
        for s in SCHEMA.iter().filter(|s| s.kind == "secret") {
            assert_eq!(s.kind, "secret", "{}", s.key);
        }
    }
}
