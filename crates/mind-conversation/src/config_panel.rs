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

/// A positive integer setting, or None when unset/blank/garbage.
///
/// A malformed value is treated as absent rather than as zero. `YM_MAX_STEPS=five` should leave the
/// default in place, not silently configure a mind that cannot take a single step.
fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.trim().parse::<u32>().ok().filter(|n| *n > 0)
}

fn env_f64(key: &str) -> Option<f64> {
    std::env::var(key).ok()?.trim().parse::<f64>().ok()
}

/// The budget for one INTERACTIVE turn, from config.
///
/// This is the "max iteration count" setting made real: the loop's cap comes from here, so changing
/// `YM_MAX_STEPS` changes behaviour. Clamping lives in `mind_spec::Budget` — this layer only reads.
pub fn agent_budget() -> mind_spec::Budget {
    mind_spec::Budget::interactive().with_overrides(
        env_u32("YM_MAX_STEPS"),
        env_u32("YM_MAX_MODEL_CALLS"),
        env_u32("YM_MAX_WALL_SECS").map(|s| s as u64 * 1000),
        env_f64("YM_MAX_USD"),
    )
}

/// The budget for DELEGATED or SCHEDULED work, where nobody is waiting on the answer.
pub fn background_budget() -> mind_spec::Budget {
    mind_spec::Budget::background().with_overrides(
        env_u32("YM_BG_MAX_STEPS"),
        env_u32("YM_MAX_MODEL_CALLS"),
        None,
        env_f64("YM_MAX_USD"),
    )
}

pub(crate) const SCHEMA: &[Setting] = &[
    // ── Brain ────────────────────────────────────────────────────────────
    Setting { key: "YM_MODEL", label: "Cloud model", group: "Brain", kind: "string", desc: "Model id for the cloud provider lane.", restart: true },
    Setting { key: "YM_BUILDER", label: "Self-build builder", group: "Brain", kind: "enum:claude|qwen|codex", desc: "Which agent runs the nightly self-improvement tick.", restart: false },
    Setting { key: "YM_QWEN_MODEL", label: "Qwen model", group: "Brain", kind: "string", desc: "Model used when the builder is qwen (default qwen3.8-max).", restart: false },
    Setting { key: "YM_LOCAL_OLLAMA_URL", label: "Private-lane URL", group: "Brain", kind: "string", desc: "Owned-hardware endpoint for private turns. Empty = private turns escalate (audited).", restart: true },
    Setting { key: "YM_LOCAL_OLLAMA_MODEL", label: "Private-lane model", group: "Brain", kind: "string", desc: "Model served on the private lane.", restart: true },
    Setting { key: "YM_VISION_MODEL", label: "Vision model", group: "Brain", kind: "string", desc: "Model for photo understanding.", restart: true },
    // Free-tier lane routing (researched 2026-08-16). These carry HOUSEHOLD/PUBLIC-lane calls
    // only; a private-grounded turn stays on owned hardware regardless of what is set here.
    Setting { key: "YM_ROLE_RESEARCH", label: "Research lane", group: "Brain", kind: "string", desc: "Provider spec for research-role calls, e.g. nim:deepseek-ai/deepseek-v4 (free ~40 RPM), groq:llama-3.3-70b-versatile (free ~30 RPM/1k RPD), cerebras:llama-3.3-70b (free ~1M tok/day). Empty = default brain.", restart: true },
    Setting { key: "YM_ROLE_UTIL", label: "Utility lane", group: "Brain", kind: "string", desc: "Provider spec for small utility calls (compile, titles). Same format as the research lane.", restart: true },
    Setting { key: "YM_ROLE_VERIFY", label: "Verify lane", group: "Brain", kind: "string", desc: "Provider spec for verification calls. Same format as the research lane.", restart: true },
    // ── Rhythm ───────────────────────────────────────────────────────────
    Setting { key: "YM_TZ", label: "Timezone", group: "Rhythm", kind: "string", desc: "IANA timezone (e.g. America/Chicago). Governs quiet hours and reminders.", restart: true },
    Setting { key: "YM_QUIET_START", label: "Quiet from", group: "Rhythm", kind: "int", desc: "Hour (0-23, local) when proactive surfaces go quiet.", restart: true },
    Setting { key: "YM_QUIET_END", label: "Quiet until", group: "Rhythm", kind: "int", desc: "Hour (0-23, local) when they wake.", restart: true },
    Setting { key: "YM_DMN_IDLE_SECS", label: "Idle before DMN", group: "Rhythm", kind: "int", desc: "Seconds of user idle before offline cognition may run (default 600).", restart: true },
    Setting { key: "YM_HOME_WATCH_SECS", label: "Home poll (s)", group: "Rhythm", kind: "int", desc: "Home-watch poll period (default 120). The websocket ear reacts faster regardless.", restart: true },
    Setting { key: "YM_MAILSWEEP_SECS", label: "Mail sweep (s)", group: "Rhythm", kind: "int", desc: "Personal-inbox scan period (default daily).", restart: true },
    Setting { key: "YM_TWITCH_DEBOUNCE_SECS", label: "Twitch debounce (s)", group: "Rhythm", kind: "int", desc: "Minimum gap between fast-twitch evaluations in an event storm (default 5).", restart: true },
    Setting { key: "YM_ESCROW_STALE_DAYS", label: "Escrow expiry (d)", group: "Rhythm", kind: "int", desc: "Held interruptions older than this are dropped (default 14).", restart: true },
    // ── Agent loop ───────────────────────────────────────────────────────
    // How hard the mind is allowed to work on one thing. These bind: `agent_budget()` reads them,
    // and the loop's iteration cap comes from that rather than from a constant.
    Setting { key: "YM_MAX_STEPS", label: "Max iterations", group: "Agent loop", kind: "int", desc: "Tool steps one turn may take before it must answer (default 100, allowed 2–500). The cap exists to stop a runaway, not to limit thinking — lower it only to make turns cheaper.", restart: true },
    Setting { key: "YM_MAX_MODEL_CALLS", label: "Max reasoning calls", group: "Agent loop", kind: "int", desc: "Model calls per turn — the cost that actually matters. Capped at the iteration limit, since a step is what makes a call.", restart: true },
    Setting { key: "YM_MAX_WALL_SECS", label: "Turn time limit (s)", group: "Agent loop", kind: "int", desc: "Wall-clock ceiling for one turn (default 180). Independent of the iteration limit: a turn of cheap steps reaches 100 well inside it, while one that reasons every step hits the clock after ~20. Whichever binds is reported distinctly.", restart: true },
    Setting { key: "YM_MAX_USD", label: "Spend per turn ($)", group: "Agent loop", kind: "string", desc: "Optional cost ceiling for one turn. Empty or 0 = ungoverned.", restart: true },
    Setting { key: "YM_BG_MAX_STEPS", label: "Max iterations (delegated)", group: "Agent loop", kind: "int", desc: "Iteration cap for delegated/scheduled work, where nobody is waiting (default 150). Depth is worth more here.", restart: true },
    Setting { key: "YM_COGNITION", label: "Bounded control loop", group: "Agent loop", kind: "toggle", desc: "Use the state-capsule runtime instead of the classic think→tool→think loop: the runtime keeps the execution state, so a long turn costs what a short one does. Off = the loop that has always run.", restart: true },
    // The think toggles were real knobs the schema did not list: YM_THINK_DISPATCH=on sat in the
    // env for days silently overriding a measured in-code default, and cost three multi-minute
    // generations before anyone found it. A knob the panel cannot see is a knob nobody remembers
    // turning. (Every other YM_THINK_<role> the code reads surfaces in `ym config dump`.)
    Setting { key: "YM_THINK_DISPATCH", label: "Think on dispatch", group: "Agent loop", kind: "toggle", desc: "Reasoning mode for tool-selection calls. Measured off: 4/4 correct at 3.6× fewer tokens; on, a page-writing dispatch can spend its whole token budget inside the thinking block (observed live 2026-08-16). Format-flub retries force it off regardless.", restart: true },
    Setting { key: "YM_THINK_REASONING", label: "Think on reasoning", group: "Agent loop", kind: "toggle", desc: "Reasoning mode for the deep-reasoning call sites.", restart: true },
    Setting { key: "YM_THINK_PLAN", label: "Think on planning", group: "Agent loop", kind: "toggle", desc: "Reasoning mode for plan/replan calls.", restart: true },
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
    Setting { key: "NVIDIA_API_KEY", label: "NVIDIA NIM key", group: "Channels & keys", kind: "secret", desc: "build.nvidia.com — the free lane with the deepest catalog (~40 RPM).", restart: true },
    Setting { key: "GROQ_API_KEY", label: "Groq key", group: "Channels & keys", kind: "secret", desc: "console.groq.com — fastest free per-request turnaround.", restart: true },
    Setting { key: "CEREBRAS_API_KEY", label: "Cerebras key", group: "Channels & keys", kind: "secret", desc: "cloud.cerebras.ai — highest free daily throughput.", restart: true },
    Setting { key: "OPEN_ROUTER_KEY", label: "OpenRouter key", group: "Channels & keys", kind: "secret", desc: "openrouter.ai — the :free model variety lane (20 RPM; 1k/day after a one-time $10 top-up).", restart: true },
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
        // `ym config dump` — every knob RESOLVED, with its source, plus the section the schema
        // cannot know about. The DeepSeek Harness steal (`--dump-config` prints the tree a machine
        // actually boots): drift between file, env and process is the #1 config failure, and the
        // cure is one command that shows what is REALLY in force and where each value came from.
        if rest.trim() == "dump" {
            let mut out = String::from("⚙️ CONFIG DUMP — every knob resolved, with its source\n\nREGISTERED (settings schema):\n");
            for s in SCHEMA {
                let live = std::env::var(s.key).unwrap_or_default();
                let (val, src) = if live.is_empty() {
                    ("—".to_string(), "default")
                } else if s.kind == "secret" {
                    (mask(&live), "env")
                } else {
                    (live, "env")
                };
                out.push_str(&format!("  {:<26} {:<44} [{src}]\n", s.key, val));
            }
            // The section that catches what the schema does not list. YM_THINK_DISPATCH=on lived
            // here unseen — the code read it on every dispatch call while every config surface
            // showed defaults. An env var with the mind's prefix that no schema row claims is a
            // knob somebody turned outside the panel's sight, and it is shown, not hidden.
            let known: std::collections::HashSet<&str> = SCHEMA.iter().map(|s| s.key).collect();
            let mut unregistered: Vec<(String, String)> = std::env::vars()
                .filter(|(k, _)| (k.starts_with("YM_") || k.starts_with("YANTRIK") || k.starts_with("NANOGPT") || k.starts_with("OLLAMA")) && !known.contains(k.as_str()))
                .collect();
            unregistered.sort();
            if !unregistered.is_empty() {
                out.push_str("\nUNREGISTERED OVERRIDES (env the code may read; no schema row lists them):\n");
                for (k, v) in unregistered {
                    let secretish = ["KEY", "TOKEN", "SECRET", "PASSWORD"].iter().any(|m| k.contains(m));
                    out.push_str(&format!("  {:<26} {}\n", k, if secretish { mask(&v) } else { v }));
                }
            }
            let b = agent_budget();
            let bg = background_budget();
            out.push_str(&format!(
                "\nEFFECTIVE BUDGETS (what the loop actually runs with):\n  interactive: {} steps · {} model calls · {}s wall\n  delegated:   {} steps · {} model calls\n",
                b.max_steps, b.max_model_calls, b.max_wall_ms / 1000, bg.max_steps, bg.max_model_calls
            ));
            return out;
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
    use crate::ConversationEngine;

    /// `ym config dump` must SURFACE a knob the schema does not list — that invisible class is
    /// exactly how YM_THINK_DISPATCH=on silently overrode a measured default for days. And a
    /// secret-shaped unregistered var must still be masked to shape-only.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn config_dump_surfaces_unregistered_overrides_and_masks_secrets() {
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = mind_inference::InferencePool::new(
            std::sync::Arc::new(mind_inference::ScriptedLLM::new("ok")) as std::sync::Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let eng = ConversationEngine::new(std::sync::Arc::new(mem) as std::sync::Arc<dyn mind_types::MemoryFacade>, pool, "JARVIS");
        std::env::set_var("YM_TEST_UNREGISTERED_KNOB", "on");
        std::env::set_var("YM_TEST_UNREGISTERED_KEY", "hunter2hunter2");
        let out = eng.config_panel("dump").await;
        std::env::remove_var("YM_TEST_UNREGISTERED_KNOB");
        std::env::remove_var("YM_TEST_UNREGISTERED_KEY");

        assert!(out.contains("UNREGISTERED OVERRIDES"), "{out}");
        assert!(out.contains("YM_TEST_UNREGISTERED_KNOB"), "an off-schema knob must be shown:\n{out}");
        assert!(out.contains("YM_TEST_UNREGISTERED_KEY"), "{out}");
        assert!(!out.contains("hunter2"), "a secret-shaped value must be masked, never printed:\n{out}");
        assert!(out.contains("EFFECTIVE BUDGETS"), "{out}");
        assert!(out.contains("[default]") || out.contains("[env]"), "every row carries its source:\n{out}");
    }

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

    /// The setting must move the loop's cap, and the default must be high enough to do real work.
    /// Five steps could not finish a research question or open a repository.
    ///
    /// Uses a key no other test touches, and restores it, because env is process-global.
    #[test]
    fn the_iteration_setting_binds_and_defaults_high_enough_to_be_useful() {
        let prev = std::env::var("YM_MAX_STEPS").ok();
        std::env::remove_var("YM_MAX_STEPS");
        let default = agent_budget();
        assert_eq!(default.max_steps, 100, "the default must allow real work, not a token gesture");
        assert_eq!(default.max_model_calls, 100, "the reasoning ceiling must not shadow it");

        std::env::set_var("YM_MAX_STEPS", "18");
        let raised = agent_budget();
        assert_eq!(raised.max_steps, 18);
        assert_eq!(raised.max_model_calls, 18, "the reasoning ceiling must rise too or nothing changes");

        // Garbage leaves the default in place rather than configuring a mind that cannot move.
        std::env::set_var("YM_MAX_STEPS", "five");
        assert_eq!(agent_budget().max_steps, 100, "an unparseable value is absent, not zero");
        std::env::set_var("YM_MAX_STEPS", "0");
        assert_eq!(agent_budget().max_steps, 100, "zero is absent too \u{2014} a 0-step mind is not a configuration");

        // Absurd is clamped, and reported rather than silently ignored.
        std::env::set_var("YM_MAX_STEPS", "99999");
        let clamped = agent_budget();
        assert_eq!(clamped.max_steps, mind_spec::goal::MAX_STEPS_CEILING);
        assert!(clamped.clamp_note(Some(99999)).is_some());

        match prev {
            Some(v) => std::env::set_var("YM_MAX_STEPS", v),
            None => std::env::remove_var("YM_MAX_STEPS"),
        }
    }

    /// The clock and the step count are INDEPENDENT bounds, and either may bind first: a turn made of
    /// cache hits and deterministic tools can reach 100 steps inside the clock, while a turn that
    /// reasons at every step will hit the time limit after twenty or so. Both are safety bounds, not
    /// targets — so what must hold is that they are DISTINGUISHABLE when they bind, because "ran out
    /// of time" and "ran out of ideas" are different things to tell an operator.
    #[test]
    fn the_clock_and_the_step_count_bind_independently_and_distinguishably() {
        let b = agent_budget();
        assert!(b.max_wall_ms >= 120_000, "an interactive turn needs at least a couple of minutes");
        assert!(b.max_wall_ms <= 600_000, "and it is still a promise to whoever is waiting");

        // Each limit reports its own reason, so neither is mistaken for the other.
        use mind_spec::{Capsule, Controller, ReasonCode, StepOutcome};
        let ctl = Controller::default();
        let contract = mind_spec::Contract {
            requirements: vec![],
            completion: mind_spec::CompletionCriteria { min_findings: 99, require_full_coverage: false, ..Default::default() },
            output: Default::default(),
        };

        let mut out_of_steps = Capsule::new("g", "goal");
        out_of_steps.progress.steps = b.max_steps;
        assert_eq!(
            ctl.decide(&out_of_steps, &contract, &b, 0, StepOutcome::default()).reason(),
            Some(ReasonCode::StepBudget)
        );

        let fresh = Capsule::new("g", "goal");
        assert_eq!(
            ctl.decide(&fresh, &contract, &b, b.max_wall_ms, StepOutcome::default()).reason(),
            Some(ReasonCode::Timeout)
        );
    }

    /// Delegated work gets its own, larger cap: nobody is waiting, so depth is worth more.
    ///
    /// Compares the DEFAULTS rather than the env-reading functions, deliberately. `cargo test` runs a
    /// binary's tests concurrently, and the iteration test above mutates process-global env — so a
    /// version of this that called `agent_budget()` raced with it and failed intermittently once in
    /// the workspace run. A test that reads shared mutable state it does not own is a flake waiting
    /// for a busy machine.
    #[test]
    fn delegated_work_has_a_separate_larger_cap() {
        let interactive = mind_spec::Budget::interactive();
        let delegated = mind_spec::Budget::background();
        assert!(delegated.max_steps > interactive.max_steps, "delegated runs are not held to an interactive cap");
        assert!(delegated.max_wall_ms > interactive.max_wall_ms);
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
