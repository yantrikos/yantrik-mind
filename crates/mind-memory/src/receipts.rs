//! Sensitive-read receipts (ARCH-1 slice 2): a hash-chained, append-only JSONL
//! ledger of every PRINCIPAL memory read — who read, through which facade
//! method, what they asked, how many results crossed the boundary. Operator
//! reads (the owner's own system paths) are not recorded; the ledger exists to
//! audit reads that crossed the authorization boundary from a channel.
//!
//! Chain discipline mirrors the immune ledger (mind-evals::immune): each line is
//! `{"chain":"<hex>","record":{…}}` where `chain = sha256(prev_chain_hex ++ record_json)`
//! and the first record chains off the literal `"genesis"`. Any edit, reorder,
//! or deletion of a middle line breaks every later chain value.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// One principal read crossing the authorization boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadReceipt {
    pub ts_ms: u64,
    /// `principal_label()` of the reading context, e.g. "private:wife" | "shared".
    pub principal: String,
    /// Facade method: "recall_typed" | "beliefs_matching" | "reflect" | …
    pub method: String,
    /// The query/needle (truncated) — enough to audit intent, not a data copy.
    pub detail: String,
    /// How many items were returned AFTER scope filtering.
    pub results: usize,
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
            _ if db_path != ":memory:" => Some(PathBuf::from(format!("{db_path}.read_receipts.jsonl"))),
            _ => None,
        };
        Self { path, head: Mutex::new(None) }
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
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        f.write_all(line.as_bytes())?;
        f.sync_all()?;
        *head = Some(chain);
        Ok(())
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
    let Ok(content) = std::fs::read_to_string(path) else { return vec![] };
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

    fn scratch(tag: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("ym_receipts_{tag}_{}.jsonl", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn receipt(method: &str, detail: &str) -> ReadReceipt {
        ReadReceipt { ts_ms: now_ms(), principal: "private:member".into(), method: method.into(), detail: detail.into(), results: 2 }
    }

    #[test]
    fn appends_chain_and_verifies() {
        let path = scratch("ok");
        let ledger = ReadReceiptLedger { path: Some(path.clone()), head: Mutex::new(None) };
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
        let ledger = ReadReceiptLedger { path: Some(path.clone()), head: Mutex::new(None) };
        ledger.append(receipt("recall_typed", "one"));
        ledger.append(receipt("recall_typed", "two"));
        // Rewrite record #1's detail without recomputing the chain: verify must flag line 0.
        let content = std::fs::read_to_string(&path).unwrap();
        let tampered = content.replacen("one", "own", 1);
        std::fs::write(&path, tampered).unwrap();
        assert!(verify_ledger(&path).is_err(), "tampered ledger must not verify");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn head_survives_reopen() {
        let path = scratch("reopen");
        {
            let ledger = ReadReceiptLedger { path: Some(path.clone()), head: Mutex::new(None) };
            ledger.append(receipt("recall_typed", "first"));
        }
        // A NEW ledger handle (fresh process) must chain off the persisted head, not genesis.
        let ledger2 = ReadReceiptLedger { path: Some(path.clone()), head: Mutex::new(None) };
        ledger2.append(receipt("recall_typed", "second"));
        assert_eq!(verify_ledger(&path), Ok(2));
        let _ = std::fs::remove_file(&path);
    }
}
