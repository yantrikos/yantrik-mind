//! Asking the desktop whether a memory credential is live (E.DOOR1, yantrik-os #447/#448).
//!
//! Another mind presents a `mem-…` credential. The server never trusts it by itself: it asks the
//! person's shell, through the minds' door, `shell.memory_validate` with the credential's SHA-256
//! (the credential itself never crosses the socket). The answer is the pinned object -- who the
//! person is, which mind, which grants, for how long -- or `null`. Anything else is no footing.
//!
//! The wire is taken from real bytes (fixtures/memory_validate_*_05ff164.txt), not a description:
//! newline-delimited JSON-RPC 2.0, `app.act` with no `app` key (the socket is the shell's own), and
//! the answer inside the standard act envelope at `result.result`.

use mind_types::AgentFooting;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one validation may take before the server gives up and refuses (fail closed).
pub const VALIDATE_TIMEOUT: Duration = Duration::from_millis(250);

/// The longest a valid answer is trusted without asking again, whatever the desktop says.
pub const MAX_TRUST: Duration = Duration::from_secs(5);

/// SHA-256 of the TRIMMED credential, lowercase hex -- exactly as the shell's `reach::token_digest`.
pub fn credential_sha256(credential: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(credential.trim().as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// The one line the server writes.
pub fn validation_request(sha: &str) -> String {
    let req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "app.act",
        "params": { "action": "memory_validate", "args": { "memory_sha256": sha } },
    });
    format!("{req}\n")
}

/// The one line the shell answers: `Ok(None)` for an unknown credential, `Ok(Some(answer))` for a
/// live one, `Err` for anything else -- which the caller treats as no footing.
pub fn read_answer(line: &str) -> Result<Option<Value>, String> {
    if line.trim().is_empty() {
        return Err("the desktop closed the connection without answering".into());
    }
    let reply: Value = serde_json::from_str(line).map_err(|e| format!("unreadable answer: {e}"))?;
    if let Some(err) = reply.get("error") {
        return Err(err.get("message").and_then(Value::as_str).unwrap_or("refused").to_string());
    }
    match reply.get("result").and_then(|r| r.get("result")) {
        Some(Value::Null) => Ok(None),
        Some(v @ Value::Object(_)) => Ok(Some(v.clone())),
        _ => Err("the answer is not where memory_validate puts it".into()),
    }
}

/// One round trip over the door: connect, write the request, read one line.
#[cfg(unix)]
pub fn ask(socket: &std::path::Path, sha: &str) -> Result<Option<Value>, String> {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let stream = UnixStream::connect(socket).map_err(|e| format!("the door is not open: {e}"))?;
    stream.set_read_timeout(Some(VALIDATE_TIMEOUT)).ok();
    stream.set_write_timeout(Some(VALIDATE_TIMEOUT)).ok();
    let mut out = stream.try_clone().map_err(|e| e.to_string())?;
    out.write_all(validation_request(sha).as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).map_err(|e| format!("read: {e}"))?;
    read_answer(&line)
}

type Asker = Box<dyn Fn(&str) -> Result<Option<Value>, String> + Send + Sync>;

/// E.DOOR3: the grants a validated answer may open only through a door the person's processes
/// cannot replace. The shell runs as the person, so whatever answers on a person-writable socket
/// may be any process of theirs -- it could grant itself these. The ordinary grants stand either way.
pub const NEEDS_A_FIXED_DOOR: [&str; 3] = ["recall_health", "recall_finance", "household"];

/// E.DOOR3: is the door's directory one only root can change? A real directory (not a symlink),
/// owned by root, with no group or other write bit. Then no non-root process can unlink the socket
/// in it and bind its own.
pub fn door_dir_is_fixed(uid: u32, mode: u32, is_symlink: bool, is_dir: bool) -> bool {
    !is_symlink && is_dir && uid == 0 && mode & 0o022 == 0
}

/// E.DOOR3: the directory holding `socket`, looked at now.
#[cfg(unix)]
pub fn door_is_fixed(socket: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Some(dir) = socket.parent() else {
        return false;
    };
    match std::fs::symlink_metadata(dir) {
        Ok(m) => door_dir_is_fixed(m.uid(), m.mode(), m.file_type().is_symlink(), m.is_dir()),
        Err(_) => false,
    }
}

/// Turns credentials into footings, asking the desktop and remembering a live answer briefly.
pub struct Validator {
    ask: Asker,
    /// E.DOOR3: may an answer open `NEEDS_A_FIXED_DOOR`? Asked on every validation.
    fixed_door: Box<dyn Fn() -> bool + Send + Sync>,
    own_person_uid: u32,
    cache: Mutex<HashMap<String, (Instant, AgentFooting)>>,
}

impl Validator {
    /// Validate through the door at `socket`, for the person this Mind belongs to.
    #[cfg(unix)]
    pub fn over_door(socket: std::path::PathBuf, own_person_uid: u32) -> Self {
        let door = socket.clone();
        Self::with_asker(Box::new(move |sha| ask(&socket, sha)), own_person_uid)
            .with_fixed_door(Box::new(move || door_is_fixed(&door)))
    }

    /// Validate with any asker -- the door in production, a script in tests.
    pub fn with_asker(ask: Asker, own_person_uid: u32) -> Self {
        Self { ask, fixed_door: Box::new(|| false), own_person_uid, cache: Mutex::new(HashMap::new()) }
    }

    /// E.DOOR3: how to tell whether the door is fixed. Without this, it is not.
    pub fn with_fixed_door(mut self, fixed: Box<dyn Fn() -> bool + Send + Sync>) -> Self {
        self.fixed_door = fixed;
        self
    }

    /// The footing a credential stands for right now, or None. Every failure is None.
    pub fn footing(&self, credential: &str) -> Option<AgentFooting> {
        let sha = credential_sha256(credential);
        let now = Instant::now();
        if let Some((until, f)) = self.cache.lock().ok()?.get(&sha) {
            if *until > now {
                return Some(f.clone());
            }
        }
        let mut answer = match (self.ask)(&sha) {
            Ok(Some(a)) => a,
            Ok(None) => {
                self.forget(&sha);
                return None;
            }
            Err(e) => {
                eprintln!("[memory] a credential could not be validated ({e}) -- refused");
                // E.PRIV1: the door failing (Private mode, closed, timed out) means nothing it said
                // before can be leaned on either -- every cached yes goes, not just this one.
                if let Ok(mut c) = self.cache.lock() {
                    c.clear();
                }
                return None;
            }
        };
        if !(self.fixed_door)() {
            if let Some(Value::Array(grants)) = answer.get_mut("grants") {
                let before = grants.len();
                grants.retain(|g| !g.as_str().is_some_and(|g| NEEDS_A_FIXED_DOOR.contains(&g.trim())));
                if grants.len() != before {
                    eprintln!(
                        "[memory] health, finance and household grants withheld: the door could be replaced by any of                          the person's processes (it needs a root-owned directory nobody else can write)"
                    );
                }
            }
        }
        let (footing, unknown) = match AgentFooting::from_validation(&answer, self.own_person_uid) {
            Ok(x) => x,
            Err(e) => {
                eprintln!("[memory] a validation answer was refused: {e}");
                return None;
            }
        };
        if !unknown.is_empty() {
            eprintln!("[memory] grants this Mind does not know were ignored: {}", unknown.join(", "));
        }
        let trust = answer
            .get("valid_for_ms")
            .and_then(Value::as_u64)
            .map(Duration::from_millis)
            .unwrap_or(Duration::ZERO)
            .min(MAX_TRUST);
        if !trust.is_zero() {
            if let Ok(mut c) = self.cache.lock() {
                c.insert(sha, (now + trust, footing.clone()));
            }
        }
        Some(footing)
    }

    /// The person this Mind belongs to.
    pub fn person_uid(&self) -> u32 {
        self.own_person_uid
    }

    /// The desktop said this credential is gone (`memory.revoked`): stop trusting it now.
    pub fn revoke(&self, sha: &str) {
        self.forget(sha);
    }

    fn forget(&self, sha: &str) {
        if let Ok(mut c) = self.cache.lock() {
            c.remove(sha);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!("{}/fixtures/memory_validate_{name}_05ff164.txt", env!("CARGO_MANIFEST_DIR"))).unwrap()
    }

    /// The request is the one 520's shell answered.
    #[test]
    fn the_request_is_the_captured_one() {
        let sha = "c8a46b7a68c82fd884d11df0794932bd99e62c109a1caa97b31969c46380bdf7";
        let ours = validation_request(sha);
        let theirs = fixture("request");
        assert!(ours.ends_with('\n') && ours.matches('\n').count() == 1, "one line");
        let a: Value = serde_json::from_str(&ours).unwrap();
        let b: Value = serde_json::from_str(&theirs).unwrap();
        assert_eq!(a, b);
        assert!(a["params"].get("app").is_none(), "the door socket is the shell's own -- no app key");
    }

    #[test]
    fn the_digest_is_of_the_trimmed_credential() {
        // SHA-256("abc"), the FIPS 180-2 test vector.
        assert_eq!(credential_sha256("  abc\n"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }

    /// The captured answers read as they were given; everything that is not an answer fails closed.
    #[test]
    fn the_captured_answers_read_as_given() {
        assert_eq!(read_answer(&fixture("unknown")), Ok(None));
        let refused = read_answer(&fixture("refused")).unwrap_err();
        assert!(refused.contains("only the person's memory server asks this"), "{refused}");
        // The live reply carries the whole shell state (34,686 bytes on 520): still one answer.
        let mut big: Value = serde_json::from_str(&fixture("unknown")).unwrap();
        big["result"]["state"] = serde_json::json!({ "pad": "x".repeat(40_000) });
        assert_eq!(read_answer(&format!("{big}\n")), Ok(None));
        assert!(read_answer("").is_err());
        assert!(read_answer("not json").is_err());
        assert!(read_answer(r#"{"jsonrpc":"2.0","result":{"accepted":true},"id":1}"#).is_err(), "no answer is not 'unknown'");
    }

    /// E.DOOR3 (yantrik-os-4c's review): the person's processes can replace a door in a directory
    /// they can write, so only a root-owned, unwritable one may open health, finance and household.
    #[test]
    fn sensitive_grants_need_a_door_the_person_cannot_replace() {
        let answer = serde_json::json!({"v": 1, "person_uid": 1000, "mind": "hermes", "attach": "hermes:c1@s1",
            "grants": ["recall_ordinary", "believe", "recall_health", "recall_finance", "household"], "valid_for_ms": 0});
        let grants_through = |fixed: bool| {
            let a = answer.clone();
            let v = Validator::with_asker(Box::new(move |_| Ok(Some(a.clone()))), 1000).with_fixed_door(Box::new(move || fixed));
            let f = v.footing("mem-x").expect("an ordinary footing either way");
            ["recall_ordinary", "believe", "recall_health", "recall_finance", "household"]
                .into_iter()
                .filter(|g| f.has(g))
                .collect::<Vec<_>>()
        };
        assert_eq!(grants_through(false), vec!["recall_ordinary", "believe"]);
        assert_eq!(grants_through(true), vec!["recall_ordinary", "believe", "recall_health", "recall_finance", "household"]);
        let a = answer.clone();
        let default = Validator::with_asker(Box::new(move |_| Ok(Some(a.clone()))), 1000).footing("mem-y").unwrap();
        assert!(!default.has("recall_health"), "a validator told nothing about its door treats it as replaceable");

        assert!(door_dir_is_fixed(0, 0o40755, false, true));
        assert!(door_dir_is_fixed(0, 0o40750, false, true));
        assert!(!door_dir_is_fixed(1000, 0o42750, false, true), "today's /run/yantrik-minds: the person's");
        assert!(!door_dir_is_fixed(0, 0o40775, false, true), "group-writable");
        assert!(!door_dir_is_fixed(0, 0o40757, false, true), "other-writable");
        assert!(!door_dir_is_fixed(0, 0o40755, true, true), "a symlink, whatever it points at");
        assert!(!door_dir_is_fixed(0, 0o100644, false, false), "not a directory");
    }

    /// E.DOOR3 on a real directory: other-writable is never fixed; 0755 is fixed only when root owns it.
    #[cfg(unix)]
    #[test]
    fn the_door_check_reads_the_real_directory() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("ym-door3-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let socket = dir.join("app-shell.sock");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(!door_is_fixed(&socket), "an other-writable directory");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let owner = std::fs::metadata(&dir).unwrap().uid();
        assert_eq!(door_is_fixed(&socket), owner == 0, "0755, owned by uid {owner}");
        let link = std::env::temp_dir().join(format!("ym-door3-link-{}", std::process::id()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        assert!(!door_is_fixed(&link.join("app-shell.sock")), "a symlinked directory");
        std::fs::remove_file(&link).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn live(uid: u32, valid_for_ms: u64) -> Value {
        serde_json::json!({"v": 1, "person_uid": uid, "mind": "hermes", "attach": "hermes:c1@s1", "grants": ["recall_ordinary"], "valid_for_ms": valid_for_ms})
    }

    fn counting(answer: Result<Option<Value>, String>) -> (Validator, Arc<AtomicUsize>) {
        let n = Arc::new(AtomicUsize::new(0));
        let m = n.clone();
        let v = Validator::with_asker(
            Box::new(move |_| {
                m.fetch_add(1, Ordering::SeqCst);
                answer.clone()
            }),
            1000,
        );
        (v, n)
    }

    /// A live answer is trusted for its window and no longer; unknown and failures are never cached.
    #[test]
    fn a_live_answer_is_trusted_briefly_and_a_null_never() {
        let (v, asked) = counting(Ok(Some(live(1000, 2000))));
        assert_eq!(v.footing("mem-x").map(|f| f.mind().to_string()).as_deref(), Some("hermes"));
        assert!(v.footing("mem-x").is_some());
        assert_eq!(asked.load(Ordering::SeqCst), 1, "a live answer inside its window was asked again");
        v.revoke(&credential_sha256("mem-x"));
        assert!(v.footing("mem-x").is_some());
        assert_eq!(asked.load(Ordering::SeqCst), 2, "revoke did not drop the cached answer");

        let (v, asked) = counting(Ok(None));
        assert!(v.footing("mem-y").is_none() && v.footing("mem-y").is_none());
        assert_eq!(asked.load(Ordering::SeqCst), 2, "an unknown credential was cached");

        let (v, asked) = counting(Err("timed out".into()));
        assert!(v.footing("mem-z").is_none() && v.footing("mem-z").is_none());
        assert_eq!(asked.load(Ordering::SeqCst), 2, "a failure was cached");

        // E.PRIV1: a failed validation drops every cached yes, not only the one asked about.
        let flip = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let asked = Arc::new(AtomicUsize::new(0));
        let (f, a) = (flip.clone(), asked.clone());
        let v = Validator::with_asker(
            Box::new(move |_| {
                a.fetch_add(1, Ordering::SeqCst);
                if f.load(Ordering::SeqCst) { Err("PRIVATE: the person has turned on Private mode".into()) } else { Ok(Some(live(1000, 2000))) }
            }),
            1000,
        );
        assert!(v.footing("mem-a").is_some());
        flip.store(true, Ordering::SeqCst);
        assert!(v.footing("mem-b").is_none(), "Private mode answered yes");
        assert!(v.footing("mem-a").is_none(), "a yes cached before Private mode was still served");
        assert_eq!(asked.load(Ordering::SeqCst), 3, "the cached credential was not asked about again");

        let (v, _) = counting(Ok(Some(live(1001, 2000))));
        assert!(v.footing("mem-w").is_none(), "another person's credential gave a footing");

        let (v, asked) = counting(Ok(Some(live(1000, 0))));
        assert!(v.footing("mem-v").is_some() && v.footing("mem-v").is_some());
        assert_eq!(asked.load(Ordering::SeqCst), 2, "an answer with no window was cached");
    }
}
