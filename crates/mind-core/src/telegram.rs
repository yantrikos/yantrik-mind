//! Telegram channel — talk to yantrik-mind from your phone. A minimal, resilient long-poll loop
//! that routes every inbound message through the same `handle_line` as the REPL, so chat, learning,
//! commitments, tasks, and commands all work over telegram. The bot token is read from the
//! `YM_TELEGRAM_TOKEN` env var — never hardcoded or committed.
//!
//! Offset is persisted (so a restart doesn't replay old messages). Network/parse errors are logged
//! and retried; the loop never crashes.

use std::io::Write;

use mind_conversation::ConversationEngine;
use mind_memory::MemoryHandle;

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

async fn tg_send(api: &str, chat_id: i64, text: &str) -> anyhow::Result<()> {
    let url = format!("{api}/sendMessage");
    let payload = serde_json::json!({ "chat_id": chat_id, "text": text });
    tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        ureq::post(&url)
            .timeout(std::time::Duration::from_secs(30))
            .send_json(payload)?;
        Ok(())
    })
    .await??;
    Ok(())
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
    let chat_lock: Option<i64> = std::env::var("YM_TELEGRAM_CHAT").ok().and_then(|s| s.trim().parse().ok());

    let mut offset = load_offset();
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
            let text = msg["text"].as_str().unwrap_or("").trim().to_string();
            if text.is_empty() {
                continue;
            }
            let reply = match handle_line(&text, &mem, &conv).await {
                Outcome::Quit => "(the mind keeps running — nothing to quit here)".to_string(),
                Outcome::Said(s) if s.is_empty() => continue,
                Outcome::Said(s) => s,
            };
            if let Err(e) = tg_send(&api, chat_id, &reply).await {
                eprintln!("[telegram] send error: {e}");
            }
        }
    }
}
