use std::{env, fs, process};

use mind_evals::promotion::{evaluate_promotion, PromotionCase};

fn main() {
    let path = env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: promotion_gate <promotion-case.json>");
        process::exit(64);
    });
    let raw = fs::read_to_string(&path).unwrap_or_else(|error| {
        eprintln!("promotion gate could not read {path}: {error}");
        process::exit(65);
    });
    let case: PromotionCase = serde_json::from_str(&raw).unwrap_or_else(|error| {
        eprintln!("promotion gate rejected invalid evidence: {error}");
        process::exit(65);
    });
    let decision = evaluate_promotion(&case);
    println!(
        "{}",
        serde_json::to_string_pretty(&decision).expect("promotion decision is serializable")
    );
    if !decision.eligible {
        process::exit(2);
    }
}
