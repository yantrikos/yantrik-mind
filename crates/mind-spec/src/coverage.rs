//! The coverage router — which attachable expertise a need calls for, decided WITHOUT a model
//! (ARCH-6 P.3, ledger E.PK3).
//!
//! Publishers ship coverage phrases in every pack manifest. This module takes a query vector and
//! each pack's coverage vectors (embedded elsewhere — this crate never embeds, never infers) and
//! answers two questions in the open: which pack matches best, and whether the match is clear
//! enough to act on. Everything here is a pure function of numbers, so the labelled corpus in
//! `mind-evals` can score it exactly, and the policy can be named by id when it changes.
//!
//! Abstaining is a first-class answer. A router that always picks something is a router that will
//! lease the wrong expertise on every off-topic turn; the bar for this policy scores abstention on
//! no-pack queries as strictly as it scores agreement on pack queries.

use mind_types::memory::{AbstainReason, PackRoute};

/// The policy id stamped on every shadow route event. Change the constants below → change this.
pub const COVERAGE_POLICY_ID: &str = "coverage-router-v1";
/// A pack's best coverage phrase must reach this similarity to be leased at all. A GUESS,
/// pre-registered in E.PK3 before any query was routed; the oracle reports the per-query
/// similarities so a recalibration is a new policy id, never a silent edit.
pub const COVERAGE_FLOOR: f64 = 0.50;
/// The top pack must beat the runner-up by this much, or the router abstains as a tie. Same
/// provenance as the floor.
pub const COVERAGE_MARGIN: f64 = 0.05;

/// Cosine similarity; zero for empty, mismatched or zero-norm vectors rather than NaN, so a broken
/// embedding can never rank first.
pub fn cosine(a: &[f32], b: &[f32]) -> f64 {
    if a.is_empty() || a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        dot += (*x as f64) * (*y as f64);
        na += (*x as f64) * (*x as f64);
        nb += (*y as f64) * (*y as f64);
    }
    if na == 0.0 || nb == 0.0 {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// One pack's coverage, embedded: the id and one vector per coverage phrase, in manifest order.
#[derive(Debug, Clone, PartialEq)]
pub struct CoverageVectors {
    pub pack_id: String,
    pub phrases: Vec<String>,
    pub vectors: Vec<Vec<f32>>,
}

/// One pack's best match for a query: the similarity and WHICH phrase earned it — the operator
/// reads the phrase to tell a real match from a vocabulary neighbour.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub pack_id: String,
    pub sim: f64,
    pub phrase: String,
}

/// Rank every pack by its best coverage phrase, best first. Deterministic: ties keep manifest order
/// (the order `packs` arrived in), and a pack with no phrases ranks at zero rather than vanishing —
/// an unrouteable pack is a fact the operator should see.
pub fn rank(query: &[f32], packs: &[CoverageVectors]) -> Vec<Ranked> {
    let mut out: Vec<Ranked> = packs
        .iter()
        .map(|p| {
            let (sim, idx) = p
                .vectors
                .iter()
                .enumerate()
                .map(|(i, v)| (cosine(query, v), i))
                .fold((0.0f64, usize::MAX), |best, (s, i)| if s > best.0 { (s, i) } else { best });
            Ranked {
                pack_id: p.pack_id.clone(),
                sim,
                phrase: p.phrases.get(idx).cloned().unwrap_or_default(),
            }
        })
        .collect();
    // Stable sort: equal similarities keep arrival order, so a tie is reported as a tie by
    // `route` rather than decided by hash order.
    out.sort_by(|a, b| b.sim.partial_cmp(&a.sim).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// The policy: lease the top pack when it clears the floor and beats the runner-up by the margin;
/// otherwise abstain, and say why.
pub fn route(ranked: &[Ranked], floor: f64, margin: f64) -> PackRoute {
    let Some(top) = ranked.first() else {
        return PackRoute::Abstain { reason: AbstainReason::NoPacks, best: None };
    };
    let best = Some((top.pack_id.clone(), top.sim));
    if !(top.sim >= floor) {
        return PackRoute::Abstain { reason: AbstainReason::BelowFloor, best };
    }
    let second = ranked.get(1).map(|r| r.sim).unwrap_or(0.0);
    if top.sim - second < margin {
        return PackRoute::Abstain { reason: AbstainReason::Tie, best };
    }
    PackRoute::Lease { pack_id: top.pack_id.clone(), sim: top.sim, margin: top.sim - second }
}

/// `rank` then `route` with the registered constants.
pub fn decide(query: &[f32], packs: &[CoverageVectors]) -> (Vec<Ranked>, PackRoute) {
    let ranked = rank(query, packs);
    let r = route(&ranked, COVERAGE_FLOOR, COVERAGE_MARGIN);
    (ranked, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(id: &str, vs: &[&[f32]]) -> CoverageVectors {
        CoverageVectors {
            pack_id: id.into(),
            phrases: vs.iter().enumerate().map(|(i, _)| format!("{id} phrase {i}")).collect(),
            vectors: vs.iter().map(|v| v.to_vec()).collect(),
        }
    }

    #[test]
    fn the_best_phrase_ranks_the_pack_and_is_named() {
        let q = [1.0f32, 0.0, 0.0];
        let packs = vec![
            pack("b", &[&[0.0, 1.0, 0.0], &[0.7, 0.7, 0.0]]), // best phrase 1 → ~0.71
            pack("a", &[&[1.0, 0.1, 0.0]]),                   // ~0.995
        ];
        let ranked = rank(&q, &packs);
        assert_eq!(ranked[0].pack_id, "a");
        assert_eq!(ranked[1].pack_id, "b");
        assert_eq!(ranked[1].phrase, "b phrase 1", "the phrase that earned the score is reported");
        assert!(ranked[0].sim > 0.99 && (ranked[1].sim - 0.7071).abs() < 0.01);
    }

    #[test]
    fn route_leases_only_a_clear_winner_and_names_every_abstention() {
        let r = |sims: &[f64]| -> Vec<Ranked> {
            sims.iter().enumerate().map(|(i, s)| Ranked { pack_id: format!("p{i}"), sim: *s, phrase: String::new() }).collect()
        };
        assert_eq!(route(&[], 0.5, 0.05), PackRoute::Abstain { reason: AbstainReason::NoPacks, best: None });
        assert_eq!(
            route(&r(&[0.49, 0.10]), 0.5, 0.05),
            PackRoute::Abstain { reason: AbstainReason::BelowFloor, best: Some(("p0".into(), 0.49)) }
        );
        assert_eq!(
            route(&r(&[0.80, 0.78]), 0.5, 0.05),
            PackRoute::Abstain { reason: AbstainReason::Tie, best: Some(("p0".into(), 0.80)) }
        );
        match route(&r(&[0.80, 0.60]), 0.5, 0.05) {
            // Floating subtraction: 0.80 - 0.60 is 0.2000…07, so the margin is checked to a tolerance.
            PackRoute::Lease { pack_id, sim, margin } => {
                assert_eq!(pack_id, "p0");
                assert_eq!(sim, 0.80);
                assert!((margin - 0.20).abs() < 1e-9, "margin {margin}");
            }
            other => panic!("a clear winner must be leased: {other:?}"),
        }
        // A single pack above the floor is a clear winner (no runner-up to beat).
        assert!(matches!(route(&r(&[0.6]), 0.5, 0.05), PackRoute::Lease { .. }));
        // NaN never leases.
        assert!(matches!(route(&r(&[f64::NAN]), 0.5, 0.05), PackRoute::Abstain { reason: AbstainReason::BelowFloor, .. }));
    }

    #[test]
    fn cosine_is_safe_on_bad_input_and_a_phraseless_pack_ranks_at_zero() {
        assert_eq!(cosine(&[], &[]), 0.0);
        assert_eq!(cosine(&[1.0], &[1.0, 2.0]), 0.0);
        assert_eq!(cosine(&[0.0, 0.0], &[1.0, 1.0]), 0.0);
        let ranked = rank(&[1.0, 0.0], &[pack("empty", &[]), pack("full", &[&[1.0, 0.0]])]);
        assert_eq!((ranked[0].pack_id.as_str(), ranked[1].pack_id.as_str()), ("full", "empty"));
        assert_eq!(ranked[1].sim, 0.0);
    }
}
