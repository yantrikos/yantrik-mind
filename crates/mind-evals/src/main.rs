//! `cargo run -p mind-evals` — run the behavioral suite, print a scorecard, and append the score
//! to evals_history.jsonl so we can watch the mind's quality trend up over commits. Exits non-zero
//! on any failure so it doubles as a gate.

use std::io::Write;

#[tokio::main]
async fn main() {
    let card = mind_evals::run_suite(&mind_evals::standard_suite()).await;
    print!("{}", card.render());

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let line = serde_json::json!({
        "ts": ts, "commit": commit,
        "passed": card.passed, "total": card.total, "score": card.score,
    });
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open("evals_history.jsonl") {
        let _ = writeln!(f, "{line}");
    }

    if card.passed != card.total {
        std::process::exit(1);
    }
}
