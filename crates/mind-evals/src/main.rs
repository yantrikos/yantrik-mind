//! `cargo run -p mind-evals` — run the behavioral suite, print a scorecard, and append the score
//! to evals_history.jsonl so we can watch the mind's quality trend up over commits. Exits non-zero
//! on any failure so it doubles as a gate.
//!
//! `mind-evals immune --db <cold-snapshot.db> [--pairs N] [--ledger PATH] [--summary PATH]
//! [--holdout fam1,fam2] [--critic null|api]` — run one seeded-belief immune trial.
//! IMPORTANT: `--db` must be a COLD copy (the nightly backup) — never the live db of a running
//! mind (`MemoryHandle::spawn` performs schema writes and would contend for the file lock).
//! The api critic reads YM_CRITIC_URL / YM_CRITIC_MODEL (/ YM_CRITIC_KEY) and MUST point at a
//! local-only endpoint: belief text never leaves home hardware.

use std::io::Write;

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("immune") {
        std::process::exit(immune_cli(&args[1..]).await);
    }
    if args.first().map(String::as_str) == Some("competitive") {
        std::process::exit(competitive_cli(&args[1..]));
    }
    // `mind-evals loops` — the promotion-gate scoreboard: the same goals through BOTH loops,
    // outcome grades plus model-call counts side by side.
    if args.first().map(String::as_str) == Some("loops") {
        let rows = mind_evals::loop_compare::run_pairs(&mind_evals::loop_compare::pairs()).await;
        print!("{}", mind_evals::loop_compare::render(&rows));
        let ok = rows
            .iter()
            .all(|r| r.legacy.passed == r.legacy.total && r.cognitive.passed == r.cognitive.total);
        std::process::exit(if ok { 0 } else { 1 });
    }

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
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open("evals_history.jsonl")
    {
        let _ = writeln!(f, "{line}");
    }

    if card.passed != card.total {
        std::process::exit(1);
    }
}

fn competitive_cli(args: &[String]) -> i32 {
    use mind_evals::competitive;

    let manifest = competitive::manifest();
    match args.first().map(String::as_str) {
        Some("manifest") => {
            let envelope = serde_json::json!({
                "manifest_sha256": competitive::manifest_sha256(&manifest),
                "manifest": manifest,
            });
            match serde_json::to_string_pretty(&envelope) {
                Ok(json) => {
                    println!("{json}");
                    0
                }
                Err(error) => {
                    eprintln!("serialize competitive manifest: {error}");
                    1
                }
            }
        }
        Some("template") => {
            match serde_json::to_string_pretty(&competitive::empty_submission(&manifest)) {
                Ok(json) => {
                    println!("{json}");
                    0
                }
                Err(error) => {
                    eprintln!("serialize competitive template: {error}");
                    1
                }
            }
        }
        Some("mind-preflight") => run_mind_preflight(args.iter().any(|arg| arg == "--json")),
        Some("report") => {
            let Some(path) = args.get(1) else {
                eprintln!("usage: mind-evals competitive report <submission.json> [--json] [--require-rankable]");
                return 2;
            };
            let bytes = match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) => {
                    eprintln!("read competitive submission {path}: {error}");
                    return 2;
                }
            };
            let submission: competitive::CompetitiveSubmission =
                match serde_json::from_slice(&bytes) {
                    Ok(submission) => submission,
                    Err(error) => {
                        eprintln!("parse competitive submission {path}: {error}");
                        return 2;
                    }
                };
            let report = competitive::evaluate(&manifest, &submission);
            if args.iter().any(|arg| arg == "--json") {
                match serde_json::to_string_pretty(&report) {
                    Ok(json) => println!("{json}"),
                    Err(error) => {
                        eprintln!("serialize competitive report: {error}");
                        return 1;
                    }
                }
            } else {
                print!("{}", report.render());
            }
            if !report.validation_errors.is_empty() {
                3
            } else if args.iter().any(|arg| arg == "--require-rankable")
                && report
                    .systems
                    .iter()
                    .any(|system| !system.eligible_for_ranking)
            {
                4
            } else {
                0
            }
        }
        _ => {
            eprintln!(
                "usage: mind-evals competitive <manifest|template|mind-preflight [--json]|report <submission.json> [--json] [--require-rankable]>"
            );
            2
        }
    }
}

fn run_mind_preflight(json: bool) -> i32 {
    use sha2::{Digest, Sha256};
    use std::process::Command;
    use std::time::Instant;

    let mut cases = Vec::new();
    let mut failed = false;
    for case in mind_evals::competitive::mind_preflight_spec() {
        let mut targets = Vec::new();
        for target in &case.targets {
            let started = Instant::now();
            let result = Command::new("cargo")
                .args([
                    "test",
                    "-p",
                    target.package.as_str(),
                    target.filter.as_str(),
                    "--locked",
                ])
                .output();
            let (passed, exit_code, output_sha256, error) = match result {
                Ok(output) => {
                    let mut evidence = output.stdout;
                    evidence.extend_from_slice(&output.stderr);
                    (
                        output.status.success(),
                        output.status.code(),
                        format!("{:x}", Sha256::digest(evidence)),
                        None,
                    )
                }
                Err(error) => (false, None, String::new(), Some(error.to_string())),
            };
            failed |= !passed;
            targets.push(serde_json::json!({
                "package": target.package,
                "filter": target.filter,
                "passed": passed,
                "exit_code": exit_code,
                "duration_ms": started.elapsed().as_millis(),
                "output_sha256": output_sha256,
                "error": error,
            }));
        }
        cases.push(serde_json::json!({
            "case_id": case.case_id,
            "coverage": case.coverage,
            "all_targets_passed": !case.targets.is_empty()
                && targets.iter().all(|target| target["passed"] == true),
            "targets": targets,
            "limitation": case.limitation,
        }));
    }
    let exact_cases = cases
        .iter()
        .filter(|case| case["coverage"] == "exact" && case["all_targets_passed"] == true)
        .count();
    let payload = serde_json::json!({
        "benchmark_id": mind_evals::competitive::BENCHMARK_ID,
        "manifest_sha256": mind_evals::competitive::manifest_sha256(
            &mind_evals::competitive::manifest()
        ),
        "adapter": "mind-local-preflight-v1",
        "competitive_grades_emitted": false,
        "exact_cases": exact_cases,
        "required_cases": cases.len(),
        "cases": cases,
    });
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&payload).unwrap_or_default()
        );
    } else {
        println!(
            "MIND COMPETITIVE PREFLIGHT — {exact_cases}/{} exact frozen cases",
            cases.len()
        );
        for case in &cases {
            let status = if case["all_targets_passed"] == true {
                "checks pass"
            } else if case["targets"]
                .as_array()
                .is_some_and(|targets| targets.is_empty())
            {
                "no exact fixture"
            } else {
                "CHECK FAILED"
            };
            println!(
                "  {}: {} · {}",
                case["case_id"].as_str().unwrap_or("unknown"),
                case["coverage"].as_str().unwrap_or("unknown"),
                status
            );
        }
        println!("No competitive grades emitted; partial checks cannot become wins.");
    }
    if failed {
        1
    } else {
        0
    }
}

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// True iff the URL's host parses as a loopback or RFC1918 IPv4 literal.
fn host_is_local_ip(url: &str) -> bool {
    let after = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => return false,
    };
    let host = after.split(['/', ':']).next().unwrap_or("");
    match host.parse::<std::net::Ipv4Addr>() {
        Ok(ip) => ip.is_loopback() || ip.is_private(),
        Err(_) => false,
    }
}

async fn immune_cli(args: &[String]) -> i32 {
    use mind_evals::immune;
    use mind_types::MemoryFacade;

    let Some(db) = flag(args, "--db") else {
        eprintln!("usage: mind-evals immune --db <cold-snapshot.db> [--pairs N] [--ledger PATH] [--summary PATH] [--holdout fam1,fam2] [--critic null|api]");
        return 2;
    };
    let pairs: usize = flag(args, "--pairs")
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let ledger = std::path::PathBuf::from(
        flag(args, "--ledger").unwrap_or_else(|| "immune_trials.jsonl".into()),
    );
    let summary_path = flag(args, "--summary");
    let holdout: Vec<String> = flag(args, "--holdout")
        .map(|s| {
            s.split(',')
                .map(|f| f.trim().to_string())
                .filter(|f| !f.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let critic_kind = flag(args, "--critic").unwrap_or_else(|| "null".into());

    // Verify the existing chain BEFORE appending — a broken chain must scream,
    // not silently grow a fresh-looking tail.
    match immune::verify_trial_ledger(&ledger) {
        Ok(_) => {}
        Err(line) => {
            eprintln!("LEDGER CHAIN BROKEN at record {line} — refusing to run. Investigate {ledger:?} first.");
            return 3;
        }
    }
    // And the internally-valid chain must still contain the externally
    // anchored head — internal validity alone cannot detect a valid-prefix
    // truncation. --anchors points at the root-owned chain_heads.log.
    if let Some(anchors) = flag(args, "--anchors") {
        let last_anchor = std::fs::read_to_string(&anchors)
            .ok()
            .and_then(|s| {
                s.lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .map(|l| l.split_whitespace().last().unwrap_or("").to_string())
            })
            .filter(|h| !h.is_empty());
        if let Some(head) = last_anchor {
            if let Err(e) = immune::verify_anchor(&ledger, &head) {
                eprintln!("ANCHOR CHECK FAILED: {e} — refusing to run.");
                return 3;
            }
        }
    }

    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let trial_id = format!("{ts}");

    // Work on the cold snapshot in a scratch copy so even ITS file is never
    // written by us (spawn does schema writes).
    let scratch = std::env::temp_dir().join(format!("ym_immune_cli_{ts}"));
    if let Err(e) = std::fs::create_dir_all(&scratch) {
        eprintln!("scratch dir: {e}");
        return 1;
    }
    let work_db = scratch.join("work.db");
    // VACUUM INTO, not fs::copy — a bare file copy silently drops whatever is
    // still sitting in the -wal file (rediscovered this the hard way).
    if let Err(e) = mind_memory::snapshot_db_to(&db, &work_db.to_string_lossy()) {
        eprintln!("copy cold snapshot: {e}");
        return 1;
    }

    let mem = match mind_memory::MemoryHandle::spawn(&work_db.to_string_lossy(), 64) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("open snapshot: {e:?}");
            return 1;
        }
    };
    let beliefs: Vec<mind_types::Belief> = match mem.export().await {
        Ok(json) => serde_json::from_str(&json).unwrap_or_default(),
        Err(e) => {
            eprintln!("export beliefs: {e:?}");
            return 1;
        }
    };
    println!("population: {} beliefs", beliefs.len());

    // Rotation state: bases used in earlier trials are excluded so the epoch
    // accumulates DISTINCT observations, not repeats of the same 15 seeds.
    let used_path = ledger.with_file_name("immune_used_bases.json");
    let mut used_bases: Vec<String> = std::fs::read_to_string(&used_path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    // Family 3 (trip_dest): ledger-verified controls from the photo-archive
    // trip ledger (sol design). Read from the SNAPSHOT copy like everything else.
    let trip_pairs = if holdout.iter().any(|h| h == "trip_dest") {
        Vec::new()
    } else {
        match mem.profile_get("trips").await {
            Ok(Some(json)) => {
                let preds = immune::trips_to_predicates(&json);
                immune::generate_trip_pairs(&preds, pairs / 3 + 1, &used_bases)
            }
            _ => Vec::new(),
        }
    };

    let Some(mut manifest) = immune::generate_manifest(
        &beliefs,
        &trial_id,
        pairs.saturating_sub(trip_pairs.len()),
        &holdout,
        &used_bases,
    ) else {
        eprintln!(
            "no usable seed pairs left in this population ({} bases already used — the epoch has exhausted this mind's value-bearing beliefs; reset {} to start a new epoch)",
            used_bases.len(),
            used_path.display()
        );
        return 4;
    };
    manifest.pairs.extend(trip_pairs);
    println!(
        "manifest: {} pairs (families: {:?})",
        manifest.pairs.len(),
        {
            let mut f: Vec<&str> = manifest.pairs.iter().map(|p| p.family.as_str()).collect();
            f.dedup();
            f
        }
    );

    let report = match critic_kind.as_str() {
        "api" => {
            let url = std::env::var("YM_CRITIC_URL").unwrap_or_default();
            let model = std::env::var("YM_CRITIC_MODEL").unwrap_or_default();
            if url.is_empty() || model.is_empty() {
                eprintln!(
                    "--critic api needs YM_CRITIC_URL and YM_CRITIC_MODEL (local-only endpoint)"
                );
                return 2;
            }
            // Belief text never leaves home hardware: the endpoint host must
            // BE a loopback/RFC1918 IP literal (hostname prefixes like
            // 127.0.0.1.evil.example are exactly the bypass this refuses).
            if !host_is_local_ip(&url) {
                eprintln!("REFUSING YM_CRITIC_URL={url} — host must be a loopback or RFC1918 IPv4 literal");
                return 2;
            }
            let backend = std::sync::Arc::new(yantrik_ml::ApiLLM::new(
                url,
                std::env::var("YM_CRITIC_KEY").ok(),
                model,
            ));
            let critic = immune::LlmCritic::new(backend);
            immune::run_seed_trial(&mem, 64, &scratch, &manifest, &critic, 0.5).await
        }
        _ => {
            immune::run_seed_trial(
                &mem,
                64,
                &scratch,
                &manifest,
                &immune::NullBaselineCritic,
                0.5,
            )
            .await
        }
    };
    let _ = std::fs::remove_dir_all(&scratch);

    let report = match report {
        Ok(r) => r,
        Err(e) => {
            eprintln!("trial failed: {e}");
            return 1;
        }
    };
    println!(
        "trial {}: critic={} detection {}/{} ({:.0}%), control damage {}/{} ({:.0}%)",
        report.trial_id,
        report.critic,
        report.seeds_flagged,
        report.n_seeds,
        report.detection_rate * 100.0,
        report.controls_flagged,
        report.n_controls,
        report.control_damage_rate * 100.0,
    );

    let head = match immune::append_trial_record(&ledger, &report) {
        Ok(h) => h,
        Err(e) => {
            eprintln!("ledger append: {e}");
            return 1;
        }
    };

    // Persist rotation state only after the trial landed in the ledger.
    for p in &manifest.pairs {
        used_bases.push(p.seed_base.clone());
        used_bases.push(p.control_base.clone());
    }
    used_bases.dedup();
    let _ = std::fs::write(
        &used_path,
        serde_json::to_string_pretty(&used_bases).unwrap_or_default(),
    );

    let all = immune::read_trial_ledger(&ledger);
    let epoch = immune::epoch_summary(&all); // homogeneous by construction: filters to the latest run-config
    println!(
        "epoch: {} trials, {} unique seeds across {} family(ies), detection LB {:.3}, damage UB {:.3}, promotion bar met: {}",
        epoch.trials, epoch.unique_seeds, epoch.families, epoch.detection_lower_bound, epoch.damage_upper_bound, epoch.promotion_bar_met
    );
    println!("chain head: {head}");

    if let Some(sp) = summary_path {
        let summary = serde_json::json!({
            "ts": ts,
            "latest": {
                "trial_id": report.trial_id,
                "critic": report.critic,
                "n_seeds": report.n_seeds,
                "n_controls": report.n_controls,
                "seeds_flagged": report.seeds_flagged,
                "controls_flagged": report.controls_flagged,
                "missed_lies": report.items.iter().filter(|i| i.is_seed && !i.flagged).map(|i| i.statement.clone()).collect::<Vec<_>>(),
                "false_alarms": report.items.iter().filter(|i| !i.is_seed && i.flagged).map(|i| i.statement.clone()).collect::<Vec<_>>(),
                "detection_rate": report.detection_rate,
                "control_damage_rate": report.control_damage_rate,
            },
            "epoch": epoch,
            "chain_head": head,
        });
        if let Err(e) = immune::write_json_atomic(
            std::path::Path::new(&sp),
            &serde_json::to_string_pretty(&summary).unwrap_or_default(),
        ) {
            eprintln!("summary write: {e}");
            return 1;
        }
        println!("summary → {sp}");
    }
    0
}
