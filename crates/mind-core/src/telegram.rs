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

use crate::{handle_line_as, Outcome};

async fn tg_get(api: &str, method_query: &str) -> anyhow::Result<serde_json::Value> {
    let url = format!("{api}/{method_query}");
    // ureq errors embed the full request URL — which contains the bot token. Redact it from any
    // error we bubble up, or the token lands verbatim in the journal (it did; see poll-error logs).
    let api_owned = api.to_string();
    let v = tokio::task::spawn_blocking(move || -> anyhow::Result<serde_json::Value> {
        let body = ureq::get(&url)
            .timeout(std::time::Duration::from_secs(35))
            .call()
            .map_err(|e| anyhow::anyhow!("{}", e.to_string().replace(&api_owned, "https://api.telegram.org/bot<token>")))?
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

/// Speech-to-text for an inbound Telegram voice note: getFile -> download the .oga -> ffmpeg to
/// 16 kHz mono wav -> whisper.cpp. None on any failure - the caller apologizes instead of guessing.
async fn tg_voice_to_text(api: &str, file_id: &str) -> Option<String> {
    let api_owned = api.to_string();
    let fid = file_id.to_string();
    tokio::task::spawn_blocking(move || -> Option<String> {
        use std::io::Read;
        let meta: serde_json::Value = ureq::get(&format!("{api_owned}/getFile?file_id={fid}"))
            .timeout(std::time::Duration::from_secs(30))
            .call()
            .ok()?
            .into_json()
            .ok()?;
        let path = meta["result"]["file_path"].as_str()?;
        // Files download from a sibling host path: /bot<token>/ -> /file/bot<token>/.
        let file_url = format!("{}/{}", api_owned.replacen("/bot", "/file/bot", 1), path);
        let mut bytes = Vec::new();
        ureq::get(&file_url)
            .timeout(std::time::Duration::from_secs(60))
            .call()
            .ok()?
            .into_reader()
            .take(20_000_000)
            .read_to_end(&mut bytes)
            .ok()?;
        let tag = format!("{}_{}", std::process::id(), now_ms());
        let dir = std::env::temp_dir();
        let oga = dir.join(format!("ym_v_{tag}.oga"));
        let wav = dir.join(format!("ym_v_{tag}.wav"));
        std::fs::write(&oga, &bytes).ok()?;
        let ff = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i", oga.to_str()?, "-ar", "16000", "-ac", "1", wav.to_str()?])
            .status()
            .ok()?;
        let _ = std::fs::remove_file(&oga);
        if !ff.success() {
            return None;
        }
        let whisper = std::env::var("YM_WHISPER_BIN").unwrap_or_else(|_| "/opt/voice/whisper.cpp/build/bin/whisper-cli".into());
        let model = std::env::var("YM_WHISPER_MODEL").unwrap_or_else(|_| "/opt/voice/models/ggml-base.en.bin".into());
        let out = std::process::Command::new(whisper)
            .args(["-m", &model, "-f", wav.to_str()?, "-nt", "-np"])
            .output()
            .ok()?;
        let _ = std::fs::remove_file(&wav);
        let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if text.len() < 2 {
            None
        } else {
            Some(text)
        }
    })
    .await
    .ok()?
}

/// Voice reply: Piper TTS -> wav -> ffmpeg to OGG/Opus -> Telegram sendVoice (curl multipart - ureq
/// has no multipart). Spoken replies are capped to the gist; the full text always goes as a message.
async fn tg_send_voice(api: &str, chat_id: i64, text: &str) -> bool {
    let speak: String = text
        .chars()
        .filter(|c| !matches!(c, '*' | '#' | '`' | '_'))
        .take(600)
        .collect();
    if speak.trim().len() < 2 {
        return false;
    }
    let api_owned = api.to_string();
    tokio::task::spawn_blocking(move || -> bool {
        use std::io::Write as _;
        let piper = std::env::var("YM_PIPER_BIN").unwrap_or_else(|_| "/opt/voice/piper/piper".into());
        let voice = std::env::var("YM_PIPER_VOICE").unwrap_or_else(|_| "/opt/voice/piper/en_US-lessac-medium.onnx".into());
        let tag = format!("{}_{}", std::process::id(), now_ms());
        let dir = std::env::temp_dir();
        let wav = dir.join(format!("ym_tts_{tag}.wav"));
        let ogg = dir.join(format!("ym_tts_{tag}.ogg"));
        let Ok(mut child) = std::process::Command::new(&piper)
            .args(["-m", &voice, "-f", wav.to_str().unwrap_or_default()])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        else {
            return false;
        };
        if let Some(mut si) = child.stdin.take() {
            let _ = si.write_all(speak.as_bytes());
        }
        if !child.wait().map(|st| st.success()).unwrap_or(false) {
            return false;
        }
        let ff = std::process::Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i", wav.to_str().unwrap_or_default(), "-c:a", "libopus", "-b:a", "32k", ogg.to_str().unwrap_or_default()])
            .status()
            .map(|st| st.success())
            .unwrap_or(false);
        let _ = std::fs::remove_file(&wav);
        if !ff {
            return false;
        }
        let out = std::process::Command::new("curl")
            .args([
                "-s",
                "-F",
                &format!("chat_id={chat_id}"),
                "-F",
                &format!("voice=@{}", ogg.to_str().unwrap_or_default()),
                &format!("{api_owned}/sendVoice"),
            ])
            .output();
        let _ = std::fs::remove_file(&ogg);
        out.map(|o| String::from_utf8_lossy(&o.stdout).contains("\"ok\":true")).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// Send a photo (JPEG bytes) with a caption — curl multipart like sendVoice (ureq has no
/// multipart). --form-string for the caption so curl never interprets ; or @ inside the text.
async fn tg_send_photo(api: &str, chat_id: i64, jpeg: Vec<u8>, caption: &str) -> bool {
    let api_owned = api.to_string();
    let caption: String = caption.chars().take(1000).collect();
    tokio::task::spawn_blocking(move || -> bool {
        let tag = format!("{}_{}", std::process::id(), now_ms());
        let path = std::env::temp_dir().join(format!("ym_ph_{tag}.jpg"));
        if std::fs::write(&path, &jpeg).is_err() {
            return false;
        }
        let out = std::process::Command::new("curl")
            .args([
                "-s",
                "--form-string",
                &format!("chat_id={chat_id}"),
                "--form-string",
                &format!("caption={caption}"),
                "-F",
                &format!("photo=@{}", path.to_str().unwrap_or_default()),
                &format!("{api_owned}/sendPhoto"),
            ])
            .output();
        let _ = std::fs::remove_file(&path);
        out.map(|o| String::from_utf8_lossy(&o.stdout).contains("\"ok\":true")).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// Send a video (MP4 bytes) with a caption — curl multipart like sendPhoto.
async fn tg_send_video(api: &str, chat_id: i64, mp4: Vec<u8>, caption: &str) -> bool {
    let api_owned = api.to_string();
    let caption: String = caption.chars().take(1000).collect();
    tokio::task::spawn_blocking(move || -> bool {
        let tag = format!("{}_{}", std::process::id(), now_ms());
        let path = std::env::temp_dir().join(format!("ym_vid_{tag}.mp4"));
        if std::fs::write(&path, &mp4).is_err() {
            return false;
        }
        let out = std::process::Command::new("curl")
            .args([
                "-s",
                "--form-string",
                &format!("chat_id={chat_id}"),
                "--form-string",
                &format!("caption={caption}"),
                "--form-string",
                "supports_streaming=true",
                "-F",
                &format!("video=@{}", path.to_str().unwrap_or_default()),
                &format!("{api_owned}/sendVideo"),
            ])
            .output();
        let _ = std::fs::remove_file(&path);
        out.map(|o| String::from_utf8_lossy(&o.stdout).contains("\"ok\":true")).unwrap_or(false)
    })
    .await
    .unwrap_or(false)
}

/// Download a Telegram file by file_id (getFile → /file/bot path). Shared by photo analysis.
async fn tg_download(api: &str, file_id: &str) -> Option<Vec<u8>> {
    let api_owned = api.to_string();
    let fid = file_id.to_string();
    tokio::task::spawn_blocking(move || -> Option<Vec<u8>> {
        use std::io::Read;
        let meta: serde_json::Value = ureq::get(&format!("{api_owned}/getFile?file_id={fid}"))
            .timeout(std::time::Duration::from_secs(30))
            .call()
            .ok()?
            .into_json()
            .ok()?;
        let path = meta["result"]["file_path"].as_str()?;
        let file_url = format!("{}/{}", api_owned.replacen("/bot", "/file/bot", 1), path);
        let mut bytes = Vec::new();
        ureq::get(&file_url)
            .timeout(std::time::Duration::from_secs(60))
            .call()
            .ok()?
            .into_reader()
            .take(20_000_000)
            .read_to_end(&mut bytes)
            .ok()?;
        Some(bytes)
    })
    .await
    .ok()?
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
    // The box runs UTC; quiet hours must be the USER's local time. DST-aware via YM_TZ (IANA name, e.g.
    // America/Chicago — CDT↔CST auto); else the fixed YM_TZ_OFFSET_MINUTES. Else a "2am" reminder slips
    // a UTC quiet window — and a wrong tz silently suppresses ALL proactive surfaces at active hours.
    let utc = chrono::Utc::now();
    let hour = if let Some(tz) = std::env::var("YM_TZ").ok().and_then(|n| n.trim().parse::<chrono_tz::Tz>().ok()) {
        utc.with_timezone(&tz).hour()
    } else {
        let off: i64 = std::env::var("YM_TZ_OFFSET_MINUTES").ok().and_then(|s| s.parse().ok()).unwrap_or(0);
        (utc + chrono::Duration::minutes(off)).hour()
    };
    is_quiet_hour(hour, start, end)
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
    // Pin proactive routing to the primary's DM from boot (Telegram private-chat id == their user
    // id), so even a fresh box never targets whoever happened to message last.
    if chat_lock.is_none() {
        if let Ok(Some(p)) = conv.memory_handle_primary_tg().await {
            if p != 0 {
                active_chat.store(p, Ordering::Relaxed);
                save_active_chat(p);
            }
        }
    }
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
    let mut last_patterns = now_ms(); // pattern-finder surface cadence (don't fire right after boot)
    let mut last_resolve = 0u64; // prediction-resolver cadence (grade due predictions, surface verdicts)
    let mut last_profile = now_ms(); // periodic profile refresh cadence (re-crawl the seed for what changed)
    let mut last_family = 0u64; // family key-date nudge cadence (birthdays/anniversaries)
    let mut last_followup = 0u64; // deadline follow-through cadence (escalating reminder nudges)
    let mut last_ics = 0u64; // external-calendar (ICS) refresh cadence
    let mut last_pricewatch = now_ms(); // price-watch drop-check cadence
    let mut last_member_beat = 0u64; // member reminders + briefs cadence
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
            // The user is active right now (the default-mode loop stays out of the way). Proactive
            // routing is pinned to the PRIMARY's chat and set only after the owner resolves below —
            // a family member messaging can never redirect briefings/studies/gift-intel to their DM.
            last_activity = now_ms();
            // A shared CONTACT CARD from the primary registers that person as a family member.
            // ("Add her by phone number" — Telegram never exposes phone lookup to bots; the shared
            // card carries the user id when the contact is on Telegram and their privacy allows.)
            if let Some(contact) = msg.get("contact") {
                let first = contact["first_name"].as_str().unwrap_or("").to_string();
                let last = contact["last_name"].as_str().unwrap_or("").to_string();
                let cuid = contact["user_id"].as_i64();
                let from_id2 = msg["from"]["id"].as_i64().unwrap_or(0);
                let (api2, conv2) = (api.clone(), conv.clone());
                tokio::spawn(async move {
                    let owner = conv2.resolve_owner(from_id2, false).await;
                    let reply = if owner != mind_types::PRIMARY {
                        "Only the primary can register members by contact card.".to_string()
                    } else {
                        match cuid {
                            Some(id) if id != 0 => conv2.register_contact(&first, &last, id).await,
                            _ => format!(
                                "{first}'s contact card doesn't carry a Telegram id (not on Telegram, or their privacy hides it from bots) — simplest fix: have them send me one message, then tell me and I'll register them."
                            ),
                        }
                    };
                    let _ = tg_send(&api2, chat_id, &reply).await;
                });
                continue;
            }
            let text = msg["text"].as_str().unwrap_or("").trim().to_string();
            // A voice note is a first-class turn: transcribed in the spawned task (whisper takes a
            // few seconds - never on the poll loop), answered in text AND voice.
            let voice_fid = msg["voice"]["file_id"]
                .as_str()
                .or_else(|| msg["audio"]["file_id"].as_str())
                .map(String::from);
            // A photo is a first-class turn too: largest size, caption = the question.
            let photo_fid = msg["photo"]
                .as_array()
                .and_then(|a| a.last())
                .and_then(|p| p["file_id"].as_str())
                .map(String::from);
            let caption = msg["caption"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() && voice_fid.is_none() && photo_fid.is_none() {
                continue;
            }
            // Group-chat read-isolation: WHO is speaking (from.id) + on WHAT channel (private DM vs a
            // shared group). The owner resolves to a memory scope so a private fact never leaks across
            // members; a shared group's facts are visible to everyone in it.
            let from_id = msg["from"]["id"].as_i64().unwrap_or(0);
            let from_name = msg["from"]["first_name"].as_str().unwrap_or("someone").to_string();
            let chat_type = msg["chat"]["type"].as_str().unwrap_or("private").to_string();
            let shared_channel = chat_type == "group" || chat_type == "supergroup";
            // Process the turn in its OWN task so the poll loop keeps polling + ticking (delegations,
            // consolidation, DMN, proactive) no matter how long this turn takes. A child timer keeps
            // the "typing…" indicator alive (Telegram clears it after ~5s) for the full think time.
            let (api2, mem2, conv2) = (api.clone(), mem.clone(), conv.clone());
            let ac2 = active_chat.clone();
            tokio::spawn(async move {
                tg_typing(&api2, chat_id).await;
                // Photo turn: download → vision-analyze (caption as the question) → reply. Recorded
                // in the transcript so the conversation stays coherent.
                if let Some(fid) = photo_fid {
                    let owner = conv2.resolve_owner(from_id, shared_channel).await;
                    if owner == mind_types::PRIMARY {
                        ac2.store(chat_id, Ordering::Relaxed);
                        save_active_chat(chat_id);
                    }
                    if owner.starts_with("guest:") && std::env::var("YM_TG_OPEN").map(|v| v != "on").unwrap_or(true) {
                        let _ = tg_send(&api2, chat_id, "Hi! I'm a private family assistant, so I can't chat until you're added — I've let the family know. 🙏").await;
                        let primary = ac2.load(Ordering::Relaxed);
                        if primary != 0 && primary != chat_id {
                            let _ = tg_send(&api2, primary, &format!("👋 {from_name} sent me a photo but isn't registered (telegram id {from_id}). Share their contact card, or: person add <slug> {from_name} {from_id}")).await;
                        }
                        return;
                    }
                    let reply = match tg_download(&api2, &fid).await {
                        Some(bytes) => conv2.analyze_photo_turn(bytes, &caption).await,
                        None => "I couldn't download that photo from Telegram — mind sending it again?".to_string(),
                    };
                    let who = if owner == mind_types::PRIMARY { "[sent a photo]".to_string() } else { format!("[{owner} sent a photo]") };
                    let _ = mem2.append_message("user", &format!("{who} {caption}")).await;
                    let _ = mem2.append_message("assistant", &reply).await;
                    if let Err(e) = tg_send(&api2, chat_id, &reply).await {
                        eprintln!("[telegram] send error: {e}");
                    }
                    return;
                }
                let (text, via_voice) = if text.is_empty() {
                    match tg_voice_to_text(&api2, voice_fid.as_deref().unwrap_or_default()).await {
                        Some(t) => {
                            eprintln!("[voice] heard {} chars", t.len());
                            (t, true)
                        }
                        None => {
                            let _ = tg_send(&api2, chat_id, "I couldn't make out that voice note - mind trying once more?").await;
                            return;
                        }
                    }
                } else {
                    (text, false)
                };
                let owner = conv2.resolve_owner(from_id, shared_channel).await;
                if owner == mind_types::PRIMARY {
                    ac2.store(chat_id, Ordering::Relaxed);
                    save_active_chat(chat_id);
                }
                // FAMILY-ONLY (default): unregistered senders get a polite hello and the primary
                // gets an approval ping with the id — one contact-card share or `person add` lets
                // them in. YM_TG_OPEN=on re-enables anonymous guest conversations.
                if owner.starts_with("guest:") && std::env::var("YM_TG_OPEN").map(|v| v != "on").unwrap_or(true) {
                    eprintln!("[members] unregistered sender {from_name} tg_id={from_id}");
                    let _ = tg_send(&api2, chat_id, "Hi! I'm a private family assistant, so I can't chat until you're added — I've let the family know you said hello. 🙏").await;
                    let primary = ac2.load(Ordering::Relaxed);
                    if primary != 0 && primary != chat_id {
                        let _ = tg_send(&api2, primary, &format!("👋 {from_name} just messaged me but isn't registered (telegram id {from_id}). Share their contact card with me, or say: person add <slug> {from_name} {from_id}")).await;
                    }
                    return;
                }
                let identity = mind_conversation::TurnIdentity::new(owner, shared_channel);
                let work = handle_line_as(&text, &mem2, conv2.as_ref(), identity);
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
                // Voice in -> voice out: they spoke to us, so we speak back (gist as audio; the
                // full text is already delivered above).
                if via_voice && tg_send_voice(&api2, chat_id, &reply).await {
                    eprintln!("[voice] replied with voice");
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
                let ok = tg_send(&api, target, &note).await.is_ok();
                eprintln!("[notify] delivered={ok}: {}", note.chars().take(80).collect::<String>());
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
                    // Tracked news: when a topic is DUE for a digest (fresh developments + paced, state
                    // PERSISTED so restarts don't swallow updates), research it into a full CROSS-DOMAIN
                    // situation brief (news × live oil/markets × the user's portfolio) and send it. The
                    // ~15s brief runs detached so it never stalls the poll loop.
                    for topic in conv.news_digests_due().await {
                        let (c, api2) = (conv.clone(), api.clone());
                        tokio::spawn(async move {
                            // Learn-by-comparing: recall the held understanding, fetch fresh, and surface
                            // the DELTA ("since I last checked…") rather than re-briefing from scratch.
                            let update = c.evolve_understanding(&topic).await;
                            if tg_send(&api2, chat, &update).await.is_ok() {
                                c.note_proactive_sent().await;
                            }
                        });
                    }
                }
            }
        }

        // Prediction-resolver tick: grade any predictions whose deadline has passed against the current
        // understanding, write the hit/miss into per-domain calibration, and surface the verdict. Paced
        // (YM_RESOLVE_SECS, default 1h) and quiet-hours-gated; this is the self-scoring half of the
        // learning curve running on its own — no user prompt needed for tracked subjects.
        {
            let period: u64 = std::env::var("YM_RESOLVE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(3600);
            let now = now_ms();
            if now.saturating_sub(last_resolve) >= period * 1000 {
                let chat = active_chat.load(Ordering::Relaxed);
                for verdict in conv.resolve_predictions(false).await {
                    if chat != 0 && !in_quiet_hours_now() {
                        let _ = tg_send(&api, chat, &verdict).await;
                    }
                }
                last_resolve = now;
            }
        }

        // Periodic profile refresh: re-crawl the registered personal seed (site + linked profiles) so
        // personal facts stay current — a new paper, a role change, a new project surfaces on its own.
        // Paced (YM_PROFILE_REFRESH_SECS, default ~3 days); beliefs dedupe/reinforce, only genuinely new
        // facts are added. Background; a re-learn summary is surfaced when quiet-hours allow.
        {
            let period: u64 = std::env::var("YM_PROFILE_REFRESH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(259_200);
            let now = now_ms();
            if now.saturating_sub(last_profile) >= period * 1000 {
                if let Some(update) = conv.refresh_profile().await {
                    let chat = active_chat.load(Ordering::Relaxed);
                    if chat != 0 && !in_quiet_hours_now() {
                        let _ = tg_send(&api, chat, &format!("🧭 Refreshed what I know about you:\n\n{update}")).await;
                    }
                }
                last_profile = now;
            }
        }

        // Family tick: surface upcoming key dates (birthdays/anniversaries) before they arrive — the
        // "keep family updated" promise made proactive. Paced (YM_FAMILY_SECS, default 12h), quiet-gated,
        // deduped once-per-year per date inside family_date_nudges.
        {
            let period: u64 = std::env::var("YM_FAMILY_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(43_200);
            let now = now_ms();
            if now.saturating_sub(last_family) >= period * 1000 {
                let chat = active_chat.load(Ordering::Relaxed);
                if chat != 0 && !in_quiet_hours_now() {
                    // Birthdays deserve LEAD TIME to plan/shop — a 21-day window was too conservative
                    // (it read as "not doing anything" until the last minute). Default 28 days, tunable.
                    let window: i64 = std::env::var("YM_FAMILY_WINDOW").ok().and_then(|s| s.parse().ok()).unwrap_or(28);
                    for nudge in conv.family_date_nudges(window).await {
                        if tg_send(&api, chat, &nudge).await.is_ok() {
                            conv.note_proactive_sent().await;
                        }
                    }
                }
                last_family = now;
            }
        }

        // Morning-briefing tick: ONE warm briefing per day — the "JARVIS every morning" felt-presence.
        // briefing_due() self-gates (morning window + persisted once-per-date, survives restarts), so
        // this fires on the first non-quiet tick of the morning and stays silent the rest of the day.
        {
            let chat = active_chat.load(Ordering::Relaxed);
            if chat != 0 && !in_quiet_hours_now() {
                if let Some(msg) = conv.briefing_due().await {
                    if tg_send(&api, chat, &msg).await.is_ok() {
                        eprintln!("[briefing] sent the daily morning briefing ({} chars)", msg.len());
                        conv.note_proactive_sent().await;
                        conv.ledger_sent("briefing", "morning briefing").await;
                        // A real photo memory from this day in a past year rides the briefing —
                        // queued here, delivered by the photo drain a tick later.
                        if conv.queue_on_this_day().await {
                            eprintln!("[briefing] attached an on-this-day photo memory");
                        }
                    }
                }
            }
        }

        // Afternoon-foresight tick: ONE unprompted forecast a day (rotating tracked subjects + "me").
        // With the morning briefing this makes TWO guaranteed daily beats — presence, not exception.
        // foresight_due() self-gates (afternoon window + persisted once-per-date + rotation cursor);
        // the forecast itself takes a minute-plus, so it runs detached and never stalls the poll loop.
        {
            let chat = active_chat.load(Ordering::Relaxed);
            if chat != 0 && !in_quiet_hours_now() {
                if let Some(subject) = conv.foresight_due().await {
                    let (c, api2) = (conv.clone(), api.clone());
                    tokio::spawn(async move {
                        let msg = c.foresee(&subject).await;
                        if tg_send(&api2, chat, &msg).await.is_ok() {
                            eprintln!("[foresight] sent the daily proactive forecast on {subject}");
                            c.note_proactive_sent().await;
                        }
                    });
                }
            }
        }

        // Evening look-ahead tick: the THIRD daily beat — tomorrow's shape tonight (once per
        // evening, persisted-by-date; same restart-safe pattern as the briefing).
        {
            let chat = active_chat.load(Ordering::Relaxed);
            if chat != 0 && !in_quiet_hours_now() {
                if let Some(msg) = conv.evening_due().await {
                    if tg_send(&api, chat, &msg).await.is_ok() {
                        eprintln!("[evening] sent the look-ahead ({} chars)", msg.len());
                        conv.note_proactive_sent().await;
                    }
                }
            }
        }

        // Follow-through tick: escalating deadline nudges on open reminders (10/5/2 days + overdue),
        // each stage once (persisted). Cheap check, paced (YM_FOLLOWUP_SECS, default 6h), quiet-gated.
        {
            let period: u64 = std::env::var("YM_FOLLOWUP_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(21_600);
            let now = now_ms();
            if now.saturating_sub(last_followup) >= period * 1000 {
                let chat = active_chat.load(Ordering::Relaxed);
                if chat != 0 && !in_quiet_hours_now() {
                    for nudge in conv.deadline_followups().await {
                        if tg_send(&api, chat, &nudge).await.is_ok() {
                            conv.note_proactive_sent().await;
                        }
                    }
                }
                last_followup = now;
            }
        }

        // Pre-event prep tick — the "JARVIS move": shortly before anything on the calendar, a
        // memory-grounded heads-up (what I know about the people involved + practicals). Marked
        // once per event (persisted) by events_needing_prep; composition is LLM+weather so it runs
        // detached. Quiet-gated like every outward surface.
        {
            let chat = active_chat.load(Ordering::Relaxed);
            if chat != 0 && !in_quiet_hours_now() {
                for (title, ms) in conv.events_needing_prep().await {
                    let (c, api2) = (conv.clone(), api.clone());
                    tokio::spawn(async move {
                        if let Some(msg) = c.compose_event_prep(&title, ms).await {
                            if tg_send(&api2, chat, &msg).await.is_ok() {
                                eprintln!("[prep] sent pre-event prep for {title}");
                                c.note_proactive_sent().await;
                            }
                        }
                    });
                }
            }
        }

        // Price-watch tick: re-price tracked items and ping on a genuine drop / target hit. Paced
        // (YM_WATCH_SECS, default 12h), quiet-gated. The deal-finder's compounding half.
        {
            let period: u64 = std::env::var("YM_WATCH_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(43_200);
            let now = now_ms();
            if now.saturating_sub(last_pricewatch) >= period * 1000 {
                let chat = active_chat.load(Ordering::Relaxed);
                if chat != 0 && !in_quiet_hours_now() {
                    for alert in conv.check_price_watches().await {
                        let _ = tg_send(&api, chat, &alert).await;
                    }
                }
                last_pricewatch = now;
            }
        }

        // Consolidation tick: distill new conversation turns into durable typed beliefs (the moat's
        // compounding loop). Self-gates until enough new turns accrue; background, not surfaced.
        let formed = conv.consolidate().await;
        if formed > 0 {
            eprintln!("[consolidate] formed {formed} durable memories");
        }

        // Compaction tick: absorb aging turns into the persisted rolling summary (continuity beyond
        // the raw-turn window; survives restarts). Cheap early-return until enough turns accrue.
        conv.compact_conversation().await;

        // Outbound video queue: growing-up reels finished by the detached builder task.
        {
            let primary = active_chat.load(Ordering::Relaxed);
            for (mp4, caption, target) in conv.take_outbound_videos() {
                let chat = target.unwrap_or(primary);
                if chat == 0 {
                    continue;
                }
                if tg_send_video(&api, chat, mp4, &caption).await {
                    eprintln!("[reel] delivered: {caption}");
                } else {
                    eprintln!("[reel] send failed: {caption}");
                }
            }
        }

        // Outbound photo queue: images the conversation layer decided to send (photo retrieval).
        // Direct answers to the user's own ask, so quiet-hours don't gate them.
        {
            let primary = active_chat.load(Ordering::Relaxed);
            for (jpeg, caption, target) in conv.take_outbound_photos() {
                let chat = target.unwrap_or(primary);
                if chat == 0 {
                    continue;
                }
                if !tg_send_photo(&api, chat, jpeg, &caption).await {
                    eprintln!("[photo] send failed: {caption}");
                }
            }
        }

        // Gift scout: someone's day within 25 days → study their photos unprompted and deliver
        // gift intelligence while there's still shipping time. Daily-capped, quiet-gated, detached
        // (12 vision reads take minutes and must never stall the poll loop).
        {
            let chat = active_chat.load(Ordering::Relaxed);
            if chat != 0 && !in_quiet_hours_now() && conv.gift_scout_due().await && conv.proactive_receptivity_ok().await {
                let c = conv.clone();
                let api2 = api.clone();
                tokio::spawn(async move {
                    if let Some(msg) = c.gift_scout_run().await {
                        if tg_send(&api2, chat, &msg).await.is_ok() {
                            eprintln!("[gift] proactive gift intel delivered");
                            c.note_proactive_sent().await;
                        }
                    }
                });
            }
        }

        // Ask-who-is-who: ONE unknown-face question per period (or immediately via `ym whois`).
        // The face crop goes as a real photo; the reply lands in the pending-slot interview path
        // and becomes people-layer knowledge + a local face-name mapping.
        {
            let chat = active_chat.load(Ordering::Relaxed);
            if chat != 0 {
                let forced = conv.whois_forced().await;
                if forced
                    || (!in_quiet_hours_now()
                        && conv.whois_due().await
                        && conv.proactive_receptivity_ok().await)
                {
                    if let Some((caption, jpeg, slot)) = conv.whois_next().await {
                        if tg_send_photo(&api, chat, jpeg, &caption).await {
                            conv.whois_arm(&slot).await;
                            eprintln!("[whois] asked about face {slot}");
                        }
                    }
                }
            }
        }

        // Member beats: every registered family member's due reminders + opt-in morning brief,
        // delivered to THEIR own chat (owner-keyed end to end). Quiet-hours respected.
        {
            let now = now_ms();
            if now.saturating_sub(last_member_beat) >= 120_000 && !in_quiet_hours_now() {
                for (chat, text) in conv.member_beats().await {
                    if tg_send(&api, chat, &text).await.is_ok() {
                        eprintln!("[member] beat delivered to {chat}");
                    }
                }
                last_member_beat = now;
            }
        }

        // Daily mail sweep: cross-account analytics with body-peek verification; the user hears
        // about it ONLY when something needs action (silence-biased). Detached — two LLM passes
        // plus IMAP round-trips must never stall the poll loop.
        if !in_quiet_hours_now() && conv.mail_sweep_due().await {
            let c = conv.clone();
            let api2 = api.clone();
            let chat = active_chat.load(Ordering::Relaxed);
            if chat != 0 {
                tokio::spawn(async move {
                    if let Some(msg) = c.mail_sweep_run().await {
                        if tg_send(&api2, chat, &msg).await.is_ok() {
                            c.note_proactive_sent().await;
                        }
                    }
                });
            }
        }

        // WEEKLY SELF-REPORT: the mind reviews its own week — scoreboard, absorbed corrections,
        // and the pacing policies it changes as a result (the learning-ledger loop, closed).
        {
            let chat = active_chat.load(Ordering::Relaxed);
            if chat != 0 && !in_quiet_hours_now() && conv.report_due().await {
                let c = conv.clone();
                let api2 = api.clone();
                tokio::spawn(async move {
                    let msg = c.self_report(true).await;
                    if tg_send(&api2, chat, &msg).await.is_ok() {
                        eprintln!("[report] weekly self-report delivered");
                        c.note_proactive_sent().await;
                    }
                });
            }
        }

        // Facebook refresh: keep the know-me lane current (daily; data-only, sends nothing).
        if conv.fb_sync_due().await {
            let c = conv.clone();
            tokio::spawn(async move {
                let r = c.fb_sync().await;
                eprintln!("[fb] {}", r.chars().take(140).collect::<String>());
            });
        }

        // Resolve a STALE proactive send (past the 90-min window, no reply) as IGNORED — the world
        // model learns dead zones from silence just as it learns receptive windows from replies.
        conv.resolve_proactive(false).await;
        conv.ledger_resolve(false).await;

        // External-calendar refresh: re-pull the read-only ICS feed if one is connected. Paced
        // (YM_ICS_SECS, default 6h); no chat gating — it only updates stored events, sends nothing.
        {
            let period: u64 = std::env::var("YM_ICS_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(21_600);
            let now = now_ms();
            if now.saturating_sub(last_ics) >= period * 1000 {
                let n = conv.refresh_ics().await;
                if n > 0 {
                    eprintln!("[calendar] refreshed {n} external event(s)");
                }
                last_ics = now;
            }
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
                std::env::var("YM_ASK_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(7_200);
            let now = now_ms();
            let chat = active_chat.load(Ordering::Relaxed);
            let idle_ok = chat != 0
                && !in_quiet_hours_now()
                && now.saturating_sub(last_activity) >= idle_secs * 1000;
            let mut spoke = false;
            if idle_ok && now.saturating_sub(last_digest) >= pd_secs * 1000 && conv.proactive_receptivity_ok().await {
                if let Some(msg) = conv.proactive_digest().await {
                    if tg_send(&api, chat, &msg).await.is_ok() {
                        eprintln!("[proactive] surfaced a digest ({} chars)", msg.len());
                        conv.note_proactive_sent().await;
                        spoke = true;
                    }
                }
                last_digest = now; // reset cadence whether or not we spoke (never hammer)
            }
            // Asking is NORMAL conversation, not a rare scheduled event — so the ask-drive gets its
            // own LIGHT gate (a 2-min lull, not the 10-min deep-idle the heavier surfaces use).
            let ask_idle: u64 =
                std::env::var("YM_ASK_IDLE_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(120);
            let ask_ok = chat != 0
                && !in_quiet_hours_now()
                && now.saturating_sub(last_activity) >= ask_idle * 1000;
            if !spoke
                && std::env::var("YM_ASK").map(|v| v != "off").unwrap_or(true)
                && ask_ok
                && now.saturating_sub(last_ask) >= ask_secs * 1000
                && conv.proactive_receptivity_ok().await
            {
                if let Some(q) = conv.proactive_ask().await {
                    if tg_send(&api, chat, &q).await.is_ok() {
                        eprintln!("[ask] posed a get-to-know-you question");
                        conv.note_proactive_sent().await;
                    }
                }
                last_ask = now; // reset cadence whether or not it asked
            }
            // Pattern-finder surface — the flagship "learn from memory" loop turned outward. On its own
            // slow cadence (default ~2 days), while idle + awake, run the cross-domain pattern analysis;
            // it SAVES survivors as learned beliefs regardless, but only MESSAGES the user when it found
            // a real, grounded one (the 💡 marker). Never competes with a digest/ask in the same tick.
            let pat_secs: u64 =
                std::env::var("YM_PATTERNS_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(172_800);
            if !spoke
                && std::env::var("YM_PATTERNS").map(|v| v != "off").unwrap_or(true)
                && idle_ok
                && now.saturating_sub(last_patterns) >= pat_secs * 1000
            {
                let msg = conv.find_patterns().await;
                if msg.starts_with('\u{1f4a1}') && tg_send(&api, chat, &msg).await.is_ok() {
                    eprintln!("[patterns] surfaced a learned pattern ({} chars)", msg.len());
                    conv.note_proactive_sent().await;
                }
                last_patterns = now; // reset cadence whether or not it found one
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
