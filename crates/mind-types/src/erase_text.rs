//! E.ERASE1: the one rule for erasing a literal from text, shared by the store and by every
//! ledger beside it -- so "forgotten" means the same in each place it is applied.
//!
//! Matching is ASCII case-insensitive (sqlite's `lower()` is ASCII-only, so the store's own search
//! agrees with this). JSON is rewritten as JSON: only string VALUES under content keys change, never
//! a key, never the value of a structural key (a type, a kind, an id, a clock) -- rewriting those
//! would change what a record means, not what it says.

/// What a forgotten literal is replaced with.
pub const ERASED: &str = "[forgotten]";

/// `hay` with every ASCII-case-insensitive occurrence of `needle_lc` replaced by `with`.
/// `to_ascii_lowercase` keeps every byte offset, so positions found in the lowered copy are
/// positions (and char boundaries) in the original.
pub fn replace_ci(hay: &str, needle_lc: &str, with: &str) -> String {
    if needle_lc.is_empty() {
        return hay.to_string();
    }
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

/// Field names that hold identity, keys, clocks or enums -- never the person's text.
const STRUCTURAL: &[&str] = &[
    "rid", "node_id", "op_id", "op_type", "target_rid", "hlc", "origin_actor", "kind", "type",
    "consolidation_status", "domain", "namespace", "status", "key", "id", "session_id",
    "fingerprint", "embedding_hash", "actor_id", "source_turn", "memory_type", "provenance",
    "idempotency_key", "certainty", "polarity", "valence", "trace_id", "lane", "chain",
];

/// Is `name` (a column or a JSON key) one of the store's own fields rather than content?
pub fn is_structural(name: &str) -> bool {
    let c = name.to_ascii_lowercase();
    STRUCTURAL.contains(&c.as_str())
        || c.ends_with("_id")
        || c.ends_with("_ids")
        || c.ends_with("_rid")
        || c.ends_with("_hash")
        || c.ends_with("_fingerprint")
        || c.ends_with("_ms")
        || c.ends_with("_at")
}

/// Erase `needle_lc` from the content strings of a JSON value, in place. Whether anything changed.
pub fn erase_json(v: &mut serde_json::Value, key: Option<&str>, needle_lc: &str) -> bool {
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

/// The erased form of one stored text, or `None` when nothing of the person's is in it (absent, or
/// only in structural JSON fields). JSON objects and arrays are rewritten as JSON.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replace_ci_takes_every_case_and_keeps_the_rest() {
        assert_eq!(replace_ci("Code qx7-canary-4417 / QX7-CANARY-4417!", "qx7-canary-4417", "[x]"), "Code [x] / [x]!");
        assert_eq!(replace_ci("héllo SECRET wörld", "secret", "[x]"), "héllo [x] wörld");
        assert_eq!(replace_ci("nothing here", "secret", "[x]"), "nothing here");
    }

    #[test]
    fn json_keeps_its_own_fields_and_loses_the_persons_text() {
        let cell = r#"{"type":"episodic","text":"my code is QX7-CANARY-4417","trace_id":"qx7-canary-4417-x"}"#;
        let erased: serde_json::Value = serde_json::from_str(&erase_cell(cell, "qx7-canary-4417").unwrap()).unwrap();
        assert_eq!(erased["type"], "episodic");
        assert_eq!(erased["text"], "my code is [forgotten]");
        assert_eq!(erased["trace_id"], "qx7-canary-4417-x", "an id is the store's own, not the person's text");
        assert_eq!(erase_cell(r#"{"type":"episodic"}"#, "episodic"), None, "only structural: nothing to erase");
    }
}

/// What redacting a hash-chained ledger did. `old_head` is recorded in the ledger's own redaction
/// line, so an auditor holding the old head can tell a sanctioned redaction from tampering.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct LedgerRedaction {
    /// Lines whose text held the literal and was rewritten.
    pub lines_rewritten: usize,
    /// The chain head before the rewrite (`None`: no ledger, or an empty one).
    pub old_head: Option<String>,
    /// The chain head after it, including the redaction line.
    pub new_head: Option<String>,
}
