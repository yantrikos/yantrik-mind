//! E.ERASE1: forgetting ERASES (Pranab, 2026-10-01).
//!
//! The engine's `forget` tombstones a row and keeps its text; the oplog keeps the text as its
//! replication stream; the Mind's own proposition-keyed tables keep it; the file's free pages and
//! the WAL keep it after all of those are rewritten. Measured on staging (ledger, E.ERASE1 Phase
//! A): one told secret landed in seven columns and three files, and `forget-belief` answered
//! "None remain" with every copy still there.
//!
//! This module is the part of the erase that is about the STORE, not about beliefs:
//! - [`sweep`] finds every content cell holding a literal (any table, any text column), and either
//!   counts them (a dry run, for the confirmation the person sees) or rewrites the literal to
//!   [`ERASED`];
//! - [`compact`] lets go of the bytes: the full-text index is optimised (its deleted entries
//!   merged away), the WAL truncated, the file vacuumed;
//! - [`raw_byte_hits`] is the proof: the literal's occurrences in the raw bytes of the database
//!   file and its WAL. The erase reports THAT, never its own tally (K15, E.FORGET1).
//!
//! Matching is ASCII case-insensitive, the same way sqlite's `lower()` is: a model that wrote the
//! code in lower case still had the code.

use std::collections::BTreeMap;

/// What a forgotten literal is replaced with, wherever it stood.
pub const ERASED: &str = "[forgotten]";

/// The shortest literal an erase takes. Shorter strings are words, not secrets, and a sweep for a
/// word rewrites every sentence that uses it.
pub const MIN_NEEDLE: usize = 4;

/// Columns that hold identity, keys, clocks or enums -- never content. A literal that happens to
/// match one of these is not the person's text, and rewriting it would corrupt the store.
const STRUCTURAL: &[&str] = &[
    "rid", "node_id", "op_id", "op_type", "target_rid", "hlc", "origin_actor", "kind", "type",
    "consolidation_status", "domain", "namespace", "status", "key", "id", "session_id",
    "fingerprint", "embedding_hash", "actor_id", "source_turn", "memory_type", "provenance",
    "idempotency_key", "certainty", "polarity", "valence",
];

/// Tables that are the engine's own bookkeeping, never content.
const STRUCTURAL_TABLES: &[&str] = &["meta"];

/// The needle, checked and lowered; an error says why it is refused.
pub fn check_needle(needle: &str) -> Result<String, String> {
    let n = needle.trim();
    if n.chars().count() < MIN_NEEDLE {
        return Err(format!(
            "give me the exact text to erase, at least {MIN_NEEDLE} characters (a shorter string is a word, not a secret)"
        ));
    }
    if n.contains(ERASED) {
        return Err(format!("`{ERASED}` is what erased text becomes; it cannot itself be erased"));
    }
    Ok(n.to_ascii_lowercase())
}

/// `hay` with every ASCII-case-insensitive occurrence of `needle_lc` replaced by `with`.
/// `to_ascii_lowercase` keeps every byte offset, so positions found in the lowered copy are
/// positions (and char boundaries) in the original.
pub fn replace_ci(hay: &str, needle_lc: &str, with: &str) -> String {
    let lc = hay.to_ascii_lowercase();
    let mut out = String::with_capacity(hay.len());
    let mut i = 0;
    while let Some(p) = lc[i..].find(needle_lc) {
        out.push_str(&hay[i..i + p]);
        out.push_str(with);
        i += p + needle_lc.len();
    }
    out.push_str(&hay[i..]);
    out
}

/// What a sweep found (dry run) or rewrote (applied), by `table.column`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct SweepCounts {
    pub cells: BTreeMap<String, usize>,
    /// Rows deleted because their rewritten form collided with a row already there.
    pub rows_deleted: usize,
}

impl SweepCounts {
    pub fn total(&self) -> usize {
        self.cells.values().sum()
    }
}

/// The erased form of one cell. A JSON object or array (the oplog's payloads, a node's payload) is
/// rewritten as JSON: only string values under content keys change, never keys, never the values
/// of structural keys (a record op's `"type":"episodic"` is the store's own field, and rewriting it
/// would change what the operation replays as). Anything else is plain text.
/// `None` when there is nothing of the person's to erase in it (the literal is absent, or stands
/// only in structural JSON fields).
pub fn erase_cell(value: &str, needle_lc: &str) -> Option<String> {
    let trimmed = value.trim_start();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(value) {
            return erase_json(&mut v, None, needle_lc).then(|| v.to_string());
        }
    }
    let erased = replace_ci(value, needle_lc, ERASED);
    (erased != value).then_some(erased)
}

/// Whether anything was replaced.
fn erase_json(v: &mut serde_json::Value, key: Option<&str>, needle_lc: &str) -> bool {
    match v {
        serde_json::Value::String(s) => {
            if key.is_some_and(is_structural) || !s.to_ascii_lowercase().contains(needle_lc) {
                return false;
            }
            *s = replace_ci(s, needle_lc, ERASED);
            true
        }
        serde_json::Value::Array(items) => items.iter_mut().fold(false, |any, i| erase_json(i, key, needle_lc) | any),
        serde_json::Value::Object(map) => {
            map.iter_mut().fold(false, |any, (k, child)| erase_json(child, Some(k.as_str()), needle_lc) | any)
        }
        _ => false,
    }
}

fn is_structural(col: &str) -> bool {
    let c = col.to_ascii_lowercase();
    STRUCTURAL.contains(&c.as_str())
        || c.ends_with("_id")
        || c.ends_with("_rid")
        || c.ends_with("_hash")
        || c.ends_with("_ms")
        || c.ends_with("_at")
}

/// The content tables: ordinary tables, minus sqlite's own, the engine's bookkeeping, every
/// virtual table and every virtual table's shadow tables (an external-content FTS index follows
/// its content table through triggers; its shadows are rewritten by `optimize`, never by hand).
fn content_tables(conn: &rusqlite::Connection) -> Result<(Vec<String>, Vec<String>), String> {
    let all: Vec<(String, String)> = conn
        .prepare("SELECT name, COALESCE(sql, '') FROM sqlite_master WHERE type = 'table'")
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|e| e.to_string())?;
    let virtuals: Vec<String> = all
        .iter()
        .filter(|(_, sql)| sql.to_ascii_uppercase().starts_with("CREATE VIRTUAL TABLE"))
        .map(|(n, _)| n.clone())
        .collect();
    let tables = all
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| !n.starts_with("sqlite_"))
        .filter(|n| !STRUCTURAL_TABLES.contains(&n.as_str()))
        .filter(|n| !virtuals.iter().any(|v| n == v || n.starts_with(&format!("{v}_"))))
        .collect();
    Ok((tables, virtuals))
}

/// Every content cell holding `needle_lc` (ASCII case-insensitive): counted when `apply` is
/// false, rewritten to [`ERASED`] when it is true. A rewrite that collides with a key already
/// there deletes the row instead -- it duplicated one that is already erased.
pub fn sweep(conn: &rusqlite::Connection, needle_lc: &str, apply: bool) -> Result<SweepCounts, String> {
    let (tables, _) = content_tables(conn)?;
    let mut out = SweepCounts::default();
    for t in tables {
        let cols: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{t}\")"))
            .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(1))?.collect::<Result<Vec<_>, _>>())
            .map_err(|e| e.to_string())?;
        for c in cols.iter().filter(|c| !is_structural(c)) {
            let found: Vec<(i64, String)> = conn
                .prepare(&format!(
                    "SELECT rowid, \"{c}\" FROM \"{t}\" WHERE typeof(\"{c}\") = 'text' AND instr(lower(\"{c}\"), ?1) > 0"
                ))
                .and_then(|mut s| {
                    s.query_map([needle_lc], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                        .collect::<Result<Vec<_>, _>>()
                })
                .map_err(|e| format!("{t}.{c}: {e}"))?;
            if found.is_empty() {
                continue;
            }
            // A cell counts only if erasing it would change it: the literal standing in a JSON
            // payload's structural field is the store's own, and is neither rewritten nor counted
            // as content (it still shows in the raw-byte count, which is the honest total).
            let found: Vec<(i64, String, String)> = found
                .into_iter()
                .filter_map(|(rowid, value)| erase_cell(&value, needle_lc).map(|erased| (rowid, value, erased)))
                .collect();
            if found.is_empty() {
                continue;
            }
            *out.cells.entry(format!("{t}.{c}")).or_default() += found.len();
            if !apply {
                continue;
            }
            for (rowid, _value, erased) in found {
                let updated = conn.execute(
                    &format!("UPDATE \"{t}\" SET \"{c}\" = ?1 WHERE rowid = ?2"),
                    rusqlite::params![erased, rowid],
                );
                match updated {
                    Ok(_) => {}
                    Err(rusqlite::Error::SqliteFailure(e, _)) if e.code == rusqlite::ErrorCode::ConstraintViolation => {
                        conn.execute(&format!("DELETE FROM \"{t}\" WHERE rowid = ?1"), [rowid])
                            .map_err(|e| format!("{t}.{c}: {e}"))?;
                        out.rows_deleted += 1;
                    }
                    Err(e) => return Err(format!("{t}.{c}: {e}")),
                }
            }
        }
    }
    Ok(out)
}

/// Let go of the bytes: optimise every FTS5 index (merging away the entries of rewritten rows),
/// truncate the WAL, vacuum the file so freed pages are gone, and truncate the WAL again.
pub fn compact(conn: &rusqlite::Connection) -> Result<(), String> {
    let (_, virtuals) = content_tables(conn)?;
    for v in virtuals {
        // Only FTS5 tables take 'optimize'; anything else refusing it is not an error here.
        let _ = conn.execute(&format!("INSERT INTO \"{v}\"(\"{v}\") VALUES('optimize')"), []);
    }
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);")
        .map_err(|e| e.to_string())
}

/// The proof: occurrences of `needle_lc` (ASCII case-insensitive) in the raw bytes of the
/// database file and its WAL. `None` for an in-memory store, which has no bytes to read.
pub fn raw_byte_hits(db_path: &str, needle_lc: &str) -> Option<usize> {
    if db_path == ":memory:" || db_path.is_empty() {
        return None;
    }
    let needle = needle_lc.as_bytes();
    let mut hits = 0;
    for p in [db_path.to_string(), format!("{db_path}-wal")] {
        if let Ok(bytes) = std::fs::read(&p) {
            let lc = bytes.to_ascii_lowercase();
            hits += lc.windows(needle.len()).filter(|w| *w == needle).count();
        }
    }
    Some(hits)
}

/// What an erase found, did, and -- the part that matters -- left behind.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct EraseReport {
    /// Beliefs tombstoned through the engine before the sweep.
    pub beliefs_tombstoned: usize,
    /// Memories tombstoned through the engine before the sweep.
    pub memories_tombstoned: usize,
    /// Cells rewritten, by `table.column`.
    pub swept: SweepCounts,
    /// AFTER the erase: content cells still holding the literal.
    pub remaining_cells: usize,
    /// AFTER the erase: occurrences in the raw bytes of the file and its WAL (`None`: in memory).
    pub remaining_bytes: Option<usize>,
}

impl EraseReport {
    /// Nothing of the literal is left anywhere this store can see.
    pub fn is_clean(&self) -> bool {
        self.remaining_cells == 0 && self.remaining_bytes.unwrap_or(0) == 0
    }
}
