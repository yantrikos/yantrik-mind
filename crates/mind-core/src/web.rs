//! The browser surface — first-time registration by pairing code, then a full chat client.
//!
//! ARCH-2 extended to browsers (E.WEB1). The native surfaces authenticate a bearer the CLIENT
//! stores (the `ym` CLI, the desktop cockpit, a WG phone); a browser cannot hold a secret a script
//! can't also read, so this surface moves the secret into an `HttpOnly` cookie the page's own
//! JavaScript can never see. The auth boundary stays the ARCH-2 device store: the cookie carries a
//! paired device token, `authenticate` hashes and compares it exactly as it does for every other
//! surface, and `ym device revoke` kills a browser session the same way it kills a phone.
//!
//! REGISTRATION: at boot, when the store holds no active browser device, a single-use pairing code
//! is minted, printed to the journal, and written owner-only beside `console.token`. The first
//! person to present it — and there can only be one, the redemption is serialized — becomes the
//! paired browser device. Wrong guesses are rate-limited and lock the endpoint out; every attempt
//! lands in the decision log. There is no self-service recovery: a lost code is re-minted by the
//! operator on the box, which is exactly the trust model of the console token.
//!
//! BIND: loopback by default. `YM_WEBUI_BIND` may name ONE concrete non-wildcard interface IP (the
//! WireGuard address is the intended value, per the E.WEB1 security prereg); a hostname, wildcard,
//! or multicast is a config error and the listener refuses to start. No TLS in this slice, so the
//! prereg's "no plaintext beyond loopback" rule is honored by BINDING, not by ceremony: loopback
//! (or an ssh tunnel to it) and WireGuard are the only intended paths. The local-CA TLS tier is
//! E.WEB1b and does not weaken anything by arriving later.
//!
//! OUTPUT CONTAINMENT: the page ships with a strict CSP (no external hosts, no inline script), and
//! everything model-authored is rendered through the client's sanitizer — the mind's output is
//! treated as hostile input to its own UI, because a prompt-injected web page must deface nothing
//! but itself.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use mind_conversation::ConversationEngine;

/// The single-page client, embedded so a deploy is atomic: a stale page beside a fresh binary is a
/// capability that reports itself present and behaves like an older version (the deploy script says
/// the same about driver scripts, and it is right).
const APP_HTML: &str = include_str!("../assets/webui.html");
const APP_CSS: &str = include_str!("../assets/webui.css");
const APP_JS: &str = include_str!("../assets/webui.js");

/// Name prefix that marks a device as a browser registration, so "is a browser already paired?"
/// is a question the store can answer without a new field.
const WEB_DEVICE_PREFIX: &str = "web:";

/// The mind's name is CHOSEN AT SETUP, not compiled in. It lives beside the console token, written
/// once by the registration ceremony and readable by every surface. The engine's prose still says
/// its legacy name in places — unifying those onto this file is the engine-side half of the slice
/// (tracked with Codex); the web surface reads only this.
const MIND_NAME_FILE: &str = "mind.name";

fn mind_name() -> Option<String> {
    let dir = crate::telegram::state_dir();
    std::fs::read_to_string(std::path::Path::new(&dir).join(MIND_NAME_FILE))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

const PAIRING_CODE_FILE: &str = "web-pairing.code";

/// Wrong-code attempts allowed before the pairing endpoint locks out.
const PAIR_MAX_ATTEMPTS: u32 = 5;
/// Lockout, in milliseconds, once the attempt budget is spent.
const PAIR_LOCKOUT_MS: u64 = 15 * 60 * 1000;

static PAIR_ATTEMPTS: AtomicU32 = AtomicU32::new(0);
static PAIR_LOCKED_UNTIL_MS: AtomicU64 = AtomicU64::new(0);
static WEBUI_CONNS: AtomicU32 = AtomicU32::new(0);
const WEBUI_MAX_CONNS: u32 = 16;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mint (or keep) the first-registration pairing code. Called at boot from both channel modes.
///
/// The code exists only while no browser is paired: a virgin store gets one minted and announced;
/// a store that already has an active `web:` device gets the stale file removed, so the artifact
/// on disk always means "registration is open".
pub(crate) fn ensure_pairing_code(devices: &mind_governance::devices::DeviceStore) {
    let dir = crate::telegram::state_dir();
    let path = std::path::Path::new(&dir).join(PAIRING_CODE_FILE);
    let browser_paired = devices
        .list()
        .iter()
        .any(|d| !d.revoked && d.name.starts_with(WEB_DEVICE_PREFIX));
    if browser_paired {
        let _ = std::fs::remove_file(&path);
        return;
    }
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if !existing.trim().is_empty() {
            eprintln!(
                "[web-ui] first-time registration is OPEN — code in {}",
                path.display()
            );
            return;
        }
    }
    let code = mint_code();
    if std::fs::write(&path, &code).is_ok() {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        // The journal line IS the installation hand-off Pranab asked for: install prints it, the
        // person types it into the page once, and the file disappears on success.
        eprintln!(
            "[web-ui] first-time registration code: {code}  (also in {})",
            path.display()
        );
    } else {
        eprintln!(
            "[web-ui] could not write {} — first-time registration UNAVAILABLE (fail-closed)",
            path.display()
        );
    }
}

/// A readable one-time code: 8 chars from an unambiguous alphabet, grouped 4-4. ~39.6 bits over a
/// 31-symbol space — plenty against 5 attempts and a 15-minute lockout, and short enough to type
/// from a journal line.
///
/// Entropy comes from the OS CSPRNG via `rand_core::OsRng` — already in this crate's dependency
/// graph through the declared `rand_core = { features = ["getrandom"] }`, so no new dependency
/// edge and no version-resolution fragility (a direct `getrandom` dep collided with three resolved
/// versions on one box; a `RandomState`-based substitute was then rightly BLOCKED in review:
/// std randomizes hash keys but promises no CryptoRng contract, and a pairing code is an
/// authentication credential). Symbols are drawn by rejection sampling so every alphabet character
/// is equally likely — a modulo over 31 would bias the first eight.
fn mint_code() -> String {
    use rand_core::RngCore;
    // Excludes 0/O/1/I/L: a code someone reads off a terminal must not have look-alikes.
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
    let n = ALPHABET.len() as u16; // 31
    let limit = (256 / n) * n; // 248: bytes at or above this would bias the low symbols
    let mut chars = [0u8; 8];
    let mut filled = 0;
    while filled < chars.len() {
        let mut raw = [0u8; 16];
        rand_core::OsRng.fill_bytes(&mut raw);
        for b in raw {
            if (b as u16) < limit {
                chars[filled] = ALPHABET[(b as usize) % ALPHABET.len()];
                filled += 1;
                if filled == chars.len() {
                    break;
                }
            }
        }
    }
    let c: Vec<char> = chars.iter().map(|b| *b as char).collect();
    format!("{}{}{}{}-{}{}{}{}", c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7])
}

/// Constant-time string equality — the pairing code is a credential and gets credential handling.
fn ct_str_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

pub(crate) fn spawn_webui_server(
    conv: Arc<ConversationEngine>,
    devices: Arc<mind_governance::devices::DeviceStore>,
    rt: tokio::runtime::Handle,
) {
    if std::env::var("YM_WEBUI")
        .map(|v| v == "off")
        .unwrap_or(false)
    {
        return;
    }
    let port: u16 = std::env::var("YM_WEBUI_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8090);
    // Loopback unless YM_WEBUI_BIND names ONE concrete interface IP. Same semantic classification
    // as the WG chat listener: wildcard/multicast/hostname is refusal, never a guess.
    let ip: std::net::IpAddr = match std::env::var("YM_WEBUI_BIND") {
        Err(_) => std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
        Ok(raw) => {
            match raw.trim().parse() {
                Ok(ip) => {
                    let ip: std::net::IpAddr = ip;
                    if ip.is_unspecified() || ip.is_multicast() {
                        eprintln!("[web-ui] YM_WEBUI_BIND='{raw}' must be one concrete interface IP, never a wildcard — DISABLED (fail-closed)");
                        return;
                    }
                    ip
                }
                Err(_) => {
                    eprintln!("[web-ui] YM_WEBUI_BIND='{raw}' is not an IP address — DISABLED (fail-closed)");
                    return;
                }
            }
        }
    };
    std::thread::spawn(move || {
        match std::net::TcpListener::bind((ip, port)) {
        Ok(listener) => {
            eprintln!("[web-ui] browser surface on http://{ip}:{port} (pairing-code registration, cookie sessions)");
            for stream in listener.incoming().flatten() {
                if WEBUI_CONNS.load(Ordering::Relaxed) >= WEBUI_MAX_CONNS {
                    let mut s = stream;
                    let _ = s.write_all(b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    continue;
                }
                WEBUI_CONNS.fetch_add(1, Ordering::Relaxed);
                let (conv, devices, rt) = (conv.clone(), devices.clone(), rt.clone());
                std::thread::spawn(move || {
                    handle(stream, conv, devices, rt);
                    WEBUI_CONNS.fetch_sub(1, Ordering::Relaxed);
                });
            }
        }
        Err(e) => eprintln!("[web-ui] COULD NOT BIND {ip}:{port}: {e} — the browser surface will answer as if it does not exist. Set YM_WEBUI_PORT to a free port."),
    }
    });
}

/// Response headers every route carries. The CSP is the output-containment boundary: no external
/// hosts, no inline script — a model-authored `<script src=…>` that somehow survived sanitization
/// still has nowhere to run from and nowhere to call home to.
const SECURITY_HEADERS: &str = concat!(
    "Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'none'; form-action 'none'; frame-ancestors 'none'\r\n",
    "X-Content-Type-Options: nosniff\r\n",
    "Referrer-Policy: no-referrer\r\n",
    "Cache-Control: no-store\r\n",
);

fn send(stream: &mut std::net::TcpStream, status: &str, ctype: &str, extra: &str, body: &str) {
    let resp = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\n{SECURITY_HEADERS}{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn send_json(
    stream: &mut std::net::TcpStream,
    status: &str,
    extra: &str,
    body: &serde_json::Value,
) {
    send(
        stream,
        status,
        "application/json; charset=utf-8",
        extra,
        &body.to_string(),
    );
}

/// Authenticate the request's cookie and require the operator role — the shared gate for every
/// cockpit route. Returns the ready-to-send refusal on failure so call sites stay one line.
fn operator(
    head: &str,
    devices: &mind_governance::devices::DeviceStore,
) -> std::result::Result<mind_governance::devices::AuthedDevice, (&'static str, &'static str)> {
    match session_cookie(head).and_then(|t| devices.authenticate(&t)) {
        None => Err(("401 Unauthorized", "not paired")),
        Some(d) if !d.is_operator() => Err(("403 Forbidden", "operator only")),
        Some(d) => Ok(d),
    }
}

/// The session cookie's value, if the request carries one. Only `ym_session` is read; everything
/// else in the Cookie header is someone else's business.
fn session_cookie(head: &str) -> Option<String> {
    let line = crate::telegram::header_value(head, "cookie:")?;
    for part in line.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix("ym_session=") {
            if !v.is_empty() {
                return Some(v.to_string());
            }
        }
    }
    None
}

fn handle(
    mut stream: std::net::TcpStream,
    conv: Arc<ConversationEngine>,
    devices: Arc<mind_governance::devices::DeviceStore>,
    rt: tokio::runtime::Handle,
) {
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(20)));
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 4096];
    let hend = loop {
        match stream.read(&mut tmp) {
            Ok(0) => return,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Some(p) = crate::telegram::find_sub(&buf, b"\r\n\r\n") {
                    break p;
                }
                if buf.len() > 32_768 {
                    let _ = stream.write_all(b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                    return;
                }
            }
            Err(_) => return,
        }
    };
    let head = String::from_utf8_lossy(&buf[..hend]).to_string();
    let mut first = head.lines().next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_string();
    let target = first.next().unwrap_or("/").to_string();
    if !target.starts_with('/') {
        send(
            &mut stream,
            "400 Bad Request",
            "text/plain",
            "",
            "bad request target",
        );
        return;
    }
    let path = target.split('?').next().unwrap_or(&target).to_string();

    // Same framing hardening as the native surfaces — one Content-Length, no Transfer-Encoding
    // games, one Host, one Cookie.
    if crate::telegram::header_count(&head, "content-length:") > 1
        || crate::telegram::header_count(&head, "host:") > 1
        || crate::telegram::header_count(&head, "cookie:") > 1
        || crate::telegram::header_value(&head, "transfer-encoding:").is_some()
    {
        send(
            &mut stream,
            "400 Bad Request",
            "text/plain",
            "",
            "ambiguous request framing",
        );
        return;
    }

    // Body, when one is declared. Capped well below anything a chat turn needs.
    let clen: usize = crate::telegram::header_value(&head, "content-length:")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if clen > 64 * 1024 {
        send(
            &mut stream,
            "413 Payload Too Large",
            "text/plain",
            "",
            "body too large",
        );
        return;
    }
    let mut body_raw: Vec<u8> = buf[hend + 4..].to_vec();
    while body_raw.len() < clen {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => body_raw.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    let body = String::from_utf8_lossy(&body_raw[..body_raw.len().min(clen)]).to_string();

    // CSRF: every mutating route requires the custom header a cross-site form cannot send, on top
    // of the SameSite=Strict cookie. Two locks, different failure modes.
    let has_client_header = crate::telegram::header_value(&head, "x-ym-web:").is_some();

    match (method.as_str(), path.as_str()) {
        ("GET", "/") => {
            send(
                &mut stream,
                "200 OK",
                "text/html; charset=utf-8",
                "",
                APP_HTML,
            );
        }
        ("GET", "/app.css") => {
            send(
                &mut stream,
                "200 OK",
                "text/css; charset=utf-8",
                "",
                APP_CSS,
            );
        }
        ("GET", "/app.js") => {
            send(
                &mut stream,
                "200 OK",
                "application/javascript; charset=utf-8",
                "",
                APP_JS,
            );
        }
        ("GET", "/api/me") => match session_cookie(&head).and_then(|t| devices.authenticate(&t)) {
            Some(d) => send_json(
                &mut stream,
                "200 OK",
                "",
                &serde_json::json!({
                    "mind": mind_name(),
                    "device": d.id,
                    "person": d.chat_person(),
                    "operator": d.is_operator(),
                }),
            ),
            None => {
                let dir = crate::telegram::state_dir();
                let open = std::path::Path::new(&dir).join(PAIRING_CODE_FILE).exists();
                send_json(
                    &mut stream,
                    "401 Unauthorized",
                    "",
                    &serde_json::json!({ "mind": mind_name(), "registration_open": open }),
                );
            }
        },
        // Setup: the mind's name, chosen once at registration. Required by the ceremony; changing
        // it later is the same call (operator-only), and the file is the single source every
        // surface reads.
        ("POST", "/api/setup") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            let Some(d) = session_cookie(&head).and_then(|t| devices.authenticate(&t)) else {
                send(
                    &mut stream,
                    "401 Unauthorized",
                    "text/plain",
                    "",
                    "not paired",
                );
                return;
            };
            if !d.is_operator() {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "operator only",
                );
                return;
            }
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let name: String = parsed["mind_name"]
                .as_str()
                .unwrap_or("")
                .trim()
                .chars()
                .take(40)
                .collect();
            if name.is_empty() {
                send(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    "",
                    "a name is required",
                );
                return;
            }
            let dir = crate::telegram::state_dir();
            match std::fs::write(std::path::Path::new(&dir).join(MIND_NAME_FILE), &name) {
                Ok(()) => {
                    eprintln!("[web-ui] the mind is named '{name}' (setup)");
                    send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "ok": true, "mind": name }),
                    );
                }
                Err(e) => send(
                    &mut stream,
                    "500 Internal Server Error",
                    "text/plain",
                    "",
                    &format!("could not persist the name: {e}"),
                ),
            }
        }
        // Agents & standing orders: thin passthroughs to the SAME console verbs the `ym` CLI and
        // the desktop cockpit use — one dispatcher, several faces, no second executor (E.SK4's
        // rule applied to surfaces). All operator-only.
        ("GET", "/api/tasks") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let out = rt.block_on(
                    conv.cli_dispatch("jobs json", &mind_types::AccessContext::operator_audit()),
                );
                match serde_json::from_str::<serde_json::Value>(&out) {
                    Ok(v) => send_json(&mut stream, "200 OK", "", &v),
                    Err(_) => send(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        "",
                        "jobs report failed",
                    ),
                }
            }
        },
        ("POST", "/api/agent") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            match operator(&head, &devices) {
                Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
                Ok(_) => {
                    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let name = parsed["name"].as_str().unwrap_or("").trim();
                    let task = parsed["task"].as_str().unwrap_or("").trim();
                    if name.is_empty() || task.is_empty() {
                        send(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain",
                            "",
                            "name and task are both required",
                        );
                        return;
                    }
                    let line = format!("delegate {name}: {task}");
                    let out = rt.block_on(
                        conv.cli_dispatch(&line, &mind_types::AccessContext::operator_audit()),
                    );
                    send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "reply": out }),
                    );
                }
            }
        }
        ("POST", "/api/import-agent") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            match operator(&head, &devices) {
                Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
                Ok(_) => {
                    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let doc = parsed["doc"].as_str().unwrap_or("").trim();
                    if doc.is_empty() {
                        send(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain",
                            "",
                            "an agent document is required",
                        );
                        return;
                    }
                    // The whole document rides as the verb's argument — the same `ym import` the
                    // desktop app uses, schedule: frontmatter and all.
                    let out = rt.block_on(conv.cli_dispatch(
                        &format!("import {doc}"),
                        &mind_types::AccessContext::operator_audit(),
                    ));
                    send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "reply": out }),
                    );
                }
            }
        }
        ("POST", "/api/task-action") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            match operator(&head, &devices) {
                Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
                Ok(_) => {
                    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let verb = parsed["verb"].as_str().unwrap_or("");
                    let id = parsed["id"].as_str().unwrap_or("").trim();
                    // A closed verb set: the web forwards known actions, never free text.
                    if !matches!(verb, "keep" | "drop" | "delete")
                        || id.is_empty()
                        || !id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    {
                        send(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain",
                            "",
                            "unknown action",
                        );
                        return;
                    }
                    let out = rt.block_on(conv.cli_dispatch(
                        &format!("jobs {verb} {id}"),
                        &mind_types::AccessContext::operator_audit(),
                    ));
                    send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "reply": out }),
                    );
                }
            }
        }
        ("GET", "/api/orders") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let out = rt.block_on(
                    conv.cli_dispatch("orders", &mind_types::AccessContext::operator_audit()),
                );
                send_json(
                    &mut stream,
                    "200 OK",
                    "",
                    &serde_json::json!({ "text": out }),
                );
            }
        },
        ("POST", "/api/order-action") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            match operator(&head, &devices) {
                Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
                Ok(_) => {
                    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    let verb = parsed["verb"].as_str().unwrap_or("");
                    let id = parsed["id"].as_str().unwrap_or("").trim();
                    if !matches!(verb, "pause" | "resume" | "run" | "cancel")
                        || id.is_empty()
                        || !id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    {
                        send(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain",
                            "",
                            "unknown action",
                        );
                        return;
                    }
                    let out = rt.block_on(conv.cli_dispatch(
                        &format!("orders {verb} {id}"),
                        &mind_types::AccessContext::operator_audit(),
                    ));
                    send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "reply": out }),
                    );
                }
            }
        }
        // Panels below are read surfaces over structures the engine already maintains. Settings and
        // devices are operator-only: a member browser gets the chat, not the cockpit.
        ("GET", "/api/capabilities") => {
            let Some(_) = session_cookie(&head).and_then(|t| devices.authenticate(&t)) else {
                send(
                    &mut stream,
                    "401 Unauthorized",
                    "text/plain",
                    "",
                    "not paired",
                );
                return;
            };
            let report = conv.capability_report();
            match serde_json::to_value(&report) {
                Ok(v) => send_json(&mut stream, "200 OK", "", &v),
                Err(_) => send(
                    &mut stream,
                    "500 Internal Server Error",
                    "text/plain",
                    "",
                    "report failed",
                ),
            }
        }
        ("GET", "/api/settings") => {
            let authed = session_cookie(&head).and_then(|t| devices.authenticate(&t));
            let Some(d) = authed else {
                send(
                    &mut stream,
                    "401 Unauthorized",
                    "text/plain",
                    "",
                    "not paired",
                );
                return;
            };
            if !d.is_operator() {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "operator only",
                );
                return;
            }
            // `config schema` nulls secret values itself — the redaction lives beside the schema,
            // not here, so a new secret knob cannot leak by web-route omission.
            let schema = rt.block_on(conv.config_panel("schema"));
            match serde_json::from_str::<serde_json::Value>(&schema) {
                Ok(v) => send_json(&mut stream, "200 OK", "", &v),
                Err(_) => send(
                    &mut stream,
                    "500 Internal Server Error",
                    "text/plain",
                    "",
                    "schema failed",
                ),
            }
        }
        ("GET", "/api/devices") => {
            let Some(d) = session_cookie(&head).and_then(|t| devices.authenticate(&t)) else {
                send(
                    &mut stream,
                    "401 Unauthorized",
                    "text/plain",
                    "",
                    "not paired",
                );
                return;
            };
            if !d.is_operator() {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "operator only",
                );
                return;
            }
            let list: Vec<serde_json::Value> = devices
                .list()
                .iter()
                .map(|dev| {
                    serde_json::json!({
                        "id": dev.id, "name": dev.name, "role": dev.role,
                        "created_ms": dev.created_ms, "revoked": dev.revoked,
                        "this_device": dev.id == d.id,
                    })
                })
                .collect();
            send_json(
                &mut stream,
                "200 OK",
                "",
                &serde_json::json!({ "devices": list }),
            );
        }
        ("POST", "/api/revoke") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            let Some(d) = session_cookie(&head).and_then(|t| devices.authenticate(&t)) else {
                send(
                    &mut stream,
                    "401 Unauthorized",
                    "text/plain",
                    "",
                    "not paired",
                );
                return;
            };
            if !d.is_operator() {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "operator only",
                );
                return;
            }
            let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            let id = parsed["id"].as_str().unwrap_or("").trim();
            if id == d.id {
                // Refusing self-revocation keeps a browser from locking itself out mid-session;
                // the store separately refuses to strand the last operator.
                send(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    "",
                    "cannot revoke the device you are using",
                );
                return;
            }
            match devices.revoke(id) {
                Ok(true) => send_json(
                    &mut stream,
                    "200 OK",
                    "",
                    &serde_json::json!({ "ok": true }),
                ),
                Ok(false) => send(
                    &mut stream,
                    "404 Not Found",
                    "text/plain",
                    "",
                    "no such device",
                ),
                Err(e) => send(
                    &mut stream,
                    "400 Bad Request",
                    "text/plain",
                    "",
                    &format!("{e}"),
                ),
            }
        }
        ("POST", "/api/pair") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            pair(&mut stream, &devices, &body);
        }
        ("POST", "/api/turn") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            let Some(authed) = session_cookie(&head).and_then(|t| devices.authenticate(&t)) else {
                send(
                    &mut stream,
                    "401 Unauthorized",
                    "text/plain",
                    "",
                    "not paired",
                );
                return;
            };
            turn_stream(&mut stream, conv, rt, &authed, &body);
        }
        ("POST", "/api/logout") => {
            if !has_client_header {
                send(
                    &mut stream,
                    "403 Forbidden",
                    "text/plain",
                    "",
                    "missing client header",
                );
                return;
            }
            // Clearing the cookie ends THIS browser's session; the device stays paired and is
            // revoked (if ever) by the operator. Signing out is not un-pairing.
            send_json(
                &mut stream,
                "200 OK",
                "Set-Cookie: ym_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0\r\n",
                &serde_json::json!({ "ok": true }),
            );
        }
        _ => send(
            &mut stream,
            "404 Not Found",
            "text/plain",
            "",
            "no such route",
        ),
    }
}

/// Serializes the read-verify-pair-delete redemption sequence. Process-wide, and sufficient:
/// one process owns the port, so every racer for this code passes through this lock.
///
/// The first shipped arbiter was the store's duplicate-NAME refusal, and the E.WEB0 race canary
/// killed it in its first run: two concurrent redemptions under DIFFERENT names both returned 200
/// and both paired. The name was never the invariant — the CODE is, and single-use has to be
/// enforced where the code is consumed, atomically with its verification.
static REDEEM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Why a redemption was refused — carried as (status, message) so the HTTP layer stays dumb.
type Refusal = (&'static str, &'static str);

/// Atomically redeem the one-time code: verify, pair, delete, all under `REDEEM_LOCK`. The loser
/// of a race re-reads the file INSIDE the lock, finds it gone (or a browser already enrolled),
/// and is refused without touching the wrong-code lockout counter.
fn redeem_code(
    devices: &mind_governance::devices::DeviceStore,
    code: &str,
    label: &str,
) -> std::result::Result<(String, mind_governance::devices::Secret), Refusal> {
    let _hold = REDEEM_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let dir = crate::telegram::state_dir();
    let path = std::path::Path::new(&dir).join(PAIRING_CODE_FILE);
    // Both reads happen INSIDE the lock: the code file and the already-enrolled check are one
    // atomic question — "is registration still open, and is this the code?"
    let already = devices
        .list()
        .iter()
        .any(|d| !d.revoked && d.name.starts_with(WEB_DEVICE_PREFIX));
    if already {
        let _ = std::fs::remove_file(&path);
        return Err((
            "409 Conflict",
            "registration is closed — a browser is already paired",
        ));
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .trim()
        .to_uppercase();
    if expected.is_empty() || !ct_str_eq(code, &expected) {
        return Err(("403 Forbidden", "wrong or expired code"));
    }
    let name = format!("{WEB_DEVICE_PREFIX}{label}");
    match devices.pair(
        &name,
        mind_governance::devices::DeviceRole::Operator {
            default_person: mind_types::PRIMARY.to_string(),
        },
    ) {
        Ok(secret) => {
            let _ = std::fs::remove_file(&path); // single-use: the code dies with its redemption
            Ok((name, secret))
        }
        Err(_) => Err(("409 Conflict", "pairing failed")),
    }
}

/// Redeem the one-time pairing code over HTTP: lockout bookkeeping outside the lock, the atomic
/// redemption inside `redeem_code`.
fn pair(
    stream: &mut std::net::TcpStream,
    devices: &mind_governance::devices::DeviceStore,
    body: &str,
) {
    let now = now_ms();
    if now < PAIR_LOCKED_UNTIL_MS.load(Ordering::Relaxed) {
        send(
            stream,
            "429 Too Many Requests",
            "text/plain",
            "",
            "pairing is locked out — try again later",
        );
        return;
    }
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let code = parsed["code"].as_str().unwrap_or("").trim().to_uppercase();
    let label = parsed["name"]
        .as_str()
        .unwrap_or("browser")
        .trim()
        .chars()
        .take(32)
        .collect::<String>();
    let label = if label.is_empty() {
        "browser".to_string()
    } else {
        label
    };

    match redeem_code(devices, &code, &label) {
        Ok((name, secret)) => {
            PAIR_ATTEMPTS.store(0, Ordering::Relaxed);
            eprintln!("[web-ui] browser paired as '{name}' — registration closed");
            let cookie = format!(
                "Set-Cookie: ym_session={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=31536000\r\n",
                secret.expose()
            );
            send_json(
                stream,
                "200 OK",
                &cookie,
                &serde_json::json!({ "ok": true, "device": name }),
            );
        }
        Err((status, msg)) => {
            // Only a WRONG code advances the lockout counter — a race loser guessed nothing.
            if status.starts_with("403") {
                let n = PAIR_ATTEMPTS.fetch_add(1, Ordering::Relaxed) + 1;
                if n >= PAIR_MAX_ATTEMPTS {
                    PAIR_LOCKED_UNTIL_MS.store(now + PAIR_LOCKOUT_MS, Ordering::Relaxed);
                    PAIR_ATTEMPTS.store(0, Ordering::Relaxed);
                    eprintln!("[web-ui] pairing LOCKED OUT after {PAIR_MAX_ATTEMPTS} wrong codes");
                }
            }
            send(stream, status, "text/plain", "", msg);
        }
    }
}

/// One streaming turn — the `/chat-stream` line protocol verbatim (`p:`/`t:`/`d:`/`k:` then `f:`),
/// so the browser client and the cockpit speak the same language and gain features together.
fn turn_stream(
    stream: &mut std::net::TcpStream,
    conv: Arc<ConversationEngine>,
    rt: tokio::runtime::Handle,
    authed: &mind_governance::devices::AuthedDevice,
    body: &str,
) {
    let parsed: serde_json::Value = serde_json::from_str(body).unwrap_or_default();
    let msg = parsed["text"].as_str().unwrap_or("").trim().to_string();
    if msg.is_empty() {
        send(stream, "400 Bad Request", "text/plain", "", "empty turn");
        return;
    }
    let ident = mind_conversation::TurnIdentity::new(
        authed.chat_person().to_string(),
        false,
        mind_conversation::OutputScope::OperatorPrivate,
    )
    .rendering_rich(true);

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let _ = stream.write_all(
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n{SECURITY_HEADERS}Transfer-Encoding: chunked\r\nConnection: close\r\n\r\n"
        )
        .as_bytes(),
    );
    let mut chunk = |s: &str| {
        let _ = stream.write_all(format!("{:x}\r\n{s}\r\n", s.len()).as_bytes());
        let _ = stream.flush();
    };
    let conv2 = conv.clone();
    let m = msg.clone();
    let turn = rt.spawn(async move {
        mind_conversation::TURN_PROGRESS
            .scope(tx, async move { conv2.turn(&m, ident).await })
            .await
    });
    rt.block_on(async {
        while let Some(p) = rx.recv().await {
            if let Some(t) = p.strip_prefix(mind_conversation::THINKING_MARK) {
                chunk(&format!("t:{}\n", t.replace('\n', "\u{1}")));
            } else if let Some(d) = p.strip_prefix(mind_conversation::DETAIL_MARK) {
                chunk(&format!("d:{}\n", d.replace('\n', "\u{1}")));
            } else if let Some(k) = p.strip_prefix(mind_conversation::TOKEN_MARK) {
                chunk(&format!("k:{}\n", k.replace('\n', "\u{1}")));
            } else {
                chunk(&format!("p:{}\n", p.replace('\n', " ")));
            }
        }
    });
    let final_text = rt
        .block_on(turn)
        .map(|r| r.unwrap_or_else(|e| format!("(error: {e})")))
        .unwrap_or_else(|e| format!("(turn crashed: {e})"));
    chunk(&format!("f:{final_text}"));
    let _ = stream.write_all(b"0\r\n\r\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minted_codes_are_grouped_and_unambiguous() {
        for _ in 0..50 {
            let c = mint_code();
            assert_eq!(c.len(), 9);
            assert_eq!(&c[4..5], "-");
            for ch in c.chars().filter(|c| *c != '-') {
                assert!(!"01OIL".contains(ch), "ambiguous glyph {ch} in {c}");
            }
        }
    }

    /// The std-only entropy source must not repeat — a fixed or low-period code would let one
    /// leaked journal line pair every future fresh install. 200 draws, all distinct.
    #[test]
    fn minted_codes_do_not_repeat() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            assert!(seen.insert(mint_code()), "mint_code repeated within 200 draws");
        }
    }

    #[test]
    fn constant_time_compare_agrees_with_equality() {
        assert!(ct_str_eq("ABCD-EFGH", "ABCD-EFGH"));
        assert!(!ct_str_eq("ABCD-EFGH", "ABCD-EFGJ"));
        assert!(!ct_str_eq("ABCD", "ABCD-EFGH"));
    }

    /// The E.WEB0 race criterion, at the exact function that must enforce it: two concurrent
    /// redemptions of ONE code under DIFFERENT names — the pairing that shipped first and both
    /// won. Exactly one may succeed, and the loser's refusal must not be the wrong-code kind
    /// (a race loser guessed nothing and must not advance the lockout).
    #[test]
    fn two_racers_one_code_exactly_one_winner() {
        let dir = std::env::temp_dir().join(format!("ym-race-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // state_dir() derives from YM_DB — point it at the scratch dir for this process.
        std::env::set_var("YM_DB", dir.join("mind.db").to_string_lossy().to_string());
        std::fs::write(dir.join(super::PAIRING_CODE_FILE), "RACE-CODE").unwrap();
        let store = std::sync::Arc::new(mind_governance::devices::DeviceStore::open(&dir).unwrap());

        let (a, b) = std::thread::scope(|s| {
            let s1 = store.clone();
            let s2 = store.clone();
            let t1 = s.spawn(move || super::redeem_code(&s1, "RACE-CODE", "racer-one").is_ok());
            let t2 = s.spawn(move || super::redeem_code(&s2, "RACE-CODE", "racer-two").is_ok());
            (t1.join().unwrap(), t2.join().unwrap())
        });
        assert!(a ^ b, "exactly one racer must win (got a={a}, b={b})");
        let web_devices = store
            .list()
            .iter()
            .filter(|d| !d.revoked && d.name.starts_with(super::WEB_DEVICE_PREFIX))
            .count();
        assert_eq!(web_devices, 1, "one code, one enrolled browser");
        assert!(
            !dir.join(super::PAIRING_CODE_FILE).exists(),
            "the code dies with its redemption"
        );
        // A third redemption after the race is refused: registration is closed.
        assert!(super::redeem_code(&store, "RACE-CODE", "latecomer").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_cookie_extraction_ignores_other_cookies() {
        let head = "GET / HTTP/1.1\r\ncookie: theme=dark; ym_session=tok123; other=x\r\n";
        assert_eq!(session_cookie(head).as_deref(), Some("tok123"));
        assert_eq!(
            session_cookie("GET / HTTP/1.1\r\ncookie: theme=dark\r\n"),
            None
        );
    }
}
