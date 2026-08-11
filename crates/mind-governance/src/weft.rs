//! Weft attestation — the mind's trust claims land on an external ledger instead of living as a
//! local mutable boolean.
//!
//! The stack this completes: YantrikDB knows, packs can, **Weft did-and-proved**. When the mind
//! certifies a capability pack it currently flips `certified: true` and the evidence evaporates.
//! An attestation lands the verdict as a signed, content-addressed note on a Weft repo: what was
//! certified (a digest of the exact document), what the evals said, and who claimed it. Demotions
//! land too, so a pack's trust history is append-only and auditable rather than asserted.
//!
//! Honest scope: this is an ATTESTATION client, not the full Weft protocol. The mind is a plain
//! client of `weftd` over its JSON prepare/sign/submit flow — no CBOR, no gate proposals, no
//! capability delegation. Weft stays independent of the mind (and of YantrikDB); the coupling is
//! configuration, not code. With no Weft configured the mind certifies exactly as before and says
//! so ("unattested") rather than pretending the claim was witnessed.

use std::sync::Mutex;

use ed25519_dalek::{Signer, SigningKey};
use sha2::{Digest, Sha256};

/// A trust claim the mind wants witnessed.
#[derive(Clone, Debug)]
pub struct Attestation {
    /// What the claim is about, e.g. "pack:csv_pack".
    pub subject: String,
    /// The verdict: did it pass?
    pub verdict: bool,
    /// Digest of the exact bytes the verdict is about — evidence bound to content, not to a name.
    pub digest: String,
    /// One line per check, as the certifier rendered them.
    pub evidence: Vec<String>,
}

impl Attestation {
    /// sha256 of the subject bytes — the caller digests whatever document it certified.
    pub fn digest_of(bytes: &[u8]) -> String {
        hexs(&Sha256::digest(bytes))
    }

    /// The durable note text. Deliberately plain: a human reading the ledger months later should
    /// see what was claimed and on what basis without needing this crate.
    fn note_text(&self) -> String {
        let mut t = format!(
            "{} {}\nsubject: {}\ndigest: sha256:{}\n",
            if self.verdict { "CERTIFIED" } else { "DEMOTED" },
            self.subject,
            self.subject,
            self.digest
        );
        for line in &self.evidence {
            t.push_str(line.trim());
            t.push('\n');
        }
        t
    }
}

/// Something that can witness a claim and return a durable reference to it.
pub trait Attestor: Send + Sync {
    /// Name of the ledger, for receipts ("weft").
    fn ledger(&self) -> &str;
    /// Land the attestation; returns the ledger's reference (a note oid). BLOCKING — async callers
    /// must run this on a blocking thread.
    fn attest(&self, a: &Attestation) -> Result<String, String>;
}

/// Attestor backed by a `weftd` HTTP endpoint.
pub struct WeftAttestor {
    base: String,
    sk: SigningKey,
    pub_hex: String,
    repo: Mutex<Option<String>>,
    timeout_secs: u64,
}

impl WeftAttestor {
    /// `YM_WEFT_URL` (e.g. http://127.0.0.1:8747) + `YM_WEFT_KEY` (64 hex chars = ed25519 seed).
    /// Absent or malformed → None, and the mind runs unattested.
    pub fn from_env() -> Option<Self> {
        let base = std::env::var("YM_WEFT_URL").ok()?;
        let key = std::env::var("YM_WEFT_KEY").ok()?;
        Self::new(&base, &key).ok()
    }

    pub fn new(base: &str, key_hex: &str) -> Result<Self, String> {
        let seed = unhexs(key_hex.trim())?;
        let seed: [u8; 32] = seed.try_into().map_err(|_| "YM_WEFT_KEY must be 32 bytes (64 hex chars)".to_string())?;
        let sk = SigningKey::from_bytes(&seed);
        let pub_hex = hexs(sk.verifying_key().as_bytes());
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            sk,
            pub_hex,
            repo: Mutex::new(None),
            timeout_secs: 10,
        })
    }

    /// This attestor's public key — the identity its notes are signed by.
    pub fn identity(&self) -> &str {
        &self.pub_hex
    }

    fn get(&self, path: &str) -> Result<serde_json::Value, String> {
        ureq::get(&format!("{}{path}", self.base))
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .call()
            .map_err(|e| format!("{path}: {e}"))?
            .into_json()
            .map_err(|e| format!("{path}: {e}"))
    }

    fn post(&self, path: &str, body: &serde_json::Value) -> Result<serde_json::Value, String> {
        ureq::post(&format!("{}{path}", self.base))
            .timeout(std::time::Duration::from_secs(self.timeout_secs))
            .send_json(body.clone())
            .map_err(|e| format!("{path}: {e}"))?
            .into_json()
            .map_err(|e| format!("{path}: {e}"))
    }

    /// The repo id, fetched once from /policy and cached.
    fn repo_id(&self) -> Result<String, String> {
        if let Some(r) = self.repo.lock().unwrap().clone() {
            return Ok(r);
        }
        let p = self.get("/policy")?;
        let repo = p.get("repo").and_then(|r| r.as_str()).ok_or("weft has no repository yet")?.to_string();
        *self.repo.lock().unwrap() = Some(repo.clone());
        Ok(repo)
    }
}

impl Attestor for WeftAttestor {
    fn ledger(&self) -> &str {
        "weft"
    }

    fn attest(&self, a: &Attestation) -> Result<String, String> {
        let repo = self.repo_id()?;
        let ts = chrono::Utc::now().timestamp_millis();
        // A note, not a proposal: the mind is recording a verdict about its own capabilities, not
        // asking the gate to land a code change. `constraint` because a demotion binds behavior.
        let env = serde_json::json!({
            "repo": format!("hex:{repo}"),
            "type": "note",
            "ts": ts,
            "author": format!("hex:{}", self.pub_hex),
            "auth": serde_json::Value::Null,
            "body": {
                "kind": if a.verdict { "decision" } else { "constraint" },
                "text": a.note_text(),
                "anchors": [{ "path": a.subject.replace(':', "/") }],
            }
        });
        // weftd canonicalizes and returns the exact bytes to sign — the client never guesses the
        // encoding, which is why this needs no CBOR implementation here.
        let prep = self.post("/prepare", &env)?;
        let payload_hex = prep.get("payload").and_then(|p| p.as_str()).ok_or("prepare: no payload")?;
        let payload = unhexs(payload_hex)?;
        let sig = self.sk.sign(&payload).to_bytes();
        let mut signed = env;
        signed["sig"] = serde_json::json!(format!("hex:{}", hexs(&sig)));
        let out = self.post("/submit", &signed)?;
        out.get("oid")
            .and_then(|o| o.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| format!("submit: {}", out.get("error").and_then(|e| e.as_str()).unwrap_or("no oid")))
    }
}

fn hexs(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhexs(s: &str) -> Result<Vec<u8>, String> {
    let s = s.strip_prefix("hex:").unwrap_or(s);
    if s.len() % 2 != 0 {
        return Err("odd-length hex".into());
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_text_states_verdict_subject_digest_and_evidence() {
        let a = Attestation {
            subject: "pack:csv_pack".into(),
            verdict: true,
            digest: Attestation::digest_of(b"doc"),
            evidence: vec!["   ✓ skill_exists(csv summer)".into()],
        };
        let t = a.note_text();
        assert!(t.starts_with("CERTIFIED pack:csv_pack"));
        assert!(t.contains("digest: sha256:"));
        assert!(t.contains("✓ skill_exists(csv summer)"));
        // a demotion is legible as its own claim
        let d = Attestation { verdict: false, ..a };
        assert!(d.note_text().starts_with("DEMOTED"));
    }

    #[test]
    fn digest_binds_to_bytes() {
        assert_eq!(Attestation::digest_of(b"a"), Attestation::digest_of(b"a"));
        assert_ne!(Attestation::digest_of(b"a"), Attestation::digest_of(b"b"));
        assert_eq!(Attestation::digest_of(b"").len(), 64);
    }

    #[test]
    fn hex_roundtrips_and_rejects_junk() {
        assert_eq!(unhexs(&hexs(&[0xde, 0xad])).unwrap(), vec![0xde, 0xad]);
        assert_eq!(unhexs("hex:dead").unwrap(), vec![0xde, 0xad]);
        assert!(unhexs("abc").is_err());
        assert!(unhexs("zz").is_err());
    }

    #[test]
    fn key_must_be_32_bytes() {
        assert!(WeftAttestor::new("http://x", &"11".repeat(32)).is_ok());
        assert!(WeftAttestor::new("http://x", "1122").is_err());
    }
}
