//! ARCH-2 slice 1 — device trust: the authenticated-principal boundary in front of the control
//! server. ARCH-1 made every memory read fail-closed at the resource boundary for an
//! *authenticated principal*; this module is what authenticates that principal. A high-entropy
//! bearer token maps (hashed) to a `Device` with a `role`; the control server derives the
//! `AccessContext` from the authenticated device, never from a client-asserted header.
//!
//! Design decisions (post-sol-redteam, verdict rid 019f5d27):
//!  - **Fail-closed, one-time init.** The console operator device is minted exactly once, for a
//!    provably-new store. A store that exists but is missing/empty/corrupt/inconsistent makes the
//!    control surface UNAVAILABLE — it never auto-mints a replacement (revoke+restart must not
//!    resurrect an operator).
//!  - **Actor ≠ principal.** An operator token authenticates the *requester*; deriving the chat
//!    `AccessContext` from it is the CALLER's job (`/cli` → Operator, `/chat` → Principal). This
//!    module only says "who authenticated, and what may they do".
//!  - **Atomic durable writes.** Temp file (owner-only) → fsync → rename → fsync dir (unix). A
//!    reported pair/revoke has hit the disk; a crash cannot half-apply it.
//!  - **Redacted secret.** The raw token lives in a `Secret` whose Debug/Display never print it.
//!
//! Deliberately deferred (documented, not silently missing): WireGuard ingress, remote member
//! devices, in-flight request cancellation on revoke, hardware/vault-backed key storage, and a
//! separate `manage_devices` capability (every operator device can currently administer devices —
//! acceptable while the ONLY operator is the local console).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The on-disk store version. Bump only on a breaking schema change (load rejects unknown).
const STORE_VERSION: u32 = 1;
/// Token entropy — 256 bits from the OS CSPRNG.
const TOKEN_BYTES: usize = 32;
/// Hard cap on devices (a runaway/abuse backstop, far above any real household).
const MAX_DEVICES: usize = 256;

/// A secret string whose Debug/Display never reveal it (defeats accidental logging/tracing leaks).
#[derive(Clone)]
pub struct Secret(String);
impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}
impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(********)")
    }
}
impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("********")
    }
}

/// What a device is allowed to do. An operator device authenticates a requester who may drive the
/// operator console (`/cli`) and administer devices; a member device speaks only as its bound person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeviceRole {
    /// Full console authority. `default_person` is the scope its `/chat` turns run under (the primary
    /// unless overridden); operator authority does NOT leak into `/chat` — see `chat_scope_tag`.
    Operator { default_person: String },
    /// Speaks only as this immutable person id. `/chat` runs `Principal(Private(person))`; `/cli` is
    /// refused. A supplied speaker that differs from `person` is a 403 at the boundary, never honored.
    Member { person: String },
}

/// One paired device. `token_sha256` is the lowercase-hex SHA-256 of the raw bearer; the raw token
/// is never stored. `revoked_ms.is_some()` = permanently unusable (revocation is not reversible here).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub role: DeviceRole,
    pub token_sha256: String,
    pub created_ms: u64,
    #[serde(default)]
    pub revoked_ms: Option<u64>,
}

impl Device {
    pub fn is_active(&self) -> bool {
        self.revoked_ms.is_none()
    }
    pub fn is_operator(&self) -> bool {
        matches!(self.role, DeviceRole::Operator { .. })
    }
    /// The person id this device's `/chat` turns are scoped to (a member's bound person, or an
    /// operator's default). This is what the control server turns into `Principal(Private(..))` —
    /// an operator's `/chat` is Principal-scoped, not Operator, so a hub token can't read all memory.
    pub fn chat_person(&self) -> &str {
        match &self.role {
            DeviceRole::Operator { default_person } => default_person,
            DeviceRole::Member { person } => person,
        }
    }
}

/// The persisted document. `initialized` is the one-time-init marker: once true, a missing/empty
/// device list is a CORRUPT store (fail closed), not a virgin one (mint the console).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoreDoc {
    version: u32,
    initialized: bool,
    devices: Vec<Device>,
}

impl Default for StoreDoc {
    fn default() -> Self {
        StoreDoc { version: STORE_VERSION, initialized: false, devices: vec![] }
    }
}

/// The outcome of authenticating a bearer token: which device, and (for the caller's convenience)
/// its role/scope. `None` from `authenticate` means "no active device for this token" —
/// indistinguishable across "unknown token" and "revoked token" (no oracle).
#[derive(Debug, Clone)]
pub struct AuthedDevice {
    pub id: String,
    pub role: DeviceRole,
}
impl AuthedDevice {
    pub fn is_operator(&self) -> bool {
        matches!(self.role, DeviceRole::Operator { .. })
    }
    pub fn chat_person(&self) -> &str {
        match &self.role {
            DeviceRole::Operator { default_person } => default_person,
            DeviceRole::Member { person } => person,
        }
    }
}

#[derive(Debug)]
pub enum DeviceError {
    /// The store file exists but is unreadable/malformed/inconsistent — the control surface must
    /// treat this as fail-closed (no device authenticates), never as a virgin store.
    Corrupt(String),
    Io(String),
    /// A pairing/administration request was invalid (bad role, duplicate, cap exceeded).
    Invalid(String),
}
impl std::fmt::Display for DeviceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceError::Corrupt(s) => write!(f, "device store corrupt: {s}"),
            DeviceError::Io(s) => write!(f, "device store io: {s}"),
            DeviceError::Invalid(s) => write!(f, "invalid device request: {s}"),
        }
    }
}
impl std::error::Error for DeviceError {}

type Result<T> = std::result::Result<T, DeviceError>;

/// The device trust store. Cheap to clone the `Arc` around it; all mutation is serialized behind an
/// internal lock and each write is atomic + durable before it returns.
pub struct DeviceStore {
    path: PathBuf,
    /// The `<state_dir>/console.token` anchor for the local console device (owner-only file).
    console_token_path: PathBuf,
    /// Serializes read-modify-write and holds the last-known-good doc as an in-memory cache.
    inner: Mutex<StoreDoc>,
}

/// SHA-256 → lowercase hex.
fn hash_token(raw: &str) -> String {
    let mut h = Sha256::new();
    h.update(raw.as_bytes());
    format!("{:x}", h.finalize())
}

/// Constant-time equality for two equal-length hex digests (defeats a timing side-channel on the
/// verify path; secondary to the 256-bit entropy but cheap).
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

/// A fresh URL-safe 256-bit token from the OS CSPRNG.
fn mint_token() -> std::result::Result<Secret, DeviceError> {
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::getrandom(&mut bytes).map_err(|e| DeviceError::Io(format!("csprng: {e}")))?;
    // URL-safe base64 without padding, hand-rolled (no extra dep).
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(43);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &b in &bytes {
        acc = (acc << 8) | b as u32;
        bits += 8;
        while bits >= 6 {
            bits -= 6;
            out.push(ALPHABET[((acc >> bits) & 0x3f) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((acc << (6 - bits)) & 0x3f) as usize] as char);
    }
    Ok(Secret(out))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl DeviceStore {
    /// Open (or prepare to create) the store at `<state_dir>/devices.json`. Does NOT mint anything —
    /// call `init_console_once` for that. A present-but-corrupt store returns `Err(Corrupt)` so the
    /// caller can refuse to start the authenticated control surface (fail closed).
    pub fn open(state_dir: impl AsRef<Path>) -> Result<Self> {
        let dir = state_dir.as_ref();
        let path = dir.join("devices.json");
        let console_token_path = dir.join("console.token");
        let doc = if path.exists() {
            let raw = std::fs::read_to_string(&path).map_err(|e| DeviceError::Io(e.to_string()))?;
            let doc: StoreDoc = serde_json::from_str(&raw)
                .map_err(|e| DeviceError::Corrupt(format!("parse: {e}")))?;
            validate(&doc)?;
            doc
        } else {
            StoreDoc::default()
        };
        Ok(DeviceStore { path, console_token_path, inner: Mutex::new(doc) })
    }

    /// Mint the console operator device EXACTLY ONCE, for a provably-new store (`initialized=false`
    /// and no devices). Writes the raw token to `<state_dir>/console.token` (owner-only) as the
    /// local trust anchor the `ym` wrapper reads. Idempotent: on an already-initialized store it is a
    /// no-op and returns `Ok(false)` — it NEVER recreates a revoked/deleted console (that would turn
    /// deletion into credential minting). `primary` is the person id the console speaks as on `/chat`.
    pub fn init_console_once(&self, primary: &str) -> Result<bool> {
        let mut doc = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if doc.initialized {
            return Ok(false);
        }
        if !doc.devices.is_empty() {
            // initialized=false but devices present = an inconsistent store we did not create.
            return Err(DeviceError::Corrupt("uninitialized store already holds devices".into()));
        }
        let token = mint_token()?;
        let device = Device {
            id: "console".to_string(),
            name: "local console".to_string(),
            role: DeviceRole::Operator { default_person: primary.to_string() },
            token_sha256: hash_token(token.expose()),
            created_ms: now_ms(),
            revoked_ms: None,
        };
        doc.devices.push(device);
        doc.initialized = true;
        persist(&self.path, &doc)?;
        write_owner_only(&self.console_token_path, token.expose())?;
        Ok(true)
    }

    /// Authenticate a raw bearer token → the active device it maps to, or `None` (unknown OR revoked;
    /// no distinction). This is the ONLY authentication entry point for the control server.
    pub fn authenticate(&self, raw_bearer: &str) -> Option<AuthedDevice> {
        let raw = raw_bearer.trim();
        if raw.is_empty() {
            return None;
        }
        let want = hash_token(raw);
        let doc = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        doc.devices
            .iter()
            .find(|d| d.is_active() && ct_eq(&d.token_sha256, &want))
            .map(|d| AuthedDevice { id: d.id.clone(), role: d.role.clone() })
    }

    /// Pair a new device, returning its raw token ONCE (never recoverable afterward). Operator-console
    /// only — the caller must have authenticated an operator device. Fails closed on duplicate name,
    /// unknown/empty person, or the device cap.
    pub fn pair(&self, name: &str, role: DeviceRole) -> Result<Secret> {
        let name = name.trim();
        if name.is_empty() {
            return Err(DeviceError::Invalid("device name required".into()));
        }
        match &role {
            DeviceRole::Operator { default_person } | DeviceRole::Member { person: default_person } => {
                if default_person.trim().is_empty() {
                    return Err(DeviceError::Invalid("device person/scope required".into()));
                }
            }
        }
        let mut doc = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if doc.devices.len() >= MAX_DEVICES {
            return Err(DeviceError::Invalid("device cap reached".into()));
        }
        if doc.devices.iter().any(|d| d.is_active() && d.name.eq_ignore_ascii_case(name)) {
            return Err(DeviceError::Invalid(format!("a device named '{name}' already exists")));
        }
        let token = mint_token()?;
        let token_sha256 = hash_token(token.expose());
        if doc.devices.iter().any(|d| d.token_sha256 == token_sha256) {
            // Astronomically unlikely with 256-bit entropy; fail closed rather than shadow.
            return Err(DeviceError::Invalid("token collision — retry".into()));
        }
        let id = format!("dev-{}", &token_sha256[..12]);
        doc.devices.push(Device {
            id,
            name: name.to_string(),
            role,
            token_sha256,
            created_ms: now_ms(),
            revoked_ms: None,
        });
        persist(&self.path, &doc)?;
        Ok(token)
    }

    /// Revoke a device by id (immediate for new requests: the next `authenticate` fails). Durable
    /// before it returns. Returns Ok(true) if a device was newly revoked, Ok(false) if none matched
    /// or it was already revoked. Refuses to revoke the LAST active operator (that would strand the
    /// console with no recovery path in this slice).
    pub fn revoke(&self, id: &str) -> Result<bool> {
        let mut doc = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let active_ops = doc.devices.iter().filter(|d| d.is_active() && d.is_operator()).count();
        let Some(target) = doc.devices.iter().position(|d| d.id == id && d.is_active()) else {
            return Ok(false);
        };
        if doc.devices[target].is_operator() && active_ops <= 1 {
            return Err(DeviceError::Invalid(
                "refusing to revoke the last operator device (would strand the console)".into(),
            ));
        }
        doc.devices[target].revoked_ms = Some(now_ms());
        persist(&self.path, &doc)?;
        Ok(true)
    }

    /// Metadata for `device list` — NEVER includes token hashes. Active + revoked, newest first.
    pub fn list(&self) -> Vec<DeviceInfo> {
        let doc = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let mut out: Vec<DeviceInfo> = doc
            .devices
            .iter()
            .map(|d| DeviceInfo {
                id: d.id.clone(),
                name: d.name.clone(),
                role: match &d.role {
                    DeviceRole::Operator { .. } => "operator".into(),
                    DeviceRole::Member { person } => format!("member:{person}"),
                },
                created_ms: d.created_ms,
                revoked: d.revoked_ms.is_some(),
            })
            .collect();
        out.sort_by(|a, b| b.created_ms.cmp(&a.created_ms));
        out
    }
}

/// Redacted device metadata for listing (no secrets).
#[derive(Debug, Clone)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub created_ms: u64,
    pub revoked: bool,
}

/// Reject a document that is structurally valid JSON but semantically inconsistent — a corrupt store
/// must fail closed, not authenticate something ambiguous.
fn validate(doc: &StoreDoc) -> Result<()> {
    if doc.version != STORE_VERSION {
        return Err(DeviceError::Corrupt(format!("unknown store version {}", doc.version)));
    }
    let mut ids = std::collections::HashSet::new();
    let mut hashes = std::collections::HashSet::new();
    for d in &doc.devices {
        if d.id.trim().is_empty() || d.token_sha256.len() != 64 || !d.token_sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(DeviceError::Corrupt(format!("malformed device record '{}'", d.id)));
        }
        if !ids.insert(&d.id) {
            return Err(DeviceError::Corrupt(format!("duplicate device id '{}'", d.id)));
        }
        if !hashes.insert(&d.token_sha256) {
            return Err(DeviceError::Corrupt("duplicate token hash".into()));
        }
        match &d.role {
            DeviceRole::Operator { default_person } | DeviceRole::Member { person: default_person } => {
                if default_person.trim().is_empty() {
                    return Err(DeviceError::Corrupt(format!("device '{}' has empty person", d.id)));
                }
            }
        }
    }
    if doc.initialized && doc.devices.iter().all(|d| d.revoked_ms.is_some()) && !doc.devices.is_empty() {
        // Every device revoked: not "corrupt", but the caller should notice there's no way in.
        // We still load it (so `device list` works); authentication simply returns None for all.
    }
    Ok(())
}

/// Atomic, durable write of the store doc: temp file (owner-only) → fsync → rename → fsync dir.
fn persist(path: &Path, doc: &StoreDoc) -> Result<()> {
    let json = serde_json::to_string_pretty(doc).map_err(|e| DeviceError::Io(e.to_string()))?;
    let dir = path.parent().ok_or_else(|| DeviceError::Io("store has no parent dir".into()))?;
    std::fs::create_dir_all(dir).map_err(|e| DeviceError::Io(e.to_string()))?;
    let tmp = path.with_extension("json.tmp");
    write_owner_only(&tmp, &json)?;
    std::fs::rename(&tmp, path).map_err(|e| DeviceError::Io(format!("rename: {e}")))?;
    fsync_dir(dir);
    Ok(())
}

/// Write `contents` to `path` as a fresh owner-only regular file, fsync'd. On unix the mode is 0600;
/// on other platforms we rely on the default ACL of the (private) state dir.
fn write_owner_only(path: &Path, contents: &str) -> Result<()> {
    use std::io::Write;
    // Remove any pre-existing file so mode/ownership are ours, and we never follow a planted symlink.
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| DeviceError::Io(format!("create {}: {e}", path.display())))?;
    f.write_all(contents.as_bytes()).map_err(|e| DeviceError::Io(e.to_string()))?;
    f.sync_all().map_err(|e| DeviceError::Io(e.to_string()))?;
    Ok(())
}

#[cfg(unix)]
fn fsync_dir(dir: &Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}
#[cfg(not(unix))]
fn fsync_dir(_dir: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ym_devtrust_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn console_init_is_one_time_and_authenticates() {
        let dir = scratch("init");
        let store = DeviceStore::open(&dir).unwrap();
        assert!(store.init_console_once("primary").unwrap(), "first init mints the console");
        assert!(!store.init_console_once("primary").unwrap(), "second init is a no-op");
        let token = std::fs::read_to_string(dir.join("console.token")).unwrap();
        let authed = store.authenticate(token.trim()).expect("console token authenticates");
        assert!(authed.is_operator());
        assert_eq!(authed.chat_person(), "primary");
        assert!(store.authenticate("not-the-token").is_none(), "a bogus token never authenticates");
    }

    #[test]
    fn revoke_then_reopen_never_resurrects_the_operator() {
        // The revoke+restart attack: a corrupt/edited store must not auto-mint a fresh operator.
        let dir = scratch("resurrect");
        let token = {
            let store = DeviceStore::open(&dir).unwrap();
            store.init_console_once("primary").unwrap();
            // Pair a SECOND operator so we're allowed to revoke the console (last-operator guard).
            store.pair("second", DeviceRole::Operator { default_person: "primary".into() }).unwrap();
            store.revoke("console").unwrap();
            std::fs::read_to_string(dir.join("console.token")).unwrap()
        };
        // Reopen: init is a no-op (already initialized), the revoked console stays dead.
        let store2 = DeviceStore::open(&dir).unwrap();
        assert!(!store2.init_console_once("primary").unwrap(), "already-initialized store must not re-init");
        assert!(store2.authenticate(token.trim()).is_none(), "revoked console must not authenticate after reopen");
    }

    #[test]
    fn corrupt_store_fails_closed() {
        let dir = scratch("corrupt");
        std::fs::write(dir.join("devices.json"), "{ this is not valid json").unwrap();
        assert!(DeviceStore::open(&dir).is_err(), "a corrupt store must fail closed, not open empty");
        // Semantically-inconsistent (duplicate id) also fails closed.
        let dir2 = scratch("dupid");
        let doc = r#"{"version":1,"initialized":true,"devices":[
          {"id":"a","name":"x","role":{"kind":"operator","default_person":"primary"},"token_sha256":"aa","created_ms":1},
          {"id":"a","name":"y","role":{"kind":"member","person":"asha"},"token_sha256":"bb","created_ms":2}]}"#;
        std::fs::write(dir2.join("devices.json"), doc).unwrap();
        assert!(DeviceStore::open(&dir2).is_err(), "duplicate device id must fail closed");
    }

    #[test]
    fn pair_member_binds_person_and_revoke_is_immediate() {
        let dir = scratch("pair");
        let store = DeviceStore::open(&dir).unwrap();
        store.init_console_once("primary").unwrap();
        let tok = store.pair("asha-phone", DeviceRole::Member { person: "asha".into() }).unwrap();
        let authed = store.authenticate(tok.expose()).expect("member token authenticates");
        assert!(!authed.is_operator());
        assert_eq!(authed.chat_person(), "asha");
        // list carries no hashes
        let ids: Vec<_> = store.list().into_iter().map(|d| d.id).collect();
        let dev_id = ids.iter().find(|i| i.starts_with("dev-")).cloned().unwrap();
        assert!(store.revoke(&dev_id).unwrap(), "revoke succeeds");
        assert!(store.authenticate(tok.expose()).is_none(), "revoked token is immediately dead");
        assert!(!store.revoke(&dev_id).unwrap(), "double-revoke is a no-op");
    }

    #[test]
    fn last_operator_cannot_be_revoked() {
        let dir = scratch("lastop");
        let store = DeviceStore::open(&dir).unwrap();
        store.init_console_once("primary").unwrap();
        assert!(store.revoke("console").is_err(), "the last operator must not be revocable");
    }

    #[test]
    fn duplicate_device_name_refused() {
        let dir = scratch("dupname");
        let store = DeviceStore::open(&dir).unwrap();
        store.init_console_once("primary").unwrap();
        store.pair("phone", DeviceRole::Member { person: "asha".into() }).unwrap();
        assert!(store.pair("phone", DeviceRole::Member { person: "bob".into() }).is_err());
    }

    #[test]
    fn tokens_are_high_entropy_and_distinct() {
        let dir = scratch("entropy");
        let store = DeviceStore::open(&dir).unwrap();
        store.init_console_once("primary").unwrap();
        let a = store.pair("a", DeviceRole::Member { person: "asha".into() }).unwrap();
        let b = store.pair("b", DeviceRole::Member { person: "bob".into() }).unwrap();
        assert_ne!(a.expose(), b.expose());
        assert!(a.expose().len() >= 40, "256-bit token encodes to >=40 chars");
        assert_eq!(format!("{a:?}"), "Secret(********)", "Debug must not leak the token");
    }
}
