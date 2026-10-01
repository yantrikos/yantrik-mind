//! Sensitive-read receipts (ARCH-1 slice 2; Purpose Gate v1): a hash-chained,
//! append-only JSONL ledger of EVERY memory read — who read, through which
//! facade method, for what declared purpose, what they asked, how many results
//! crossed the boundary and how many the purpose gate suppressed.
//!
//! Operator reads used to be exempt ("the trusted owner path"). The purpose
//! gate ends that: the operator's background lanes (dream/proactive/research/…)
//! are exactly the cross-subject reads a purpose audit exists to catch, so a
//! ledger blind to them would be theater. Every context is receipted now.
//!
//! Chain discipline mirrors the immune ledger (mind-evals::immune): each line is
//! `{"chain":"<hex>","record":{…}}` where `chain = sha256(prev_chain_hex ++ record_json)`
//! and the first record chains off the literal `"genesis"`. Any edit, reorder,
//! or deletion of a middle line breaks every later chain value. The purpose
//! fields are optional-and-omitted-when-absent so pre-gate ledger lines still
//! verify byte-for-byte.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One read crossing the authorization boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReceipt {
    pub ts_ms: u64,
    /// `principal_label()` of the reading context, e.g. "private:wife" | "shared" | "operator".
    pub principal: String,
    /// Facade method: "recall_typed" | "beliefs_matching" | "reflect" | …
    pub method: String,
    /// The query/needle (truncated) — enough to audit intent, not a data copy.
    pub detail: String,
    /// How many items were returned AFTER scope + purpose filtering.
    pub results: usize,
    /// The declared purpose label, e.g. "proactive→member:primary". Absent only
    /// on pre-gate ledger lines (kept optional so those still verify).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub purpose: Option<String>,
    /// How many scope-visible items the purpose gate suppressed. Omitted when zero.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suppressed: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct ChainedLine {
    chain: String,
    record: ReadReceipt,
}

/// Append-only receipt sink. `path: None` (in-memory scratch DBs without an
/// explicit env override) disables recording — a real spawned mind always has
/// a file-backed DB and therefore a ledger.
pub struct ReadReceiptLedger {
    path: Option<PathBuf>,
    /// Cached chain head so appends don't re-read the whole file.
    head: Mutex<Option<String>>,
}

impl ReadReceiptLedger {
    /// Resolve the ledger location: `YM_READ_RECEIPTS` env wins; otherwise the
    /// ledger lives next to the DB (`<db_path>.read_receipts.jsonl`); an
    /// in-memory DB with no override gets no ledger.
    pub fn for_db(db_path: &str) -> Self {
        let path = match std::env::var("YM_READ_RECEIPTS") {
            Ok(p) if !p.trim().is_empty() => Some(PathBuf::from(p)),
            _ if db_path != ":memory:" => {
                Some(PathBuf::from(format!("{db_path}.read_receipts.jsonl")))
            }
            _ => None,
        };
        Self {
            path,
            head: Mutex::new(None),
        }
    }

    /// Append one receipt. Best-effort: a ledger failure must never fail the
    /// read itself (availability), but it is loudly logged (auditability).
    pub fn append(&self, receipt: ReadReceipt) {
        let Some(path) = &self.path else { return };
        if let Err(e) = self.append_inner(path, &receipt) {
            tracing::warn!("read-receipt ledger append failed: {e}");
        }
    }

    fn append_inner(&self, path: &Path, receipt: &ReadReceipt) -> std::io::Result<()> {
        let mut head = self.head.lock().unwrap_or_else(|p| p.into_inner());
        let prev = match head.clone() {
            Some(h) => h,
            None => chain_head(path).unwrap_or_else(|| "genesis".to_string()),
        };
        let record_json = serde_json::to_string(receipt).map_err(std::io::Error::other)?;
        let mut hasher = Sha256::new();
        hasher.update(prev.as_bytes());
        hasher.update(record_json.as_bytes());
        let chain = format!("{:x}", hasher.finalize());
        let line = format!("{{\"chain\":\"{chain}\",\"record\":{record_json}}}\n");
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
        *head = Some(chain);
        Ok(())
    }
}

impl ReadReceiptLedger {
    /// E.ERASE1: erase a literal from every receipt (a recall's query can carry what the person later
    /// asks to forget), re-chain the whole ledger, and append one `redact` receipt naming how many
    /// lines changed and the OLD head -- never the literal, and no fingerprint of it either: a hash
    /// of a four-digit code is the code. Refuses a ledger that does not verify (it would launder a
    /// break as a redaction). The rewrite is atomic: a temporary file renamed over the ledger.
    pub fn redact(&self, needle_lc: &str) -> Result<mind_types::erase_text::LedgerRedaction, String> {
        use mind_types::erase_text::{erase_json, LedgerRedaction};
        let Some(path) = &self.path else { return Ok(LedgerRedaction::default()) };
        let mut head = self.head.lock().unwrap_or_else(|p| p.into_inner());
        let Ok(content) = std::fs::read_to_string(path) else { return Ok(LedgerRedaction::default()) };
        if let Err(i) = verify_ledger(path) {
            return Err(format!("the read-receipt ledger does not verify at line {i}; refusing to rewrite it"));
        }
        let old_head = chain_head(path);
        let mut records = Vec::new();
        let mut rewritten = 0usize;
        for line in content.lines().filter(|l| !l.trim().is_empty()) {
            let parsed: ChainedLine = serde_json::from_str(line).map_err(|e| e.to_string())?;
            let mut v = serde_json::to_value(&parsed.record).map_err(|e| e.to_string())?;
            if erase_json(&mut v, None, needle_lc) {
                rewritten += 1;
                records.push(serde_json::from_value::<ReadReceipt>(v).map_err(|e| e.to_string())?);
            } else {
                records.push(parsed.record);
            }
        }
        if rewritten == 0 {
            return Ok(LedgerRedaction { lines_rewritten: 0, old_head: old_head.clone(), new_head: old_head });
        }
        records.push(ReadReceipt {
            ts_ms: now_ms(),
            principal: "operator".into(),
            method: "redact".into(),
            detail: format!(
                "erased what the person asked to forget from {rewritten} receipt(s); previous head {}",
                old_head.as_deref().unwrap_or("genesis")
            ),
            results: rewritten,
            purpose: None,
            suppressed: None,
        });
        let mut prev = "genesis".to_string();
        let mut out = String::new();
        for record in &records {
            let record_json = serde_json::to_string(record).map_err(|e| e.to_string())?;
            let mut hasher = Sha256::new();
            hasher.update(prev.as_bytes());
            hasher.update(record_json.as_bytes());
            prev = format!("{:x}", hasher.finalize());
            out.push_str(&format!("{{\"chain\":\"{prev}\",\"record\":{record_json}}}\n"));
        }
        let tmp = path.with_extension("jsonl.redact-tmp");
        {
            let mut f = std::fs::File::create(&tmp).map_err(|e| e.to_string())?;
            f.write_all(out.as_bytes()).map_err(|e| e.to_string())?;
            f.sync_all().map_err(|e| e.to_string())?;
        }
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        *head = Some(prev.clone());
        Ok(LedgerRedaction { lines_rewritten: rewritten, old_head, new_head: Some(prev) })
    }
}

/// The current chain head (last line's chain value), or None for a missing/empty ledger.
pub fn chain_head(path: &Path) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let last = content.lines().rev().find(|l| !l.trim().is_empty())?;
    let parsed: ChainedLine = serde_json::from_str(last).ok()?;
    Some(parsed.chain)
}

/// Recompute the chain line-by-line. Ok(n) = n valid records; Err(i) = the
/// first line index (0-based) whose chain value does not verify.
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
        let expect = format!("{:x}", hasher.finalize());
        if expect != parsed.chain {
            return Err(i);
        }
        prev = parsed.chain;
        n += 1;
    }
    Ok(n)
}

/// Deserialize all receipts (chain not verified — pair with `verify_ledger`).
pub fn read_ledger(path: &Path) -> Vec<ReadReceipt> {
    let Ok(content) = std::fs::read_to_string(path) else {
        return vec![];
    };
    content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<ChainedLine>(l).ok())
        .map(|c| c.record)
        .collect()
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique per RUN and removed when the test ends (E.SCRATCH1).
    fn scratch(tag: &str) -> mind_types::scratch::Scratch {
        mind_types::scratch::file(&format!("receipts_{tag}"), "jsonl")
    }

    fn receipt(method: &str, detail: &str) -> ReadReceipt {
        ReadReceipt {
            ts_ms: now_ms(),
            principal: "private:member".into(),
            method: method.into(),
            detail: detail.into(),
            results: 2,
            purpose: Some("conversation→member:member".into()),
            suppressed: None,
        }
    }

    #[test]
    fn appends_chain_and_verifies() {
        let path = scratch("ok");
        let ledger = ReadReceiptLedger {
            path: Some(path.to_path_buf()),
            head: Mutex::new(None),
        };
        ledger.append(receipt("recall_typed", "safe combination"));
        ledger.append(receipt("beliefs_matching", "gift"));
        ledger.append(receipt("conflicts", ""));
        assert_eq!(verify_ledger(&path), Ok(3));
        let rs = read_ledger(&path);
        assert_eq!(rs.len(), 3);
        assert_eq!(rs[0].method, "recall_typed");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn tamper_breaks_the_chain() {
        let path = scratch("tamper");
        let ledger = ReadReceiptLedger {
            path: Some(path.to_path_buf()),
            head: Mutex::new(None),
        };
        ledger.append(receipt("recall_typed", "one"));
        ledger.append(receipt("recall_typed", "two"));
        // Rewrite record #1's detail without recomputing the chain: verify must flag line 0.
        let content = std::fs::read_to_string(&path).unwrap();
        let tampered = content.replacen("one", "own", 1);
        std::fs::write(&path, tampered).unwrap();
        assert!(
            verify_ledger(&path).is_err(),
            "tampered ledger must not verify"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn head_survives_reopen() {
        let path = scratch("reopen");
        {
            let ledger = ReadReceiptLedger {
                path: Some(path.to_path_buf()),
                head: Mutex::new(None),
            };
            ledger.append(receipt("recall_typed", "first"));
        }
        // A NEW ledger handle (fresh process) must chain off the persisted head, not genesis.
        let ledger2 = ReadReceiptLedger {
            path: Some(path.to_path_buf()),
            head: Mutex::new(None),
        };
        ledger2.append(receipt("recall_typed", "second"));
        assert_eq!(verify_ledger(&path), Ok(2));
        let _ = std::fs::remove_file(&path);
    }

    /// E.ERASE1: a recall's query carried the secret into a receipt. Redacting erases it from the
    /// file, re-chains so the ledger still verifies, keeps the near miss, and appends a redaction
    /// receipt naming the old head -- not the secret.
    #[test]
    fn a_redaction_erases_the_literal_and_the_ledger_still_verifies() {
        let s = scratch("redact");
        let path = s.path().to_path_buf();
        let ledger = ReadReceiptLedger { path: Some(path.clone()), head: Mutex::new(None) };
        ledger.append(receipt("recall_typed", "family birthdays"));
        ledger.append(receipt("recall_typed", "storage locker code QX7-Canary-4417"));
        ledger.append(receipt("recall_typed", "gym locker QX7-CANARY-4418"));
        let old_head = chain_head(&path);
        assert_eq!(verify_ledger(&path), Ok(3));
        let r = ledger.redact("qx7-canary-4417").unwrap();
        assert_eq!(r.lines_rewritten, 1);
        assert_eq!(r.old_head, old_head);
        assert_eq!(verify_ledger(&path), Ok(4), "the rewritten ledger does not verify");
        let bytes = std::fs::read_to_string(&path).unwrap().to_ascii_lowercase();
        assert!(!bytes.contains("qx7-canary-4417"), "the literal survived in the ledger");
        assert!(bytes.contains("qx7-canary-4418"), "the near miss was erased too");
        let all = read_ledger(&path);
        let last = all.last().unwrap();
        assert_eq!(last.method, "redact");
        assert!(last.detail.contains(old_head.as_deref().unwrap()), "{}", last.detail);
        assert!(!last.detail.to_ascii_lowercase().contains("qx7-canary"), "the redaction line names the secret");
        // Appends after a redaction chain onto the new head.
        ledger.append(receipt("recall_typed", "after"));
        assert_eq!(verify_ledger(&path), Ok(5));
        assert_eq!(ledger.redact("nothing-like-this").unwrap().lines_rewritten, 0);
    }

    /// A ledger that does not verify is refused, not "redacted" into verifying again.
    #[test]
    fn a_broken_ledger_is_not_laundered_by_a_redaction() {
        let s = scratch("redact_broken");
        let path = s.path().to_path_buf();
        let ledger = ReadReceiptLedger { path: Some(path.clone()), head: Mutex::new(None) };
        ledger.append(receipt("recall_typed", "one QX7-CANARY-4417"));
        ledger.append(receipt("recall_typed", "two"));
        let tampered = std::fs::read_to_string(&path).unwrap().replacen("\"two\"", "\"TWO\"", 1);
        std::fs::write(&path, tampered).unwrap();
        assert!(verify_ledger(&path).is_err());
        assert!(ledger.redact("qx7-canary-4417").is_err(), "a broken ledger was rewritten");
        assert!(std::fs::read_to_string(&path).unwrap().contains("QX7-CANARY-4417"), "it was changed anyway");
    }
}
