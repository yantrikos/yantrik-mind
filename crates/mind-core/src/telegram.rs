//! Telegram channel — talk to yantrik-mind from your phone. A minimal, resilient long-poll loop
//! that routes every inbound message through the same `handle_line` as the REPL, so chat, learning,
//! commitments, tasks, and commands all work over telegram. The bot token is read from the
//! `YM_TELEGRAM_TOKEN` env var — never hardcoded or committed.
//!
//! Offset is persisted (so a restart doesn't replay old messages). Network/parse errors are logged
//! and retried; the loop never crashes.

use std::collections::HashSet;
use std::io::Write;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use mind_conversation::ConversationEngine;
use mind_memory::MemoryHandle;
use mind_types::MemoryFacade;

use crate::{handle_line, Outcome};

async fn tg_get(api: &str, method_query: &str) -> anyhow::Result<serde_json::Value> {
    let url = format!("{api}/{method_query}");
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let body = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(35))
            .call()?
            .into_string()?;
        Ok(serde_json::from_str(&body)?)
    })
    .await??;
    Ok(v)
}

/// Split text into <=max-char chunks on line/char boundaries — Telegram rejects messages over 4096
/// chars with HTTP 400 (this silently ate long agent replies). Returns at least one chunk.
fn chunk_text(s: &str, max: usize) -> Vec<String> {
    if s.chars().count() <= max {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    let mut cur = String::new();
    for line in s.split_inclusive('\n') {
        if cur.chars().count() + line.chars().count() > max && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        if line.chars().count() > max {
            for ch in line.chars() {
                if cur.chars().count() >= max {
                    out.push(std::mem::take(&mut cur));
                }
                cur.push(ch);
            }
        } else {
            cur.push_str(line);
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

async fn tg_send(api: &str, chat_id: i64, text: &str) -> anyhow::Result<()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    for chunk in chunk_text(text, 4000) {
        let url = format!("{api}/sendMessage");
        let api_owned = api.to_string();
        let payload = serde_json::json!({ "chat_id": chat_id, "text": chunk });
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            ureq::post(&url)
                .timeout(std::time::Duration::from_secs(30))
                .send_json(payload)
                .map_err(|e| anyhow::anyhow!("{}", e.to_string().replace(&api_owned, "https://api.telegram.org/bot<token>")))?;
            Ok(())
        })
        .await??;
    }
    Ok(())
}

/// Show the "typing…" indicator (Telegram clears it after ~5s or on the next message) — covers the
/// agentic loop's think time so a slow turn doesn't feel like dead air. Best-effort; errors ignored.
async fn tg_typing(api: &str, chat_id: i64) {
    let url = format!("{api}/sendChatAction");
    let payload = serde_json::json!({ "chat_id": chat_id, "action": "typing" });
    let _ = tokio::task::spawn_blocking(move || {
        let _ = ureq::post(&url).timeout(std::time::Duration::from_secs(10)).send_json(payload);
    })
    .await;
}

fn offset_path() -> String {
    std::env::var("YM_TG_OFFSET").unwrap_or_else(|_| "telegram_offset".to_string())
}

fn load_offset() -> i64 {
    std::fs::read_to_string(offset_path())
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn save_offset(n: i64) {
    if let Ok(mut f) = std::fs::File::create(offset_path()) {
        let _ = write!(f, "{n}");
    }
}

fn reminded_path() -> String {
    format!("{}.reminded", offset_path())
}

fn active_chat_path() -> String {
    format!("{}.active_chat", offset_path())
}

/// Persist the last-active chat id so proactive/reminders/ask survive a restart (active_chat used to
/// reset to 0 on every restart, leaving the bot unable to reach the operator until they messaged again).
fn save_active_chat(id: i64) {
    if let Ok(mut f) = std::fs::File::create(active_chat_path()) {
        let _ = write!(f, "{id}");
    }
}

fn load_active_chat() -> i64 {
    std::fs::read_to_string(active_chat_path()).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0)
}

fn load_reminded() -> HashSet<String> {
    std::fs::read_to_string(reminded_path())
        .map(|s| s.lines().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect())
        .unwrap_or_default()
}

fn save_reminded(set: &HashSet<String>) {
    if let Ok(mut f) = std::fs::File::create(reminded_path()) {
        let _ = write!(f, "{}", set.iter().cloned().collect::<Vec<_>>().join("\n"));
    }
}

/// Quiet-hours check with wraparound (e.g. start=22, end=7 means 22:00–06:59 is quiet).
fn is_quiet_hour(hour: u32, start: u32, end: u32) -> bool {
    if start == end {
        false
    } else if start < end {
        hour >= start && hour < end
    } else {
        hour >= start || hour < end
    }
}

fn in_quiet_hours_now() -> bool {
    use chrono::Timelike;
    let start = std::env::var("YM_QUIET_START").ok().and_then(|s| s.parse().ok()).unwrap_or(22);
    let end = std::env::var("YM_QUIET_END").ok().and_then(|s| s.parse().ok()).unwrap_or(7);
    // The box runs UTC; quiet hours must be the USER's local time (YM_TZ_OFFSET_MINUTES, e.g. 330 IST),
    // else a "2am" reminder slips through a UTC quiet window. chrono::Local == UTC on the box, so shift.
    let off: i64 = std::env::var("YM_TZ_OFFSET_MINUTES").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    let local = chrono::Utc::now() + chrono::Duration::minutes(off);
    is_quiet_hour(local.hour(), start, end)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Proactive reminders: a background tick that messages the operator when a commitment they asked
/// to be reminded of comes due. Conservative by design — it only surfaces *due* tasks (never
/// free-form outreach), honors quiet hours, and dedupes so a reminder fires once.
async fn reminder_loop(api: String, mem: MemoryHandle, active_chat: Arc<AtomicI64>) {
    let mut reminded = load_reminded();
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        let chat = active_chat.load(Ordering::Relaxed);
        if chat == 0 || in_quiet_hours_now() {
            continue;
        }
        let now = now_ms();
        let tasks = mem.list_tasks(false).await.unwrap_or_default();
        for t in tasks {
            let due = match t.due_ms {
                Some(d) if d <= now => d,
                _ => continue,
            };
            let _ = due;
            if reminded.contains(&t.id) {
                continue;
            }
            let msg = format!("⏰ Reminder: {}", t.description);
            if tg_send(&api, chat, &msg).await.is_ok() {
                reminded.insert(t.id.clone());
                save_reminded(&reminded);
            }
        }
    }
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// One control request from `ym`: `POST /chat` (body = message → handle_turn → reply) or
/// `GET /status` (liveness). Runs the async turn on the shared runtime via `rt.block_on` (this is a
/// plain OS thread, not a runtime worker, so block_on is allowed). Shares the live conv → live memory.
fn ctl_handle(mut stream: std::net::TcpStream, conv: Arc<ConversationEngine>, rt: tokio::runtime::Handle) {
    use std::io::{Read, Write};
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(150)));
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    // read until the headers are complete
    let hend = loop {
        match stream.read(&mut tmp) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = find_sub(&buf, b"\r\n\r\n") {
                    break p;
                }
                if buf.len() > 2_000_000 {
                    return;
                }
            }
            Err(_) => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..hend]).to_string();
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("");
    let path = first.next().unwrap_or("/");
    let clen: usize = head
        .lines()
        .find_map(|l| l.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse().unwrap_or(0)))
        .unwrap_or(0);
    // body = whatever followed the headers, plus any remaining content-length bytes
    let mut body = buf[hend + 4..].to_vec();
    while body.len() < clen {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    let body = String::from_utf8_lossy(&body).trim().to_string();

    let (status, reply) = match (method, path.split('?').next().unwrap_or(path)) {
        // `ym <name> <args>` — the top-level CLI router (core commands + skill-registered commands +
        // chat fallback). Data-driven: a new capability skill becomes a new `ym` command, no recompile.
        ("POST", "/cli") if !body.is_empty() => ("200 OK", rt.block_on(conv.cli_dispatch(&body))),
        ("POST", "/chat") if !body.is_empty() => {
            let r = rt.block_on(conv.handle_turn(&body)).unwrap_or_else(|e| format!("(error: {e})"));
            ("200 OK", r)
        }
        ("POST", "/chat") | ("POST", "/cli") => ("400 Bad Request", "(empty message)".to_string()),
        ("GET", "/status") => ("200 OK", "ok".to_string()),
        _ => ("404 Not Found", "not found".to_string()),
    };
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
        reply.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

/// Tiny localhost-only control server (own thread) backing the `ym` CLI. Lets a terminal talk to the
/// SAME running companion as telegram (shared memory). 127.0.0.1 only; YM_CTL=off disables.
fn spawn_control_server(conv: Arc<ConversationEngine>, rt: tokio::runtime::Handle) {
    if std::env::var("YM_CTL").map(|v| v == "off").unwrap_or(false) {
        return;
    }
    let port: u16 = std::env::var("YM_CTL_PORT").ok().and_then(|s| s.parse().ok()).unwrap_or(8077);
    std::thread::spawn(move || match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            eprintln!("[ctl] control endpoint on 127.0.0.1:{port} (for the `ym` CLI)");
            for stream in listener.incoming().flatten() {
                let (conv, rt) = (conv.clone(), rt.clone());
                std::thread::spawn(move || ctl_handle(stream, conv, rt));
            }
        }
        Err(e) => eprintln!("[ctl] could not bind 127.0.0.1:{port}: {e}"),
    });
}

/// Run the telegram channel until killed. `chat_lock` (YM_TELEGRAM_CHAT) optionally restricts to a
/// single chat id; if unset, the first chatter is accepted (single-user companion).
pub async fn run(token: String, mem: MemoryHandle, conv: ConversationEngine) -> anyhow::Result<()> {
    let api = format!("https://api.telegram.org/bot{token}");
    match tg_get(&api, "getMe").await {
        Ok(me) => {
            let name = me["result"]["username"].as_str().unwrap_or("?");
            println!("telegram channel live as @{name} — message it from your phone.");
        }
        Err(e) => {
            return Err(anyhow::anyhow!("telegram getMe failed (bad token?): {e}"));
        }
    }
    // Shared so each turn can be processed in its OWN task — a slow turn (a multi-step agent loop with
    // big generations) must never freeze the poll loop or the background ticks (the old "no-reply" /
    // frozen-bot failure mode). The memory actor serializes writes, so concurrent turns are safe.
    let conv = Arc::new(conv);

    // Local control endpoint for the `ym` CLI: same running process → SHARES live memory/continuity
    // with the telegram channel (one mind, two surfaces). Bound to 127.0.0.1 only (no new LAN port;
    // SSH stays the trust boundary). Disable with YM_CTL=off.
    spawn_control_server(conv.clone(), tokio::runtime::Handle::current());

    let chat_lock: Option<i64> = std::env::var("YM_TELEGRAM_CHAT").ok().and_then(|s| s.trim().parse().ok());

    // Proactive reminders run in the background, messaging the last-active chat when a due
    // commitment arrives. (Disabled with YM_REMINDERS=off.)
    let active_chat = Arc::new(AtomicI64::new(chat_lock.unwrap_or_else(load_active_chat)));
    if std::env::var("YM_REMINDERS").map(|v| v != "off").unwrap_or(true) {
        tokio::spawn(reminder_loop(api.clone(), mem.clone(), active_chat.clone()));
    }

    let mut offset = load_offset();
    // Default-mode ("sleep") loop state: when the user has been idle a while, run one offline cognition
    // tick (rehearse/reconcile/associate). Tracked inline on the poll loop so it never competes with a
    // live turn and needs no extra task. Disabled with YM_DMN=off.
    let mut last_activity = now_ms();
    let mut last_dmn = 0u64;
    let mut last_digest = now_ms(); // don't surface a proactive digest right after boot
    let mut last_ask = 0u64; // 0 = the ask-drive may pose its first get-to-know-you question once idle
    let mut last_home_watch = 0u64; // proactive home-anomaly watch cadence
    loop {
        let updates = match tg_get(&api, &format!("getUpdates?timeout=25&offset={offset}")).await {
            Ok(u) => u,
            Err(e) => {
                eprintln!("[telegram] poll error: {e}");
                tokio::time::sleep(std::time::Duration::from_secs(3)).await;
                continue;
            }
        };
        let Some(results) = updates["result"].as_array() else { continue };
        for upd in results {
            if let Some(uid) = upd["update_id"].as_i64() {
                offset = uid + 1;
                save_offset(offset); // consume even if we skip, so no resend loop
            }
            let msg = &upd["message"];
            let chat_id = match msg["chat"]["id"].as_i64() {
                Some(id) => id,
                None => continue,
            };
            if let Some(lock) = chat_lock {
                if chat_id != lock {
                    continue;
                }
            }
            // Remember where to push proactive messages, and that the user is active right now (so the
            // default-mode loop stays out of the way until they've been idle a while).
            active_chat.store(chat_id, Ordering::Relaxed);
            save_active_chat(chat_id); // persist so proactive/reminders/backchannel survive restarts
            last_activity = now_ms();
            let text = msg["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                continue;
            }
            // Process the turn in its OWN task so the poll loop keeps polling + ticking (delegations,
            // consolidation, DMN, proactive) no matter how long this turn takes. A child timer keeps
            // the "typing…" indicator alive (Telegram clears it after ~5s) for the full think time.
            let (api2, mem2, conv2) = (api.clone(), mem.clone(), conv.clone());
            tokio::spawn(async move {
                tg_typing(&api2, chat_id).await;
                let work = handle_line(&text, &mem2, conv2.as_ref());
                tokio::pin!(work);
                let outcome = loop {
                    tokio::select! {
                        r = &mut work => break r,
                        _ = tokio::time::sleep(std::time::Duration::from_secs(4)) => tg_typing(&api2, chat_id).await,
                    }
                };
                let reply = match outcome {
                    Outcome::Quit => "(the mind keeps running — nothing to quit here)".to_string(),
                    Outcome::Said(s) if s.is_empty() => return,
                    Outcome::Said(s) => s,
                };
                if let Err(e) = tg_send(&api2, chat_id, &reply).await {
                    eprintln!("[telegram] send error: {e}");
                }
            });
        }

        // Persistent-delegation tick: wake any due WaitUntil/WaitForCondition runs and deliver what
        // they surfaced to the active chat (~25s idle cadence — the getUpdates long-poll interval).
        for note in conv.tick_delegations().await {
            let target = active_chat.load(Ordering::Relaxed);
            if target != 0 {
                let _ = tg_send(&api, target, &note).await;
            }
        }

        // Delegated background jobs (research/code) deliver their results here when finished.
        for note in conv.take_notifications() {
            let target = active_chat.load(Ordering::Relaxed);
            if target != 0 {
                let _ = tg_send(&api, target, &note).await;
            }
        }

        // Proactive HOME WATCH — the moat in action: flag grounded home anomalies (TV on while away,
        // internet down, door unlocked, low ink) UNPROMPTED. Deduped (fires once per condition until it
        // clears), paced (YM_HOME_WATCH_SECS, default 120s), quiet-hours-gated. YM_HOME_WATCH=off disables.
        if std::env::var("YM_HOME_WATCH").map(|v| v != "off").unwrap_or(true) {
            let period: u64 =
                std::env::var("YM_HOME_WATCH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(120);
            let now = now_ms();
            if now.saturating_sub(last_home_watch) >= period * 1000 {
                last_home_watch = now;
                let chat = active_chat.load(Ordering::Relaxed);
                if chat != 0 && !in_quiet_hours_now() {
                    for alert in conv.home_watch().await {
                        let _ = tg_send(&api, chat, &alert).await;
                    }
                    // Bills due soon (deduped once per month) ride the same cadence.
                    for note in conv.bill_watch().await {
                        let _ = tg_send(&api, chat, &note).await;
                    }
                    // Tracked news topics: surface fresh headlines (deduped per topic).
                    for note in conv.news_watch().await {
                        let _ = tg_send(&api, chat, &note).await;
                    }
                }
            }
        }

        // Consolidation tick: distill new conversation turns into durable typed beliefs (the moat's
        // compounding loop). Self-gates until enough new turns accrue; background, not surfaced.
        let formed = conv.consolidate().await;
        if formed > 0 {
            eprintln!("[consolidate] formed {formed} durable memories");
        }

        // Default-mode ("sleep") tick: when the user has been idle past the threshold, run ONE bounded
        // offline-cognition pass (rehearse → reconcile → associate over the typed substrate). Paced so
        // it fires at most every YM_DMN_SECS, and only while idle so it never competes with a live turn.
        if std::env::var("YM_DMN").map(|v| v != "off").unwrap_or(true) {
            let idle_secs: u64 =
                std::env::var("YM_DMN_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(600);
            let period: u64 = std::env::var("YM_DMN_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300);
            let now = now_ms();
            if now.saturating_sub(last_activity) >= idle_secs * 1000
                && now.saturating_sub(last_dmn) >= period * 1000
            {
                for line in conv.dmn_tick().await {
                    eprintln!("{line}");
                }
                last_dmn = now;
            }
        }

        // Proactive: the unprompted paths — all heavily gated (idle + quiet-hours + a once-per-period
        // cap) and capped at ONE message per tick. A value DIGEST (urges that cleared the bar) takes
        // precedence; otherwise, while the brain is still sparse, the ASK-DRIVE poses ONE get-to-know-you
        // question (curiosity turned outward — cures cold-start instead of waiting to be fed).
        if std::env::var("YM_PROACTIVE").map(|v| v != "off").unwrap_or(true) {
            let idle_secs: u64 =
                std::env::var("YM_DMN_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(600);
            let pd_secs: u64 =
                std::env::var("YM_PROACTIVE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(86_400);
            let ask_secs: u64 =
                std::env::var("YM_ASK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(86_400);
            let now = now_ms();
            let chat = active_chat.load(Ordering::Relaxed);
            let idle_ok = chat != 0
                && !in_quiet_hours_now()
                && now.saturating_sub(last_activity) >= idle_secs * 1000;
            let mut spoke = false;
            if idle_ok && now.saturating_sub(last_digest) >= pd_secs * 1000 {
                if let Some(msg) = conv.proactive_digest().await {
                    if tg_send(&api, chat, &msg).await.is_ok() {
                        eprintln!("[proactive] surfaced a digest ({} chars)", msg.len());
                        spoke = true;
                    }
                }
                last_digest = now; // reset cadence whether or not we spoke (never hammer)
            }
            if !spoke
                && std::env::var("YM_ASK").map(|v| v != "off").unwrap_or(true)
                && idle_ok
                && now.saturating_sub(last_ask) >= ask_secs * 1000
            {
                if let Some(q) = conv.proactive_ask().await {
                    if tg_send(&api, chat, &q).await.is_ok() {
                        eprintln!("[ask] posed a get-to-know-you question");
                    }
                }
                last_ask = now; // reset cadence whether or not it asked
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_quiet_hour;

    #[test]
    fn quiet_hours_wraparound_overnight() {
        // 22:00–07:00 quiet
        assert!(is_quiet_hour(23, 22, 7));
        assert!(is_quiet_hour(2, 22, 7));
        assert!(is_quiet_hour(6, 22, 7));
        assert!(!is_quiet_hour(7, 22, 7)); // end is exclusive
        assert!(!is_quiet_hour(12, 22, 7));
        assert!(!is_quiet_hour(21, 22, 7));
        assert!(is_quiet_hour(22, 22, 7)); // start inclusive
    }

    #[test]
    fn quiet_hours_same_day_window() {
        // 1:00–5:00 quiet (non-wrapping)
        assert!(is_quiet_hour(3, 1, 5));
        assert!(!is_quiet_hour(6, 1, 5));
        assert!(!is_quiet_hour(0, 1, 5));
    }

    #[test]
    fn no_quiet_window_when_equal() {
        assert!(!is_quiet_hour(3, 0, 0));
    }
}
