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

use crate::{handle_line_as, Outcome};

/// What a person types to choose this mind, and what the picker shows.
const ID: &str = "mind";
const NAME: &str = "Yantrik Mind";

/// The six methods. Spelled out rather than imported — see the module doc.
#[cfg(unix)]
const ATTACH: &str = "harness.attach";
#[cfg_attr(not(unix), allow(dead_code))]
const POLL: &str = "harness.poll";
#[cfg_attr(not(unix), allow(dead_code))]
const CHUNK: &str = "harness.chunk";
#[cfg_attr(not(unix), allow(dead_code))]
const COMPLETE: &str = "harness.complete";
/// E.CARDS1: a tool call's card, beside the text.
const EVENT: &str = "harness.event";
#[cfg_attr(not(unix), allow(dead_code))]
const FAIL: &str = "harness.fail";

/// How long to wait after an empty poll. The OS's `poll` answers at once either way, so this is
/// the client's own pacing, and it is the interval the protocol documents.
#[cfg(unix)]
const IDLE: Duration = Duration::from_millis(200);

/// How often a turn still being thought about tells the desktop it is present. Well inside the
/// desktop's 90-second presence window.
#[cfg_attr(not(unix), allow(dead_code))]
const HEARTBEAT: Duration = Duration::from_secs(30);

/// How long to wait before looking for the desktop again.
///
/// Hit in two ordinary situations: this mind started before the shell did, and the shell
/// restarted. Neither is an error and neither needs a person, so both are simply retried.
#[cfg_attr(not(unix), allow(dead_code))]
const RETRY: Duration = Duration::from_secs(5);

/// E.PRIV1 (yantrik-os #492/#498): while the person is in Private mode the door refuses everything
/// ("PRIVATE: …") and its sockets refuse new connects (EACCES). Ask again about once a minute, not
/// every five seconds.
#[cfg_attr(not(unix), allow(dead_code))]
const PRIVATE_RETRY: Duration = Duration::from_secs(60);

/// E.PRIV1: how long to wait before trying the desktop again, after `why` it failed.
#[cfg_attr(not(unix), allow(dead_code))]
fn retry_after(why: &str) -> Duration {
    if why.contains("PRIVATE:") || why.contains("Permission denied") || why.contains("os error 13") {
        PRIVATE_RETRY
    } else {
        RETRY
    }
}

#[cfg(test)]
mod private_tests {
    /// E.PRIV1: Private mode (refused, or the door closed to new connects) waits a minute; the
    /// desktop merely being down still retries quickly.
    #[test]
    fn private_mode_is_asked_about_once_a_minute() {
        use std::time::Duration;
        assert_eq!(super::retry_after("PRIVATE: the person has turned on Private mode. Nothing was run."), Duration::from_secs(60));
        assert_eq!(super::retry_after("connect /run/yantrik-minds/harness.sock: Permission denied (os error 13)"), Duration::from_secs(60));
        assert_eq!(super::retry_after("connect /run/yantrik-minds/harness.sock: Connection refused (os error 111)"), Duration::from_secs(5));
    }
}

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

/// E.CARDS1: the params of one `harness.event`.
fn event_params(session: &str, turn_id: u64, event: &mind_conversation::CallEvent) -> serde_json::Value {
    serde_json::json!({ "session": session, "turn_id": turn_id, "event": event })
}

#[cfg(test)]
mod zone_tests {
    /// E.ARENA1-F37: the zone from a real turn's context (520, ce8c715).
    #[test]
    fn the_desktops_zone_is_read_from_the_context() {
        let ctx = include_str!("../../mind-conversation/fixtures/desktop/context_handover_ce8c715.json");
        assert_eq!(super::machine_timezone(ctx).as_deref(), Some("America/Chicago"));
        assert_eq!(super::machine_timezone("{}"), None);
    }
}

#[cfg(test)]
mod card_tests {
    /// E.CARDS1: the params are the harness library's -- `{session, turn_id, event}`, `kind` inside.
    #[test]
    fn a_card_travels_as_the_library_sends_it() {
        let e = mind_conversation::CallEvent::ToolEnd { call: "s3".into(), ok: true, summary: "Done".into() };
        let p = super::event_params("s7-00ff", 42, &e);
        assert_eq!(p["session"], "s7-00ff");
        assert_eq!(p["turn_id"], 42);
        assert_eq!(p["event"]["kind"], "tool_end");
        assert_eq!(p["event"]["call"], "s3");
        assert_eq!(p["event"]["ok"], true);
    }
}

/// One reply line from the desktop: its `result`, or its error message. Outside `wire` so it is
/// tested on every platform.
fn parse_reply(line: &str) -> Result<serde_json::Value, String> {
    if line.trim().is_empty() {
        return Err("the desktop closed the connection without answering".into());
    }
    // E.SEC-POLL: a poll reply carries this conversation's agent token and memory credential, and
    // this error is logged -- so what is echoed has every long hex run masked.
    let reply: serde_json::Value = serde_json::from_str(line)
        .map_err(|e| format!("parse: {e} — got: {}", masked(line.trim())))?;
    if let Some(err) = reply.get("error") {
        let msg = err
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("unknown error");
        return Err(msg.to_string());
    }
    Ok(reply.get("result").cloned().unwrap_or(serde_json::Value::Null))
}

/// E.SEC-POLL: a desktop reply made safe to log -- every run of 16+ hex digits (agent tokens,
/// memory credentials) replaced by `<hex:N>`, and at most 200 characters kept.
fn masked(line: &str) -> String {
    let mut out = String::new();
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        if run.len() >= 16 {
            out.push_str(&format!("<hex:{}>", run.len()));
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in line.chars() {
        if c.is_ascii_hexdigit() {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out.chars().take(200).collect()
}

#[cfg(test)]
mod reply_tests {
    use super::parse_reply;

    /// E.SEC-POLL: a malformed poll reply is reported, and neither secret it carried is.
    #[test]
    fn a_malformed_reply_is_reported_without_its_secrets() {
        let token = "0123456789abcdef0123456789abcdef";
        let cred = format!("mem-{}", "a1".repeat(32));
        let line = format!(r#"{{"jsonrpc":"2.0","id":1,"result":{{"turn_id":7,"agent_token":"{token}","memory_credential":"{cred}""#);
        let err = parse_reply(&line).unwrap_err();
        assert!(err.starts_with("parse:"), "{err}");
        assert!(!err.contains(token) && !err.contains(&cred[4..]), "a secret reached the log line: {err}");
        assert!(err.contains("<hex:32>") && err.contains("mem-<hex:64>"), "{err}");
        assert_eq!(parse_reply(r#"{"result":{"ok":true}}"#).unwrap()["ok"], true);
        assert_eq!(parse_reply(r#"{"error":{"message":"no such session"}}"#).unwrap_err(), "no such session");
    }
}

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
        super::parse_reply(&line)
    }

    /// Where the desktop's harness socket is, if it is anywhere.
    ///
    /// The same places the OS itself tries, in the same order, because the shell binds the first
    /// one it can WRITE and a client has to find the one it actually chose. Only paths that exist
    /// are returned — a missing socket is the normal state of a machine with no desktop up, not a
    /// failure to report.
    pub fn socket() -> Option<String> {
        super::socket_from(
            std::env::var("YANTRIK_HARNESS_SOCKET").ok(),
            std::env::var("YANTRIK_MIND_RUN").ok(),
            std::env::var("XDG_RUNTIME_DIR").ok(),
        )
    }
}

/// The search behind `socket()`, with the two overrides passed in so tests need not touch the
/// process environment.
#[cfg_attr(not(unix), allow(dead_code))]
fn socket_from(explicit: Option<String>, mind_run: Option<String>, runtime_dir: Option<String>) -> Option<String> {
    use std::path::PathBuf;
    {
        if let Some(explicit) = explicit {
            let explicit = explicit.trim();
            if !explicit.is_empty() {
                // Named outright: honour it and look nowhere else, so pointing this mind at a
                // particular desktop cannot silently fall through to a different one.
                let p = PathBuf::from(explicit);
                return p.exists().then(|| p.display().to_string());
            }
        }
        // yantrik-os #411: a mind running under its own account is given the minds' door (the
        // unit sets YANTRIK_MIND_RUN=/run/yantrik-minds). It is the only place this mind may
        // attach, so it is honoured outright -- never a fall-through to the person's runtime dir
        // or /tmp, where a socket this account can reach would be somebody else's.
        if let Some(dir) = mind_run {
            let dir = dir.trim();
            if !dir.is_empty() {
                let p = PathBuf::from(dir).join("harness.sock");
                return p.exists().then(|| p.display().to_string());
            }
        }

        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(dir) = runtime_dir {
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

#[cfg(test)]
mod door_tests {
    use super::socket_from;

    /// yantrik-os #411: YANTRIK_MIND_RUN names the minds' door, and nothing else is searched.
    #[test]
    fn the_minds_door_is_the_only_place_looked() {
        let d = std::env::temp_dir().join(format!("ym-door-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let door = d.to_string_lossy().into_owned();
        // The person's runtime dir has a socket (the old search would take it); the door does not.
        let person = d.join("person");
        std::fs::create_dir_all(person.join("yantrik")).unwrap();
        std::fs::write(person.join("yantrik").join("harness.sock"), b"").unwrap();
        let person_dir = Some(person.to_string_lossy().into_owned());
        assert!(socket_from(None, None, person_dir.clone()).is_some(), "control: without a door, the runtime dir is found");
        let door_only = d.join("door");
        std::fs::create_dir_all(&door_only).unwrap();
        assert_eq!(socket_from(None, Some(door_only.to_string_lossy().into_owned()), person_dir), None,
                   "a door with no socket must not fall through to the person's");
        std::fs::write(d.join("harness.sock"), b"").unwrap();
        assert_eq!(socket_from(None, Some(door.clone()), None), Some(d.join("harness.sock").display().to_string()));
        let explicit = d.join("harness.sock").display().to_string();
        assert_eq!(socket_from(Some(explicit.clone()), Some("/nowhere".into()), None), Some(explicit), "an explicit socket still wins");
        assert_eq!(socket_from(None, Some("  ".into()), None), socket_from(None, None, None), "a blank door is no door");
        let _ = std::fs::remove_dir_all(&d);
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
            attach_payload(&detail),
            timeout,
        )
        .await;
        let session = match attached {
            Ok(reply) => reply["session"].as_str().unwrap_or_default().to_string(),
            Err(e) => {
                say(format!("could not attach at {address}: {e}"));
                tokio::time::sleep(retry_after(&e)).await;
                continue;
            }
        };
        say(format!(
            "attached to the desktop as `{ID}` (session {session}) — choose `{NAME}` in the mind picker"
        ));

        // ── Answer, until the desktop goes away ──
        let why = serve(&mem, &conv, &address, &session, timeout).await;
        say("lost the desktop; re-attaching".to_string());
        tokio::time::sleep(retry_after(&why)).await;
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
) -> String {
    // One setup conversation per session: attaching again starts it over, which is what a person
    // would expect after the desktop restarted.
    let first_run = NEEDS_SETUP.load(std::sync::atomic::Ordering::Relaxed).then(|| {
        Arc::new(std::sync::Mutex::new(crate::first_run::FirstRun::new(
            crate::first_run::HttpOllama,
            crate::first_run::env_path(),
            crate::first_run::supervised(),
        )))
    });

    // E.ERASE2: turns the desktop handed over while this mind waited on a question mid-turn, run
    // next, in the order they came.
    let mut held: std::collections::VecDeque<serde_json::Value> = std::collections::VecDeque::new();
    loop {
        let polled = match held.pop_front() {
            Some(t) => Ok(t),
            None => call(address.to_string(), POLL, serde_json::json!({ "session": session }), timeout).await,
        };
        let turn = match polled
        {
            Ok(turn) => turn,
            // The shell restarted, or this session aged out. Either way the recovery is to attach
            // again, which the caller does — there is no reconnect dance in this protocol because
            // attaching IS the reconnect.
            Err(e) => {
                eprintln!("[harness] poll failed: {e}");
                // E.PRIV1: the caller waits according to why.
                return e;
            }
        };

        let Some(turn_id) = turn["turn_id"].as_u64() else {
            tokio::time::sleep(IDLE).await;
            continue;
        };
        let text = turn["text"].as_str().unwrap_or_default().to_string();
        eprintln!("[harness] turn {turn_id}: {text}");
        // E.ARENA1-F37: the person's time zone, for "now" and everything that reads local time.
        if let Some(zone) = turn["context"].as_str().and_then(machine_timezone) {
            mind_conversation::set_machine_timezone(Some(zone));
        }
        if let Some(place) = turn["context"].as_str().and_then(machine_place) {
            mind_conversation::set_machine_place(Some(place));
        }

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
            // E.TOKEN1 (yantrik-os #411): the desktop's tools act for this conversation's agent.
            // The token goes only into yos-mcp's environment -- never a log line, never the model.
            if let Some(token) = turn["agent_token"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
                let (c, token) = (conv.clone(), token.to_string());
                match tokio::task::spawn_blocking(move || c.set_desktop_agent_token(&token)).await {
                    Ok(Ok(true)) => eprintln!("[harness] turn {turn_id}: the desktop's tools now carry this conversation's agent token"),
                    Ok(Ok(false)) => {}
                    Ok(Err(e)) => eprintln!("[harness] turn {turn_id}: could not hand the desktop's tools the agent token: {e}"),
                    Err(e) => eprintln!("[harness] turn {turn_id}: agent-token hand-off panicked: {e}"),
                }
            }
            let from_context = turn["context"].as_str().and_then(handover_from_context);
            // E.HOME1: the person's home as the desktop reports it (#411: not this process's $HOME).
            let home = turn["context"].as_str().and_then(machine_home);
            if from_context.is_some() {
                // The raw context once, so the journal can show #394's shape as it really arrives.
                let raw: String = turn["context"].as_str().unwrap_or_default().chars().take(4000).collect();
                eprintln!("[harness] turn {turn_id}: hand-over in the context: {raw}");
            }
            // E.CARDS1: the turn's tool calls, forwarded as cards in order while it thinks.
            let (cards, mut card_rx) = tokio::sync::mpsc::unbounded_channel::<mind_conversation::CallEvent>();
            let forward = {
                let (address, session) = (address.to_string(), session.to_string());
                tokio::spawn(async move {
                    let mut listening = true;
                    while let Some(e) = card_rx.recv().await {
                        if !listening {
                            continue;
                        }
                        if let Err(msg) = call(address.clone(), EVENT, event_params(&session, turn_id, &e), timeout).await {
                            if msg.contains("unknown method") {
                                eprintln!("[harness] this desktop does not take harness.event; no cards this turn");
                                listening = false;
                            }
                        }
                    }
                })
            };
            // E.STATUS1: the turn's status lines, as the work card's one live activity line.
            let (status_tx, status_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let latest: Latest = Default::default();
            let started = std::time::Instant::now();
            let status_fwd = {
                let (address, session) = (address.to_string(), session.to_string());
                tokio::spawn(forward_status(
                    status_rx,
                    move |ev| call(address.clone(), EVENT, serde_json::json!({ "session": session, "turn_id": turn_id, "event": ev }), timeout),
                    latest.clone(),
                ))
            };
            // E.STALL3: where the turn is, for the heartbeat to say when it runs long.
            let trail: mind_conversation::StageTrail = Default::default();
            let watched = trail.clone();
            // E.ERASE2: a question the turn asks the person, put on the desktop and answered here.
            let (ask_tx, mut ask_rx) = tokio::sync::mpsc::unbounded_channel::<mind_conversation::Ask>();
            // E.ERASE4: after a pressed Erase, the desktop's own copies -- digests only.
            let (redact_tx, mut redact_rx) = tokio::sync::mpsc::unbounded_channel::<mind_conversation::erase_redact::Redact>();
            // E.EGRESS3b: the desktop conversation, so a new chat hands nothing over from the last.
            let chat = turn["conversation"].as_str().unwrap_or_default().to_string();
            let mut thinking = tokio::spawn(async move {
                mind_conversation::TURN_CONVERSATION
                    .scope(
                        chat,
                        mind_conversation::erase_redact::TURN_REDACT.scope(
                            redact_tx,
                            mind_conversation::TURN_ASK.scope(
                                ask_tx,
                                mind_conversation::TURN_STAGE.scope(
                                    trail,
                                    mind_conversation::TURN_STATUS.scope(
                                        status_tx,
                                        mind_conversation::TURN_CALLS
                                            .scope(cards, mind_conversation::with_person_home(home, take_turn(&mem, &conv, &text, from_context))),
                                    ),
                                ),
                            ),
                        ),
                    )
                    .await
            });
            // A turn can outlast the desktop's 90-second presence window, and this loop does not
            // poll while it thinks: "Write a note titled Shopping…" took 93 seconds and the
            // desktop reported "mind stopped responding" to a mind that was mid-answer. An empty
            // chunk every 30 seconds keeps the session present without adding text.
            let mut beat = tokio::time::interval(HEARTBEAT);
            beat.tick().await; // the first tick is immediate
            let done = loop {
                tokio::select! {
                    done = &mut thinking => break done,
                    Some(ask) = ask_rx.recv() => {
                        let send = |m: &'static str, p: serde_json::Value| call(address.to_string(), m, p, timeout);
                        let conversation = turn["conversation"].as_str().unwrap_or_default();
                        let answer = ask_the_person(send, session, turn_id, conversation, &ask.request_id, &ask.prompt, &ask.options, ASK_WAIT, &mut held).await;
                        eprintln!("[harness] turn {turn_id}: question {} answered: {}", ask.request_id, answer.as_deref().unwrap_or("(nothing)"));
                        let _ = ask.reply.send(answer);
                    }
                    Some(r) = redact_rx.recv() => {
                        let send = |m: &'static str, p: serde_json::Value| call(address.to_string(), m, p, timeout);
                        let reply = send_redact(send, session, turn_id, &r.request_id, &r.needles).await;
                        // Never the needles: only what the desktop said.
                        eprintln!("[harness] turn {turn_id}: redact for {}: {}", r.request_id, redact_said(&reply));
                        let _ = r.reply.send(Some(reply));
                    }
                    _ = beat.tick() => {
                        if let Some(line) = stalled_line(turn_id, started.elapsed(), &watched) {
                            eprintln!("{line}");
                        }
                        let _ = call(
                            address.to_string(),
                            CHUNK,
                            serde_json::json!({ "session": session, "turn_id": turn_id, "delta": "" }),
                            timeout,
                        )
                        .await;
                        // E.STATUS1: a long model call sends nothing else; say it is still going.
                        let now = latest.lock().ok().and_then(|l| l.clone());
                        if let Some(text) = now {
                            let ev = serde_json::json!({ "kind": "status", "text": heartbeat_status(&text, started.elapsed()) });
                            let _ = call(address.to_string(), EVENT, serde_json::json!({ "session": session, "turn_id": turn_id, "event": ev }), timeout).await;
                        }
                    }
                }
            };
            // E.CARDS1: every card lands before the answer does (the sender went with the turn).
            let _ = forward.await;
            let _ = status_fwd.await;
            done
        };

        // ── Answer ──
        match answer {
            Ok(said) => {
                // Sent whole, because it IS whole: `handle_line_as` returns a finished answer,
                // and chopping it into pieces with sleeps between them would be an animation of
                // streaming rather than streaming. The desktop shows its thinking state until
                // this lands, which is the truth of what is happening.
                deliver(|m, p| call(address.to_string(), m, p, timeout), session, turn_id, &said).await;
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

/// E.STATUS1: the latest status line of a turn, for the heartbeat to repeat.
type Latest = std::sync::Arc<std::sync::Mutex<Option<String>>>;

/// E.STATUS1: a status line as the desktop's work card shows it, or None for a line that is not a
/// status (reasoning, tokens, lane, detail -- each starts with its `\u{1}` marker). Code-authored
/// lines only, so nothing from the model or a tool rides here.
pub(crate) fn status_event(line: &str) -> Option<serde_json::Value> {
    let t = line.trim();
    if t.is_empty() || t.starts_with('\u{1}') {
        return None;
    }
    let t = t
        .replace("using mcp.yantrik-os.os_act", "doing an action on the desktop")
        .replace("using mcp.yantrik-os.os_describe", "looking at the desktop")
        .replace("mcp.yantrik-os.", "the desktop's ");
    let mut c = t.chars();
    let text: String = match c.next() {
        Some(f) => f.to_uppercase().chain(c).take(120).collect(),
        None => return None,
    };
    Some(serde_json::json!({ "kind": "status", "text": text }))
}

/// E.STATUS1: the latest status, with how long the turn has run, to the heartbeat's 30 seconds.
/// E.ERASE2: how long a question waits for the person -- under the desktop's own 120 s for a
/// pending request -- and how often the desktop is asked whether it has been answered.
#[cfg_attr(not(unix), allow(dead_code))]
const ASK_WAIT: Duration = Duration::from_secs(110);
const ASK_POLL: Duration = if cfg!(test) { Duration::from_millis(1) } else { Duration::from_secs(2) };
/// E.ERASE2: the status line while a question waits, and how often it is said again.
const ASK_STATUS: &str = "waiting for your answer";
const ASK_STATUS_EVERY: Duration = Duration::from_secs(60);

/// E.ERASE2: what one poll said about the question this turn asked.
#[derive(Debug, PartialEq)]
pub(crate) enum Heard {
    Nothing,
    /// The person chose; the label, when the answer carries one.
    Answer(Option<String>),
    /// The desktop stopped waiting for this turn.
    Cancelled,
}

/// E.ERASE2: read a poll made while waiting on a question. A turn the desktop hands over meanwhile
/// is kept for after this one, never dropped. Only an answer for THIS turn and THIS request counts
/// -- the host refuses a second answer and one to a question never asked, and this side checks too.
pub(crate) fn read_while_asking(
    reply: &serde_json::Value,
    turn_id: u64,
    conversation: &str,
    request_id: &str,
    held: &mut std::collections::VecDeque<serde_json::Value>,
) -> Heard {
    if reply["turn_id"].as_u64().is_some() {
        let mut turn = reply.clone();
        if let Some(o) = turn.as_object_mut() {
            o.remove("answers");
            o.remove("cancelled");
            o.remove("ended");
        }
        held.push_back(turn);
    }
    // A turn the desktop stopped while it was held here is not run afterwards (4c's review).
    let cancelled: Vec<u64> = reply["cancelled"].as_array().into_iter().flatten().filter_map(|t| t.as_u64()).collect();
    held.retain(|t| !t["turn_id"].as_u64().is_some_and(|id| cancelled.contains(&id)));
    if cancelled.contains(&turn_id) {
        return Heard::Cancelled;
    }
    // `stop_agent` ends this turn's conversation: no answer is coming.
    if reply["ended"].as_array().is_some_and(|e| e.iter().any(|c| c.as_str() == Some(conversation))) {
        return Heard::Cancelled;
    }
    // `/stop` for THIS conversation is its own turn, held above and closed after this one; the
    // question it interrupts is not answered. A `/stop` for another conversation is only held.
    if reply["text"].as_str().is_some_and(|t| t.trim().to_ascii_lowercase().starts_with("/stop"))
        && reply["conversation"].as_str() == Some(conversation)
    {
        return Heard::Cancelled;
    }
    for a in reply["answers"].as_array().into_iter().flatten() {
        if a["turn_id"].as_u64() == Some(turn_id) && a["request_id"].as_str() == Some(request_id) {
            return Heard::Answer(a["answer"].as_str().map(str::to_string));
        }
    }
    Heard::Nothing
}

/// E.ERASE2: put a question in front of the person (the harness `request` event: a card in this
/// agent's pane with these options as buttons) and wait within the turn for the answer, which the
/// desktop gives only on a poll and only while the turn is in flight. None for no answer: declined
/// to send, cancelled, or the wait ran out.
pub(crate) async fn ask_the_person<C, F>(
    send: C,
    session: &str,
    turn_id: u64,
    conversation: &str,
    request_id: &str,
    prompt: &str,
    options: &[String],
    wait: Duration,
    held: &mut std::collections::VecDeque<serde_json::Value>,
) -> Option<String>
where
    C: Fn(&'static str, serde_json::Value) -> F,
    F: std::future::Future<Output = Result<serde_json::Value, String>>,
{
    let ev = serde_json::json!({ "kind": "request", "request_id": request_id, "prompt": prompt, "options": options });
    if send(EVENT, serde_json::json!({ "session": session, "turn_id": turn_id, "event": ev })).await.is_err() {
        return None;
    }
    // A poll keeps the session present; a status line keeps the agent from being shown as stuck,
    // which the shell does after 90 quiet seconds -- it does not yet count an open question as
    // waiting on the person (4c, agents/progress.rs:68-71).
    let waiting = |s: &str| send(EVENT, serde_json::json!({ "session": s, "turn_id": turn_id, "event": { "kind": "status", "text": ASK_STATUS } }));
    let _ = waiting(session).await;
    let started = std::time::Instant::now();
    let mut beat = std::time::Instant::now();
    while started.elapsed() < wait {
        tokio::time::sleep(ASK_POLL).await;
        if beat.elapsed() >= ASK_STATUS_EVERY {
            let _ = waiting(session).await;
            beat = std::time::Instant::now();
        }
        let Ok(reply) = send(POLL, serde_json::json!({ "session": session })).await else {
            return None;
        };
        match read_while_asking(&reply, turn_id, conversation, request_id, held) {
            Heard::Nothing => {}
            Heard::Answer(a) => return a,
            Heard::Cancelled => return None,
        }
    }
    None
}

/// E.ERASE4: one `redact` event for the question `request_id` -- digests and lengths, never the
/// words -- and the desktop's reply: `{"redacted": n, ...}` or `{"refused": why}`, or
/// `{"unsent": why}` when it could not be delivered.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) async fn send_redact<C, F>(send: C, session: &str, turn_id: u64, request_id: &str, needles: &[(String, usize)]) -> serde_json::Value
where
    C: Fn(&'static str, serde_json::Value) -> F,
    F: std::future::Future<Output = Result<serde_json::Value, String>>,
{
    let needles: Vec<serde_json::Value> = needles.iter().map(|(sha256, len)| serde_json::json!({ "sha256": sha256, "len": len })).collect();
    let ev = serde_json::json!({ "kind": "redact", "request_id": request_id, "needles": needles });
    match send(EVENT, serde_json::json!({ "session": session, "turn_id": turn_id, "event": ev })).await {
        Ok(reply) if reply.is_object() => reply,
        Ok(other) => serde_json::json!({ "unsent": format!("the desktop answered {other}") }),
        Err(e) => serde_json::json!({ "unsent": e }),
    }
}

/// E.ERASE4: the log line for a redact's outcome -- counts and reasons only.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn redact_said(reply: &serde_json::Value) -> String {
    match (reply.get("redacted"), reply.get("refused"), reply.get("unsent")) {
        (Some(n), _, _) => format!("erased {n} place(s)"),
        (_, Some(why), _) => format!("refused: {why}"),
        (_, _, Some(why)) => format!("not sent: {why}"),
        _ => "no answer".to_string(),
    }
}

/// E.STALL3: from 60 s on, the line that says where a running turn is -- its last stage and the
/// trail that led there. None before then, so ordinary long turns stay quiet for their first minute.
pub(crate) fn stalled_line(turn_id: u64, elapsed: Duration, trail: &mind_conversation::StageTrail) -> Option<String> {
    if elapsed < STALL_REPORT_AFTER {
        return None;
    }
    let t = trail.lock().ok()?.clone();
    let last = t.last().copied().unwrap_or("(none yet: before the turn engine)");
    Some(format!(
        "[harness] turn {turn_id} still running after {} s; last stage: {last} (trail: {})",
        elapsed.as_secs(),
        t.join(" > ")
    ))
}

/// E.STALL3: how long a turn runs before the heartbeat says where it is.
const STALL_REPORT_AFTER: Duration = Duration::from_secs(60);

pub(crate) fn heartbeat_status(text: &str, elapsed: Duration) -> String {
    let secs = elapsed.as_secs() / 30 * 30;
    format!("{text} ({secs} s)")
}

/// E.STATUS1: send each status line as a `harness.event` of kind `status`, in order, and keep the
/// latest for the heartbeat. A desktop that does not take `harness.event` gets none after the first.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) async fn forward_status<C, F>(mut rx: tokio::sync::mpsc::UnboundedReceiver<String>, send: C, latest: Latest)
where
    C: Fn(serde_json::Value) -> F,
    F: std::future::Future<Output = Result<serde_json::Value, String>>,
{
    let mut listening = true;
    while let Some(line) = rx.recv().await {
        let Some(ev) = status_event(&line) else {
            continue;
        };
        if let Ok(mut l) = latest.lock() {
            *l = ev["text"].as_str().map(str::to_string);
        }
        if !listening {
            continue;
        }
        if let Err(msg) = send(ev).await {
            if msg.contains("unknown method") {
                listening = false;
            }
        }
    }
}

#[cfg(test)]
mod status_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn a_status_line_is_shown_and_nothing_else_is() {
        assert_eq!(status_event("thinking…"), Some(serde_json::json!({ "kind": "status", "text": "Thinking…" })));
        assert_eq!(status_event("using mcp.yantrik-os.os_act…").unwrap()["text"], "Doing an action on the desktop…");
        assert_eq!(status_event("using mcp.yantrik-os.os_describe…").unwrap()["text"], "Looking at the desktop…");
        for mark in [
            mind_conversation::THINKING_MARK,
            mind_conversation::TOKEN_MARK,
            mind_conversation::LANE_MARK,
            mind_conversation::DETAIL_MARK,
        ] {
            assert_eq!(status_event(&format!("{mark}the model's own words")), None, "{mark:?}");
        }
        assert_eq!(heartbeat_status("Thinking…", Duration::from_secs(61)), "Thinking… (60 s)");
    }

    /// E.ERASE2: only this turn's answer to this request counts; a turn handed over meanwhile is kept;
    /// a cancel of this turn, or a `/stop`, is no answer.
    #[test]
    fn a_poll_while_asking_is_read_strictly() {
        let mut held = std::collections::VecDeque::new();
        let ans = |turn: u64, req: &str, a: &str| serde_json::json!({ "answers": [{ "turn_id": turn, "request_id": req, "answer": a }] });
        assert_eq!(read_while_asking(&serde_json::json!({}), 5, "c-main", "erase-1", &mut held), Heard::Nothing);
        assert_eq!(read_while_asking(&ans(5, "erase-0", "Erase"), 5, "c-main", "erase-1", &mut held), Heard::Nothing, "another request");
        assert_eq!(read_while_asking(&ans(6, "erase-1", "Erase"), 5, "c-main", "erase-1", &mut held), Heard::Nothing, "another turn");
        assert_eq!(read_while_asking(&ans(5, "erase-1", "Erase"), 5, "c-main", "erase-1", &mut held), Heard::Answer(Some("Erase".into())));
        assert!(held.is_empty());
        let mut other = ans(5, "erase-0", "Erase");
        other["turn_id"] = serde_json::json!(99);
        other["text"] = serde_json::json!("what's the weather");
        assert_eq!(read_while_asking(&other, 5, "c-main", "erase-1", &mut held), Heard::Nothing);
        assert_eq!(held.len(), 1, "the handed-over turn was dropped");
        assert_eq!(held[0]["turn_id"], 99);
        assert!(held[0].get("answers").is_none(), "the held turn carries the answers along");
        // 4c: answers ride the poll reply, not the turn in it -- one reply can carry both.
        let both = serde_json::json!({ "turn_id": 101, "text": "hello", "answers": [{ "turn_id": 5, "request_id": "erase-1", "answer": "Erase" }] });
        let before = held.len();
        assert_eq!(read_while_asking(&both, 5, "c-main", "erase-1", &mut held), Heard::Answer(Some("Erase".into())), "the answer beside a turn was lost");
        assert_eq!(held.len(), before + 1, "the turn beside the answer was lost");
        assert!(held.back().unwrap().get("answers").is_none());
        // 4c's review: a cancel of a HELD turn drops it; `ended` for this conversation ends the wait;
        // a /stop counts only for this conversation.
        assert!(held.iter().any(|t| t["turn_id"] == 99));
        assert_eq!(read_while_asking(&serde_json::json!({ "cancelled": [99] }), 5, "c-main", "erase-1", &mut held), Heard::Nothing);
        assert!(!held.iter().any(|t| t["turn_id"] == 99), "a cancelled held turn would still run");
        assert_eq!(read_while_asking(&serde_json::json!({ "cancelled": [4, 5] }), 5, "c-main", "erase-1", &mut held), Heard::Cancelled);
        assert_eq!(read_while_asking(&serde_json::json!({ "ended": ["c-other"] }), 5, "c-main", "erase-1", &mut held), Heard::Nothing);
        assert_eq!(read_while_asking(&serde_json::json!({ "ended": ["c-main"] }), 5, "c-main", "erase-1", &mut held), Heard::Cancelled);
        let elsewhere = serde_json::json!({ "turn_id": 102, "text": "/stop", "conversation": "c-other", "ended": ["c-x"] });
        assert_eq!(read_while_asking(&elsewhere, 5, "c-main", "erase-1", &mut held), Heard::Nothing, "another conversation's /stop");
        assert!(held.back().is_some_and(|t| t["turn_id"] == 102 && t.get("ended").is_none()));
        let stop = serde_json::json!({ "turn_id": 100, "text": "/stop", "conversation": "c-main" });
        assert_eq!(read_while_asking(&stop, 5, "c-main", "erase-1", &mut held), Heard::Cancelled);
        assert_eq!(held.back().map(|t| t["turn_id"].clone()), Some(serde_json::json!(100)), "the /stop turn is closed after this one");
    }

    /// E.ERASE4: a redact goes out as ONE `redact` event on the turn, needles as {sha256, len} and
    /// nothing else; the desktop's reply comes back as it is, and a failed send as `unsent`.
    #[tokio::test]
    async fn a_redact_goes_out_as_one_event_of_digests() {
        type Calls = std::sync::Arc<std::sync::Mutex<Vec<(&'static str, serde_json::Value)>>>;
        let run = |answer: Result<serde_json::Value, String>| async move {
            let calls: Calls = Default::default();
            let c = calls.clone();
            let send = move |m: &'static str, params: serde_json::Value| {
                let (c, a) = (c.clone(), answer.clone());
                async move {
                    c.lock().unwrap().push((m, params));
                    a
                }
            };
            let needles = vec![("de4eb651078a4b17390362aabb5461503693864b7ce52f05f4ff617278c1c709".to_string(), 16)];
            let got = send_redact(send, "s1", 5, "erase-1", &needles).await;
            let calls = calls.lock().unwrap().clone();
            (got, calls)
        };
        let accepted = serde_json::json!({ "redacted": 2, "where": ["transcript", "runs"] });
        let (got, calls) = run(Ok(accepted.clone())).await;
        assert_eq!(got, accepted);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, EVENT);
        assert_eq!(calls[0].1["turn_id"], 5);
        assert_eq!(
            calls[0].1["event"],
            serde_json::json!({ "kind": "redact", "request_id": "erase-1", "needles": [{ "sha256": "de4eb651078a4b17390362aabb5461503693864b7ce52f05f4ff617278c1c709", "len": 16 }] })
        );
        assert_eq!(redact_said(&got), "erased 2 place(s)");
        let (refused, _) = run(Ok(serde_json::json!({ "refused": "a needle is not in the question the person answered" }))).await;
        assert!(redact_said(&refused).starts_with("refused:"));
        let (unsent, _) = run(Err("no socket".to_string())).await;
        assert_eq!(unsent, serde_json::json!({ "unsent": "no socket" }));
    }

    /// E.ERASE2: the question goes out as one `request` event with its options, a status line says
    /// the agent is waiting, and the answer is read off later polls; with none, the wait ends empty.
    #[tokio::test]
    async fn a_question_is_asked_and_its_answer_read() {
        type Calls = std::sync::Arc<std::sync::Mutex<Vec<(&'static str, serde_json::Value)>>>;
        let run = |polls: Vec<serde_json::Value>, wait: Duration| async move {
            let calls: Calls = Default::default();
            let polls = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(polls)));
            let (c, p) = (calls.clone(), polls.clone());
            let send = move |m: &'static str, params: serde_json::Value| {
                let (c, p) = (c.clone(), p.clone());
                async move {
                    c.lock().unwrap().push((m, params));
                    Ok(if m == POLL { p.lock().unwrap().pop_front().unwrap_or_else(|| serde_json::json!({})) } else { serde_json::json!({}) })
                }
            };
            let mut held = std::collections::VecDeque::new();
            let options = vec!["Keep".to_string(), "Erase".to_string()];
            let got = ask_the_person(send, "s1", 5, "c-main", "erase-1", "Erase the text?", &options, wait, &mut held).await;
            let calls = calls.lock().unwrap().clone();
            (got, calls, held)
        };
        let answered = vec![
            serde_json::json!({}),
            serde_json::json!({ "turn_id": 99, "text": "hi", "answers": [{ "turn_id": 5, "request_id": "erase-0", "answer": "Erase" }] }),
            serde_json::json!({ "answers": [{ "turn_id": 5, "request_id": "erase-1", "answer": "Keep" }] }),
        ];
        let (got, calls, held) = run(answered, Duration::from_secs(5)).await;
        assert_eq!(got.as_deref(), Some("Keep"));
        assert_eq!(held.len(), 1);
        let (m, first) = &calls[0];
        assert_eq!(*m, EVENT);
        assert_eq!(first["event"], serde_json::json!({ "kind": "request", "request_id": "erase-1", "prompt": "Erase the text?", "options": ["Keep", "Erase"] }));
        assert!(calls.iter().any(|(m, p)| *m == EVENT && p["event"]["kind"] == "status"), "no waiting status: {calls:?}");
        assert_eq!(calls.iter().filter(|(m, _)| *m == POLL).count(), 3);
        let (none, _, _) = run(vec![], Duration::from_millis(20)).await;
        assert_eq!(none, None, "no answer, no answer");
    }

    /// E.STALL3: quiet for the first minute; then the turn, its seconds, its last stage and trail.
    #[test]
    fn a_long_turn_says_where_it_is() {
        let trail: mind_conversation::StageTrail = Default::default();
        assert_eq!(stalled_line(676, Duration::from_secs(59), &trail), None);
        assert_eq!(
            stalled_line(676, Duration::from_secs(90), &trail).as_deref(),
            Some("[harness] turn 676 still running after 90 s; last stage: (none yet: before the turn engine) (trail: )")
        );
        trail.lock().unwrap().extend(["grade previous", "handle turn", "claim", "episode"]);
        assert_eq!(
            stalled_line(676, Duration::from_secs(60), &trail).as_deref(),
            Some("[harness] turn 676 still running after 60 s; last stage: episode (trail: grade previous > handle turn > claim > episode)")
        );
    }

    #[tokio::test]
    async fn status_lines_reach_the_desktop_in_order() {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        for l in ["grounding from memory…", &format!("{}secret reasoning", mind_conversation::THINKING_MARK), "thinking…", "using web_search…"] {
            tx.send(l.to_string()).unwrap();
        }
        drop(tx);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let log = sent.clone();
        let latest: Latest = Default::default();
        forward_status(
            rx,
            move |ev: serde_json::Value| {
                log.lock().unwrap().push(ev["text"].as_str().unwrap_or("").to_string());
                std::future::ready(Ok(serde_json::Value::Null))
            },
            latest.clone(),
        )
        .await;
        assert_eq!(*sent.lock().unwrap(), vec!["Grounding from memory…", "Thinking…", "Using web_search…"]);
        assert_eq!(latest.lock().unwrap().as_deref(), Some("Using web_search…"), "the heartbeat repeats the latest");
    }
}

/// E.REPLY1: what a turn that produced no text says, so the person never sees a blank bubble.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) const EMPTY_TURN: &str = "That turn ended without an answer I could show you -- nothing came back from \
     my side. Ask again, or ask me what I did.";

/// E.REPLY1: did the desktop throw this away? The host answers `{"dropped": true}`, not an error,
/// when the chat a turn was for is gone -- New chat, a mind switch, the person stopping it, or a
/// shell restart (yantrik-harness host.rs).
fn dropped(reply: &serde_json::Value) -> bool {
    reply.get("dropped").and_then(serde_json::Value::as_bool) == Some(true)
}

/// E.REPLY1: hand a finished answer to the desktop, and say so when it was not taken. VM 561,
/// turns 58 and 59: the person saw no reply at all, and this used to send with `let _ =`, so an
/// answer the desktop refused or dropped vanished without a trace. True when the whole turn landed.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) async fn deliver<C, F>(call: C, session: &str, turn_id: u64, said: &str) -> bool
where
    C: Fn(&'static str, serde_json::Value) -> F,
    F: std::future::Future<Output = Result<serde_json::Value, String>>,
{
    let text = if said.trim().is_empty() { EMPTY_TURN } else { said };
    match call(CHUNK, serde_json::json!({ "session": session, "turn_id": turn_id, "delta": text })).await {
        Ok(reply) if dropped(&reply) => {
            eprintln!(
                "[harness] turn {turn_id}: the desktop dropped the answer -- the chat it was for is gone \
                 (New chat, a mind switch, or the person stopped it)"
            );
            return false;
        }
        Ok(_) => {}
        Err(e) => {
            eprintln!("[harness] turn {turn_id}: the desktop did not take the answer ({e}) -- telling it the turn failed");
            let error = format!("the answer could not be delivered: {e}");
            if let Err(e) = call(FAIL, serde_json::json!({ "session": session, "turn_id": turn_id, "error": error })).await {
                eprintln!("[harness] turn {turn_id}: it did not take that either ({e}) -- this answer is lost");
            }
            return false;
        }
    }
    match call(COMPLETE, serde_json::json!({ "session": session, "turn_id": turn_id })).await {
        Ok(reply) if dropped(&reply) => {
            eprintln!("[harness] turn {turn_id}: the desktop dropped the end of the turn -- the chat it was for is gone");
            false
        }
        Ok(_) => true,
        Err(e) => {
            eprintln!("[harness] turn {turn_id}: the desktop did not take the end of the turn ({e})");
            false
        }
    }
}

#[cfg(test)]
mod deliver_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    type Seen = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    /// A desktop that records every call; `answer` says what it gives back for each method.
    fn desktop(
        answer: fn(&str) -> Result<serde_json::Value, String>,
    ) -> (Seen, impl Fn(&'static str, serde_json::Value) -> std::future::Ready<Result<serde_json::Value, String>>) {
        let seen: Seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        (seen, move |m: &'static str, p: serde_json::Value| {
            log.lock().unwrap().push((m.to_string(), p));
            std::future::ready(answer(m))
        })
    }

    fn methods(seen: &Seen) -> Vec<String> {
        seen.lock().unwrap().iter().map(|(m, _)| m.clone()).collect()
    }

    #[tokio::test]
    async fn an_answer_is_delivered_or_its_loss_is_said() {
        let (seen, call) = desktop(|_| Ok(serde_json::Value::Null));
        assert!(deliver(&call, "s1", 58, "Here it is.").await);
        assert_eq!(methods(&seen), vec![CHUNK, COMPLETE]);
        assert_eq!(seen.lock().unwrap()[0].1["delta"], "Here it is.");

        let (seen, call) = desktop(|m| if m == CHUNK { Err("not_in_flight".into()) } else { Ok(serde_json::Value::Null) });
        assert!(!deliver(&call, "s1", 58, "Here it is.").await);
        assert_eq!(methods(&seen), vec![CHUNK, FAIL]);
        assert_eq!(seen.lock().unwrap()[1].1["error"], "the answer could not be delivered: not_in_flight");

        // VM 561, turns 58-59: the chat was gone, and the host said so in an Ok.
        let (seen, call) = desktop(|_| Ok(serde_json::json!({ "dropped": true })));
        assert!(!deliver(&call, "s1", 59, "Here it is.").await);
        assert_eq!(methods(&seen), vec![CHUNK], "nothing more is sent into a chat that is gone");

        let (seen, call) = desktop(|_| Ok(serde_json::Value::Null));
        assert!(deliver(&call, "s1", 60, "  \n").await);
        assert_eq!(seen.lock().unwrap()[0].1["delta"], EMPTY_TURN, "a blank bubble was sent");

        let (_, call) = desktop(|m| if m == COMPLETE { Err("not_in_flight".into()) } else { Ok(serde_json::Value::Null) });
        assert!(!deliver(&call, "s1", 61, "ok").await);
    }
}

/// Where the desktop says the computer is, from a turn's context, in words: "Bentonville,
/// Arkansas, US (America/Chicago)". `None` when the context says nothing about a place.
fn machine_place(context: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(context).ok()?;
    let machine = &v["machine"];
    let city = machine["place"]["city"].as_str().map(str::trim).filter(|c| !c.is_empty())?;
    let mut place: Vec<&str> = vec![city];
    for part in [&machine["place"]["region"], &machine["place"]["country"]] {
        if let Some(p) = part.as_str().map(str::trim).filter(|p| !p.is_empty()) {
            place.push(p);
        }
    }
    let mut out = place.join(", ");
    if let Some(tz) = machine["timezone"].as_str().map(str::trim).filter(|t| !t.is_empty()) {
        out.push_str(&format!(" ({tz})"));
    }
    Some(out)
}

/// E.ARENA1-F37: the person's time zone from a turn's context (`machine.timezone`), when it says.
fn machine_timezone(context: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(context).ok()?;
    v["machine"]["timezone"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_string)
}

/// What this mind says about itself when it attaches.
///
/// `tools` and `memory` sit at the TOP LEVEL because that is where the OS reads them:
/// `yantrik_harness::protocol::Attach` is `{id, name, detail, tools, memory}`, each with
/// `#[serde(default)]`. This used to nest them inside a `capabilities` object -- the shape the OS's
/// own protocol doc describes, and not the shape its struct parses -- so serde filled both with
/// `false`, and every Yantrik OS machine's mind picker advertised this mind as unable to act or
/// remember while Hermes, OpenClaw, Pi and DeepSeek, which send them top-level, showed as fully
/// capable. Found on the live nightly (VM 520, 2026-09-22): `"id": "mind", "tools": false`.
pub(crate) fn attach_payload(detail: &str) -> serde_json::Value {
    serde_json::json!({
        "id": ID,
        "name": NAME,
        "detail": detail,
        "tools": true,
        "memory": true,
        // E.ARENA1-F28 / yantrik-os #394: the hand-over comes in `context.handover`, and `text` is
        // the person's words alone. An OS without #394 ignores the field (Attach does not deny
        // unknown fields) and keeps prefixing, which `split_handover` still handles.
        "handover_context": true,
    })
}

/// E.HOME1: the person's home folder from a turn's context (`machine.home`), as the desktop asked
/// for it under yantrik-os #411 reports it.
#[cfg_attr(not(unix), allow(dead_code))]
fn machine_home(context: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(context).ok()?;
    v["machine"]["home"]
        .as_str()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .map(str::to_string)
}

/// E.ARENA1-F28 / yantrik-os #394: the hand-over paragraph from a turn's context, when the desktop
/// sent it there. `context` is the turn's context string (JSON), as `machine_place` reads it.
#[cfg_attr(not(unix), allow(dead_code))]
fn handover_from_context(context: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(context).ok()?;
    v["handover"]["text"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod attach_tests {
    /// Reads a field the way the OS's `Attach` does: `#[serde(default)] bool` at the TOP LEVEL, so
    /// a field that is missing -- or nested somewhere else, which is what happened -- is `false`.
    /// Checking that the word "tools" appears somewhere in the payload would have passed the bug.
    fn as_the_os_reads(payload: &serde_json::Value, field: &str) -> bool {
        payload.get(field).and_then(serde_json::Value::as_bool).unwrap_or(false)
    }

    #[test]
    fn the_os_reads_this_mind_as_able_to_act_and_remember() {
        let payload = super::attach_payload("test");
        assert_eq!(payload["id"], "mind");
        assert!(payload["name"].is_string(), "the OS requires a name");
        assert!(as_the_os_reads(&payload, "tools"), "the OS would show this mind as unable to act");
        assert!(as_the_os_reads(&payload, "memory"), "the OS would show this mind as unable to remember");
    }

    /// The old shape, kept as the thing this test exists to refuse.
    #[test]
    fn a_nested_capability_is_invisible_to_the_os() {
        let old = serde_json::json!({ "id": "mind", "name": "Yantrik Mind",
            "capabilities": { "tools": true, "memory": true } });
        assert!(!as_the_os_reads(&old, "tools"));
        assert!(!as_the_os_reads(&old, "memory"));
    }
}

#[cfg(test)]
mod handover_tests {
    use super::*;
    use mind_inference::{InferencePool, ScriptedLLM};
    use mind_types::MemoryFacade;
    use yantrik_ml::LLMBackend;

    /// E.ARENA1-F28 end to end, on the real turn from OS e855ad7: a get-to-know-you question is
    /// pending, the desktop hands the Mind a conversation from Hermes with an instruction after it.
    /// Before the fix the whole block was the answer: "Love that — noted", and an interest was filed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_handover_is_not_taken_as_the_answer_to_a_pending_question() {
        const TURN: &str = include_str!("../../mind-conversation/fixtures/desktop/handover_e855ad7.txt");
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
        let conv = Arc::new(crate::engine(&mem, pool));
        mem.profile_set("pending_onboard", "interest:hobbies").await.unwrap();
        let reply = take_turn(&mem, &conv, TURN, None).await;
        assert!(!reply.contains("noted"), "the hand-over was filed as a hobby: {reply}");
        let filed = mem.profile_get("interest_hobbies").await.unwrap();
        assert!(filed.is_none(), "an interest was stored: {filed:?}");
    }

    /// E.ASK1: an answer to a get-to-know-you question is acknowledged, and nothing more is asked or
    /// left pending -- the next question waits for idle time.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn an_answer_is_acknowledged_without_the_next_question() {
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = InferencePool::new(Arc::new(ScriptedLLM::new("{}")) as Arc<dyn LLMBackend>, 1);
        let conv = Arc::new(crate::engine(&mem, pool));
        mem.profile_set("pending_onboard", "interest:hobbies").await.unwrap();
        let reply = take_turn(&mem, &conv, "I love hiking and playing chess on weekends.", None).await;
        assert_eq!(reply, "Love that — noted.");
        let pending = mem.profile_get("pending_onboard").await.unwrap().unwrap_or_default();
        assert!(pending.is_empty(), "a question was left pending: {pending}");
        assert!(mem.profile_get("interest_hobbies").await.unwrap().is_some(), "the answer itself was kept");
    }

    /// yantrik-os #394 as it really arrived (VM 520, OS ce8c715, turn 377, after a switch from
    /// Hermes): the hand-over in `context.handover.text`, next to `machine`.
    #[test]
    fn the_handover_is_read_from_the_turns_context() {
        let ctx = include_str!("../../mind-conversation/fixtures/desktop/context_handover_ce8c715.json").trim();
        let h = handover_from_context(ctx).expect("the handover is read");
        assert!(h.starts_with("[From the desktop:") && h.ends_with("Carry on from here.]"), "{h}");
        assert_eq!(handover_from_context(r#"{"machine":{"timezone":"UTC"}}"#), None);
        assert_eq!(handover_from_context(r#"{"handover":{"text":"  "}}"#), None);
        assert_eq!(handover_from_context("not json"), None);
        assert_eq!(super::attach_payload("t")["handover_context"], serde_json::json!(true), "the Mind opts in");
    }

    /// With #394 the text is the person's own, and a hand-over from the context must not be taken as
    /// the answer to a pending question either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_handover_from_the_context_is_not_an_answer_either() {
        const TURN: &str = include_str!("../../mind-conversation/fixtures/desktop/handover_e855ad7.txt");
        let (block, message) = mind_conversation::split_handover(TURN);
        let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
        let conv = Arc::new(crate::engine(&mem, pool));
        mem.profile_set("pending_onboard", "interest:hobbies").await.unwrap();
        let reply = take_turn(&mem, &conv, &message, block).await;
        assert!(!reply.contains("noted"), "{reply}");
        assert!(mem.profile_get("interest_hobbies").await.unwrap().is_none());
    }
}

#[cfg(test)]
mod machine_place_tests {
    use super::machine_place;

    /// E.HOME1, from #411's agreed shape (no build sends it yet; replace with a capture when one
    /// does). Today's real context has no `home`: nothing is claimed.
    #[test]
    fn the_persons_home_is_read_from_the_context() {
        assert_eq!(super::machine_home(r#"{"machine":{"home":"/home/yantrik","timezone":"UTC"}}"#).as_deref(), Some("/home/yantrik"));
        let real = include_str!("../../mind-conversation/fixtures/desktop/context_handover_ce8c715.json").trim();
        assert_eq!(super::machine_home(real), None, "today's desktop does not send it");
        assert_eq!(super::machine_home(r#"{"machine":{"home":"  "}}"#), None);
        assert_eq!(super::machine_home("not json"), None);
    }

    #[test]
    fn the_desktop_context_becomes_a_place_in_words() {
        let ctx = r#"{"machine":{"place":{"city":"Bentonville","region":"Arkansas","country":"US"},"timezone":"America/Chicago"}}"#;
        assert_eq!(machine_place(ctx).as_deref(), Some("Bentonville, Arkansas, US (America/Chicago)"));
        let partial = r#"{"machine":{"place":{"city":"Pune","region":"","country":"IN"}}}"#;
        assert_eq!(machine_place(partial).as_deref(), Some("Pune, IN"));
    }

    #[test]
    fn no_place_is_claimed_from_nothing() {
        for ctx in [r#"{"machine":{}}"#, r#"{"machine":{"timezone":"UTC"}}"#, "not json", ""] {
            assert_eq!(machine_place(ctx), None, "{ctx}");
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
/// E.ARENA1-F28: one desktop turn. A hand-over from another mind is context for the model, never
/// the person's message: the onboarding gate and every heuristic read only what they typed.
#[cfg_attr(not(unix), allow(dead_code))]
async fn take_turn(
    mem: &MemoryHandle,
    conv: &Arc<ConversationEngine>,
    text: &str,
    from_context: Option<String>,
) -> String {
    // yantrik-os #394 puts it in the context and leaves `text` the person's own; an OS without it
    // prefixes the text, and that is split here.
    let (handover, message) = match from_context {
        Some(h) => (Some(h), text.to_string()),
        None => mind_conversation::split_handover(text),
    };
    if let Some(b) = &handover {
        eprintln!("[harness] a hand-over from another mind ({} chars) kept as context", b.chars().count());
    }
    mind_conversation::with_handover(handover, think(mem, conv, &message)).await
}

#[cfg_attr(not(unix), allow(dead_code))]
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
