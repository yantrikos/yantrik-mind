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
    format!(
        "{}{}{}{}-{}{}{}{}",
        c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]
    )
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

/// The `id=` query value of a horizon-history request, PERCENT-DECODED (the client encodes the
/// colons in `goal:horizon:…`), before the caller applies the charset check. Malformed escapes
/// yield None, which the caller treats as an invalid id.
fn horizon_history_id(path: &str) -> Option<String> {
    let raw = path.split_once("id=")?.1.split('&').next().unwrap_or("");
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3)?;
            let v = u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?;
            out.push(v);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// E.WEB11: the published-pages directory — the ONE folder the Files listing may read, fixed
/// server-side. Same source the static server on :8088 serves; the client never names a path.
fn published_pages_dir() -> String {
    std::env::var("YM_WEB_DIR").unwrap_or_else(|_| "/var/lib/yantrik-mind/public".to_string())
}

/// List the published pages: files only, servable extensions only, no dotfiles, no traversal —
/// the directory is fixed and the client supplies no path, so there is nothing to traverse WITH.
fn list_published_pages() -> Vec<serde_json::Value> {
    const ALLOWED: &[&str] = &[
        ".html", ".txt", ".css", ".js", ".json", ".png", ".jpg", ".svg",
    ];
    let dir = published_pages_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue; // no dotfiles
        }
        let low = name.to_lowercase();
        if !ALLOWED.iter().any(|x| low.ends_with(x)) {
            continue; // servable types only
        }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue; // files only — never a subdir, never a symlink to elsewhere
        }
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        out.push(serde_json::json!({ "name": name, "size": meta.len(), "modified_ms": modified }));
    }
    out.sort_by(|a, b| b["modified_ms"].as_u64().cmp(&a["modified_ms"].as_u64()));
    out
}

/// The identity a WEB device speaks and reads as (E.WEB5). Same rule as the WG chat listener:
/// an operator device is the primary's private surface; a member device is Principal-scoped as its
/// bound person, NEVER operator — and history reads use the same identity as turns, so a route
/// cannot widen what a device may see.
fn identity_for(d: &mind_governance::devices::AuthedDevice) -> mind_conversation::TurnIdentity {
    let scope = if d.is_operator() {
        mind_conversation::OutputScope::OperatorPrivate
    } else {
        mind_conversation::OutputScope::HouseholdMember
    };
    mind_conversation::TurnIdentity::new(d.chat_person().to_string(), false, scope)
        .rendering_rich(true)
}

/// Pending member invites (E.WEB5): operator-minted, single-use, short-TTL, bound to a person.
/// In-memory ON PURPOSE — a service restart clears outstanding invites, which is the fail-safe
/// direction for a credential that exists to be redeemed within minutes.
struct MemberInvite {
    code: String,
    person: String,
    expires_ms: u64,
}
static PENDING_INVITES: std::sync::Mutex<Vec<MemberInvite>> = std::sync::Mutex::new(Vec::new());
const INVITE_TTL_MS: u64 = 15 * 60 * 1000;

/// E.SEC18: the security posture, composed from existing read surfaces only — no new probes, no
/// new state, and NEVER a secret: credentials render as counts and booleans. mind-core is the one
/// crate where every instrument already meets (listeners, devices, capabilities, lanes, build),
/// which is why the audit lives here rather than behind the conversation-layer verb table — a
/// recorded deviation from the prereg's TYPED_VERBS placement, forced by dependency direction.
pub(crate) fn security_audit_json(
    conv: &ConversationEngine,
    devices: &mind_governance::devices::DeviceStore,
) -> serde_json::Value {
    // Listeners, with each one's bind rule stated rather than guessed.
    let listeners: Vec<serde_json::Value> = crate::telegram::listener_plan()
        .into_iter()
        .map(|(var, port)| {
            let bind = match var {
                "YM_CTL_PORT" => "loopback (authenticated bearer)".to_string(),
                "YM_CHAT_PORT" => std::env::var("YM_CHAT_BIND")
                    .map(|b| format!("wireguard {b} (fail-closed without it)"))
                    .unwrap_or_else(|_| "disabled (no YM_CHAT_BIND)".to_string()),
                "YM_FRAME_PORT" => {
                    if std::env::var("YM_FRAME_TOKEN").is_ok() {
                        "lan (token-guarded, read-only)".to_string()
                    } else {
                        "disabled (no YM_FRAME_TOKEN)".to_string()
                    }
                }
                "YM_WEB_PORT" => "lan 0.0.0.0 (static read-only dashboards)".to_string(),
                "YM_WEBUI_PORT" => std::env::var("YM_WEBUI_BIND")
                    .map(|b| format!("bound {b} (cookie sessions)"))
                    .unwrap_or_else(|_| "loopback (cookie sessions)".to_string()),
                _ => "unknown".to_string(),
            };
            serde_json::json!({ "listener": var, "port": port, "bind": bind })
        })
        .collect();
    let device_rows = devices.list();
    let (mut operators, mut members, mut revoked) = (0u32, 0u32, 0u32);
    for d in &device_rows {
        if d.revoked {
            revoked += 1;
        } else if d.role.contains("perator") {
            operators += 1;
        } else {
            members += 1;
        }
    }
    let report = conv.capability_report();
    let gated_write: Vec<serde_json::Value> = report
        .capabilities
        .iter()
        .filter(|c| c.security == "gated_write")
        .map(|c| serde_json::json!({ "id": c.id, "availability": serde_json::to_value(&c.availability).unwrap_or(serde_json::Value::Null) }))
        .collect();
    let dir = crate::telegram::state_dir();
    let boot_code_outstanding = std::path::Path::new(&dir).join(PAIRING_CODE_FILE).exists();
    let live_invites = {
        let mut inv = PENDING_INVITES.lock().unwrap_or_else(|p| p.into_inner());
        inv.retain(|i| i.expires_ms > now_ms());
        inv.len()
    };
    serde_json::json!({
        "build_commit": crate::build_commit(),
        "listeners": listeners,
        "devices": { "active_operators": operators, "active_members": members, "revoked": revoked },
        "gated_write_capabilities": gated_write,
        "privacy_lanes": mind_inference::privacy_lane_counts(),
        "registration": { "boot_code_outstanding": boot_code_outstanding, "live_member_invites": live_invites },
    })
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
        ("POST", "/api/brain-check") => {
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
            let (ok, served) = rt.block_on(conv.brain_preflight());
            send_json(
                &mut stream,
                "200 OK",
                "",
                &serde_json::json!({ "ok": ok, "served": served }),
            );
        }
        ("GET", "/api/security") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let audit = security_audit_json(&conv, &devices);
                send_json(&mut stream, "200 OK", "", &audit);
            }
        },
        ("GET", "/api/history") => {
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
            let n: usize = target
                .split('?')
                .nth(1)
                .and_then(|q| q.strip_prefix("n="))
                .and_then(|v| v.parse().ok())
                .unwrap_or(50);
            let ident = identity_for(&d);
            let msgs = rt.block_on(conv.web_recent_history(&ident, n));
            let rows: Vec<serde_json::Value> = msgs
                .iter()
                .map(|(role, text)| serde_json::json!({ "role": role, "text": text }))
                .collect();
            send_json(
                &mut stream,
                "200 OK",
                "",
                &serde_json::json!({ "messages": rows }),
            );
        }
        ("POST", "/api/invite") => {
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
                    let person: String = parsed["person"]
                        .as_str()
                        .unwrap_or("")
                        .trim()
                        .to_lowercase()
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
                        .take(24)
                        .collect();
                    if person.is_empty() || person == mind_types::PRIMARY {
                        send(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain",
                            "",
                            "a member person id is required (and it cannot be the primary)",
                        );
                        return;
                    }
                    let code = mint_code();
                    PENDING_INVITES
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .push(MemberInvite {
                            code: code.clone(),
                            person: person.clone(),
                            expires_ms: now_ms() + INVITE_TTL_MS,
                        });
                    eprintln!("[web-ui] member invite minted for '{person}' (expires in 15m)");
                    // Shown ONCE to the operator, never persisted server-side beyond the pending list.
                    send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "code": code, "person": person, "ttl_minutes": 15 }),
                    );
                }
            }
        }
        ("GET", "/api/horizons") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let out = rt.block_on(conv.cli_dispatch(
                    "horizons_json",
                    &mind_types::AccessContext::operator_audit(),
                ));
                match serde_json::from_str::<serde_json::Value>(&out) {
                    Ok(v) => send_json(&mut stream, "200 OK", "", &v),
                    Err(_) => send(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        "",
                        "horizons failed",
                    ),
                }
            }
        },
        // E.WEB14: one goal's verified receipt chain. Operator-only like the horizons list; the
        // id is charset-checked AGAIN at the engine boundary (defence in depth, same as plugin ids).
        ("GET", p) if p.starts_with("/api/horizon-history") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                // Codex's E.WEB14 composition audit: the client percent-encodes the id (colons
                // become %3A) and the server validated the RAW value — every real request was a
                // 400. Decode first, then validate the decoded id against the charset.
                // The route matches on `path` (query stripped); the id lives in the QUERY, so the
                // extractor must read `target`. Passing `p` here made every request a 400 on
                // staging — found by curling each listed goal after the decode fix "landed".
                let id = horizon_history_id(&target).unwrap_or_default();
                if id.is_empty()
                    || id.len() > 64
                    || !id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == ':' || c == '-' || c == '_')
                {
                    send(
                        &mut stream,
                        "400 Bad Request",
                        "text/plain",
                        "",
                        "invalid goal id",
                    );
                    return;
                }
                let out = rt.block_on(conv.cli_dispatch(
                    &format!("horizon_history_json {id}"),
                    &mind_types::AccessContext::operator_audit(),
                ));
                match serde_json::from_str::<serde_json::Value>(&out) {
                    Ok(v) if v.get("error").is_none() => send_json(&mut stream, "200 OK", "", &v),
                    Ok(v) => send_json(&mut stream, "409 Conflict", "", &v),
                    Err(_) => send(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        "",
                        "horizon history failed",
                    ),
                }
            }
        },
        // E.WEB14: the self-claims registry — readable by ANY paired device: what the mind states
        // about its own boundaries is exactly what a household member is entitled to know.
        // E.WEB15: the provenance gate as counts for the instrument column (operator).
        ("GET", "/api/chains") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let out = rt.block_on(
                    conv.cli_dispatch("chains_json", &mind_types::AccessContext::operator_audit()),
                );
                match serde_json::from_str::<serde_json::Value>(&out) {
                    Ok(v) => send_json(&mut stream, "200 OK", "", &v),
                    Err(_) => send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "available": false }),
                    ),
                }
            }
        },
        // E.WEB16: the skill library as templates for the agent composer (operator, read-only).
        ("GET", "/api/skills") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let out = rt.block_on(
                    conv.cli_dispatch("skills_json", &mind_types::AccessContext::operator_audit()),
                );
                match serde_json::from_str::<serde_json::Value>(&out) {
                    Ok(v) => send_json(&mut stream, "200 OK", "", &v),
                    Err(_) => send(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        "",
                        "skills report failed",
                    ),
                }
            }
        },
        ("GET", "/api/claims") => {
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
            let out = rt.block_on(
                conv.cli_dispatch("claims_json", &mind_types::AccessContext::operator_audit()),
            );
            match serde_json::from_str::<serde_json::Value>(&out) {
                Ok(v) => send_json(&mut stream, "200 OK", "", &v),
                Err(_) => send(
                    &mut stream,
                    "500 Internal Server Error",
                    "text/plain",
                    "",
                    "claims failed",
                ),
            }
        }
        ("POST", "/api/horizon") => {
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
                    let delay = parsed["delay"].as_str().unwrap_or("").trim();
                    let goal = parsed["goal"].as_str().unwrap_or("").trim();
                    // The delay grammar is the engine's (bounded durations); the web only refuses
                    // shapes that could smuggle a second verb into the dispatcher line.
                    let delay_ok = !delay.is_empty()
                        && delay.len() <= 4
                        && delay.chars().all(|c| c.is_ascii_alphanumeric());
                    if !delay_ok || goal.is_empty() {
                        send(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain",
                            "",
                            "delay and goal are required",
                        );
                        return;
                    }
                    let line = format!("horizon {delay} :: {goal}");
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
        ("GET", "/api/dreaming") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let n: usize = target
                    .split('?')
                    .nth(1)
                    .and_then(|q| q.strip_prefix("n="))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(80);
                // The engine sanitizes DmnLogEntry at write AND read; the route only serializes.
                let entries = conv.dmn_log_tail(n);
                match serde_json::to_value(&entries) {
                    Ok(v) => send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "dreaming": v }),
                    ),
                    Err(_) => send(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        "",
                        "serialize failed",
                    ),
                }
            }
        },
        ("GET", "/api/decisions") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let n: usize = target
                    .split('?')
                    .nth(1)
                    .and_then(|q| q.strip_prefix("n="))
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(60);
                let rows = conv.web_recent_decisions(n);
                send_json(
                    &mut stream,
                    "200 OK",
                    "",
                    &serde_json::json!({ "decisions": rows }),
                );
            }
        },
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
        // E.WEB17: standing orders as items (typed report), for the Dormant view. The text form
        // above is untouched; this is the same store read through the same verb.
        ("GET", "/api/standing-orders") => match operator(&head, &devices) {
            Err(resp) => send(&mut stream, resp.0, "text/plain", "", resp.1),
            Ok(_) => {
                let out = rt.block_on(
                    conv.cli_dispatch("orders json", &mind_types::AccessContext::operator_audit()),
                );
                match serde_json::from_str::<serde_json::Value>(&out) {
                    Ok(v) => send_json(&mut stream, "200 OK", "", &v),
                    Err(_) => send(
                        &mut stream,
                        "500 Internal Server Error",
                        "text/plain",
                        "",
                        "orders report failed",
                    ),
                }
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
        ("GET", "/api/files") => {
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
            // The web static server's port, for building the shareable links the client shows.
            let web_port: u16 = std::env::var("YM_WEB_PORT")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(8088);
            send_json(
                &mut stream,
                "200 OK",
                "",
                &serde_json::json!({ "files": list_published_pages(), "web_port": web_port }),
            );
        }
        ("GET", "/api/plugins") => {
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
            send_json(
                &mut stream,
                "200 OK",
                "",
                &serde_json::json!({ "plugins": conv.web_plugins() }),
            );
        }
        ("POST", "/api/plugin-toggle") => {
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
                    let id = parsed["id"].as_str().unwrap_or("").trim();
                    let enable = parsed["enable"].as_bool().unwrap_or(false);
                    // Closed verb set + id charset check: only enable|disable reach the dispatcher,
                    // and the id can carry nothing that isn't a plugin name.
                    if id.is_empty()
                        || !id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                    {
                        send(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain",
                            "",
                            "invalid plugin id",
                        );
                        return;
                    }
                    let verb = if enable { "enable" } else { "disable" };
                    let out = rt.block_on(conv.cli_dispatch(
                        &format!("plugin {verb} {id}"),
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
        // E.WEB13: restart — the mind asks nothing of the OS but its own exit. systemd's
        // `Restart=always` does the restarting; this handler's whole mechanism is a clean
        // process::exit AFTER the response has flushed. Operator device only, exact confirm
        // body only, two witnesses before the exit (hash chain best-effort + journald always).
        ("POST", "/api/restart") => {
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
                Ok(dev) => {
                    let parsed: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
                    if parsed["confirm"].as_str() != Some("restart") {
                        send(
                            &mut stream,
                            "400 Bad Request",
                            "text/plain",
                            "",
                            "restart requires {\"confirm\":\"restart\"}",
                        );
                        return;
                    }
                    // Witness 1: the hash-chained decision log. record() is fail-safe by
                    // construction — a broken chain logs its own degradation and must never
                    // block an operator's restart (E.WEB13 kill criterion).
                    let mut restart_event = mind_observability::DecisionEvent::new(
                        &format!("web-restart-{}", dev.id),
                        "operator_restart",
                    );
                    restart_event.actor = Some("console".into());
                    restart_event.lane = Some("operator".into());
                    conv.recorder().record(restart_event);
                    // Witness 2: journald, unconditionally.
                    eprintln!("[restart] operator-requested restart (device={})", dev.id);
                    send_json(
                        &mut stream,
                        "200 OK",
                        "",
                        &serde_json::json!({ "restarting": true }),
                    );
                    // The response is flushed; give the socket a beat, then exit cleanly and
                    // let the supervisor bring the mind back. This is the ONLY process::exit
                    // in this crate, and it sits behind the operator gate above (source-guarded).
                    std::thread::spawn(|| {
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        std::process::exit(0);
                    });
                }
            }
        }
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
/// Why a redemption was refused: (status, message, counts_as_guess). The flag is the lockout's
/// signal (Codex's E.WEB5 security review): a WRONG code while ANY live credential exists — boot
/// code or member invite — must spend an attempt, even when the public response is the generic
/// registration-closed 409. A race loser after the last credential was consumed guessed nothing
/// and must not count.
type Refusal = (&'static str, &'static str, bool);

/// Atomically redeem the one-time code: verify, pair, delete, all under `REDEEM_LOCK`. The loser
/// of a race re-reads the file INSIDE the lock, finds it gone (or a browser already enrolled),
/// and is refused without touching the wrong-code lockout counter.
fn redeem_code(
    devices: &mind_governance::devices::DeviceStore,
    code: &str,
    label: &str,
) -> std::result::Result<(String, mind_governance::devices::Secret), Refusal> {
    let _hold = REDEEM_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    // MEMBER INVITES first (E.WEB5): explicit operator-minted codes work regardless of whether
    // first-boot registration is open — they are a different grant with a narrower role. Single
    // use: the entry is removed under the same lock that validates it. Expiry is checked at
    // redemption; an invite can only ever mint a MEMBER for the person it was bound to.
    let live_invites_exist;
    {
        let mut invites = PENDING_INVITES.lock().unwrap_or_else(|p| p.into_inner());
        invites.retain(|i| i.expires_ms > now_ms());
        live_invites_exist = !invites.is_empty();
        if let Some(pos) = invites.iter().position(|i| ct_str_eq(code, &i.code)) {
            let invite = invites.remove(pos);
            drop(invites);
            let name = format!("{WEB_DEVICE_PREFIX}member:{}", invite.person);
            return match devices.pair(
                &name,
                mind_governance::devices::DeviceRole::Member {
                    person: invite.person.clone(),
                },
            ) {
                Ok(secret) => Ok((name, secret)),
                Err(_) => Err(("409 Conflict", "pairing failed", false)),
            };
        }
    }
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
        // The public response stays the generic 409 (no oracle about which credentials exist),
        // but a wrong guess while live invites were outstanding SPENDS AN ATTEMPT — without this
        // flag, invite codes had no online guess cap at all (Codex's E.WEB5 finding).
        return Err((
            "409 Conflict",
            "registration is closed — a browser is already paired",
            live_invites_exist,
        ));
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .trim()
        .to_uppercase();
    if expected.is_empty() || !ct_str_eq(code, &expected) {
        return Err(("403 Forbidden", "wrong or expired code", true));
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
        Err(_) => Err(("409 Conflict", "pairing failed", false)),
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
        Err((status, msg, counts_as_guess)) => {
            // The refusal itself carries the verdict: a wrong code against ANY live credential
            // spends an attempt (even behind the generic 409); a race loser after the last
            // credential was consumed guessed nothing and does not count.
            if counts_as_guess {
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
    // E.WEB5: role-correct identity — a member browser must never speak as the operator.
    let ident = identity_for(authed);

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
            if let Some(l) = p.strip_prefix(mind_conversation::LANE_MARK) {
                chunk(&format!("l:{}\n", l.replace('\n', " ")));
            } else if let Some(t) = p.strip_prefix(mind_conversation::THINKING_MARK) {
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
            assert!(
                seen.insert(mint_code()),
                "mint_code repeated within 200 draws"
            );
        }
    }

    #[test]
    fn constant_time_compare_agrees_with_equality() {
        assert!(ct_str_eq("ABCD-EFGH", "ABCD-EFGH"));
        assert!(!ct_str_eq("ABCD-EFGH", "ABCD-EFGJ"));
        assert!(!ct_str_eq("ABCD", "ABCD-EFGH"));
    }

    /// Tests that must point `state_dir()` somewhere (via YM_DB) serialize here: the env var is
    /// process-global, and two such tests in parallel would read each other's scratch dirs —
    /// the exact hygiene class Codex flagged on the private-lane fixture.
    static WEB_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// The E.WEB0 race criterion, at the exact function that must enforce it: two concurrent
    /// redemptions of ONE code under DIFFERENT names — the pairing that shipped first and both
    /// won. Exactly one may succeed, and the loser's refusal must not be the wrong-code kind
    /// (a race loser guessed nothing and must not advance the lockout).
    #[test]
    fn two_racers_one_code_exactly_one_winner() {
        let _env = WEB_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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

    /// The mutation matrix, guarded at the source (Codex reviews f1287b7b/865248f9/ead24438):
    /// every mutating call site must go through `postJson` (which alone carries the transport
    /// verdict), no handler may parse-fallback an error body into success copy, and the horizon
    /// form must write its OWN reply element rather than the delegation area's.
    #[test]
    fn mutating_calls_ride_post_json_and_forms_own_their_reply_targets() {
        assert!(
            APP_JS.contains("async function postJson"),
            "the shared helper must exist"
        );
        // No raw fetch() against any mutating route — postJson is the single door.
        for route in [
            "/api/agent",
            "/api/import-agent",
            "/api/horizon",
            "/api/revoke",
            "/api/task-action",
            "/api/pair",
            "/api/setup",
        ] {
            let raw = format!("fetch(\"{route}\"");
            let allowed = route == "/api/pair" || route == "/api/setup"; // pre-postJson handlers check r.ok explicitly
            if !allowed {
                assert!(
                    !APP_JS.contains(&raw),
                    "{route} must be called via postJson, not raw fetch"
                );
            }
        }
        // The false-success pattern is banned where it was found: no POST result may be parsed
        // with a fallback-to-empty (reads in boot()/panel loaders may — an empty read renders as
        // empty, an "empty" WRITE rendered as success copy, which is the reviewed defect).
        let post_sites: Vec<&str> = APP_JS.split("postJson(").collect();
        assert!(
            post_sites.len() >= 6,
            "agent/import/horizon/revoke/task-action must all ride postJson"
        );
        // The horizon form owns its reply element; the shared area belongs to delegate/import.
        let hz = APP_JS
            .find("$(\"horizon-form\")")
            .expect("horizon form handler exists");
        let end = APP_JS[hz..]
            .find("$(\"import-form\")")
            .map(|e| hz + e)
            .unwrap_or(APP_JS.len());
        let handler = &APP_JS[hz..end];
        assert!(
            handler.contains("$(\"horizon-reply\")"),
            "horizon status must bind horizon-reply"
        );
        assert!(
            !handler.contains("$(\"agent-reply\")"),
            "horizon handler must not write the delegation reply area"
        );
    }

    /// E.OBS1 criterion (3): the client renders lane chips ONLY from the dispatcher's `l:` lines —
    /// no client-side lane inference, no default badge.
    #[test]
    fn lane_chips_render_only_from_dispatcher_lane_lines() {
        assert!(APP_JS.contains("lane-chip"), "the chip renderer exists");
        let l_arm = APP_JS
            .find("if (kind === \"l:\")")
            .expect("the l: arm exists");
        let arm = &APP_JS[l_arm..APP_JS.len().min(l_arm + 900)];
        assert!(
            arm.contains("lane-chip"),
            "chips are created inside the l: arm"
        );
        // Exactly one construction site for lane chips — the l: arm's.
        assert_eq!(
            APP_JS.matches("lane-chip lane-").count(),
            1,
            "lane chips must have exactly one construction site (the l: arm)"
        );
        // Labels carry colons (nanogpt:deepseek/deepseek-v4-pro): the parse must cut at the FIRST
        // delimiter and keep the remainder. JS split-with-limit DISCARDS the tail — banned here.
        assert!(
            !arm.contains("split(\":\""),
            "the l: arm must not parse with split(\":\") — it truncates colon-bearing labels"
        );
        assert!(
            arm.contains("indexOf(\":\")"),
            "the l: arm parses scope/label at the first delimiter, preserving the remainder"
        );
    }

    /// E.WEB5 criterion (3): a member invite is single-use and expiry-bounded, and only ever mints
    /// a Member — verified at the pending-invite layer without a device store, since redemption
    /// wiring is covered by the pairing tests.
    #[test]
    fn member_invites_are_single_use_and_expiring() {
        PENDING_INVITES.lock().unwrap().clear();
        let now = now_ms();
        {
            let mut inv = PENDING_INVITES.lock().unwrap();
            inv.push(super::MemberInvite {
                code: "LIVE-CODE".into(),
                person: "brishti".into(),
                expires_ms: now + 60_000,
            });
            inv.push(super::MemberInvite {
                code: "DEAD-CODE".into(),
                person: "arka".into(),
                expires_ms: now.saturating_sub(1),
            });
        }
        // The expiry sweep the redeem path runs first.
        {
            let mut inv = PENDING_INVITES.lock().unwrap();
            inv.retain(|i| i.expires_ms > now_ms());
            assert!(
                inv.iter().any(|i| i.code == "LIVE-CODE"),
                "the live invite survives"
            );
            assert!(
                !inv.iter().any(|i| i.code == "DEAD-CODE"),
                "the expired invite is swept"
            );
        }
        // Single use: taking the live one removes it; a second take finds nothing.
        {
            let mut inv = PENDING_INVITES.lock().unwrap();
            let pos = inv
                .iter()
                .position(|i| ct_str_eq("LIVE-CODE", &i.code))
                .unwrap();
            let taken = inv.remove(pos);
            assert_eq!(taken.person, "brishti", "the invite is bound to its person");
            assert!(
                inv.iter()
                    .position(|i| ct_str_eq("LIVE-CODE", &i.code))
                    .is_none(),
                "single use: gone after redemption"
            );
        }
        PENDING_INVITES.lock().unwrap().clear();
    }

    /// Codex's E.WEB5 review, both asks in one fixture: (a) END-TO-END — an invite redeems
    /// through `redeem_code` into a persisted Member device whose authenticate→identity_for chain
    /// yields HouseholdMember scope, and can never widen to operator; (b) THE LOCKOUT BYPASS —
    /// a wrong guess while a live invite exists returns the generic 409 but MUST count as a guess,
    /// and after the invite is consumed the same wrong guess no longer counts (race-loser rule).
    #[test]
    fn invite_redemption_is_member_scoped_end_to_end_and_wrong_guesses_count() {
        let _env = WEB_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("ym-invite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YM_DB", dir.join("mind.db").to_string_lossy().to_string());
        let store = mind_governance::devices::DeviceStore::open(&dir).unwrap();
        // A paired operator browser already exists (the normal state when invites are in play).
        let _op = store
            .pair(
                "web:owner",
                mind_governance::devices::DeviceRole::Operator {
                    default_person: mind_types::PRIMARY.to_string(),
                },
            )
            .unwrap();

        PENDING_INVITES.lock().unwrap().clear();
        PENDING_INVITES.lock().unwrap().push(super::MemberInvite {
            code: "INVITE-99".into(),
            person: "brishti".into(),
            expires_ms: now_ms() + 60_000,
        });

        // (b1) wrong guess WHILE the invite is live: generic 409, counts_as_guess = true.
        let miss = super::redeem_code(&store, "WRONG-GUESS", "x");
        match miss {
            Err((status, _, counts)) => {
                assert!(
                    status.starts_with("409"),
                    "public response stays generic: {status}"
                );
                assert!(
                    counts,
                    "a wrong guess against a live invite must spend an attempt"
                );
            }
            Ok(_) => panic!("a wrong code must not redeem"),
        }

        // (a) the real redemption: Member role, correct person, member scope end to end.
        let (name, secret) =
            super::redeem_code(&store, "INVITE-99", "ignored").expect("invite redeems");
        assert_eq!(name, "web:member:brishti");
        let authed = store
            .authenticate(secret.expose())
            .expect("token authenticates");
        assert!(
            !authed.is_operator(),
            "an invite must never mint an operator"
        );
        assert_eq!(authed.chat_person(), "brishti");
        let ident = super::identity_for(&authed);
        assert_eq!(ident.owner, "brishti");
        assert!(
            matches!(
                ident.output_scope,
                mind_conversation::OutputScope::HouseholdMember
            ),
            "member device identity must be HouseholdMember scope"
        );

        // (b2) after consumption: same wrong guess, still 409, but counts_as_guess = false.
        let after = super::redeem_code(&store, "WRONG-GUESS", "x");
        match after {
            Err((status, _, counts)) => {
                assert!(status.starts_with("409"));
                assert!(!counts, "with no live credentials, a miss is not a guess");
            }
            Ok(_) => panic!("nothing left to redeem"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// E.SEC18 criterion (2): the audit never renders a secret — no code, token, or credential
    /// value. With a live invite outstanding, the JSON carries its COUNT and nothing that matches
    /// the code itself.
    #[test]
    fn the_security_audit_renders_counts_never_credentials() {
        let _env = WEB_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("ym-audit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("YM_DB", dir.join("mind.db").to_string_lossy().to_string());
        let store = mind_governance::devices::DeviceStore::open(&dir).unwrap();
        let secret = store
            .pair(
                "web:owner",
                mind_governance::devices::DeviceRole::Operator {
                    default_person: mind_types::PRIMARY.to_string(),
                },
            )
            .unwrap();
        PENDING_INVITES.lock().unwrap().clear();
        PENDING_INVITES.lock().unwrap().push(super::MemberInvite {
            code: "SECRET-INVITE-CODE".into(),
            person: "brishti".into(),
            expires_ms: now_ms() + 60_000,
        });
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let conv = ConversationEngine::new(
            std::sync::Arc::new(mem) as std::sync::Arc<dyn mind_types::MemoryFacade>,
            mind_inference::InferencePool::new(
                std::sync::Arc::new(mind_inference::ScriptedLLM::new("x"))
                    as std::sync::Arc<dyn yantrik_ml::LLMBackend>,
                1,
            ),
            "JARVIS",
        );
        let audit = super::security_audit_json(&conv, &store).to_string();
        assert!(
            !audit.contains("SECRET-INVITE-CODE"),
            "an invite code must never render: {audit}"
        );
        assert!(
            !audit.contains(secret.expose()),
            "a device token must never render"
        );
        assert!(
            audit.contains("\"live_member_invites\":1"),
            "the count renders: {audit}"
        );
        assert!(
            audit.contains("dispatched_exposure"),
            "lane semantics named: {audit}"
        );
        PENDING_INVITES.lock().unwrap().clear();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// E.WEB8 criterion (3): plugin-toggle forwards only enable|disable to the dispatcher, with a
    /// charset-checked id — a source guard on the route's construction, no free-text passthrough.
    #[test]
    fn plugin_toggle_forwards_only_a_closed_verb_set() {
        // The route builds exactly "plugin {enable|disable} {id}"; assert the source contains that
        // shape and no interpolation that could smuggle a second verb.
        let src = include_str!("web.rs");
        assert!(
            src.contains("format!(\"plugin {verb} {id}\")"),
            "the dispatcher line is fixed-shape"
        );
        assert!(
            src.contains("if enable { \"enable\" } else { \"disable\" }"),
            "verb is a closed set, not request text"
        );
        assert!(
            src.contains("c.is_ascii_alphanumeric() || c == '-' || c == '_'"),
            "the plugin id is charset-checked before dispatch"
        );
    }

    /// E.WEB13 gate (4): the exit is reachable ONLY from the authenticated restart handler.
    /// There is exactly one process::exit in this file, it sits inside the "/api/restart" arm,
    /// and that arm passes the operator gate and the exact-confirm check before reaching it.
    #[test]
    fn restart_exit_is_unique_operator_gated_and_confirm_locked() {
        let src = include_str!("web.rs");
        // Build the needle at runtime so this test's own source cannot satisfy the count.
        let needle = String::from("std::process::") + "exit(0)";
        assert_eq!(
            src.matches(needle.as_str()).count(),
            1,
            "exactly one exit site in the web surface"
        );
        let arm_start = src
            .find("(\"POST\", \"/api/restart\")")
            .expect("restart arm exists");
        let arm_end = src[arm_start..]
            .find("(\"GET\", \"/api/capabilities\")")
            .expect("next arm bounds the slice")
            + arm_start;
        let arm = &src[arm_start..arm_end];
        assert!(
            arm.contains(needle.as_str()),
            "the one exit lives inside the restart arm"
        );
        let gate = arm
            .find("operator(&head, &devices)")
            .expect("operator gate present");
        let confirm = arm
            .find("parsed[\"confirm\"].as_str() != Some(\"restart\")")
            .expect("exact-confirm check present");
        let exit = arm.find(needle.as_str()).unwrap();
        assert!(
            gate < confirm && confirm < exit,
            "gate, then confirm, then exit — in that order"
        );
        assert!(
            !arm.contains("cli_dispatch"),
            "the restart arm forwards NOTHING to the tool dispatcher — no model-visible path"
        );
    }

    /// E.WEB13 gate (3): both witnesses precede the exit — the hash-chain record and the
    /// journald line appear in the arm before the exit site, and the response is sent before
    /// the exit is scheduled (the operator sees success, not a dropped socket).
    #[test]
    fn restart_witnesses_and_response_precede_the_exit() {
        let src = include_str!("web.rs");
        let arm_start = src.find("(\"POST\", \"/api/restart\")").unwrap();
        let arm_end = src[arm_start..]
            .find("(\"GET\", \"/api/capabilities\")")
            .unwrap()
            + arm_start;
        let arm = &src[arm_start..arm_end];
        let needle = String::from("std::process::") + "exit(0)";
        let exit = arm.find(needle.as_str()).expect("exit in arm");
        let chain = arm.find("operator_restart").expect("chain witness present");
        let journald = arm
            .find("[restart] operator-requested")
            .expect("journald witness present");
        let respond = arm.find("\"restarting\": true").expect("response present");
        assert!(
            chain < exit && journald < exit && respond < exit,
            "chain witness, journald witness, and the flushed response all precede the exit"
        );
    }

    /// E.WEB17 gates: agents are a sub-menu of three views over the SAME data; one classifier
    /// decides the bucket; standing orders arrive typed through a read-only operator route; the
    /// text dump is gone; every state the server emits is named in the classifier's contract.
    #[test]
    fn the_agents_submenu_is_three_views_over_one_classifier() {
        let src = include_str!("web.rs");
        let html = APP_HTML;
        let js = APP_JS;
        for view in ["running", "dormant", "new"] {
            assert!(
                html.contains(&format!("data-panel=\"tasks\" data-view=\"{view}\"")),
                "nav has the {view} view"
            );
        }
        assert!(
            html.contains("id=\"panel-tasks\" data-view=\"running\""),
            "the panel opens on what is working now"
        );
        assert!(
            !html.contains("orders-text"),
            "the standing-orders text dump is gone"
        );
        assert!(
            js.contains("function agentBucket(item)")
                && [
                    "bucketAdd(card, j)",
                    "bucketAdd(card, g)",
                    "bucketAdd(card, o)"
                ]
                .iter()
                .all(|call| js.contains(call)),
            "one classifier, applied to jobs, goals and orders alike"
        );
        // The classifier's contract names every state the server emits.
        for word in [
            "running|done|failed",
            "RUNNING|PENDING|SCHEDULED|COMPLETED|FAILED",
            "sleeping|paused",
        ] {
            assert!(
                js.contains(word),
                "classifier contract names the server's states: {word}"
            );
        }
        assert!(
            js.contains("fetch(\"/api/standing-orders\""),
            "orders are read typed, from the server"
        );
        let route = src
            .find("(\"GET\", \"/api/standing-orders\")")
            .expect("standing-orders route exists");
        assert!(
            src[route..route + 300].contains("operator(&head, &devices)")
                && src[route..route + 400].contains("\"orders json\""),
            "standing-orders is operator-gated and read-only"
        );
        // The text form still serves the Activity panel's summary; the agents views never read it.
        let tasks_fn = js
            .find("async function loadTasks()")
            .expect("loadTasks exists");
        let orders_fn = js
            .find("async function loadStandingOrders()")
            .expect("loadStandingOrders exists");
        for start in [tasks_fn, orders_fn] {
            assert!(
                !js[start..start + 1500].contains("fetch(\"/api/orders\""),
                "the agents views read orders typed, never as text"
            );
        }
    }

    /// E.WEB16 gates: no new mutation path (schedules become an agent DOCUMENT through the
    /// existing import route; re-runs ride /api/agent; lifecycle rides /api/task-action), the
    /// skills route is operator-gated and read-only, results render through the DOM-only
    /// markdown renderer, the timeline is built from the job's own timestamps, and the
    /// "what an agent may do" line is composed from the claims registry, not literal copy.
    #[test]
    fn the_agent_composer_adds_no_mutation_path_and_renders_results_safely() {
        let src = include_str!("web.rs");
        let skills = src
            .find("(\"GET\", \"/api/skills\")")
            .expect("skills route exists");
        assert!(
            src[skills..skills + 300].contains("operator(&head, &devices)"),
            "skills is operator-gated"
        );
        assert!(
            !src[skills..skills + 900].contains("(\"POST\", \"/api/skills\")"),
            "skills is read-only"
        );
        let tasks = APP_JS
            .find("async function loadTasks()")
            .expect("run renderer exists");
        let body: String = APP_JS[tasks..].chars().take(6000).collect();
        assert!(
            body.contains("renderMarkdown(md, String(j.result))"),
            "results render DOM-only"
        );
        assert!(
            !body.contains("innerHTML"),
            "nothing in the run renderer touches innerHTML"
        );
        assert!(
            body.contains("for (const n of notes) addStep(n.t, String(n.note || \"\"))"),
            "timeline from the job's notes"
        );
        assert!(
            body.contains(
                "postJson(\"/api/agent\", { name: j.name || j.id, task: j.task || j.goal })"
            ),
            "re-run rides the delegation gate"
        );
        assert!(
            body.contains("brief.classList.add(\"clamp\")"),
            "briefs clamp, never dump"
        );
        // Schedules: composed as a document and submitted through the import gate only.
        assert_eq!(
            APP_JS.matches("postJson(\"/api/import-agent\"").count(),
            2,
            "import gate: the paste form and the composer, nothing else"
        );
        assert!(
            APP_JS.contains("schedule: ${line}"),
            "the composer writes schedule frontmatter"
        );
        assert!(
            !APP_JS.contains("postJson(\"/api/schedule\""),
            "no side door for schedules"
        );
        // Allowance from the registry.
        assert!(
            APP_JS.contains("for (const id of [\"real-money\", \"self-edit\", \"tool-learning\"])"),
            "allowance composed from claims"
        );
    }

    /// E.WEB15 gates: the chains route is operator-gated; the instrument column renders ONLY
    /// from fetched JSON (no literal percentage or count is written into the page); the Board
    /// classifies horizon goals by queue_status so a failed goal can never read as RUNNING; the
    /// two looks are one code path behind a data-mode token switch.
    #[test]
    fn the_cockpit_instruments_are_evidence_only_and_the_board_classifies_by_queue_status() {
        let src = include_str!("web.rs");
        let chains = src
            .find("(\"GET\", \"/api/chains\")")
            .expect("chains route exists");
        assert!(
            src[chains..chains + 300].contains("operator(&head, &devices)"),
            "chains is operator-gated"
        );
        // Instruments: the renderer exists and never invents a number.
        let inst = APP_JS
            .find("async function loadInstruments()")
            .expect("instrument loader");
        // Char-boundary safe: the JS carries "·" and "→", so a byte-offset slice can panic.
        let body: String = APP_JS[inst..].chars().take(5200).collect();
        assert!(
            body.contains("fetch(\"/api/chains\""),
            "gate read from the server"
        );
        assert!(
            body.contains("fetch(\"/api/horizons\""),
            "goal read from the server"
        );
        assert!(
            body.contains("fetch(\"/api/decisions"),
            "recorder read from the server"
        );
        // E.G1c: the shadow card names WHICH sample it shows; the two are never pooled.
        assert!(
            body.contains(":knock-receptivity") && body.contains(":headless-cadence"),
            "the world-shadow card labels the paired and unpaired samples"
        );
        assert!(
            !body.contains("81.2") && !body.contains("65 /") && !body.contains("100%"),
            "no literal metric is written into the instrument column"
        );
        // Board: horizon goals classify by queue_status (budget-expired reads as failed).
        assert!(
            APP_JS.contains(
                "const st = g.budget_expired ? \"failed\" : (g.queue_status || g.status)"
            ),
            "the Board classifies horizon goals by queue_status"
        );
        // Two looks, one code path: a token switch persisted per device.
        assert!(
            APP_CSS.contains(":root[data-mode=\"companion\"]"),
            "companion tokens exist"
        );
        assert!(
            APP_JS.contains("document.documentElement.dataset.mode = mode"),
            "mode switch sets the token root"
        );
        assert!(
            APP_HTML.contains("id=\"mode-btn\""),
            "the switch is in the top strip"
        );
        // Members: no instrument column, no operator facts.
        assert!(
            APP_JS.contains("const inst = $(\"instruments\"); if (inst) inst.remove();"),
            "members never see the instrument column"
        );
        // Long agent briefs are clamped, never dumped.
        assert!(
            APP_JS.contains("brief.classList.add(\"clamp\")"),
            "long briefs clamp with an expander"
        );
        // Decisions speak plainly first, with the recorded kind beneath.
        assert!(
            APP_JS.contains("function phraseFor(d)"),
            "plain-phrase renderer exists"
        );
    }

    /// Codex's E.WEB14 composition finding: the client's encoded id must reach the engine as the
    /// real id. A colon-bearing id round-trips through the actual query parser; a bad escape and
    /// a smuggled slash are refused; the charset check runs on the DECODED value.
    #[test]
    fn horizon_history_ids_are_decoded_before_the_charset_check() {
        let ok = |c: char| c.is_ascii_alphanumeric() || c == ':' || c == '-' || c == '_';
        let decoded = horizon_history_id("/api/horizon-history?id=goal%3Ahorizon%3A1a05e8e69b4")
            .expect("decodes");
        assert_eq!(decoded, "goal:horizon:1a05e8e69b4");
        assert!(
            decoded.chars().all(ok),
            "the decoded id passes the charset check"
        );
        assert_eq!(
            horizon_history_id("/api/horizon-history?id=goal:horizon:abc&x=1").as_deref(),
            Some("goal:horizon:abc"),
            "an unencoded id still works, and trailing params are ignored"
        );
        assert_eq!(
            horizon_history_id("/api/horizon-history?id=%zz"),
            None,
            "bad escape refused"
        );
        // The route must hand the extractor the FULL target: a query-stripped path has no id.
        assert_eq!(
            horizon_history_id("/api/horizon-history"),
            None,
            "no query, no id"
        );
        let src = include_str!("web.rs");
        let arm = src.find("p.starts_with(\"/api/horizon-history\")").unwrap();
        assert!(
            src[arm..arm + 900].contains("horizon_history_id(&target)"),
            "the route reads the id from target (with query), never from the stripped path"
        );
        let smuggled = horizon_history_id("/api/horizon-history?id=..%2Fetc").unwrap();
        assert!(
            !smuggled.chars().all(ok),
            "a decoded slash fails the charset check"
        );
        assert!(
            APP_JS.contains("encodeURIComponent(g.goal_id)")
                || APP_JS.contains("encodeURIComponent(pick.goal_id)"),
            "the client still encodes — the server now meets it halfway"
        );
    }

    /// E.WEB14 gates (2) and (4): horizon-history is operator-gated with an id charset check
    /// BEFORE dispatch; claims requires a paired device; the client renders receipts DOM-only.
    #[test]
    fn the_console_doors_are_gated_and_charset_checked() {
        let src = include_str!("web.rs");
        let hh = src
            .find("p.starts_with(\"/api/horizon-history\")")
            .expect("horizon-history route exists");
        let arm = &src[hh..hh + 2200];
        let gate = arm
            .find("operator(&head, &devices)")
            .expect("operator gate");
        let check = arm
            .find("c.is_ascii_alphanumeric() || c == ':' || c == '-' || c == '_'")
            .expect("id charset check");
        let dispatch = arm.find("horizon_history_json").expect("dispatch");
        assert!(
            gate < check && check < dispatch,
            "gate, then charset, then dispatch"
        );
        let cl = src
            .find("(\"GET\", \"/api/claims\")")
            .expect("claims route exists");
        assert!(
            src[cl..cl + 600].contains("devices.authenticate(&t)"),
            "claims requires a paired device"
        );
        assert!(
            APP_JS.contains("encodeURIComponent(g.goal_id)"),
            "the client encodes the id it sends"
        );
        assert!(
            APP_JS.contains("line.textContent = "),
            "receipt lines are rendered as text, never markup"
        );
    }

    /// E.WEB13 gate (2), client side: the console asks the HUMAN before posting, and posts the
    /// exact confirm body from its single call site — nothing else in the app reaches the route.
    #[test]
    fn restart_ui_is_confirm_locked_with_one_call_site() {
        assert_eq!(
            APP_JS.matches("postJson(\"/api/restart\"").count(),
            1,
            "exactly one restart call site in the app"
        );
        assert!(
            APP_JS.contains("window.confirm(\"Restart the mind now?"),
            "a human confirmation precedes the post"
        );
        assert!(
            APP_JS.contains("postJson(\"/api/restart\", { confirm: \"restart\" })"),
            "the exact confirm body is posted"
        );
    }

    /// E.WEB9: the board is a read-only lens — its classifier routes by status into a CLOSED set
    /// of four columns and performs no fetch or mutation itself. Source-guarded like the other
    /// client logic.
    #[test]
    fn the_board_classifier_is_closed_and_read_only() {
        assert!(
            APP_JS.contains("function columnFor("),
            "the classifier exists"
        );
        for col in ["\"needs\"", "\"scheduled\"", "\"running\"", "\"done\""] {
            assert!(
                APP_JS.contains(&format!("return {col}")),
                "column {col} is a classifier outcome"
            );
        }
        // The classifier body must not fetch or mutate — it is pure routing.
        let start = APP_JS.find("function columnFor(").unwrap();
        let body = &APP_JS[start..start + 400];
        assert!(!body.contains("fetch("), "columnFor must not fetch");
        assert!(!body.contains("postJson"), "columnFor must not mutate");
    }

    /// E.WEB11: the Files listing reads ONLY the fixed published-pages dir — a file outside it and
    /// a dotfile inside it never appear, and only servable extensions do.
    #[test]
    fn files_listing_is_confined_to_the_published_dir() {
        let _env = WEB_ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let dir = std::env::temp_dir().join(format!("ym-files-{}", std::process::id()));
        let outside = std::env::temp_dir().join(format!("ym-files-outside-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(dir.join("dashboard.html"), "<html></html>").unwrap();
        std::fs::write(dir.join(".secret.html"), "hidden").unwrap();
        std::fs::write(dir.join("notes.md"), "not servable").unwrap();
        std::fs::write(outside.join("elsewhere.html"), "<html></html>").unwrap();
        std::env::set_var("YM_WEB_DIR", dir.to_string_lossy().to_string());

        let names: Vec<String> = super::list_published_pages()
            .into_iter()
            .filter_map(|f| f["name"].as_str().map(String::from))
            .collect();
        assert!(
            names.contains(&"dashboard.html".to_string()),
            "servable file listed: {names:?}"
        );
        assert!(
            !names.iter().any(|n| n.starts_with('.')),
            "no dotfiles: {names:?}"
        );
        assert!(
            !names.contains(&"notes.md".to_string()),
            "non-servable excluded: {names:?}"
        );
        assert!(
            !names.contains(&"elsewhere.html".to_string()),
            "nothing outside the dir: {names:?}"
        );
        std::env::remove_var("YM_WEB_DIR");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// E.WEB10: the light theme redefines only tokens already on :root — no color gets its sole
    /// definition inside the theme block (which would make it undefined in the default theme).
    #[test]
    fn the_light_theme_only_overrides_existing_tokens() {
        let css = APP_CSS;
        let light_start = css
            .find("[data-theme=\"light\"]")
            .expect("light block exists");
        let light_block = &css[light_start
            ..css[light_start..]
                .find('}')
                .map(|e| light_start + e)
                .unwrap_or(css.len())];
        for tok in [
            "--bg0",
            "--ink",
            "--line",
            "--accent-a",
            "--danger",
            "--user-bubble",
        ] {
            let defined_on_root = css[..light_start].contains(&format!("{tok}:"));
            let in_light = light_block.contains(&format!("{tok}:"));
            if in_light {
                assert!(
                    defined_on_root,
                    "{tok} is overridden in light but never defined on :root"
                );
            }
        }
    }

    /// E.WEB12: the dreaming route serializes the engine's already-sanitized DmnLogEntry and adds
    /// no raw content of its own — it exposes exactly at_ms/tick_no/phase/message and nothing else.
    #[test]
    fn the_dreaming_route_exposes_only_the_sanitized_entry_shape() {
        // Serialize a representative entry the way the route does; the shape must be the four
        // sanitized fields — no path, no raw error object, no secret-carrying extension.
        let entry = mind_conversation::DmnLogEntry {
            at_ms: 1,
            tick_no: 2,
            phase: "reconcile".into(),
            message: "consolidated 3 beliefs".into(),
        };
        let v = serde_json::to_value(vec![entry]).unwrap();
        let obj = v[0].as_object().unwrap();
        let mut keys: Vec<&String> = obj.keys().collect();
        keys.sort();
        assert_eq!(
            keys,
            vec!["at_ms", "message", "phase", "tick_no"],
            "the route must expose only the sanitized entry fields: {keys:?}"
        );
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
