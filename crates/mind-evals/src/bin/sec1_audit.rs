//! E.SEC1b — the read-only historical audit, with production's own detector.
//!
//! Replaces `tools/sec1_audit.py`, which reimplemented the classification rules in a second
//! language. Codex's objection was exact: a duplicated detector can certify a policy production
//! does not run. Everything here classifies through `mind_types` — the same functions guarding the
//! memory write gate, the flight recorder's redaction, the egress tripwire and the eval export.
//! The walking of files and rows is orchestration; none of it decides what is sensitive.
//!
//! REPORTS COUNTS, KINDS AND RECORD IDENTIFIERS ONLY. No matching value is printed, not truncated,
//! not as an offset into content nobody may read. It opens everything read-only and writes nothing:
//! the decision logs are hash-chained and remediation is a separate, preregistered exercise.
//!
//! THE SENSITIVITY CONTROL. Deleting a second implementation costs something real — the Python twin
//! found the 28 chain-hash false positives — so before scanning anything this runs canaries that
//! MUST be flagged and clean strings that must NOT be. If the control fails it exits **2 =
//! INCONCLUSIVE** rather than reporting a clean bill of health. A negative result must demonstrate
//! its own sensitivity, in the same run, on the same build: an instrument that cannot fail is the
//! same defect as a detector that cannot fire.
//!
//! Exit codes: 0 clean · 1 findings (with `--fail-closed`) · 2 the instrument could not prove itself.

use std::collections::BTreeMap;

/// Known-sensitive inputs, one per kind the detector names. Every one must be flagged.
const CANARIES: &[&str] = &[
    "my password is hunter2",
    "ghp_SECRET12345",
    "-----BEGIN RSA PRIVATE KEY-----",
    "my card is 4471 9302 1122 8890",
    "ssn 123456789",
    "AKIAIOSFODNN7EXAMPLE",
];

/// Known-clean inputs. Every one must pass. These are the false positives the old detector had —
/// it refused "asian food recipes" while admitting a password — so they are the control's control.
const CLEAN: &[&str] = &[
    "asian food recipes",
    "the task list for Tuesday",
    "how do passwords work?",
    "where is order 100000000000",
    "what happened at 1756170000000",
];

fn control_passes() -> Result<(), String> {
    for c in CANARIES {
        if mind_types::first_sensitive(c).is_none() {
            return Err(format!("a canary was NOT flagged ({} chars) — the detector cannot fire", c.len()));
        }
    }
    for c in CLEAN {
        if let Some(f) = mind_types::first_sensitive(c) {
            return Err(format!("a clean control WAS flagged as {} — the detector cannot pass", f.kind.label()));
        }
    }
    // The pair rule, which the value-only walk depends on.
    if mind_types::sensitive_pair("api_key", "9f2b1c4d8e").is_none() {
        return Err("the key/value rule did not fire on a named key with a token value".into());
    }
    if mind_types::sensitive_pair("note", "9f2b1c4d8e").is_some() {
        return Err("the key/value rule fired on an unnamed key".into());
    }
    Ok(())
}

/// Every kind present in one scalar. Never returns any part of it.
fn kinds_of(text: &str) -> Vec<&'static str> {
    let mut k: Vec<&'static str> = mind_types::sensitive_findings(text).iter().map(|f| f.kind.label()).collect();
    k.sort_unstable();
    k.dedup();
    k
}

/// Walk parsed JSON, judging each scalar AND each key beside its scalar value.
///
/// FIELD-AWARE, never raw-line. A raw scan lets a digit run continue across JSON punctuation and
/// manufacture a card-shaped number out of two unrelated fields, which is what a first pass
/// appeared to find. The key/value pass is the other half: `{"api_key": "9f2b1c4d8e"}` has nothing
/// sensitive in either half alone, so a value-only walk reports it clean (Codex point 2).
fn walk(node: &serde_json::Value, out: &mut Vec<&'static str>) {
    match node {
        serde_json::Value::String(s) => out.extend(kinds_of(s)),
        serde_json::Value::Number(n) => out.extend(kinds_of(&n.to_string())),
        serde_json::Value::Array(a) => a.iter().for_each(|v| walk(v, out)),
        serde_json::Value::Object(map) => {
            for (k, v) in map {
                if let Some(scalar) = match v {
                    serde_json::Value::String(s) => Some(s.clone()),
                    serde_json::Value::Number(n) => Some(n.to_string()),
                    _ => None,
                } {
                    if let Some(f) = mind_types::sensitive_pair(k, &scalar) {
                        out.push(f.kind.label());
                    }
                }
                walk(v, out);
            }
        }
        _ => {}
    }
}

fn kinds_of_json_line(line: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    match serde_json::from_str::<serde_json::Value>(line) {
        Ok(v) => walk(&v, &mut found),
        // A line that will not parse is scanned as text — assuming a schema that does not apply
        // would report a clean bill of health for a file the audit never looked inside.
        Err(_) => found.extend(kinds_of(line)),
    }
    found.sort_unstable();
    found.dedup();
    found
}

struct Report {
    scanned: usize,
    per_kind: BTreeMap<&'static str, usize>,
    flagged: Vec<(String, Vec<&'static str>)>,
}

impl Report {
    fn new() -> Self {
        Report { scanned: 0, per_kind: BTreeMap::new(), flagged: Vec::new() }
    }
    fn record(&mut self, id: String, kinds: Vec<&'static str>) {
        self.scanned += 1;
        if kinds.is_empty() {
            return;
        }
        for k in &kinds {
            *self.per_kind.entry(k).or_insert(0) += 1;
        }
        self.flagged.push((id, kinds));
    }
    fn print(&self, path: &str, unit: &str) {
        println!("{path}");
        println!("  {unit} scanned : {}", self.scanned);
        println!("  {unit} flagged : {}", self.flagged.len());
        for (k, n) in &self.per_kind {
            println!("    {k:22} {n}");
        }
        for (id, kinds) in self.flagged.iter().take(20) {
            println!("    id={id} kinds={}", kinds.join(","));
        }
        if self.flagged.len() > 20 {
            println!("    … {} more flagged records not listed", self.flagged.len() - 20);
        }
        println!();
    }
}

fn audit_jsonl(path: &str) -> std::io::Result<Report> {
    use std::io::BufRead;
    let f = std::fs::File::open(path)?;
    let mut rep = Report::new();
    for (i, line) in std::io::BufReader::new(f).lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            // Invalid UTF-8 is itself worth seeing rather than skipping silently.
            Err(_) => {
                rep.record(format!("line:{}", i + 1), vec!["unreadable-line"]);
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        rep.record(format!("line:{}", i + 1), kinds_of_json_line(&line));
    }
    Ok(rep)
}

fn audit_db(path: &str) -> Result<Report, String> {
    let conn = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )
    .map_err(|e| e.to_string())?;
    let mut stmt = conn.prepare("SELECT rid, text FROM memories").map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))
        .map_err(|e| e.to_string())?;
    let mut rep = Report::new();
    for row in rows {
        let (rid, text) = row.map_err(|e| e.to_string())?;
        rep.record(rid, kinds_of(&text));
    }
    Ok(rep)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fail_closed = args.iter().any(|a| a == "--fail-closed");
    let paths: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();

    println!("E.SEC1b READ-ONLY AUDIT — production's detector; counts, kinds and record ids only\n");

    // The instrument proves itself BEFORE it is trusted to report anything.
    match control_passes() {
        Ok(()) => println!("sensitivity control: PASS ({} canaries flagged, {} clean controls passed)\n", CANARIES.len(), CLEAN.len()),
        Err(why) => {
            println!("sensitivity control: FAIL — {why}");
            println!("\nINCONCLUSIVE. This run proves nothing about the files it was pointed at.");
            std::process::exit(2);
        }
    }

    let mut any = false;
    for path in paths {
        if !std::path::Path::new(path).exists() {
            println!("{path}: absent\n");
            continue;
        }
        let rep = if path.ends_with(".jsonl") {
            match audit_jsonl(path) {
                Ok(r) => {
                    r.print(path, "lines");
                    r
                }
                Err(e) => {
                    println!("{path}\n  unreadable: {e}\n");
                    continue;
                }
            }
        } else {
            match audit_db(path) {
                Ok(r) => {
                    r.print(path, "memories");
                    r
                }
                Err(e) => {
                    println!("{path}\n  unreadable: {e}\n");
                    continue;
                }
            }
        };
        any |= !rep.flagged.is_empty();
    }

    if any && fail_closed {
        println!("FINDINGS PRESENT — exiting non-zero (fail-closed).");
        std::process::exit(1);
    }
}
