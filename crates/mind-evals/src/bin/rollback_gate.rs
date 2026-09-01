use std::{env, fs, process};

use mind_evals::promotion::{evaluate_rollback, RollbackCase};

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: rollback_gate <rollback-case.json>");
        process::exit(64);
    });
    let raw = fs::read_to_string(&path).unwrap_or_else(|error| {
        eprintln!("rollback gate could not read {path}: {error}");
        process::exit(65);
    });
    let case: RollbackCase = serde_json::from_str(&raw).unwrap_or_else(|error| {
        eprintln!("rollback gate rejected invalid evidence: {error}");
        process::exit(65);
    });
    let decision = evaluate_rollback(&case);
    println!(
        "{}",
        serde_json::to_string_pretty(&decision).expect("rollback decision is serializable")
    );
    if decision.rollback_required {
        process::exit(2);
    }
}
