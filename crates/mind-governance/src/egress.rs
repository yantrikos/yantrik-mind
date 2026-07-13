//! ARCH-3A — outbound tool mediation + credential tripwire.
//!
//! HONEST SCOPE (read this before trusting it — sol redteam verdict rid 019f5d48):
//! This module is the MEDIATION FOUNDATION for the egress kernel, NOT a confidentiality guard.
//! It does two things well and names the rest as out of scope:
//!   1. **Registry + reject-unknown.** Every tool that can reach an external service is explicitly
//!      classified `Local` (args never leave the process) or `External { connector }`. An
//!      unregistered tool is DENIED loudly (deny-by-default *registration*, not "scan then allow").
//!   2. **Mandatory broker → permit.** An `External` dispatch must first obtain an `EgressPermit`
//!      from `EgressBroker::authorize`. The permit cannot be constructed outside this module, so a
//!      call site that forgets to authorize simply cannot call a permit-typed dispatch. Every
//!      decision is written to a keyed-HMAC, hash-chained receipt (privacy-preserving: no raw args).
//!      A **credential tripwire** (secret-marker scan over the whole canonical arg tree, keys
//!      included) denies the categorically-bad case as defense in depth.
//!
//! Enforcement-wiring caveat (slice 1): the broker's `authorize` denies an unregistered tool when
//! asked, but the CURRENT call sites (agent loop, recipe host, mail fast-path) invoke it only for
//! tools that classify as a recognized `External` connector. The ~150-arm agent tool table is NOT
//! yet audited for comprehensive coverage, and a tool absent from the registry passes through the
//! gate rather than being denied. Comprehensive, deny-by-default coverage = move the gate to the
//! transport layer + audit the table (slice 2). This module is named accordingly (ARCH-3A).
//!
//! What this does NOT do yet (explicitly, so no one over-trusts it — all slice 2+):
//!   - It does NOT stop ordinary private household facts from leaving in a tool arg (the dominant
//!     threat). That needs per-field data-class manifests + declassification/consent.
//!   - It does NOT gate `inference.chat` content to a (possibly remote) LLM backend — the largest
//!     memory egress. The backend is *inventoried* here as a destination; its content is not filtered.
//!   - It does NOT sandbox connector subprocesses or the coder job's own network (a guarded enqueue
//!     does not guard what the job later sends). Coder net must be disabled or the coder excluded.
//!   - The credential scan misses encoded/split/paraphrased secrets — it is a tripwire, not a proof.
//!   - Permits are required at the CURRENT call sites; making the low-level transports module-private
//!     so a permit is *structurally* unforgeable at the wire is slice 2.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use mind_types::contains_secret;

/// The connector an external tool talks to. This is the DECLARED class; the model-controlled
/// effective target (host/recipient/repo/mcp-tool) is captured separately in `EgressRequest.target`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Connector {
    Imap,            // mail search/inbox
    Web,             // http fetch / web search (destination further constrained by SSRF guard)
    Github,          // github.com API
    HomeAssistant,   // local smart-home hub
    ThirdParty,      // translate / wikipedia / crypto / stock public APIs
    LlmApi,          // a REMOTE inference backend (inventory only — content NOT filtered here)
    Mcp(String),     // an MCP server, by configured id (read-only or not, always External)
    Coder,           // the agentic coder subprocess (its OWN network is NOT mediated — see scope note)
}

impl Connector {
    pub fn label(&self) -> String {
        match self {
            Connector::Imap => "imap".into(),
            Connector::Web => "web".into(),
            Connector::Github => "github".into(),
            Connector::HomeAssistant => "home-assistant".into(),
            Connector::ThirdParty => "third-party".into(),
            Connector::LlmApi => "llm-api".into(),
            Connector::Mcp(s) => format!("mcp:{s}"),
            Connector::Coder => "coder".into(),
        }
    }
}

/// A tool is either local (no bytes leave the process) or external (args reach a connector).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressClass {
    Local,
    External(Connector),
}

/// The explicit registry: classify a tool by name. `None` = UNREGISTERED → the broker denies it
/// (deny-by-default registration). Keep this exhaustive; a new tool must be added here on purpose.
/// MCP tools (`mcp.<server>.<tool>`) are ALWAYS external, keyed to their server id — a "read-only"
/// hint does not make them local (an MCP server can proxy arbitrary network).
pub fn classify(tool: &str) -> Option<EgressClass> {
    if let Some(rest) = tool.strip_prefix("mcp.") {
        let server = rest.split('.').next().unwrap_or(rest).to_string();
        return Some(EgressClass::External(Connector::Mcp(server)));
    }
    use Connector::*;
    let ext = |c: Connector| Some(EgressClass::External(c));
    match tool {
        // ── Local: computed in-process, or a memory op already gated by ARCH-1 (no external bytes) ──
        "now" | "date" | "datetime" | "time" | "getcurrentdatetime" => Some(EgressClass::Local),
        "calc" | "calculate" | "math" => Some(EgressClass::Local),
        "recall" | "remember" | "due_tasks" => Some(EgressClass::Local), // memory boundary is ARCH-1's job, not egress
        // ── External: model-authored args reach a connector ──
        "mail_search" | "mailsearch" | "search_mail" | "findmail" => ext(Imap),
        "inbox" | "mail" | "check_mail" => ext(Imap),
        "web_fetch" | "fetch" | "web" => ext(Web),
        "search" | "web_search" | "google" | "ddg" | "research" => ext(Web),
        "github" | "github_repo_items" | "github_notifications" => ext(Github),
        "home" | "home_status" | "house" | "smart_home" => ext(HomeAssistant),
        "translate" | "tr" | "wikipedia" | "wiki" | "crypto" | "coin" | "stock" | "ticker" | "weather" | "wx" => ext(ThirdParty),
        "code" | "coder" => ext(Coder),
        _ => None,
    }
}

/// A request to send model-authored data outward. `principal` is the effective speaker (never None —
/// the identity-blind era is over). `args_canonical` is the canonicalized JSON the tool will send.
pub struct EgressRequest<'a> {
    pub principal: &'a str,
    pub tool: &'a str,
    /// The model-controlled effective target where known (host/recipient/repo/query) — audit + future
    /// per-destination policy. Slice 1 records it; it does not yet enforce a destination allowlist.
    pub target: Option<&'a str>,
    /// Where the call came from (audit): "agent_tool" | "recipe_host" | "mail_fastpath" | "cli".
    pub source: &'a str,
    /// The full argument tree, canonicalized to a stable string (keys sorted). The tripwire scans
    /// this whole string — values AND keys.
    pub args_canonical: &'a str,
}

/// The broker's answer. `Allow(EgressPermit)` is the ONLY way to obtain a permit; a dispatch that
/// requires `&EgressPermit` therefore cannot run without a broker decision.
pub enum EgressDecision {
    Allow(EgressPermit),
    Deny(String),
}

/// An opaque authorization to perform ONE external dispatch. Constructed only inside this module
/// (private field), so no call site can forge one. Carries the connector + a trace id so the
/// dispatch outcome can be receipted against the original decision.
pub struct EgressPermit {
    connector: Connector,
    trace: String,
    // private unit field prevents struct-literal construction outside this module
    _seal: (),
}
impl EgressPermit {
    pub fn connector(&self) -> &Connector {
        &self.connector
    }
    pub fn trace(&self) -> &str {
        &self.trace
    }
}

/// Canonicalize a JSON value to a stable string: object keys sorted, so the same logical args always
/// produce the same digest and the tripwire scan is order-independent. Bounded depth to avoid a DoS.
pub fn canonicalize(v: &serde_json::Value) -> String {
    fn go(v: &serde_json::Value, out: &mut String, depth: u32) {
        if depth > 32 {
            out.push_str("…");
            return;
        }
        match v {
            serde_json::Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(k);
                    out.push(':');
                    go(&m[*k], out, depth + 1);
                }
                out.push('}');
            }
            serde_json::Value::Array(a) => {
                out.push('[');
                for (i, e) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    go(e, out, depth + 1);
                }
                out.push(']');
            }
            serde_json::Value::String(s) => out.push_str(s),
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    go(v, &mut out, 0);
    out
}

// ── HMAC-SHA256 (RFC 2104), hand-rolled over sha2 to avoid a new dependency ──
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let d = Sha256::digest(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    let mut out = [0u8; 32];
    out.copy_from_slice(&outer.finalize());
    out
}

/// One receipt line (privacy-preserving): NO raw args, only a keyed HMAC digest. `outcome` is filled
/// on completion so a decision (allow) is never mistaken for a successful transmission.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EgressReceipt {
    pub ts_ms: u64,
    pub principal: String,
    pub tool: String,
    pub connector: String,
    pub source: String,
    /// "allow" | "deny:<reason-code>"
    pub decision: String,
    /// keyed HMAC of the canonical args — equality-checkable by an auditor holding the key, but not a
    /// reusable fingerprint that leaks low-entropy content across exports.
    pub args_hmac: String,
    /// Filled on completion: "ok" | "error" | "" (decision-only, dispatch not attempted / denied).
    #[serde(default)]
    pub outcome: String,
}

#[derive(Serialize, Deserialize)]
struct ChainedLine {
    chain: String,
    record: EgressReceipt,
}

/// The egress broker: policy + audited receipts. Cheap to share behind an `Arc`.
pub struct EgressBroker {
    /// Keyed digest key (per-deployment, owner-only file) — makes receipts non-guessable across exports.
    hmac_key: Vec<u8>,
    ledger: Option<PathBuf>,
    head: Mutex<Option<String>>,
    trace_seq: std::sync::atomic::AtomicU64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl EgressBroker {
    /// Open a broker rooted at `state_dir`: the receipt ledger is `<dir>/egress_receipts.jsonl`, the
    /// HMAC key `<dir>/egress.key` (owner-only, minted once). `:memory:`-style dirs (tests) get an
    /// in-memory-only broker with a random key and no ledger file.
    pub fn open(state_dir: impl AsRef<Path>, persist: bool) -> Self {
        let dir = state_dir.as_ref();
        let (hmac_key, ledger) = if persist {
            let key_path = dir.join("egress.key");
            let key = load_or_mint_key(&key_path);
            (key, Some(dir.join("egress_receipts.jsonl")))
        } else {
            let mut k = [0u8; 32];
            let _ = getrandom::getrandom(&mut k);
            (k.to_vec(), None)
        };
        EgressBroker { hmac_key, ledger, head: Mutex::new(None), trace_seq: std::sync::atomic::AtomicU64::new(0) }
    }

    /// Authorize (or refuse) one external dispatch. Denies: an unregistered tool (deny-by-default),
    /// a Local tool routed here by mistake, or a credential marker anywhere in the canonical args.
    /// Every decision is receipted. Returns a permit ONLY on allow.
    pub fn authorize(&self, req: &EgressRequest) -> EgressDecision {
        let class = classify(req.tool);
        let (connector, deny_reason) = match class {
            None => (Connector::ThirdParty, Some("unregistered-tool".to_string())),
            Some(EgressClass::Local) => (Connector::ThirdParty, Some("local-tool-not-egress".to_string())),
            Some(EgressClass::External(c)) => {
                // Credential tripwire (defense in depth, NOT a confidentiality guarantee): scan the
                // whole canonical arg tree — values AND keys. Two passes: the RAW canonical (markers
                // like "ghp_" / "app password" carry their own punctuation/spaces), and a
                // whitespace-stripped copy (catches "g h p _ A B C" spacing evasion). We deliberately
                // do NOT use `squeeze` here — it drops the underscores/digits that ARE the marker.
                let ws_stripped: String = req.args_canonical.chars().filter(|ch| !ch.is_whitespace()).collect();
                if contains_secret(req.args_canonical) || contains_secret(&ws_stripped) {
                    (c, Some("credential-marker".to_string()))
                } else {
                    (c, None)
                }
            }
        };
        let seq = self.trace_seq.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let trace = format!("eg-{seq:x}");
        let args_hmac = {
            let mac = hmac_sha256(&self.hmac_key, req.args_canonical.as_bytes());
            format!("{:x}", HexSlice(&mac))
        };
        let decision_str = match &deny_reason {
            None => "allow".to_string(),
            Some(r) => format!("deny:{r}"),
        };
        self.write_receipt(EgressReceipt {
            ts_ms: now_ms(),
            principal: req.principal.to_string(),
            tool: req.tool.to_string(),
            connector: connector.label(),
            source: req.source.to_string(),
            decision: decision_str,
            args_hmac,
            outcome: String::new(),
        });
        match deny_reason {
            Some(r) => EgressDecision::Deny(refusal_text(&r, req.tool)),
            None => EgressDecision::Allow(EgressPermit { connector, trace, _seal: () }),
        }
    }

    /// Record the DISPATCH OUTCOME for a permit (allow != transmitted). Appends a completion receipt
    /// so the audit trail distinguishes "authorized" from "actually sent ok/error".
    pub fn record_outcome(&self, permit: &EgressPermit, principal: &str, tool: &str, ok: bool) {
        self.write_receipt(EgressReceipt {
            ts_ms: now_ms(),
            principal: principal.to_string(),
            tool: tool.to_string(),
            connector: permit.connector.label(),
            source: "outcome".to_string(),
            decision: format!("outcome:{}", permit.trace),
            args_hmac: String::new(),
            outcome: if ok { "ok".into() } else { "error".into() },
        });
    }

    fn write_receipt(&self, receipt: EgressReceipt) {
        let Some(path) = &self.ledger else { return };
        if let Err(e) = self.append_chained(path, &receipt) {
            eprintln!("[egress] receipt append failed: {e}");
        }
    }

    fn append_chained(&self, path: &Path, receipt: &EgressReceipt) -> std::io::Result<()> {
        use std::io::Write;
        let mut head = self.head.lock().unwrap_or_else(|p| p.into_inner());
        let prev = match head.clone() {
            Some(h) => h,
            None => chain_head(path).unwrap_or_else(|| "genesis".to_string()),
        };
        let record_json = serde_json::to_string(receipt).map_err(std::io::Error::other)?;
        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(record_json.as_bytes());
        let chain = format!("{:x}", HexSlice(&hasher.finalize()));
        let line = format!("{{\"chain\":\"{chain}\",\"record\":{record_json}}}\n");
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
        *head = Some(chain);
        Ok(())
    }
}

/// The user-facing refusal for a denied egress (never echoes the offending arg text — sol #7).
fn refusal_text(reason: &str, tool: &str) -> String {
    match reason {
        "credential-marker" => format!("(refused: the arguments to `{tool}` look like they contain a credential/secret — I won't send that outward)"),
        "unregistered-tool" => format!("(refused: `{tool}` isn't a registered egress tool — it can't send data outward until it's added to the egress registry)"),
        "local-tool-not-egress" => format!("(internal: `{tool}` is a local tool and shouldn't be brokered as egress)"),
        other => format!("(refused: egress policy denied `{tool}`: {other})"),
    }
}

/// The current hash-chain head, or None for a missing/empty ledger.
pub fn chain_head(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let last = content.lines().rev().find(|l| !l.trim().is_empty())?;
    let parsed: ChainedLine = serde_json::from_str(last).ok()?;
    Some(parsed.chain)
}

/// Recompute the receipt chain. Ok(n) valid records; Err(i) first bad line index.
pub fn verify_ledger(path: &Path) -> std::result::Result<usize, usize> {
    let content = std::fs::read_to_string(path).map_err(|_| 0usize)?;
    let mut prev = "genesis".to_string();
    let mut n = 0usize;
    for (i, line) in content.lines().filter(|l| !l.trim().is_empty()).enumerate() {
        let parsed: ChainedLine = serde_json::from_str(line).map_err(|_| i)?;
        let record_json = serde_json::to_string(&parsed.record).map_err(|_| i)?;
        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(record_json.as_bytes());
        let expect = format!("{:x}", HexSlice(&hasher.finalize()));
        if expect != parsed.chain {
            return Err(i);
        }
        prev = parsed.chain;
        n += 1;
    }
    Ok(n)
}

pub fn read_ledger(path: &Path) -> Vec<EgressReceipt> {
    let Ok(content) = std::fs::read_to_string(path) else { return vec![] };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ChainedLine>(l).ok())
        .map(|c| c.record)
        .collect()
}

fn load_or_mint_key(path: &Path) -> Vec<u8> {
    if let Ok(raw) = std::fs::read(path) {
        if raw.len() >= 32 {
            return raw;
        }
    }
    let mut k = [0u8; 32];
    let _ = getrandom::getrandom(&mut k);
    // owner-only mint (best-effort; a failure just means non-persistent keying this boot)
    let _ = write_key_owner_only(path, &k);
    k.to_vec()
}

fn write_key_owner_only(path: &Path, key: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(key)?;
    f.sync_all()?;
    Ok(())
}

/// Lowercase-hex formatter for a byte slice (no per-byte alloc).
struct HexSlice<'a>(&'a [u8]);
impl std::fmt::LowerHex for HexSlice<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn broker() -> EgressBroker {
        EgressBroker::open(std::env::temp_dir(), false)
    }

    #[test]
    fn registry_rejects_unknown_tools() {
        assert_eq!(classify("now"), Some(EgressClass::Local));
        assert_eq!(classify("web_search"), Some(EgressClass::External(Connector::Web)));
        assert!(matches!(classify("mcp.filesystem.read"), Some(EgressClass::External(Connector::Mcp(ref s))) if s == "filesystem"));
        assert_eq!(classify("some_new_unlisted_tool"), None, "unknown tool must be unregistered");
    }

    #[test]
    fn unregistered_tool_is_denied() {
        let b = broker();
        let req = EgressRequest { principal: "primary", tool: "totally_unknown", target: None, source: "test", args_canonical: "{q:hi}" };
        assert!(matches!(b.authorize(&req), EgressDecision::Deny(_)), "unregistered tool must be denied");
    }

    #[test]
    fn credential_marker_in_args_is_denied() {
        let b = broker();
        let args = canonicalize(&serde_json::json!({ "query": "please email ghp_ABCDEF1234567890 to bob" }));
        let req = EgressRequest { principal: "primary", tool: "web_search", target: None, source: "test", args_canonical: &args };
        match b.authorize(&req) {
            EgressDecision::Deny(msg) => {
                assert!(msg.contains("credential"), "deny reason names the tripwire");
                assert!(!msg.contains("ghp_"), "refusal must NOT echo the secret");
            }
            EgressDecision::Allow(_) => panic!("a credential marker must be denied"),
        }
    }

    #[test]
    fn credential_in_object_key_is_also_scanned() {
        // sol #7: keys, not just values.
        let b = broker();
        let args = canonicalize(&serde_json::json!({ "ghp_ABCDEF1234567890": "value" }));
        let req = EgressRequest { principal: "primary", tool: "web_search", target: None, source: "test", args_canonical: &args };
        assert!(matches!(b.authorize(&req), EgressDecision::Deny(_)), "a marker in a KEY must also be caught");
    }

    #[test]
    fn benign_external_call_is_allowed_and_permit_gates_dispatch() {
        let b = broker();
        let args = canonicalize(&serde_json::json!({ "query": "weather in Pune tomorrow" }));
        let req = EgressRequest { principal: "primary", tool: "web_search", target: Some("duckduckgo"), source: "agent_tool", args_canonical: &args };
        match b.authorize(&req) {
            EgressDecision::Allow(permit) => {
                assert_eq!(permit.connector(), &Connector::Web);
                b.record_outcome(&permit, "primary", "web_search", true);
            }
            EgressDecision::Deny(m) => panic!("benign call must be allowed: {m}"),
        }
    }

    #[test]
    fn receipts_are_hmac_chained_and_verify() {
        let dir = std::env::temp_dir().join(format!("ym_egress_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let b = EgressBroker::open(&dir, true);
        let ledger = dir.join("egress_receipts.jsonl");
        for q in ["one", "two", "three"] {
            let args = canonicalize(&serde_json::json!({ "query": q }));
            let req = EgressRequest { principal: "primary", tool: "web_search", target: None, source: "test", args_canonical: &args };
            let _ = b.authorize(&req);
        }
        let rs = read_ledger(&ledger);
        assert_eq!(rs.len(), 3);
        assert!(rs.iter().all(|r| r.decision == "allow" && !r.args_hmac.is_empty()));
        // No raw args in the receipt — only the keyed digest.
        assert!(rs.iter().all(|r| !r.args_hmac.contains("query")));
        assert_eq!(verify_ledger(&ledger), Ok(3));
        // Tamper → chain breaks.
        let content = std::fs::read_to_string(&ledger).unwrap();
        std::fs::write(&ledger, content.replacen("web_search", "evil_tool", 1)).unwrap();
        assert!(verify_ledger(&ledger).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn canonicalize_is_key_order_independent() {
        let a = canonicalize(&serde_json::json!({ "b": 1, "a": 2 }));
        let b = canonicalize(&serde_json::json!({ "a": 2, "b": 1 }));
        assert_eq!(a, b, "canonical form must be stable across key order");
    }
}
