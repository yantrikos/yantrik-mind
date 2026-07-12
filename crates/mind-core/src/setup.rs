//! setup — the first-run onboarding wizard (`ym setup`).
//!
//! The install script's job ends at "the binary exists + a service user exists";
//! everything a human decides happens HERE, in the binary, so it is
//! re-runnable (`ym setup` again) and robust. The design is lifted from the
//! best-in-class installers researched 2026-07-12 (Ollama, Tailscale,
//! Homebrew, rustup, BotFather flows):
//!
//!   1. Validate the pasted Telegram token on the spot with getMe — confirm it
//!      works and auto-derive the bot's @username (never ask for it).
//!   2. The Tailscale trick, but the product IS the auth channel: print
//!      t.me/<bot>?start=<one-time-code>, long-poll getUpdates, and the moment
//!      the owner taps Start we capture their chat_id and greet them live.
//!   3. Write /etc/yantrik-mind.env (mode 600) with only what's needed to
//!      breathe — Telegram + the brain key. Email/GitHub are optional and
//!      added later; first-run is never blocked on them.
//!   4. End at a DOORWAY: the last thing printed is the t.me link, and the
//!      companion's first message is already waiting in Telegram.
//!
//! Non-interactive twin: every prompt reads its default from an env var
//! (YM_TELEGRAM_TOKEN, NANOGPT_KEY, YM_ENV_PATH), so a provisioning script can
//! run `ym setup` unattended.

use std::io::{BufRead, Write};

const C_RESET: &str = "\x1b[0m";
const C_DIM: &str = "\x1b[2m";
const C_BOLD: &str = "\x1b[1m";
const C_SIG: &str = "\x1b[38;5;179m"; // warm amber — the "signal" accent
const C_OK: &str = "\x1b[38;5;42m"; // teal-green — verified
const C_ERR: &str = "\x1b[38;5;203m"; // soft red

fn color() -> bool {
    // Colors only on a real terminal, and honor NO_COLOR.
    std::env::var("NO_COLOR").is_err() && atty_stdout()
}
fn atty_stdout() -> bool {
    // Minimal TTY check without a dep: isatty(1) via libc is unavailable here,
    // so approximate — TERM set and not "dumb". Good enough for cosmetics.
    std::env::var("TERM").map(|t| t != "dumb" && !t.is_empty()).unwrap_or(false)
}
fn paint(c: &str, s: &str) -> String {
    if color() { format!("{c}{s}{C_RESET}") } else { s.to_string() }
}
fn step(n: u8, total: u8, msg: &str) {
    println!("\n{} {}", paint(C_SIG, &format!("[{n}/{total}]")), paint(C_BOLD, msg));
}
fn ok(msg: &str) {
    println!("  {} {msg}", paint(C_OK, "✓"));
}
fn warn(msg: &str) {
    println!("  {} {msg}", paint(C_SIG, "!"));
}
fn err(msg: &str) {
    eprintln!("  {} {msg}", paint(C_ERR, "✗"));
}

/// Prompt reading from /dev/tty when possible (so it works even under a pipe),
/// falling back to stdin. Returns the trimmed line; empty string on EOF.
fn ask(prompt: &str) -> String {
    print!("{} ", paint(C_SIG, prompt));
    std::io::stdout().flush().ok();
    // Prefer /dev/tty so `curl | bash` still gets keystrokes.
    if let Ok(tty) = std::fs::OpenOptions::new().read(true).open("/dev/tty") {
        let mut line = String::new();
        if std::io::BufReader::new(tty).read_line(&mut line).is_ok() {
            return line.trim().to_string();
        }
    }
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok();
    line.trim().to_string()
}

/// Bail cleanly when there's no input source (piped stdin, no /dev/tty, and
/// the required value wasn't provided via env) — rather than spinning forever.
fn die_no_input(env_key: &str) -> ! {
    err(&format!(
        "no {env_key} and no interactive terminal — set {env_key} in the environment to run setup non-interactively."
    ));
    std::process::exit(2);
}

fn tg_api(token: &str) -> String {
    format!("https://api.telegram.org/bot{token}")
}

/// getMe → the bot's @username, or an error. Redacts the token from any error.
fn validate_token(token: &str) -> Result<String, String> {
    let api = tg_api(token);
    let masked = |e: String| e.replace(&api, "https://api.telegram.org/bot<token>");
    let resp: serde_json::Value = ureq::get(&format!("{api}/getMe"))
        .timeout(std::time::Duration::from_secs(12))
        .call()
        .map_err(|e| masked(e.to_string()))?
        .into_json()
        .map_err(|e| e.to_string())?;
    if resp["ok"].as_bool() != Some(true) {
        return Err("Telegram rejected that token".into());
    }
    resp["result"]["username"]
        .as_str()
        .map(|u| u.to_string())
        .ok_or_else(|| "no bot username in getMe response".into())
}

/// Validate the inference key with a cheap authenticated call — the mind is
/// no "doorway" if it can't think (sol: without a brain key main falls silently
/// to scripted). Hits NanoGPT's models list; 200 = the key authenticates.
fn validate_brain(key: &str) -> Result<(), String> {
    let resp = ureq::get("https://nano-gpt.com/api/v1/models")
        .set("Authorization", &format!("Bearer {key}"))
        .timeout(std::time::Duration::from_secs(15))
        .call();
    match resp {
        Ok(r) if r.status() == 200 => Ok(()),
        Ok(r) => Err(format!("the key was rejected (HTTP {})", r.status())),
        Err(ureq::Error::Status(401 | 403, _)) => Err("the key was rejected (unauthorized)".into()),
        Err(_) => Err("couldn't reach the inference backend".into()),
    }
}

/// Long-poll getUpdates until a `/start <code>` (or any message, if code is
/// empty) arrives, returning (chat_id, first_name, next_offset). The offset is
/// handed to the service so its single poller resumes AFTER the linking
/// message and never reprocesses it (Telegram allows one getUpdates consumer
/// per token — setup owns it exclusively, then relinquishes cleanly).
fn await_first_contact(token: &str, code: &str, secs: u64) -> Result<(i64, String, i64), String> {
    let api = tg_api(token);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut offset: i64 = 0;
    // Drain any stale updates first so an old message can't false-trigger.
    while std::time::Instant::now() < deadline {
        let url = format!("{api}/getUpdates?timeout=20&offset={offset}");
        let resp: serde_json::Value = match ureq::get(&url)
            .timeout(std::time::Duration::from_secs(30))
            .call()
            .ok()
            .and_then(|r| r.into_json().ok())
        {
            Some(v) => v,
            None => {
                std::thread::sleep(std::time::Duration::from_secs(2));
                continue;
            }
        };
        for upd in resp["result"].as_array().cloned().unwrap_or_default() {
            offset = upd["update_id"].as_i64().unwrap_or(offset) + 1;
            let msg = &upd["message"];
            let text = msg["text"].as_str().unwrap_or("");
            let chat_id = msg["chat"]["id"].as_i64();
            let first = msg["from"]["first_name"].as_str().unwrap_or("there").to_string();
            let matches = if code.is_empty() {
                !text.is_empty()
            } else {
                text == format!("/start {code}") || text == format!("/start@_ {code}")
            };
            if matches {
                if let Some(cid) = chat_id {
                    return Ok((cid, first, offset));
                }
            }
        }
    }
    Err("timed out waiting for the first message".into())
}

/// A warm greeting that DELIBERATELY does not ask the name. The name question
/// must come from the running mind's onboarding state machine (proactive_ask),
/// which arms the pending slot so the reply is captured — a raw question here
/// would bypass that and the answer would be lost (caught in sol review).
fn send_greeting(token: &str, chat_id: i64) {
    let api = tg_api(token);
    let text = "I'm here. 🌱\n\nI'm your companion — I'll remember what matters to your family, help you show up for the people you love, and I can always prove what I know and how sure I am.\n\nWaking up fully now — say hello and we'll get to know each other.";
    let _ = ureq::post(&format!("{api}/sendMessage"))
        .timeout(std::time::Duration::from_secs(12))
        .send_json(serde_json::json!({ "chat_id": chat_id, "text": text }));
}

/// Hand the linked chat + resume-offset to the service files it reads on boot,
/// so the moment the poller starts it already knows the active chat and picks
/// up cleanly after the /start message.
fn persist_handoff(chat_id: i64, next_offset: i64) {
    let offset_path = std::env::var("YM_TG_OFFSET").unwrap_or_else(|_| "/var/lib/yantrik-mind/tg_offset".into());
    let _ = std::fs::write(&offset_path, next_offset.to_string());
    let _ = std::fs::write(format!("{offset_path}.active_chat"), chat_id.to_string());
}

/// Start the always-on service, if we can (setup owns the transition to
/// running per sol's decision). Best-effort: returns whether it started.
fn start_service() -> bool {
    let ok = std::process::Command::new("systemctl")
        .args(["enable", "--now", "yantrik-mind"])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    ok
}

/// A short, human one-time code (avoids ambiguous chars). Deterministic-free
/// randomness isn't available without a dep; derive from the nanosecond clock,
/// which is fine for a single-use linking code.
fn one_time_code() -> String {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    const ALPH: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut x = n as u64 ^ 0x9e3779b97f4a7c15;
    (0..6)
        .map(|_| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
            ALPH[(x >> 33) as usize % ALPH.len()] as char
        })
        .collect()
}

/// Run the wizard. Returns the path written on success.
pub fn run() -> anyhow::Result<()> {
    let total = 4u8;
    println!(
        "\n{}\n{}",
        paint(C_BOLD, "  yantrik-mind · first-run setup"),
        paint(C_DIM, "  a companion that remembers, helps you show up, and proves what it knows")
    );

    let env_path = std::env::var("YM_ENV_PATH").unwrap_or_else(|_| "/etc/yantrik-mind.env".into());

    // ---- Step 1: the brain key -------------------------------------------
    step(1, total, "The brain — an inference key");
    println!("  {}", paint(C_DIM, "yantrik-mind thinks via a hosted model (no GPU needed). Paste your NanoGPT key."));
    let mut nano = std::env::var("NANOGPT_KEY").unwrap_or_default();
    let mut empties = 0;
    loop {
        if nano.trim().is_empty() {
            nano = ask("  NanoGPT key ›");
        }
        if nano.trim().is_empty() {
            empties += 1;
            if empties >= 3 {
                die_no_input("NANOGPT_KEY");
            }
            err("a brain key is required — without it the mind can't actually think. (Ctrl-C to abort.)");
            continue;
        }
        match validate_brain(nano.trim()) {
            Ok(()) => {
                ok("brain key verified — the mind can think.");
                break;
            }
            Err(e) => {
                err(&format!("{e} — check the key and try again."));
                nano.clear();
            }
        }
    }

    // ---- Step 2: the Telegram bot ----------------------------------------
    step(2, total, "The phone line — a Telegram bot");
    println!("  {}", paint(C_DIM, "In Telegram, message @BotFather → /newbot → pick a name. It replies with a token."));
    println!("  {}", paint(C_DIM, "Tap to open BotFather:  https://t.me/BotFather"));
    let mut token = std::env::var("YM_TELEGRAM_TOKEN").unwrap_or_default();
    let mut tok_empties = 0;
    let bot_username = loop {
        if token.trim().is_empty() {
            token = ask("  Bot token ›");
        }
        if token.trim().is_empty() {
            tok_empties += 1;
            if tok_empties >= 3 {
                die_no_input("YM_TELEGRAM_TOKEN");
            }
            err("a token is required to link your companion. (Ctrl-C to abort.)");
            continue;
        }
        match validate_token(token.trim()) {
            Ok(u) => {
                ok(&format!("token verified — your bot is @{u}"));
                break u;
            }
            Err(e) => {
                err(&format!("{e}. Expected a token like 123456789:ABCdef...  — try again."));
                token.clear();
            }
        }
    };
    let token = token.trim().to_string();

    // ---- Step 3: write the environment -----------------------------------
    step(3, total, "Writing configuration");
    println!("  {} {}", paint(C_DIM, "will write:"), paint(C_BOLD, &env_path));
    let existing = std::fs::read_to_string(&env_path).unwrap_or_default();
    let env_body = render_env(&existing, &token, &nano);
    write_env_600(&env_path, &env_body)?;
    ok(&format!("{env_path} (mode 600 — secrets stay on this box)"));

    // ---- Step 4: the doorway — link the owner live -----------------------
    step(4, total, "Meet your companion");
    let code = one_time_code();
    let link = format!("https://t.me/{bot_username}?start={code}");
    println!("\n  {}", paint(C_BOLD, "Open this on your phone — it opens the chat with your bot:"));
    println!("      {}", paint(C_SIG, &link));
    print_qr(&link);
    println!("\n  {}", paint(C_DIM, "Waiting for you to tap Start… (2 min)"));

    match await_first_contact(&token, &code, 120) {
        Ok((chat_id, name, offset)) => {
            // Lock the companion to this chat, hand the offset + chat to the
            // service, greet in-app, then start the service so the mind itself
            // asks the name through its onboarding state machine.
            let updated = upsert_env_line(&env_body, "YM_TELEGRAM_CHAT", &chat_id.to_string());
            let _ = write_env_600(&env_path, &updated);
            persist_handoff(chat_id, offset + 1);
            send_greeting(&token, chat_id);
            println!();
            ok(&format!("Linked to {} — your companion just said hello.", paint(C_BOLD, &name)));
            if start_service() {
                ok("always-on service started.");
            } else {
                println!("  {}", paint(C_DIM, "Start the always-on service:  sudo systemctl enable --now yantrik-mind"));
            }
            println!(
                "\n  {}\n      {}",
                paint(C_BOLD, "👉 Open Telegram — the conversation is already waiting:"),
                paint(C_SIG, &format!("https://t.me/{bot_username}"))
            );
        }
        Err(_) => {
            warn("didn't see your message yet — no problem, that step can happen anytime.");
            println!(
                "  {}\n      {}\n  {}",
                paint(C_BOLD, "Start the service, then open your bot and say hi:"),
                paint(C_SIG, &format!("sudo systemctl enable --now yantrik-mind   ·   https://t.me/{bot_username}")),
                paint(C_DIM, "It will greet you and lock to your chat on your first message.")
            );
        }
    }
    println!();
    Ok(())
}

/// Render the env file: preserve any existing non-secret lines, upsert the two
/// keys we manage here. Keeps the file human-editable.
fn render_env(existing: &str, token: &str, nano: &str) -> String {
    let mut body = if existing.trim().is_empty() {
        default_env_scaffold()
    } else {
        existing.to_string()
    };
    body = upsert_env_line(&body, "YM_TELEGRAM_TOKEN", token);
    if !nano.trim().is_empty() {
        body = upsert_env_line(&body, "NANOGPT_KEY", nano.trim());
    }
    body
}

fn default_env_scaffold() -> String {
    "# yantrik-mind environment — written by `ym setup`. chmod 600. Never commit.\n\
     YM_OPERATOR=you\n\
     YM_DB=/var/lib/yantrik-mind/mind.db\n\
     YM_TG_OFFSET=/var/lib/yantrik-mind/tg_offset\n\
     YM_MODEL=deepseek/deepseek-v4-pro-cheaper\n\
     # Optional integrations — add later with `ym connect`:\n\
     # YM_EMAIL=  YM_EMAIL_PASSWORD=  YM_GITHUB_TOKEN=\n"
        .to_string()
}

/// Insert or replace `KEY=value`, preserving surrounding lines.
fn upsert_env_line(body: &str, key: &str, value: &str) -> String {
    let mut found = false;
    let mut out: Vec<String> = body
        .lines()
        .map(|l| {
            if l.trim_start().starts_with(&format!("{key}=")) {
                found = true;
                format!("{key}={value}")
            } else {
                l.to_string()
            }
        })
        .collect();
    if !found {
        out.push(format!("{key}={value}"));
    }
    let mut s = out.join("\n");
    if !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

fn write_env_600(path: &str, body: &str) -> anyhow::Result<()> {
    std::fs::write(path, body)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// A compact terminal QR of the link, so a phone can scan it. Best-effort:
/// uses the `qrencode` CLI if present (common on Linux), else prints nothing —
/// the tappable link above always works.
fn print_qr(link: &str) {
    if let Ok(out) = std::process::Command::new("qrencode")
        .args(["-t", "ANSIUTF8", "-m", "1", link])
        .output()
    {
        if out.status.success() {
            print!("\n{}", String::from_utf8_lossy(&out.stdout));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upsert_replaces_and_appends() {
        let body = "YM_OPERATOR=you\nYM_TELEGRAM_TOKEN=old\n";
        let r = upsert_env_line(body, "YM_TELEGRAM_TOKEN", "new");
        assert!(r.contains("YM_TELEGRAM_TOKEN=new"));
        assert!(!r.contains("=old"));
        assert!(r.contains("YM_OPERATOR=you"));
        let r2 = upsert_env_line(&r, "YM_TELEGRAM_CHAT", "42");
        assert!(r2.contains("YM_TELEGRAM_CHAT=42"));
        assert!(r2.ends_with('\n'));
    }

    #[test]
    fn render_preserves_existing_and_sets_secrets() {
        let existing = "# hand notes\nYM_QUIET_START=22\nNANOGPT_KEY=__placeholder__\n";
        let r = render_env(existing, "tok123", "realkey");
        assert!(r.contains("YM_QUIET_START=22"), "preserves user lines");
        assert!(r.contains("YM_TELEGRAM_TOKEN=tok123"));
        assert!(r.contains("NANOGPT_KEY=realkey"));
        assert!(!r.contains("__placeholder__"));
    }

    #[test]
    fn one_time_code_is_shaped() {
        let c = one_time_code();
        assert_eq!(c.len(), 6);
        assert!(c.chars().all(|ch| ch.is_ascii_alphanumeric()));
        // No ambiguous chars.
        assert!(!c.contains(['0', 'O', 'I', '1']));
    }
}
