//! The person sets their Mind's model -- key included -- from the desktop's Settings (E.PROV1).
//!
//! Pranab's decision: every person picks their own Mind's provider, and entering it is their
//! consent. A key must never pass through a chat transcript (the desktop shows the chat and reports
//! it to other agents), and a root helper would add a root code path that parses a secret while
//! gating nothing (every process of the person could run it). So the Mind takes the setting itself:
//! `POST /provider` on the person's memory socket (E.SOCK1 -- never on TCP), accepted only from the
//! person's own uid, written into the Mind's own 0600 settings file. The desktop's page
//! (Settings -> AI & Intelligence -> Yantrik Mind -> Model) sends it straight here and keeps nothing.

use std::path::{Path, PathBuf};

use axum::extract::{ConnectInfo, State};
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use mind_memory_mcp::PeerUid;
use serde::Deserialize;

/// Where the desktop sends a person to set this Mind's model.
pub const SETTINGS_PLACE: &str = "Settings \u{2192} AI & Intelligence \u{2192} Yantrik Mind \u{2192} Model";

/// What the desktop sends.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ProviderSet {
    /// Absent on the gateway's revert (`{"source": "yantrik-gateway", "remove": true}`).
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// The gateway sends its token as `api_key`.
    #[serde(default, alias = "api_key")]
    pub key: Option<String>,
    /// May the person's private context go to this (cloud) provider? Explicit, per provider. From
    /// the gateway (E.PROV2) it is consent DELEGATED to the gateway's per-account choice.
    #[serde(default)]
    pub private_context: bool,
    /// Who sent it: `yantrik-gateway` for the OS's "Use Yantrik models" (yantrik-os #675).
    #[serde(default)]
    pub source: Option<String>,
    /// E.PROV2: the gateway's revert -- put back what the person had before it.
    #[serde(default)]
    pub remove: bool,
}

/// E.PROV2: the OS's local model gateway.
const GATEWAY: &str = mind_inference::GATEWAY_PROVIDER;

/// What was saved -- never the key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Saved {
    pub provider: String,
    pub model: Option<String>,
}

/// The cloud providers the default chain runs, with the variable naming each one's model.
const CLOUD: &[(&str, &str)] = &[("nanogpt", "YM_MODEL"), ("ollama-cloud", "YM_OLLAMA_MODEL"), ("minimax", "YM_MINIMAX_MODEL")];

/// One line, no control characters: a value cannot add lines to the settings file. The error never
/// quotes the value.
fn one_line(v: &str, what: &str) -> Result<String, String> {
    let v = v.trim();
    if v.is_empty() {
        return Err(format!("{what} is empty"));
    }
    if v.chars().any(|c| c.is_control()) || v.len() > 1024 {
        return Err(format!("{what} must be a single line of text"));
    }
    Ok(v.to_string())
}

fn model_name(v: &str) -> Result<String, String> {
    let v = one_line(v, "the model")?;
    if v.len() > 200 || !v.chars().all(|c| c.is_ascii_alphanumeric() || "._:/@-".contains(c)) {
        return Err("the model name has characters a model name does not have".into());
    }
    Ok(v)
}

/// Save the setting into the Mind's own settings file, keeping everything else in it.
pub fn apply(env_path: &Path, req: &ProviderSet) -> Result<Saved, String> {
    let existing = std::fs::read_to_string(env_path)
        .unwrap_or_else(|_| "# Yantrik Mind's own settings. Yantrik OS never reads this file.\n".to_string());
    let upsert = crate::setup::upsert_env_line;
    let mut provider = req.provider.trim().to_string();
    if provider.is_empty() && req.source.as_deref() == Some(GATEWAY) {
        provider = GATEWAY.to_string();
    }
    if req.remove && provider != GATEWAY {
        return Err("only Yantrik models can be put back this way".into());
    }
    let (body, model) = if provider == GATEWAY && req.remove {
        (put_back(env_path, &existing), None)
    } else if provider == GATEWAY {
        take_gateway(env_path, &existing, req)?
    } else if provider == "ollama" {
        let url = req
            .base_url
            .as_deref()
            .and_then(crate::first_run::parse_address)
            .ok_or("a local model needs the address of the machine running Ollama")?;
        let model = model_name(req.model.as_deref().ok_or("a local model needs the model's name")?)?;
        let mut body = upsert(&existing, "YM_LOCAL_OLLAMA_URL", &url);
        body = upsert(&body, "YM_LOCAL_OLLAMA_MODEL", &model);
        if let Some(k) = req.key.as_deref().filter(|k| !k.trim().is_empty()) {
            body = upsert(&body, "YM_LOCAL_OLLAMA_KEY", &one_line(k, "the key")?);
        }
        (body, Some(model))
    } else if let Some((_, model_var)) = CLOUD.iter().find(|(p, _)| *p == provider) {
        if req.base_url.as_deref().is_some_and(|u| !u.trim().is_empty()) {
            return Err(format!("{provider} is reached at its own address; a base_url is not taken"));
        }
        let key = one_line(req.key.as_deref().unwrap_or_default(), "the key")?;
        if key.len() < 8 {
            return Err("that key is too short to be a key".into());
        }
        let (_, key_var, _) = mind_inference::provider_catalog(&provider).ok_or("unknown provider")?;
        let mut body = upsert(&existing, key_var, &key);
        let model = match req.model.as_deref().filter(|m| !m.trim().is_empty()) {
            Some(m) => {
                let m = model_name(m)?;
                body = upsert(&body, model_var, &m);
                Some(m)
            }
            None => None,
        };
        body = upsert(&body, "YM_PRIVATE_PROVIDERS", &private_list(&existing, &provider, req.private_context));
        (body, model)
    } else {
        return Err("unknown provider -- this Mind takes ollama, nanogpt, ollama-cloud, minimax or yantrik-gateway".into());
    };
    crate::setup::write_env_600(&env_path.display().to_string(), &body).map_err(|e| format!("could not save: {e}"))?;
    if provider == GATEWAY && req.remove {
        let _ = std::fs::remove_file(backup_path(env_path));
    }
    Ok(Saved { provider, model })
}

/// E.PROV2: where the lines the gateway replaced are kept until it is put back. 0600, beside the
/// settings file.
fn backup_path(env_path: &Path) -> PathBuf {
    let mut p = env_path.as_os_str().to_owned();
    p.push(".pre-gateway");
    PathBuf::from(p)
}

fn env_value(body: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    body.lines().find_map(|l| l.trim_start().strip_prefix(prefix.as_str()).map(str::to_string))
}

fn drop_env_line(body: &str, key: &str) -> String {
    let prefix = format!("{key}=");
    let mut s = body.lines().filter(|l| !l.trim_start().starts_with(prefix.as_str())).collect::<Vec<_>>().join("\n");
    if !s.is_empty() && !s.ends_with('\n') {
        s.push('\n');
    }
    s
}

/// E.PROV2: take "Use Yantrik models". Only this machine's gateway (a loopback address); it becomes
/// the brain, alone (`default_chain_from_env`); the private list holds it when the person's private
/// context may go there -- consent delegated to the gateway's per-account choice. NOT the local
/// lane: the gateway forwards to cloud accounts, and `YM_LOCAL_OLLAMA_URL` means owned hardware.
fn take_gateway(env_path: &Path, existing: &str, req: &ProviderSet) -> Result<(String, Option<String>), String> {
    let upsert = crate::setup::upsert_env_line;
    let url = req.base_url.as_deref().map(|u| u.trim().trim_end_matches('/').to_string()).unwrap_or_default();
    if !mind_inference::is_loopback_http(&url) {
        return Err("Yantrik models are taken only from this machine's own gateway (http://127.0.0.1:...)".into());
    }
    let key = one_line(req.key.as_deref().unwrap_or_default(), "the gateway token")?;
    if key.len() < 8 {
        return Err("that token is too short to be a token".into());
    }
    let model = model_name(req.model.as_deref().filter(|m| !m.trim().is_empty()).unwrap_or("picked"))?;
    // The first take keeps what it replaces; a later one (a new token) keeps that first copy.
    let backup = backup_path(env_path);
    if !backup.exists() {
        let prior = serde_json::json!({
            "primary_brain": env_value(existing, "YM_PRIMARY_BRAIN"),
            "private_line": env_value(existing, "YM_PRIVATE_PROVIDERS").is_some(),
        });
        crate::setup::write_env_600(&backup.display().to_string(), &prior.to_string()).map_err(|e| format!("could not save: {e}"))?;
    }
    let mut body = upsert(existing, "YM_GATEWAY_URL", &url);
    body = upsert(&body, "YM_GATEWAY_KEY", &key);
    body = upsert(&body, "YM_PRIMARY_BRAIN", &format!("{GATEWAY}:{model}"));
    body = upsert(&body, "YM_PRIVATE_PROVIDERS", &private_list(existing, GATEWAY, req.private_context));
    Ok((body, Some(model)))
}

/// E.PROV2: put back what the person had before Yantrik models. The gateway's lines go; the brain
/// line is restored as it was (or removed if there was none); the gateway leaves the private list,
/// and a list line the gateway created is removed again.
fn put_back(env_path: &Path, existing: &str) -> String {
    let upsert = crate::setup::upsert_env_line;
    let prior: Option<serde_json::Value> = std::fs::read_to_string(backup_path(env_path)).ok().and_then(|t| serde_json::from_str(&t).ok());
    let mut body = drop_env_line(existing, "YM_GATEWAY_URL");
    body = drop_env_line(&body, "YM_GATEWAY_KEY");
    match prior.as_ref().map(|p| p["primary_brain"].as_str()) {
        Some(Some(v)) => body = upsert(&body, "YM_PRIMARY_BRAIN", v),
        Some(None) => body = drop_env_line(&body, "YM_PRIMARY_BRAIN"),
        None => {
            if env_value(&body, "YM_PRIMARY_BRAIN").is_some_and(|v| v.trim().starts_with(GATEWAY)) {
                body = drop_env_line(&body, "YM_PRIMARY_BRAIN");
            }
        }
    }
    if env_value(&body, "YM_PRIVATE_PROVIDERS").is_some() {
        let list = private_list(&body, GATEWAY, false);
        let created = prior.as_ref().is_some_and(|p| p["private_line"] == false);
        body = if list.is_empty() && created { drop_env_line(&body, "YM_PRIVATE_PROVIDERS") } else { upsert(&body, "YM_PRIVATE_PROVIDERS", &list) };
    }
    body
}

/// `YM_PRIVATE_PROVIDERS` with `provider` added (consent) or removed (no consent).
fn private_list(existing: &str, provider: &str, consent: bool) -> String {
    let current = existing
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("YM_PRIVATE_PROVIDERS="))
        .unwrap_or_default();
    let mut items: Vec<String> = current
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty() && x != provider)
        .collect();
    if consent {
        items.push(provider.to_string());
    }
    items.join(",")
}

#[derive(Clone)]
struct Setter {
    person_uid: u32,
    env_path: PathBuf,
    supervised: bool,
    /// E.SOCK2: the one gateway address the socket accepts.
    gateway: String,
}

/// E.SOCK2: this machine's model gateway (yantrik-os #675), the only provider the person's socket
/// may set.
pub const GATEWAY_URL: &str = "http://127.0.0.1:7460/v1";

/// The route, for the person's socket only.
///
/// E.SOCK2: what the socket may change is only what the person already chose. Every process of the
/// person can reach it -- harnesses running model-written code included -- and nothing on it can
/// tell the desktop from them (yantrik-os-dc, 12 Oct: the shell binds the door, so any directory it
/// can bind in, a harness can too). So `/provider` takes the gateway at its fixed address and
/// nothing else (never a key, an address or the private lane), and `/consent` is not here: a forged
/// snapshot would be a forged consent. Both wait for a path that knows the person is there.
pub fn router(person_uid: u32, env_path: PathBuf, supervised: bool) -> Router {
    router_at(person_uid, env_path, supervised, GATEWAY_URL)
}

/// [`router`] with the gateway at `gateway` -- tests run a fake one on a free port.
pub fn router_at(person_uid: u32, env_path: PathBuf, supervised: bool, gateway: &str) -> Router {
    Router::new()
        .route("/provider", post(set_provider))
        .with_state(Setter { person_uid, env_path, supervised, gateway: gateway.trim().trim_end_matches('/').to_string() })
}

/// E.PRIV1's `/consent`, kept for a path that can tell the person from their programs (yantrik-os-dc's
/// option (c), person presence). NOT served on the socket (E.SOCK2).
pub fn consent_router(person_uid: u32, env_path: PathBuf, supervised: bool) -> Router {
    Router::new()
        .route("/consent", post(set_consent))
        .with_state(Setter { person_uid, env_path, supervised, gateway: GATEWAY_URL.to_string() })
}

/// E.SOCK2: what the socket may set. A gateway offer at exactly `gateway`, or the gateway's revert.
fn socket_allows(req: &ProviderSet, gateway: &str) -> Result<(), String> {
    let provider = req.provider.trim();
    let from_gateway = provider == GATEWAY || (provider.is_empty() && req.source.as_deref() == Some(GATEWAY));
    if !from_gateway {
        return Err("from the desktop this Mind takes only Yantrik models; other models, keys and a model on your own hardware are set in its own first-run chat".into());
    }
    if req.remove {
        return Ok(());
    }
    let url = req.base_url.as_deref().unwrap_or_default().trim().trim_end_matches('/');
    if url != gateway {
        return Err(format!("Yantrik models are taken only from this machine's gateway at {gateway}"));
    }
    Ok(())
}

/// E.SOCK2: which harness the gateway issued `token` to -- `Ok(None)` when the gateway has no
/// `/whoami` (404), so the token cannot be checked yet. Without this a person's process could hand
/// the Mind its OWN gateway token, and the Mind's calls would run under that harness's grant.
fn gateway_harness(gateway: &str, token: &str) -> Result<Option<String>, String> {
    // Through mind-net, like every request the Mind makes: it decides the route (loopback goes direct).
    let url = format!("{gateway}/whoami");
    let agent = mind_net::route(mind_net::builder().timeout(std::time::Duration::from_secs(3)), &url);
    match agent.get(&url).set("Authorization", &format!("Bearer {token}")).call() {
        Ok(resp) => {
            let v: serde_json::Value = resp.into_json().map_err(|e| format!("the gateway's whoami answer could not be read: {e}"))?;
            match v["harness"].as_str() {
                Some(h) if !h.trim().is_empty() => Ok(Some(h.trim().to_string())),
                _ => Err("the gateway's whoami answer names no harness".into()),
            }
        }
        Err(ureq::Error::Status(404, _)) => Ok(None),
        Err(ureq::Error::Status(code, _)) => Err(format!("the gateway did not vouch for this token (status {code})")),
        Err(e) => Err(format!("the gateway could not be asked about this token: {e}")),
    }
}

/// E.PRIV1: the person's AI-accounts Share choices, mirrored by the OS shell (yantrik-os, from its
/// own in-memory state, on the person's act -- never from a file a harness could write). A full
/// snapshot each time; `share` is the RESOLVED choice (consent.rs: every account of that provider
/// resolves to Share).
#[derive(Debug, Clone, Deserialize)]
pub struct ConsentSet {
    #[serde(default)]
    pub source: Option<String>,
    pub accounts: Vec<AccountConsent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AccountConsent {
    pub provider: String,
    /// Required: a snapshot that does not say is not consent.
    pub share: bool,
}

/// E.PRIV1: the OS catalogue's provider ids as this Mind names them, for the clouds it runs. Any
/// other entry of the private list (a local lane, a LAN name, the gateway) is not the shell's to
/// set and is kept as it is.
const CONSENT_CLOUDS: [(&str, &str); 9] = [
    ("nanogpt", "nanogpt"),
    ("ollama-cloud", "ollama-cloud"),
    ("minimax", "minimax"),
    ("groq", "groq"),
    ("openrouter", "openrouter"),
    ("anthropic", "anthropic"),
    ("qwen", "qwen"),
    ("nvidia-nim", "nim"),
    ("nim", "nim"),
];

/// E.PRIV1: `YM_PRIVATE_PROVIDERS` after a snapshot: the non-cloud entries as they were, then the
/// clouds the person shares, in the snapshot's order.
pub fn consented_list(existing: &str, consent: &ConsentSet) -> (String, Vec<String>) {
    let current = existing
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("YM_PRIVATE_PROVIDERS="))
        .unwrap_or_default();
    // E.PRIV1b: an entry is the shell's when its PROVIDER names a managed cloud -- a bare id, a
    // provider:model spec from an earlier consent (VM 520: `ollama-cloud:deepseek-v4.1-flash`), or
    // the catalogue alias `ollama`. The private gate matches entries as substrings of the route
    // label, so any of them left behind would keep clearing a cloud the person stopped sharing with.
    let is_cloud = |x: &str| {
        let provider = x.split(':').next().unwrap_or("").trim().to_ascii_lowercase();
        provider == "ollama" || CONSENT_CLOUDS.iter().any(|(_, mind)| *mind == provider)
    };
    let mut items: Vec<String> = current
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty() && !is_cloud(x))
        .collect();
    let mut shared = Vec::new();
    for a in &consent.accounts {
        let id = a.provider.trim().to_ascii_lowercase();
        let Some((_, mind)) = CONSENT_CLOUDS.iter().find(|(os, _)| *os == id) else {
            continue;
        };
        if a.share && !shared.iter().any(|s: &String| s == mind) {
            shared.push(mind.to_string());
        }
    }
    items.extend(shared.iter().cloned());
    (items.join(","), shared)
}

async fn set_consent(
    State(s): State<Setter>,
    ext: axum::http::Extensions,
    Json(req): Json<ConsentSet>,
) -> (StatusCode, Json<serde_json::Value>) {
    let peer = ext.get::<ConnectInfo<PeerUid>>().and_then(|ConnectInfo(p)| p.0);
    if peer != Some(s.person_uid) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "saved": false, "error": "only the person may set what this Mind may share" })),
        );
    }
    let existing = std::fs::read_to_string(&s.env_path)
        .unwrap_or_else(|_| "# Yantrik Mind's own settings. Yantrik OS never reads this file.\n".to_string());
    let (list, shared) = consented_list(&existing, &req);
    let body = crate::setup::upsert_env_line(&existing, "YM_PRIVATE_PROVIDERS", &list);
    if let Err(e) = crate::setup::write_env_600(&s.env_path.display().to_string(), &body) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "saved": false, "error": format!("could not save: {e}") })));
    }
    eprintln!("[consent] the person's sharing, from {}: {}", req.source.as_deref().unwrap_or("the desktop"), if shared.is_empty() { "none".to_string() } else { shared.join(", ") });
    if s.supervised {
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            std::process::exit(0);
        });
    }
    (StatusCode::OK, Json(serde_json::json!({ "saved": true, "shared": shared, "restarting": s.supervised })))
}

async fn set_provider(
    State(s): State<Setter>,
    ext: axum::http::Extensions,
    Json(req): Json<ProviderSet>,
) -> (StatusCode, Json<serde_json::Value>) {
    // The connection's peer uid, set for the person's socket only (E.SOCK1); absent anywhere else.
    let peer = ext.get::<ConnectInfo<PeerUid>>().and_then(|ConnectInfo(p)| p.0);
    if peer != Some(s.person_uid) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "saved": false, "error": "only the person may set this Mind's model" })),
        );
    }
    // E.SOCK2: only what the person already chose, and only a token the gateway issued to this Mind.
    if let Err(e) = socket_allows(&req, &s.gateway) {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "saved": false, "error": e })));
    }
    if !req.remove {
        let (gateway, token) = (s.gateway.clone(), req.key.clone().unwrap_or_default());
        let whose = tokio::task::spawn_blocking(move || gateway_harness(&gateway, &token)).await.unwrap_or_else(|e| Err(e.to_string()));
        match whose {
            Ok(Some(h)) if h == "mind" => {}
            Ok(Some(h)) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "saved": false, "error": format!("that gateway token was issued to {h}, not to this Mind") })),
                )
            }
            // E.SOCK2b: the socket exists only beside a gateway that has whoami (yantrik-os #783);
            // a 404 means something else answers on its port.
            Ok(None) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "saved": false, "error": "the desktop is older than this Mind: its gateway cannot say whose token this is" })),
                )
            }
            Err(e) => return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "saved": false, "error": e }))),
        }
    }
    match apply(&s.env_path, &req) {
        Ok(saved) => {
            eprintln!("[provider] the person set this Mind's model: {} {}", saved.provider, saved.model.as_deref().unwrap_or("(its default)"));
            if s.supervised {
                // Reply first, then restart with the new model (the unit starts it again).
                tokio::spawn(async {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    std::process::exit(0);
                });
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({ "saved": true, "provider": saved.provider, "model": saved.model, "restarting": s.supervised })),
            )
        }
        Err(e) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "saved": false, "error": e }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ym-prov-{}-{name}.env", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }
    fn set(provider: &str, base_url: Option<&str>, model: Option<&str>, key: Option<&str>, private: bool) -> ProviderSet {
        ProviderSet {
            provider: provider.into(),
            base_url: base_url.map(str::to_string),
            model: model.map(str::to_string),
            key: key.map(str::to_string),
            private_context: private,
            ..Default::default()
        }
    }

    #[test]
    fn the_setting_lands_in_the_right_lines_and_keeps_the_rest() {
        let p = tmp("ok", "YM_DB=/x/mind.db\nYM_PRIVATE_PROVIDERS=ollama-local\n");
        apply(&p, &set("nanogpt", None, Some("deepseek/deepseek-v4-pro"), Some("sk-abcdef123456"), true)).unwrap();
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_DB=/x/mind.db\n"), "{env}");
        assert!(env.contains("NANOGPT_KEY=sk-abcdef123456\n"), "{env}");
        assert!(env.contains("YM_MODEL=deepseek/deepseek-v4-pro\n"), "{env}");
        assert!(env.contains("YM_PRIVATE_PROVIDERS=ollama-local,nanogpt\n"), "consent adds the provider: {env}");
        apply(&p, &set("nanogpt", None, None, Some("sk-abcdef123456"), false)).unwrap();
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_PRIVATE_PROVIDERS=ollama-local\n"), "no consent removes it: {env}");
        apply(&p, &set("ollama", Some("192.168.4.35"), Some("qwen3.5:9b"), None, false)).unwrap();
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_LOCAL_OLLAMA_URL=http://192.168.4.35:11434\n") && env.contains("YM_LOCAL_OLLAMA_MODEL=qwen3.5:9b\n"), "{env}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn nothing_can_add_a_line_and_a_refusal_changes_nothing() {
        let before = "YM_DB=/x/mind.db\n";
        let p = tmp("inject", before);
        let refused = [
            set("nanogpt", None, None, Some("sk-abcdef123456\nYM_X=1"), false),
            set("nanogpt", None, Some("m\nYM_X=1"), Some("sk-abcdef123456"), false),
            set("nanogpt", None, None, None, false),
            set("nanogpt", Some("https://evil.example"), None, Some("sk-abcdef123456"), false),
            set("openai", None, None, Some("sk-abcdef123456"), false),
            set("ollama", None, Some("m"), None, false),
        ];
        for r in &refused {
            let e = apply(&p, r).unwrap_err();
            assert!(!e.contains("sk-abcdef"), "an error quoted the key: {e}");
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before, "a refused setting changed the file");
        let _ = std::fs::remove_file(&p);
    }

    /// On unix a new settings file is the owner's alone from its first byte (it holds keys).
    #[cfg(unix)]
    #[test]
    fn a_new_settings_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let p = std::env::temp_dir().join(format!("ym-prov-{}-mode.env", std::process::id()));
        let _ = std::fs::remove_file(&p);
        apply(&p, &set("minimax", None, None, Some("mm-secret-key-1234"), false)).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_file(&p);
    }

    /// Only the person's uid is served, and no reply carries the key.
    #[tokio::test]
    async fn only_the_person_sets_it_and_the_key_is_never_echoed() {
        use tower::ServiceExt;
        let p = tmp("route", "");
        let call = |uid: Option<u32>| {
            let app = router(1000, p.clone(), false);
            async move {
                let mut req = axum::http::Request::builder()
                    .method("POST")
                    .uri("/provider")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(r#"{"provider":"minimax","key":"mm-secret-key-1234","private_context":false}"#))
                    .unwrap();
                req.extensions_mut().insert(ConnectInfo(PeerUid(uid)));
                let res = app.oneshot(req).await.unwrap();
                let status = res.status();
                let body = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
                (status, String::from_utf8_lossy(&body).into_owned())
            }
        };
        let (status, body) = call(Some(1001)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "", "another account's request changed the file");
        // E.SOCK2: even from the person's uid the socket sets no cloud key -- any of their
        // processes could have sent it.
        let (status, body) = call(Some(1000)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(!body.contains("mm-secret"), "the reply carried the key: {body}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "", "a cloud key reached the file through the socket");
        let _ = std::fs::remove_file(&p);
    }
}

/// E.PROV2: "Use Yantrik models" from the OS gateway (yantrik-os #675) -- loopback only, taken and
/// put back without disturbing anything else the person set.
#[cfg(test)]
mod gateway_tests {
    use super::*;

    fn tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ym-gw-{}-{name}.env", std::process::id()));
        std::fs::write(&p, body).unwrap();
        let _ = std::fs::remove_file(backup_path(&p));
        p
    }

    /// The body the OS sends (provider_handoff/mind.rs offer_body, with #675's `provider` fix).
    fn offer(base_url: &str, key: &str, private: bool) -> ProviderSet {
        serde_json::from_value(serde_json::json!({
            "source": "yantrik-gateway", "provider": "yantrik-gateway", "name": "Yantrik models",
            "base_url": base_url, "model": "picked", "api_key": key, "private_context": private
        }))
        .unwrap()
    }

    fn revert() -> ProviderSet {
        serde_json::from_value(serde_json::json!({ "source": "yantrik-gateway", "remove": true })).unwrap()
    }

    #[test]
    fn only_this_machines_gateway_is_taken_and_a_refusal_changes_nothing() {
        let before = "YM_DB=/x/mind.db\nYM_PRIMARY_BRAIN=nanogpt:deepseek\n";
        let p = tmp("refuse", before);
        for url in ["https://127.0.0.1:7460/v1", "http://localhost:7460/v1", "http://192.168.4.35:7460/v1", "http://evil.example/v1", ""] {
            let e = apply(&p, &offer(url, "ygw-0123456789abcdef", true)).unwrap_err();
            assert!(!e.contains("ygw-"), "an error quoted the token: {e}");
        }
        assert!(apply(&p, &offer("http://127.0.0.1:7460/v1", "short", true)).is_err());
        assert!(apply(&p, &offer("http://127.0.0.1:7460/v1", "ygw-012345\nYM_X=1", true)).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before, "a refused offer changed the file");
        assert!(!backup_path(&p).exists(), "a refused offer left a backup");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_take_writes_the_gateway_as_the_brain_and_consent_follows_the_flag() {
        let p = tmp("take", "YM_DB=/x/mind.db\nYM_PRIVATE_PROVIDERS=ollama-local\n");
        let saved = apply(&p, &offer("http://127.0.0.1:7460/v1/", "ygw-0123456789abcdef", true)).unwrap();
        assert_eq!((saved.provider.as_str(), saved.model.as_deref()), ("yantrik-gateway", Some("picked")));
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_GATEWAY_URL=http://127.0.0.1:7460/v1\n"), "{env}");
        assert!(env.contains("YM_GATEWAY_KEY=ygw-0123456789abcdef\n"), "{env}");
        assert!(env.contains("YM_PRIMARY_BRAIN=yantrik-gateway:picked\n"), "{env}");
        assert!(env.contains("YM_PRIVATE_PROVIDERS=ollama-local,yantrik-gateway\n"), "delegated consent: {env}");
        assert!(!env.contains("YM_LOCAL_OLLAMA_URL"), "the gateway is not the owned-hardware lane: {env}");
        apply(&p, &offer("http://127.0.0.1:7460/v1", "ygw-0123456789abcdef", false)).unwrap();
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_PRIVATE_PROVIDERS=ollama-local\n"), "no consent removes it: {env}");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(backup_path(&p));
    }

    #[test]
    fn take_then_put_back_restores_the_file_byte_for_byte() {
        for (name, before) in [
            ("had", "YM_DB=/x/mind.db\nYM_PRIMARY_BRAIN=nanogpt:deepseek\nNANOGPT_KEY=sk-abcdef123456\nYM_PRIVATE_PROVIDERS=nanogpt\nYM_TZ=x\n"),
            ("hadnt", "YM_DB=/x/mind.db\nNANOGPT_KEY=sk-abcdef123456\n"),
        ] {
            let p = tmp(name, before);
            apply(&p, &offer("http://127.0.0.1:7460/v1", "ygw-0123456789abcdef", true)).unwrap();
            assert_ne!(std::fs::read_to_string(&p).unwrap(), before);
            assert!(backup_path(&p).exists(), "{name}: no backup kept");
            let saved = apply(&p, &revert()).unwrap();
            assert_eq!(saved.provider, "yantrik-gateway");
            assert_eq!(std::fs::read_to_string(&p).unwrap(), before, "{name}: not restored");
            assert!(!backup_path(&p).exists(), "{name}: the backup outlived the put-back");
            let _ = std::fs::remove_file(&p);
        }
    }

    #[test]
    fn a_second_take_keeps_the_first_backup() {
        let before = "YM_PRIMARY_BRAIN=nanogpt:deepseek\nYM_PRIVATE_PROVIDERS=nanogpt\n";
        let p = tmp("twice", before);
        apply(&p, &offer("http://127.0.0.1:7460/v1", "ygw-first-token-0001", true)).unwrap();
        apply(&p, &offer("http://127.0.0.1:7460/v1", "ygw-second-token-002", false)).unwrap();
        let env = std::fs::read_to_string(&p).unwrap();
        assert!(env.contains("YM_GATEWAY_KEY=ygw-second-token-002\n") && !env.contains("ygw-first"), "the new token replaces the old: {env}");
        apply(&p, &revert()).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before, "the brain the person had before the FIRST take");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn a_put_back_without_a_take_drops_only_the_gateway() {
        let p = tmp("bare", "YM_PRIMARY_BRAIN=yantrik-gateway:picked\nYM_GATEWAY_URL=http://127.0.0.1:7460/v1\nYM_PRIVATE_PROVIDERS=nanogpt,yantrik-gateway\n");
        apply(&p, &revert()).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "YM_PRIVATE_PROVIDERS=nanogpt\n");
        assert!(apply(&p, &ProviderSet { provider: "nanogpt".into(), remove: true, ..Default::default() }).is_err(), "only the gateway is put back");
        let _ = std::fs::remove_file(&p);
    }

    /// The real route takes the OS's exact body from the person's uid, never echoes the token, and
    /// the brain built from what it wrote calls the gateway with that token and the model `picked`.
    #[tokio::test]
    async fn the_route_takes_the_offer_and_the_brain_calls_the_gateway_with_its_token() {
        use std::io::{BufRead, BufReader, Read, Write};
        use tower::ServiceExt;
        // A fake gateway: records the request head and body, answers one OpenAI-shaped reply.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let log = seen.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let (mut head, mut len) = (String::new(), 0usize);
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 {
                        break;
                    }
                    if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if line.trim().is_empty() {
                        break;
                    }
                    head.push_str(&line);
                }
                // E.SOCK2: the gateway vouches that the token is this Mind's.
                let reply = if head.starts_with("GET /v1/whoami") {
                    r#"{"harness":"mind"}"#.to_string()
                } else {
                    let mut body = vec![0u8; len];
                    reader.read_exact(&mut body).unwrap();
                    *log.lock().unwrap() = format!("{head}\n{}", String::from_utf8_lossy(&body));
                    r#"{"choices":[{"message":{"content":"from the gateway"},"finish_reason":"stop"}]}"#.to_string()
                };
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len());
            }
        });
        let p = tmp("route", "YM_DB=/x/mind.db\n");
        let body = serde_json::json!({
            "source": "yantrik-gateway", "provider": "yantrik-gateway", "name": "Yantrik models",
            "base_url": format!("http://127.0.0.1:{port}/v1"), "model": "picked",
            "api_key": "ygw-route-token-0123456789", "private_context": true
        });
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri("/provider")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(PeerUid(Some(1000))));
        let res = router_at(1000, p.clone(), false, &format!("http://127.0.0.1:{port}/v1")).oneshot(req).await.unwrap();
        let status = res.status();
        let reply = String::from_utf8_lossy(&axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap()).into_owned();
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert!(!reply.contains("ygw-route"), "the reply carried the token: {reply}");
        let env = std::fs::read_to_string(&p).unwrap();
        let get = |k: &str| env.lines().find_map(|l| l.strip_prefix(&format!("{k}=")).map(str::to_string));
        let spec = get("YM_PRIMARY_BRAIN").expect("the brain line");
        let (provider, model) = mind_inference::split_spec(&spec);
        assert_eq!(provider, mind_inference::GATEWAY_PROVIDER);
        let brain = mind_inference::gateway_backend_from(model, &get).expect("the brain builds from what was written");
        let said = tokio::task::spawn_blocking(move || brain.chat(&[yantrik_ml::ChatMessage::user("hi")], &yantrik_ml::GenerationConfig::default(), None))
            .await
            .unwrap()
            .expect("the gateway answered");
        assert_eq!(said.text, "from the gateway");
        let seen = seen.lock().unwrap().clone();
        assert!(seen.to_ascii_lowercase().contains("authorization: bearer ygw-route-token-0123456789"), "{seen}");
        assert!(seen.contains("\"model\":\"picked\""), "{seen}");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(backup_path(&p));
    }
}

/// E.PRIV1: the person's AI-accounts sharing, mirrored into the Mind's private list.
#[cfg(test)]
mod consent_tests {
    use super::*;
    use tower::ServiceExt;

    /// 22's pinned body (11 Oct).
    const PINNED: &str = r#"{"source":"yantrik-os","accounts":[{"provider":"ollama-cloud","share":true},{"provider":"nanogpt","share":false}]}"#;

    fn tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ym-consent-{}-{name}.env", std::process::id()));
        std::fs::write(&p, body).unwrap();
        p
    }

    async fn post(p: &Path, uid: Option<u32>, body: &str) -> (StatusCode, String) {
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri("/consent")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(PeerUid(uid)));
        // E.SOCK2: /consent is not on the socket; its handler is tested where it is kept.
        let res = consent_router(1000, p.to_path_buf(), false).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn list(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap().lines().find_map(|l| l.strip_prefix("YM_PRIVATE_PROVIDERS=").map(str::to_string)).unwrap_or_default()
    }

    #[tokio::test]
    async fn the_pinned_snapshot_shares_what_the_person_shares_and_keeps_the_rest() {
        let p = tmp("pinned", "YM_DB=/x/mind.db\nYM_PRIVATE_PROVIDERS=ollama-local,nanogpt,yantrik-gateway,192.168.4.35\n");
        let (status, body) = post(&p, Some(1000), PINNED).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body.contains(r#""shared":["ollama-cloud"]"#), "{body}");
        assert_eq!(list(&p), "ollama-local,yantrik-gateway,192.168.4.35,ollama-cloud", "nanogpt out, the rest kept");
        assert!(std::fs::read_to_string(&p).unwrap().contains("YM_DB=/x/mind.db\n"));
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn ids_map_unknown_ones_are_ignored_and_a_later_snapshot_takes_one_out() {
        let p = tmp("ids", "");
        let snap = r#"{"source":"yantrik-os","accounts":[{"provider":"nvidia-nim","share":true},{"provider":"some-new-cloud","share":true},{"provider":"Groq","share":true}],"future":1}"#;
        assert_eq!(post(&p, Some(1000), snap).await.0, StatusCode::OK);
        assert_eq!(list(&p), "nim,groq");
        let later = r#"{"source":"yantrik-os","accounts":[{"provider":"nvidia-nim","share":false},{"provider":"groq","share":true}]}"#;
        assert_eq!(post(&p, Some(1000), later).await.0, StatusCode::OK);
        assert_eq!(list(&p), "groq", "share:false takes it out");
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn a_snapshot_without_share_or_from_another_account_changes_nothing() {
        let before = "YM_PRIVATE_PROVIDERS=nanogpt\n";
        let p = tmp("refused", before);
        let (status, _) = post(&p, Some(1000), r#"{"accounts":[{"provider":"ollama-cloud"}]}"#).await;
        assert!(status.is_client_error(), "a snapshot that does not say is not consent: {status}");
        let (status, _) = post(&p, Some(1001), PINNED).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        let (status, _) = post(&p, None, PINNED).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "no peer uid (not the person's socket)");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), before);
        let _ = std::fs::remove_file(&p);
    }
}

/// E.PRIV1b: a withdrawal reaches every entry that clears that cloud.
#[cfg(test)]
mod consent_spec_tests {
    use super::*;

    fn snap(share: bool) -> ConsentSet {
        serde_json::from_value(serde_json::json!({ "source": "yantrik-os", "accounts": [{ "provider": "ollama-cloud", "share": share }] })).unwrap()
    }

    /// VM 520's list (11 Oct) plus the catalogue alias: a provider:model spec and `ollama` are the
    /// shell's too.
    const BEFORE: &str = "YM_PRIVATE_PROVIDERS=ollama-local,ollama-cloud:deepseek-v4.1-flash,ollama,yantrik-gateway\n";

    #[test]
    fn a_withdrawal_removes_spec_and_alias_entries() {
        let (list, shared) = consented_list(BEFORE, &snap(false));
        assert_eq!(list, "ollama-local,yantrik-gateway");
        assert!(shared.is_empty());
        let household = mind_inference::DEFAULT_HOUSEHOLD;
        assert!(
            !mind_inference::scope_allows(mind_inference::PrivacyScope::Private, "ollama-cloud:deepseek-v4.1-flash", household, &list),
            "the withdrawn cloud is still cleared: {list}"
        );
        assert!(mind_inference::scope_allows(mind_inference::PrivacyScope::Private, "ollama-cloud:deepseek-v4.1-flash", household, "ollama-cloud:deepseek-v4.1-flash"), "the control: the old list did clear it");
    }

    #[test]
    fn a_share_replaces_them_with_the_bare_id() {
        let (list, shared) = consented_list(BEFORE, &snap(true));
        assert_eq!(list, "ollama-local,yantrik-gateway,ollama-cloud");
        assert_eq!(shared, vec!["ollama-cloud".to_string()]);
    }

    #[test]
    fn the_banner_never_says_escalate() {
        for cleared in [true, false] {
            let b = crate::no_local_lane_banner("ollama-cloud:deepseek-v4.1-flash", cleared);
            assert!(!b.contains("escalat"), "{b}");
            assert!(b.contains("ollama-cloud:deepseek-v4.1-flash"), "{b}");
        }
        assert!(crate::no_local_lane_banner("x", false).contains("refused"));
    }
}

/// E.SOCK2: the person's socket can change only what the person already chose.
#[cfg(test)]
mod socket_tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use tower::ServiceExt;

    /// A gateway that answers only `GET /v1/whoami`: `Some(h)` → `{"harness": h}`, `None` → 404.
    fn fake_gateway(harness: Option<&'static str>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                        break;
                    }
                }
                let (status, body) = match harness {
                    Some(h) => ("200 OK", format!(r#"{{"harness":"{h}"}}"#)),
                    None => ("404 Not Found", "{}".to_string()),
                };
                let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            }
        });
        format!("http://127.0.0.1:{port}/v1")
    }

    fn tmp(name: &str, body: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ym-sock2-{}-{name}.env", std::process::id()));
        std::fs::write(&p, body).unwrap();
        let _ = std::fs::remove_file(backup_path(&p));
        p
    }

    async fn post(gateway: &str, p: &Path, uri: &str, body: serde_json::Value) -> (StatusCode, String) {
        let mut req = axum::http::Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap();
        req.extensions_mut().insert(ConnectInfo(PeerUid(Some(1000))));
        let res = router_at(1000, p.to_path_buf(), false, gateway).oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 16).await.unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    fn offer(base_url: &str) -> serde_json::Value {
        serde_json::json!({ "source": "yantrik-gateway", "provider": "yantrik-gateway", "base_url": base_url, "model": "picked",
                            "api_key": "ygw-socket-token-0123456789", "private_context": true })
    }

    const BEFORE: &str = "YM_DB=/x/mind.db\nYM_PRIMARY_BRAIN=nanogpt:deepseek\nYM_PRIVATE_PROVIDERS=ollama-local\n";

    #[tokio::test]
    async fn the_socket_refuses_a_private_lane_a_key_and_any_other_gateway_address() {
        let gw = fake_gateway(Some("mind"));
        let port: u16 = gw.trim_start_matches("http://127.0.0.1:").trim_end_matches("/v1").parse().unwrap();
        let p = tmp("refuse", BEFORE);
        for body in [
            serde_json::json!({ "provider": "ollama", "base_url": "http://192.168.4.66:11434", "model": "qwen3.5:9b" }),
            serde_json::json!({ "provider": "nanogpt", "key": "sk-abcdef1234567890" }),
            // At the gateway's own address but not the gateway: only the gateway-only rule refuses
            // this, and without it the gateway address would become the private lane.
            serde_json::json!({ "provider": "ollama", "base_url": gw.clone(), "model": "qwen3.5:9b" }),
            offer(&format!("http://127.0.0.2:{port}/v1")),
            offer(&format!("http://127.0.0.1:{}/v1", port + 1)),
        ] {
            let (status, reply) = post(&gw, &p, "/provider", body.clone()).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body} -> {reply}");
            assert!(!reply.contains("sk-abcdef") && !reply.contains("ygw-socket"), "{reply}");
        }
        assert_eq!(std::fs::read_to_string(&p).unwrap(), BEFORE, "a refused offer changed the file");
        let (status, _) = post(&gw, &p, "/consent", serde_json::json!({ "source": "yantrik-os", "accounts": [{ "provider": "nanogpt", "share": true }] })).await;
        assert!(status == StatusCode::NOT_FOUND || status == StatusCode::METHOD_NOT_ALLOWED, "/consent is on the socket: {status}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), BEFORE, "a consent reached the file through the socket");
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn the_gateway_at_its_own_address_is_taken_and_put_back() {
        let gw = fake_gateway(Some("mind"));
        let p = tmp("take", BEFORE);
        let (status, reply) = post(&gw, &p, "/provider", offer(&format!("{gw}/"))).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert!(std::fs::read_to_string(&p).unwrap().contains("YM_PRIMARY_BRAIN=yantrik-gateway:picked\n"));
        let (status, reply) = post(&gw, &p, "/provider", serde_json::json!({ "source": "yantrik-gateway", "remove": true })).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), BEFORE);
        let _ = std::fs::remove_file(&p);
    }

    #[tokio::test]
    async fn a_token_the_gateway_issued_to_another_harness_is_refused() {
        let p = tmp("whoami", BEFORE);
        let hermes = fake_gateway(Some("hermes"));
        let (status, reply) = post(&hermes, &p, "/provider", offer(&hermes)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
        assert!(reply.contains("hermes"), "{reply}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), BEFORE);
        // E.SOCK2b: a gateway with no whoami is not this OS's gateway; refused, nothing changes.
        let older = fake_gateway(None);
        let (status, reply) = post(&older, &p, "/provider", offer(&older)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{reply}");
        assert!(reply.contains("older than this Mind"), "{reply}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), BEFORE);
        let mind = fake_gateway(Some("mind"));
        let (status, reply) = post(&mind, &p, "/provider", offer(&mind)).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(backup_path(&p));
    }
}
