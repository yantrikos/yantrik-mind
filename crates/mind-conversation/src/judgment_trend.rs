//! judgment_trend — the proof metric: is the mind's judgment getting BETTER over time, on frozen
//! weights? A falling Brier score across months, with the model weights never changing, is the
//! falsifiable form of "wiser without getting smarter". That is the claim worth publishing, so the
//! measurement has to be built so it cannot flatter itself.
//!
//! THE TRAP THIS MODULE EXISTS TO AVOID. Raw Brier falls for two very different reasons:
//!   1. the mind's judgment genuinely improved  (the claim), or
//!   2. the QUESTIONS GOT EASIER — a period whose outcomes are 90% one-way is trivially predictable,
//!      so Brier drops even from a fixed, mediocre forecaster (no wisdom involved at all).
//!
//! A chart of raw Brier cannot tell those apart, and (2) drifts on its own as the mind changes what
//! it chooses to predict about. Publishing (2) as (1) is exactly the failure mode where a
//! plausible-but-wrong story GAINS force because the system remembers so much.
//!
//! So the headline number here is the BRIER SKILL SCORE, not Brier:
//!
//!   uncertainty = base_rate · (1 − base_rate)          ← the difficulty of THAT period
//!   BSS         = 1 − Brier / uncertainty              ← skill ABOVE always guessing the base rate
//!
//! BSS > 0 means the forecasts beat a base-rate-only baseline; BSS ≈ 0 means the mind added nothing
//! a constant guess wouldn't have. Because the baseline is recomputed per period from that period's
//! own outcomes, a period of easier questions raises the baseline too — the skill number stays
//! honest. `easier_questions_do_not_read_as_improvement` in the tests locks that property.
//!
//! Everything here is a pure function over already-parsed ledger rows so the statistics are testable
//! with synthetic data and no database.

/// One graded prediction: the probability asserted at emission, and what actually happened.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Graded {
    /// Emission timestamp (epoch ms) — buckets by WHEN THE CALL WAS MADE, not when it resolved.
    pub t_ms: i64,
    /// Probability asserted at emission, 0..=1.
    pub p: f64,
    /// Observed binary outcome.
    pub hit: bool,
}

/// One time bucket's scorecard.
#[derive(Debug, Clone)]
pub(crate) struct Bucket {
    pub label: String,
    pub n: usize,
    pub brier: f64,
    /// Fraction of outcomes that were true — the period's difficulty.
    pub base_rate: f64,
    /// Skill above a base-rate-only forecaster. `None` when the period is degenerate (base rate 0
    /// or 1 ⇒ uncertainty 0 ⇒ skill undefined; every forecaster "wins" trivially).
    pub bss: Option<f64>,
}

/// What the trend actually supports — deliberately conservative.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verdict {
    /// Not enough graded outcomes to say anything. Carries what's needed vs what's there.
    Insufficient { have: usize, need: usize },
    /// Skill rose by at least `MIN_DELTA` **and** the newer half actually beats a base-rate guess.
    Improving { delta: f64 },
    /// Moved less than `MIN_DELTA` in either direction.
    Flat { delta: f64 },
    /// Skill fell by at least `MIN_DELTA` — the claim is failing, and it must be able to say so.
    Degrading { delta: f64 },
    /// Skill may have risen, but the mind still does not beat a base-rate-only forecaster. Reported
    /// separately because a rising-but-negative skill is the signature of the questions getting
    /// easier rather than the judgment getting better — see the module header. Claiming
    /// "improving" here is precisely how this metric would flatter itself.
    BelowBaseline { skill: f64, delta: f64 },
}

/// Minimum graded predictions in EACH half before a direction is claimed. Below this, sampling noise
/// swamps any real movement — a 6-item "improvement" is a coin flip with a narrative.
pub(crate) const MIN_PER_HALF: usize = 15;
/// Minimum BSS change to call a direction rather than Flat.
pub(crate) const MIN_DELTA: f64 = 0.05;

/// Brier score = mean squared error of the probabilities. Lower is better.
fn brier(rows: &[Graded]) -> f64 {
    if rows.is_empty() {
        return f64::NAN;
    }
    rows.iter()
        .map(|r| (r.p - if r.hit { 1.0 } else { 0.0 }).powi(2))
        .sum::<f64>()
        / rows.len() as f64
}

fn base_rate(rows: &[Graded]) -> f64 {
    if rows.is_empty() {
        return f64::NAN;
    }
    rows.iter().filter(|r| r.hit).count() as f64 / rows.len() as f64
}

/// Brier Skill Score against a base-rate-only baseline for the SAME rows. `None` when the baseline
/// is perfect by construction (all-true or all-false period).
fn skill(rows: &[Graded]) -> Option<f64> {
    let b = base_rate(rows);
    let uncertainty = b * (1.0 - b);
    if uncertainty.is_nan() || uncertainty <= 1e-9 {
        return None; // degenerate period: no spread to have skill over
    }
    Some(1.0 - brier(rows) / uncertainty)
}

fn score(label: String, rows: &[Graded]) -> Bucket {
    Bucket {
        label,
        n: rows.len(),
        brier: brier(rows),
        base_rate: base_rate(rows),
        bss: skill(rows),
    }
}

/// Split graded rows into `n_buckets` consecutive windows of `bucket_days`, oldest first, ending at
/// `now_ms`. Empty buckets are kept so a gap in the record is visible rather than silently closed.
pub(crate) fn buckets(
    rows: &[Graded],
    now_ms: i64,
    bucket_days: i64,
    n_buckets: usize,
) -> Vec<Bucket> {
    let span = bucket_days * 86_400_000;
    (0..n_buckets)
        .map(|i| {
            // i = 0 is the OLDEST window.
            let back = (n_buckets - i) as i64;
            let start = now_ms - back * span;
            let end = start + span;
            let in_win: Vec<Graded> = rows
                .iter()
                .copied()
                .filter(|r| r.t_ms >= start && r.t_ms < end)
                .collect();
            let label = format!("-{}d", back * bucket_days);
            if in_win.is_empty() {
                Bucket {
                    label,
                    n: 0,
                    brier: f64::NAN,
                    base_rate: f64::NAN,
                    bss: None,
                }
            } else {
                score(label, &in_win)
            }
        })
        .collect()
}

/// The direction verdict: compare the OLDER half of the window against the NEWER half by SKILL.
///
/// Halves (rather than a regression slope) are used deliberately — with the tens-of-predictions
/// counts a household companion realistically produces, a slope over sparse noisy buckets invents
/// precision it does not have. Two pooled halves keep the per-side sample as large as possible.
pub(crate) fn verdict(rows: &[Graded], now_ms: i64, window_days: i64) -> Verdict {
    let span = window_days * 86_400_000;
    let start = now_ms - span;
    let mid = now_ms - span / 2;
    let older: Vec<Graded> = rows
        .iter()
        .copied()
        .filter(|r| r.t_ms >= start && r.t_ms < mid)
        .collect();
    let newer: Vec<Graded> = rows
        .iter()
        .copied()
        .filter(|r| r.t_ms >= mid && r.t_ms <= now_ms)
        .collect();
    let have = older.len().min(newer.len());
    if have < MIN_PER_HALF {
        return Verdict::Insufficient {
            have,
            need: MIN_PER_HALF,
        };
    }
    // A degenerate half (all outcomes one way) has no defined skill — refuse to claim a direction
    // rather than fabricate one.
    let (Some(a), Some(b)) = (skill(&older), skill(&newer)) else {
        return Verdict::Insufficient {
            have,
            need: MIN_PER_HALF,
        };
    };
    let delta = b - a;
    // ORDER MATTERS. A decline is always reported first (never hidden behind another state). Then
    // the baseline gate: while the newer half still loses to a base-rate-only guess, no amount of
    // upward delta earns the word "improving" — an unchanged mediocre forecaster shows exactly that
    // signature when the questions get easier (module header + the anti-flattery test).
    if delta <= -MIN_DELTA {
        Verdict::Degrading { delta }
    } else if b <= 0.0 {
        Verdict::BelowBaseline { skill: b, delta }
    } else if delta >= MIN_DELTA {
        Verdict::Improving { delta }
    } else {
        Verdict::Flat { delta }
    }
}

/// Render the trend for the morning board / `ym judgment`. States the metric's own limitation in the
/// insufficient case rather than showing a number that looks like evidence.
pub(crate) fn render(rows: &[Graded], now_ms: i64, bucket_days: i64, n_buckets: usize) -> String {
    let window_days = bucket_days * n_buckets as i64;
    let bs = buckets(rows, now_ms, bucket_days, n_buckets);
    let v = verdict(rows, now_ms, window_days);
    let mut out =
        String::from("📈 Judgment TREND (skill above a base-rate guess; frozen weights)\n");
    for b in &bs {
        if b.n == 0 {
            out.push_str(&format!("   {:>6} · (no graded outcomes)\n", b.label));
        } else {
            let sk = match b.bss {
                Some(s) => format!("skill {s:+.2}"),
                None => "skill n/a (one-sided period)".to_string(),
            };
            out.push_str(&format!(
                "   {:>6} · n={:<3} Brier {:.3} · base rate {:.0}% · {sk}\n",
                b.label,
                b.n,
                b.brier,
                b.base_rate * 100.0
            ));
        }
    }
    out.push_str(&match v {
        Verdict::Insufficient { have, need } => format!(
            "   → NOT YET PROVABLE: {have} graded in the smaller half, need {need}. \
             The claim stays unproven until the record earns it."
        ),
        Verdict::Improving { delta } => format!(
            "   → IMPROVING: skill up {delta:+.2} (older half → newer half) on unchanged weights. \
             This is the thesis holding."
        ),
        Verdict::Flat { delta } => format!(
            "   → FLAT: skill moved {delta:+.2}, inside the noise band (±{MIN_DELTA:.2}). No claim."
        ),
        Verdict::Degrading { delta } => format!(
            "   → DEGRADING: skill down {delta:+.2}. The thesis is failing here — worth diagnosing, not hiding."
        ),
        Verdict::BelowBaseline { skill, delta } => format!(
            "   → STILL BELOW BASELINE: skill {skill:+.2} (moved {delta:+.2}). My forecasts are not yet \
             beating a plain base-rate guess, so any movement here is more likely easier questions than \
             better judgment. No claim until skill goes positive."
        ),
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    const NOW: i64 = 1_800_000_000_000;

    /// Build n predictions at `days_ago`, each asserting `p`, with `hit_frac` of them true.
    fn rows(n: usize, days_ago: i64, p: f64, hit_frac: f64) -> Vec<Graded> {
        (0..n)
            .map(|i| Graded {
                t_ms: NOW - days_ago * DAY + i as i64,
                p,
                hit: (i as f64) < (n as f64) * hit_frac,
            })
            .collect()
    }

    #[test]
    fn brier_and_skill_are_textbook() {
        // A forecaster saying 0.5 on a 50/50 world: Brier 0.25, uncertainty 0.25, skill exactly 0.
        let r = rows(40, 10, 0.5, 0.5);
        assert!((brier(&r) - 0.25).abs() < 1e-9);
        assert!((skill(&r).unwrap() - 0.0).abs() < 1e-9);
        // A one-sided period has no defined skill (baseline is already perfect).
        let all_true = rows(20, 10, 0.9, 1.0);
        assert!(
            skill(&all_true).is_none(),
            "degenerate period must not report skill"
        );
    }

    // The verdict window is split at its midpoint, so with a 180d window "older" must sit beyond
    // 90 days ago and "newer" inside it. (An earlier draft put both at <90d, which silently emptied
    // the older half — the trend then reported Insufficient rather than a direction.)
    const OLD_D: i64 = 150;
    const NEW_D: i64 = 30;

    #[test]
    fn genuine_calibration_improvement_reads_as_improving() {
        // Older half: confident and wrong half the time (p=0.9, 50% hit) — worse than a coin flip.
        // Newer half: SAME 50/50 difficulty, but forecasts now separate the outcomes.
        let mut r = rows(30, OLD_D, 0.9, 0.5);
        for i in 0..20 {
            r.push(Graded {
                t_ms: NOW - NEW_D * DAY + i,
                p: 0.95,
                hit: true,
            });
            r.push(Graded {
                t_ms: NOW - (NEW_D - 1) * DAY + i,
                p: 0.05,
                hit: false,
            });
        }
        match verdict(&r, NOW, 180) {
            Verdict::Improving { delta } => assert!(delta > 0.05, "delta {delta}"),
            v => panic!("real calibration gain must read as Improving, got {v:?}"),
        }
    }

    /// THE ANTI-FLATTERY TEST. The forecaster is IDENTICAL in both halves (same p, same calibration
    /// quality); only the questions get easier — the newer period's outcomes are 90% one-way, which
    /// drops raw Brier sharply. Skill must NOT read that as the mind improving.
    /// THE ANTI-FLATTERY TEST. The forecaster is IDENTICAL across both halves — same fixed p=0.8,
    /// same (poor) quality. ONLY the questions change: the newer period's outcomes are 90% one-way.
    /// Raw Brier collapses 0.34 → 0.10, which a naive chart would publish as "3x better judgment".
    /// It is not: the forecaster never learned anything.
    ///
    /// Note skill still *rises* here (−0.36 → −0.11) because a fixed p=0.8 is less badly matched to
    /// a 90/10 world — which is exactly why the BSS delta alone is NOT a sufficient guard, and why
    /// `verdict` additionally requires the newer half to actually beat the baseline before it will
    /// say "improving". This test is what forced that design.
    #[test]
    fn easier_questions_do_not_read_as_improvement() {
        let older = rows(40, OLD_D, 0.8, 0.5); // 50/50 world
        let newer = rows(40, NEW_D, 0.8, 0.9); // easier world, SAME forecaster
        let r = [older.clone(), newer.clone()].concat();

        // The trap is real: raw Brier drops hard from an unchanged forecaster.
        assert!(
            brier(&newer) < brier(&older) - 0.15,
            "setup check: raw Brier must drop on easier questions ({:.3} -> {:.3})",
            brier(&older),
            brier(&newer)
        );
        // And the skill delta alone would have been fooled too.
        assert!(
            skill(&newer).unwrap() > skill(&older).unwrap() + MIN_DELTA,
            "setup check: the BSS delta alone is not enough of a guard"
        );
        // The verdict must NOT claim improvement — the forecaster still loses to a base-rate guess.
        match verdict(&r, NOW, 180) {
            Verdict::BelowBaseline { skill, .. } => {
                assert!(
                    skill <= 0.0,
                    "still not beating the baseline, skill {skill}"
                )
            }
            v => panic!("easier questions must not read as improvement — got {v:?}"),
        }
    }

    #[test]
    fn degradation_is_reported_not_hidden() {
        // Older: well-calibrated. Newer: confidently wrong.
        let mut r: Vec<Graded> = Vec::new();
        for i in 0..20 {
            r.push(Graded {
                t_ms: NOW - OLD_D * DAY + i,
                p: 0.95,
                hit: true,
            });
            r.push(Graded {
                t_ms: NOW - (OLD_D - 1) * DAY + i,
                p: 0.05,
                hit: false,
            });
        }
        for i in 0..20 {
            r.push(Graded {
                t_ms: NOW - NEW_D * DAY + i,
                p: 0.95,
                hit: false,
            });
            r.push(Graded {
                t_ms: NOW - (NEW_D - 1) * DAY + i,
                p: 0.05,
                hit: true,
            });
        }
        assert!(
            matches!(verdict(&r, NOW, 180), Verdict::Degrading { .. }),
            "a real decline must surface as Degrading"
        );
    }

    #[test]
    fn thin_record_refuses_to_claim_a_direction() {
        let r = [rows(5, OLD_D, 0.7, 0.4), rows(5, NEW_D, 0.7, 0.9)].concat();
        match verdict(&r, NOW, 180) {
            Verdict::Insufficient { have, need } => {
                assert_eq!(need, MIN_PER_HALF);
                assert!(have < MIN_PER_HALF);
            }
            v => panic!("a 10-item record must not claim a trend, got {v:?}"),
        }
        assert!(render(&r, NOW, 30, 6).contains("NOT YET PROVABLE"));
    }

    #[test]
    fn buckets_keep_gaps_visible() {
        // Only one bucket has data; the rest must still be rendered as empty, not collapsed.
        let r = rows(10, 15, 0.6, 0.5);
        let bs = buckets(&r, NOW, 30, 6);
        assert_eq!(bs.len(), 6);
        assert_eq!(
            bs.iter().filter(|b| b.n > 0).count(),
            1,
            "one populated bucket"
        );
        assert!(
            render(&r, NOW, 30, 6).contains("(no graded outcomes)"),
            "gaps stay visible"
        );
    }
}
