//! Deterministic, evidence-only synthesis for bounded specialist fan-out.
//!
//! Specialist prose is untrusted. This module therefore accepts only bounded reports tied to a
//! predeclared assignment, accounts for every unit of spend, deduplicates immutable evidence by
//! digest, and retains every attributed stance. The result is evidence for the parent agent: it
//! cannot grant authority or write durable belief. Promotion remains a separate governed step.

use std::collections::{BTreeMap, BTreeSet};

const MAX_SPECIALISTS: usize = 16;
const MAX_FINDINGS_PER_SPECIALIST: usize = 64;
const MAX_ID_BYTES: usize = 128;
const MAX_CLAIM_BYTES: usize = 4_096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialistAssignment {
    pub specialist_id: String,
    /// Stable, caller-issued unit of work. Reuse is rejected before dispatch.
    pub work_unit_id: String,
    pub budget_units: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialistPlan {
    assignments: Vec<SpecialistAssignment>,
    total_budget_units: u64,
}

impl SpecialistPlan {
    pub fn new(
        assignments: Vec<SpecialistAssignment>,
        total_budget_units: u64,
    ) -> Result<Self, SpecialistSynthesisError> {
        if assignments.is_empty() || assignments.len() > MAX_SPECIALISTS {
            return Err(SpecialistSynthesisError::InvalidPlan);
        }
        let mut specialists = BTreeSet::new();
        let mut work_units = BTreeSet::new();
        let mut assigned = 0_u64;
        for assignment in &assignments {
            if !valid_id(&assignment.specialist_id)
                || !valid_id(&assignment.work_unit_id)
                || assignment.budget_units == 0
                || !specialists.insert(assignment.specialist_id.as_str())
                || !work_units.insert(assignment.work_unit_id.as_str())
            {
                return Err(SpecialistSynthesisError::InvalidPlan);
            }
            assigned = assigned
                .checked_add(assignment.budget_units)
                .ok_or(SpecialistSynthesisError::BudgetExceeded)?;
        }
        if total_budget_units == 0 || assigned > total_budget_units {
            return Err(SpecialistSynthesisError::BudgetExceeded);
        }
        Ok(Self {
            assignments,
            total_budget_units,
        })
    }

    pub fn assignments(&self) -> &[SpecialistAssignment] {
        &self.assignments
    }

    pub fn total_budget_units(&self) -> u64 {
        self.total_budget_units
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FindingStance {
    Supports,
    Dissents,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialistFinding {
    /// SHA-256 of the immutable evidence payload.
    pub evidence_sha256: String,
    /// Opaque receipt ID; raw source content is deliberately not accepted here.
    pub receipt_id: String,
    pub claim: String,
    pub stance: FindingStance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialistReport {
    pub specialist_id: String,
    pub work_unit_id: String,
    pub spent_units: u64,
    pub findings: Vec<SpecialistFinding>,
    /// A report may describe evidence, but it cannot enlarge the caller's capability set.
    pub requests_authority: bool,
    /// Durable memory requires the parent's promotion gate; specialists cannot bypass it.
    pub requests_durable_write: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedFinding {
    pub specialist_id: String,
    pub work_unit_id: String,
    pub receipt_id: String,
    pub claim: String,
    pub stance: FindingStance,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthesizedEvidence {
    pub evidence_sha256: String,
    /// All provenance and dissent are retained even when several specialists found the same bytes.
    pub findings: Vec<AttributedFinding>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionStatus {
    /// The synthesis is evidence only. A separate governed promotion is required for durable belief.
    EvidenceOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpecialistSynthesis {
    pub total_spent_units: u64,
    pub budget_limit_units: u64,
    pub evidence: Vec<SynthesizedEvidence>,
    pub promotion_status: PromotionStatus,
}

impl SpecialistSynthesis {
    pub fn has_dissent(&self) -> bool {
        self.evidence.iter().any(|evidence| {
            evidence
                .findings
                .iter()
                .any(|finding| finding.stance == FindingStance::Dissents)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpecialistSynthesisError {
    InvalidPlan,
    ReportSetMismatch,
    InvalidFinding,
    BudgetExceeded,
    AuthorityLaundering,
}

/// Validate and synthesize one complete specialist round.
///
/// The function is pure: it neither invokes tools nor writes memory. Any invalid or authority-
/// seeking report fails the entire round, so a partial synthesis cannot be mistaken for consensus.
pub fn synthesize_specialists(
    plan: &SpecialistPlan,
    reports: Vec<SpecialistReport>,
) -> Result<SpecialistSynthesis, SpecialistSynthesisError> {
    if reports.len() != plan.assignments.len() {
        return Err(SpecialistSynthesisError::ReportSetMismatch);
    }

    let assignments: BTreeMap<&str, &SpecialistAssignment> = plan
        .assignments
        .iter()
        .map(|assignment| (assignment.specialist_id.as_str(), assignment))
        .collect();
    let mut reported = BTreeSet::new();
    let mut total_spent_units = 0_u64;
    let mut evidence: BTreeMap<String, Vec<AttributedFinding>> = BTreeMap::new();

    for report in reports {
        if report.requests_authority || report.requests_durable_write {
            return Err(SpecialistSynthesisError::AuthorityLaundering);
        }
        let Some(assignment) = assignments.get(report.specialist_id.as_str()) else {
            return Err(SpecialistSynthesisError::ReportSetMismatch);
        };
        if !reported.insert(report.specialist_id.clone())
            || assignment.work_unit_id != report.work_unit_id
        {
            return Err(SpecialistSynthesisError::ReportSetMismatch);
        }
        if report.spent_units > assignment.budget_units
            || report.findings.len() > MAX_FINDINGS_PER_SPECIALIST
        {
            return Err(SpecialistSynthesisError::BudgetExceeded);
        }
        total_spent_units = total_spent_units
            .checked_add(report.spent_units)
            .ok_or(SpecialistSynthesisError::BudgetExceeded)?;
        if total_spent_units > plan.total_budget_units {
            return Err(SpecialistSynthesisError::BudgetExceeded);
        }

        for finding in report.findings {
            if !valid_sha256(&finding.evidence_sha256)
                || !valid_id(&finding.receipt_id)
                || finding.claim.trim().is_empty()
                || finding.claim.len() > MAX_CLAIM_BYTES
            {
                return Err(SpecialistSynthesisError::InvalidFinding);
            }
            evidence
                .entry(finding.evidence_sha256)
                .or_default()
                .push(AttributedFinding {
                    specialist_id: report.specialist_id.clone(),
                    work_unit_id: report.work_unit_id.clone(),
                    receipt_id: finding.receipt_id,
                    claim: finding.claim,
                    stance: finding.stance,
                });
        }
    }

    if reported.len() != assignments.len() {
        return Err(SpecialistSynthesisError::ReportSetMismatch);
    }

    let evidence = evidence
        .into_iter()
        .map(|(evidence_sha256, mut findings)| {
            findings.sort_by(|left, right| {
                left.specialist_id
                    .cmp(&right.specialist_id)
                    .then(left.receipt_id.cmp(&right.receipt_id))
                    .then(left.stance.cmp(&right.stance))
            });
            SynthesizedEvidence {
                evidence_sha256,
                findings,
            }
        })
        .collect();

    Ok(SpecialistSynthesis {
        total_spent_units,
        budget_limit_units: plan.total_budget_units,
        evidence,
        promotion_status: PromotionStatus::EvidenceOnly,
    })
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.'))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assignment(
        specialist_id: &str,
        work_unit_id: &str,
        budget_units: u64,
    ) -> SpecialistAssignment {
        SpecialistAssignment {
            specialist_id: specialist_id.into(),
            work_unit_id: work_unit_id.into(),
            budget_units,
        }
    }

    fn report(
        specialist_id: &str,
        work_unit_id: &str,
        spent_units: u64,
        receipt_id: &str,
        stance: FindingStance,
    ) -> SpecialistReport {
        SpecialistReport {
            specialist_id: specialist_id.into(),
            work_unit_id: work_unit_id.into(),
            spent_units,
            findings: vec![SpecialistFinding {
                evidence_sha256: "a".repeat(64),
                receipt_id: receipt_id.into(),
                claim: "The same immutable record supports different interpretations.".into(),
                stance,
            }],
            requests_authority: false,
            requests_durable_write: false,
        }
    }

    #[test]
    fn specialist_plan_synthesizes_bounded_overlap_with_dissent_and_provenance() {
        let plan = SpecialistPlan::new(
            vec![
                assignment("fundamentals", "work:fundamentals", 4),
                assignment("risk", "work:risk", 3),
                assignment("countercase", "work:countercase", 3),
            ],
            10,
        )
        .expect("three unique work units fit the strict budget");

        let synthesis = synthesize_specialists(
            &plan,
            vec![
                report(
                    "fundamentals",
                    "work:fundamentals",
                    4,
                    "receipt:one",
                    FindingStance::Supports,
                ),
                report(
                    "risk",
                    "work:risk",
                    3,
                    "receipt:two",
                    FindingStance::Supports,
                ),
                report(
                    "countercase",
                    "work:countercase",
                    3,
                    "receipt:three",
                    FindingStance::Dissents,
                ),
            ],
        )
        .expect("bounded evidence-only synthesis");

        assert_eq!(plan.assignments().len(), 3);
        assert_eq!(synthesis.total_spent_units, 10);
        assert_eq!(synthesis.budget_limit_units, 10);
        assert_eq!(
            synthesis.evidence.len(),
            1,
            "overlapping evidence is deduplicated"
        );
        assert_eq!(
            synthesis.evidence[0].findings.len(),
            3,
            "all provenance remains named"
        );
        assert!(
            synthesis.has_dissent(),
            "the countercase must not be majority-collapsed"
        );
        assert_eq!(synthesis.promotion_status, PromotionStatus::EvidenceOnly);
    }

    #[test]
    fn specialist_plan_rejects_duplicate_work_and_authority_laundering() {
        let duplicate_work = SpecialistPlan::new(
            vec![
                assignment("one", "same-work", 1),
                assignment("two", "same-work", 1),
            ],
            2,
        );
        assert_eq!(duplicate_work, Err(SpecialistSynthesisError::InvalidPlan));

        let plan = SpecialistPlan::new(vec![assignment("one", "work-one", 1)], 1).unwrap();
        let mut laundering = report("one", "work-one", 1, "receipt:one", FindingStance::Supports);
        laundering.requests_durable_write = true;
        assert_eq!(
            synthesize_specialists(&plan, vec![laundering]),
            Err(SpecialistSynthesisError::AuthorityLaundering)
        );
    }

    #[test]
    fn specialist_plan_rejects_over_budget_reports() {
        let plan = SpecialistPlan::new(vec![assignment("one", "work-one", 1)], 1).unwrap();
        let over_budget = report("one", "work-one", 2, "receipt:one", FindingStance::Supports);
        assert_eq!(
            synthesize_specialists(&plan, vec![over_budget]),
            Err(SpecialistSynthesisError::BudgetExceeded)
        );
    }
}
