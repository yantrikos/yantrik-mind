//! The desktop channel — this mind attaches to Yantrik OS and answers what is typed there.
//!
//! Telegram is the phone, `run_headless` is the service, the REPL is a terminal. This is the
//! fourth surface and the one the OS was built for: the Lens on the desktop, the chat panel, the
//! Ask bar in every app. One mind behind all of them, because they all end up here.
//!
//! # Why this dials out, and why it looks so small
//!
//! The OS never reaches for a mind. It has no endpoint for one, no model name, no key, and no
//! config file describing what minds exist — deliberately, because the moment it had any of that
//! it would own decisions belonging to whoever runs the mind. What it offers instead is a socket
//! to attach to. A harness announces itself, asks for work, and answers; being attached is the
//! whole of what it means to exist there. Nothing is registered, nothing is installed, and
//! nothing has to be torn down when this process stops — the picker simply stops listing it.
//!
//! So everything about being THIS mind — the provider chain, the typed memory, the tool bus, the
//! governance around a turn — stays exactly where it already was. What arrives here is a line of
//! text and what leaves is the answer, through `handle_line_as`, which is the same door the
//! terminal and the phone knock on.
//!
//! # Nothing from the OS is linked in
//!
//! Not one line below imports a Yantrik OS crate, and that is a test rather than an accident: if
//! the mind needed the OS's types to talk to it, then so would every other harness, and the claim
//! that anything speaking JSON-RPC can attach would be false. It is a unix socket, one JSON
//! object per line, six methods. That is the entire integration surface, and it fits on a screen.
//!
//! # What it is allowed to do
//!
//! Operator, like the terminal REPL — not Principal, like Telegram. The difference is not about
//! who is typing but about what the channel can prove about itself. Telegram's transport is the
//! open internet and a bot token; anyone who obtains the token is on the channel. This socket
//! lives in a directory the OS creates at mode 0700, so the only processes that can reach it are
//! ones already running as the owner — which is precisely the boundary a tty sits behind. A
//! channel exactly as private as the console gets exactly what the console gets.
//!
//! `YM_HARNESS_SCOPE=member` narrows it to the Telegram footing for anyone who disagrees, and
//! `YM_HARNESS=off` keeps the mind off the desktop entirely.

use std::sync::Arc;
use std::time::Duration;

use mind_conversation::ConversationEngine;
use mind_memory::MemoryHandle;

#[cfg(unix)]
use crate::{handle_line_as, Outcome};

/// What a person types to choose this mind, and what the picker shows.
const ID: &str = "mind";
const NAME: &str = "Yantrik Mind";

/// The six methods. Spelled out rather than imported — see the module doc.
#[cfg(unix)]
const ATTACH: &str = "harness.attach";
#[cfg(unix)]
const POLL: &str = "harness.poll";
#[cfg(unix)]
const CHUNK: &str = "harness.chunk";
#[cfg(unix)]
const COMPLETE: &str = "harness.complete";
#[cfg(unix)]
const FAIL: &str = "harness.fail";

/// How long to wait after an empty poll. The OS's `poll` answers at once either way, so this is
/// the client's own pacing, and it is the interval the protocol documents.
#[cfg(unix)]
const IDLE: Duration = Duration::from_millis(200);

/// How long to wait before looking for the desktop again.
///
/// Hit in two ordinary situations: this mind started before the shell did, and the shell
/// restarted. Neither is an error and neither needs a person, so both are simply retried.
#[cfg(unix)]
const RETRY: Duration = Duration::from_secs(5);

/// What the picker shows under the name: which backend is behind this mind, and which memory.
///
/// Set once by `main`, which is the only place that knows both. A OnceLock rather than an
/// argument because the three channels that start this — telegram, headless, REPL — do not have
/// that knowledge and should not have to carry it through to hand it back.
static DETAIL: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Say what this mind is running on, before any channel starts.
pub fn announce(detail: String) {
    let _ = DETAIL.set(detail);
}

/// Set when this mind started without a model. The desktop channel then runs the first-run
/// conversation (see [`crate::first_run`]) instead of passing turns to a placeholder.
static NEEDS_SETUP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Say that there is no model yet, before any channel starts.
pub fn announce_needs_setup() {
    NEEDS_SETUP.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Start answering the desktop, if there is one. Returns immediately.
///
/// Safe to call on a machine that has never heard of Yantrik OS: it finds no socket, says so
/// once, and keeps looking quietly in case one appears.
pub fn attach_in_background(mem: MemoryHandle, conv: Arc<ConversationEngine>) {
    let detail = DETAIL.get().cloned().unwrap_or_default();
    if std::env::var("YM_HARNESS")
        .map(|v| v.trim().eq_ignore_ascii_case("off"))
        .unwrap_or(false)
    {
        eprintln!("[harness] desktop channel disabled (YM_HARNESS=off)");
        return;
    }
    #[cfg(unix)]
    {
        tokio::spawn(run(mem, conv, detail));
    }
    #[cfg(not(unix))]
    {
        // Yantrik OS is Linux and its bus is a unix socket. Saying so beats a silent no-op on a
        // Windows dev box, where a missing desktop channel would otherwise look like a bug.
        let _ = (mem, conv, detail);
        eprintln!("[harness] desktop channel is unix-only; not attaching on this platform");
    }
}

// ═══════════════════════════════════════════════════════════════════════════════════════════
// The client
// ═══════════════════════════════════════════════════════════════════════════════════════════

#[cfg(unix)]
mod wire {
    use std::io::{BufRead, BufReader, Write};
    use std::path::PathBuf;
    use std::time::Duration;

    /// One round trip: connect, write a line, read a line, close.
    ///
    /// A connection per call, which is what the bus expects and what makes this robust: there is
    /// no session held open at the socket level, so a shell that restarts costs one failed call
    /// rather than a stuck stream.
    pub fn call(
        address: &str,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Result<serde_json::Value, String> {
        use std::os::unix::net::UnixStream;

        let stream = UnixStream::connect(address).map_err(|e| format!("connect {address}: {e}"))?;
        stream.set_read_timeout(Some(timeout)).ok();
        // A peer that has stopped reading blocks the writer just as effectively as a silent one
        // blocks the reader, so both directions are bounded.
        stream.set_write_timeout(Some(timeout)).ok();

        let req = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
            "id": 1,
        });
        let mut out = stream.try_clone().map_err(|e| format!("clone: {e}"))?;
        out.write_all(format!("{req}\n").as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        out.flush().map_err(|e| format!("flush: {e}"))?;

        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .map_err(|e| format!("read: {e}"))?;
        if line.trim().is_empty() {
            return Err("the desktop closed the connection without answering".into());
        }
        let reply: serde_json::Value = serde_json::from_str(&line)
            .map_err(|e| format!("parse: {e} — got: {}", line.trim()))?;
        if let Some(err) = reply.get("error") {
            let msg = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(msg.to_string());
        }
        Ok(reply.get("result").cloned().unwrap_or(serde_json::Value::Null))
    }

    /// Where the desktop's harness socket is, if it is anywhere.
    ///
    /// The same places the OS itself tries, in the same order, because the shell binds the first
    /// one it can WRITE and a client has to find the one it actually chose. Only paths that exist
    /// are returned — a missing socket is the normal state of a machine with no desktop up, not a
    /// failure to report.
    pub fn socket() -> Option<String> {
        if let Ok(explicit) = std::env::var("YANTRIK_HARNESS_SOCKET") {
            let explicit = explicit.trim();
            if !explicit.is_empty() {
                // Named outright: honour it and look nowhere else, so pointing this mind at a
                // particular desktop cannot silently fall through to a different one.
                let p = PathBuf::from(explicit);
                return p.exists().then(|| p.display().to_string());
            }
        }

        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
            let dir = dir.trim();
            if !dir.is_empty() {
                candidates.push(PathBuf::from(dir).join("yantrik"));
            }
        }
        candidates.push(PathBuf::from("/run/yantrik"));
        // The OS's last resort is /tmp/yantrik-<uid>, and this process may be a different uid
        // than the one that named it — so read the directory rather than compute the name.
        if let Ok(entries) = std::fs::read_dir("/tmp") {
            for entry in entries.flatten() {
                if entry.file_name().to_string_lossy().starts_with("yantrik-") {
                    candidates.push(entry.path());
                }
            }
        }
        candidates
            .into_iter()
            .map(|d| d.join("harness.sock"))
            .find(|p| p.exists())
            .map(|p| p.display().to_string())
    }
}

#[cfg(unix)]
async fn call(
    address: String,
    method: &'static str,
    params: serde_json::Value,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    // The socket is blocking and this is a tokio task, so the round trip goes to the blocking
    // pool. It costs microseconds on a local socket; the correctness matters more than the cost,
    // because a blocked worker here would stall every other task sharing it.
    tokio::task::spawn_blocking(move || wire::call(&address, method, params, timeout))
        .await
        .unwrap_or_else(|e| Err(format!("task: {e}")))
}

#[cfg(unix)]
async fn run(mem: MemoryHandle, conv: Arc<ConversationEngine>, detail: String) {
    let timeout = Duration::from_secs(10);
    // What this loop has already said. Every branch below retries every five seconds forever, so
    // without this a machine with no desktop, or one refusing the attach, would write the same
    // line seventeen thousand times a day — and the one line that matters, the attach that
    // finally worked, would be buried in it.
    let mut last_said: Option<String> = None;
    let mut say = move |line: String| {
        if last_said.as_deref() != Some(line.as_str()) {
            eprintln!("[harness] {line}");
            last_said = Some(line);
        }
    };

    loop {
        // ── Find the desktop ──
        let Some(address) = wire::socket() else {
            say("no Yantrik OS desktop found — will attach if one appears".to_string());
            tokio::time::sleep(RETRY).await;
            continue;
        };

        // ── Say who we are ──
        let attached = call(
            address.clone(),
            ATTACH,
            serde_json::json!({
                "id": ID,
                "name": NAME,
                "detail": detail,
                "capabilities": { "streaming": false, "tools": true, "memory": true },
            }),
            timeout,
        )
        .await;
        let session = match attached {
            Ok(reply) => reply["session"].as_str().unwrap_or_default().to_string(),
            Err(e) => {
                say(format!("could not attach at {address}: {e}"));
                tokio::time::sleep(RETRY).await;
                continue;
            }
        };
        say(format!(
            "attached to the desktop as `{ID}` (session {session}) — choose `{NAME}` in the mind picker"
        ));

        // ── Answer, until the desktop goes away ──
        serve(&mem, &conv, &address, &session, timeout).await;
        say("lost the desktop; re-attaching".to_string());
        tokio::time::sleep(RETRY).await;
    }
}

/// Poll and answer until the session stops working. Returns when it does.
#[cfg(unix)]
async fn serve(
    mem: &MemoryHandle,
    conv: &Arc<ConversationEngine>,
    address: &str,
    session: &str,
    timeout: Duration,
) {
    // One setup conversation per session: attaching again starts it over, which is what a person
    // would expect after the desktop restarted.
    let first_run = NEEDS_SETUP.load(std::sync::atomic::Ordering::Relaxed).then(|| {
        Arc::new(std::sync::Mutex::new(crate::first_run::FirstRun::new(
            crate::first_run::HttpOllama,
            crate::first_run::env_path(),
            crate::first_run::supervised(),
        )))
    });

    loop {
        let turn = match call(
            address.to_string(),
            POLL,
            serde_json::json!({ "session": session }),
            timeout,
        )
        .await
        {
            Ok(turn) => turn,
            // The shell restarted, or this session aged out. Either way the recovery is to attach
            // again, which the caller does — there is no reconnect dance in this protocol because
            // attaching IS the reconnect.
            Err(e) => {
                eprintln!("[harness] poll failed: {e}");
                return;
            }
        };

        let Some(turn_id) = turn["turn_id"].as_u64() else {
            tokio::time::sleep(IDLE).await;
            continue;
        };
        let text = turn["text"].as_str().unwrap_or_default().to_string();
        eprintln!("[harness] turn {turn_id}: {text}");

        // ── Or, with no model yet, set one up ──
        if let Some(first_run) = &first_run {
            set_up(first_run.clone(), address, session, turn_id, &text, timeout).await;
            continue;
        }

        // ── Think ──
        //
        // In its own task, so a panic inside a turn becomes a failed turn rather than a dead
        // channel. Without it one bad turn would stop the mind polling, the desktop would drop it
        // after ninety seconds, and the picker would quietly lose an entry with nothing said
        // about why.
        let answer = {
            let mem = mem.clone();
            let conv = conv.clone();
            tokio::spawn(async move { think(&mem, &conv, &text).await }).await
        };

        // ── Answer ──
        match answer {
            Ok(said) => {
                // Sent whole, because it IS whole: `handle_line_as` returns a finished answer,
                // and chopping it into pieces with sleeps between them would be an animation of
                // streaming rather than streaming. The desktop shows its thinking state until
                // this lands, which is the truth of what is happening.
                let _ = call(
                    address.to_string(),
                    CHUNK,
                    serde_json::json!({ "session": session, "turn_id": turn_id, "delta": said }),
                    timeout,
                )
                .await;
                let _ = call(
                    address.to_string(),
                    COMPLETE,
                    serde_json::json!({ "session": session, "turn_id": turn_id }),
                    timeout,
                )
                .await;
            }
            Err(e) => {
                // Said, not swallowed. A turn that simply never completed would leave the person
                // watching an empty bubble with no reason and no end.
                let _ = call(
                    address.to_string(),
                    FAIL,
                    serde_json::json!({
                        "session": session,
                        "turn_id": turn_id,
                        "error": format!("the mind failed on this turn: {e}"),
                    }),
                    timeout,
                )
                .await;
            }
        }
    }
}

/// One turn of the first-run conversation.
///
/// Parts of the reply go to the desktop as they are ready, so a slow model check is preceded by a
/// line saying what is happening — and those calls also keep this session's presence fresh while
/// it is not polling. When the model is saved, the turn is completed first and then the process
/// exits for its unit to start it again with the model.
#[cfg(unix)]
async fn set_up(
    first_run: Arc<std::sync::Mutex<crate::first_run::FirstRun<crate::first_run::HttpOllama>>>,
    address: &str,
    session: &str,
    turn_id: u64,
    text: &str,
    timeout: Duration,
) {
    let (address, session, text) = (address.to_string(), session.to_string(), text.to_string());
    let outcome = tokio::task::spawn_blocking(move || {
        let mut sent = 0usize;
        let mut say = |part: &str| {
            let delta = if sent == 0 { part.to_string() } else { format!("\n\n{part}") };
            sent += 1;
            let _ = wire::call(
                &address,
                CHUNK,
                serde_json::json!({ "session": session, "turn_id": turn_id, "delta": delta }),
                timeout,
            );
        };
        let then = match first_run.lock() {
            Ok(mut fr) => fr.respond(&text, &mut say),
            Err(_) => {
                say("Setup hit an internal error. Restart Yantrik Mind and try again.");
                crate::first_run::Then::Wait
            }
        };
        let _ = wire::call(
            &address,
            COMPLETE,
            serde_json::json!({ "session": session, "turn_id": turn_id }),
            timeout,
        );
        then
    })
    .await;

    if let Ok(crate::first_run::Then::Restart) = outcome {
        eprintln!("[harness] a model is configured; exiting so the service starts again with it");
        tokio::time::sleep(Duration::from_millis(500)).await;
        std::process::exit(0);
    }
}

/// One turn, through the same door the terminal and the phone use.
#[cfg(unix)]
async fn think(mem: &MemoryHandle, conv: &Arc<ConversationEngine>, text: &str) -> String {
    let member = std::env::var("YM_HARNESS_SCOPE")
        .map(|v| v.trim().eq_ignore_ascii_case("member"))
        .unwrap_or(false);

    let (identity, ctx) = if member {
        let identity = mind_conversation::TurnIdentity::new(
            mind_types::PRIMARY.to_string(),
            false,
            mind_conversation::OutputScope::HouseholdMember,
        );
        let ctx = mind_types::AccessContext::principal(
            identity.viewer(),
            mind_types::Purpose::conversation(&identity.owner),
        );
        (identity, ctx)
    } else {
        // The console's footing — see the module doc on why a 0700 socket earns it.
        (
            mind_conversation::TurnIdentity::primary(),
            mind_types::AccessContext::operator_audit(),
        )
    };

    match handle_line_as(text, mem, conv, identity, &ctx).await {
        // The desktop is one surface of a mind that is also on a phone and a terminal. `:quit`
        // typed into the Lens must end this channel's turn, not the process everyone else is
        // talking to.
        Outcome::Quit => {
            "(the mind keeps running — there is nothing to quit from here)".to_string()
        }
        Outcome::Said(s) if s.trim().is_empty() => {
            // An empty answer renders as an empty bubble, which reads as a failure. Say what
            // actually happened: the command did its work and had nothing to report.
            "(done — nothing to show)".to_string()
        }
        Outcome::Said(s) => s,
    }
}
