//! E.F2 (second amendment, v10): the replan lifecycle reducer.
//!
//! A parked goal leaves a claimable *replan carrier* in the queue. When the scheduler claims it,
//! nothing may fire on the strength of "the latest receipt": the whole hash-chained lifecycle is
//! reduced here, pure and deterministic, into exactly one of three acquisitions — an initial
//! attempt, the resumption of the one open attempt after a crash, or a receipt-bound retry after
//! a closed failure — or a block. Every attempt carries a receipt ordinal and the carrier's
//! identity (assumption id, target revision), closures are validated in chain order against the
//! marker they close, and any other chain shape is a lifecycle-integrity failure, terminal for
//! this slice.
use crate::horizon::{HorizonLifecycleEvent, HorizonLifecycleReceipt};
use sha2::{Digest, Sha256};

/// The one bounded code an integrity failure carries. Terminal for this slice: the operator
/// retry control refuses it, and only a future, preregistered reconciliation may move the goal.
pub const REPLAN_LIFECYCLE_MISMATCH: &str = "replan_lifecycle_mismatch";

/// The identity a carrier claims and every attempt on it must carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplanIdentity {
    pub assumption_id: String,
    pub target_revision: u32,
}

/// One `replan_started` marker as the chain recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplanMarker {
    pub attempt: u32,
    pub identity: ReplanIdentity,
    /// Closed by a `replanned` (true) or an attempt-scoped `failed` (false); `None` = open.
    pub closed_by_success: Option<bool>,
}

/// What the chain says about replan attempts, read once per claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplanChain {
    /// Every marker in chain order, with its closure.
    pub markers: Vec<ReplanMarker>,
    /// The latest event that is not a code-owned `wake_started` / `recovered` transition.
    pub latest_substantive: Option<HorizonLifecycleEvent>,
    /// The assumption id the latest `awaiting_replan` receipt carried.
    pub latest_awaiting: Option<String>,
    /// `chain_digest` of an existing integrity receipt, if the chain already failed integrity.
    pub integrity_failed: Option<String>,
    /// Digest of the prefix that ends immediately BEFORE any integrity receipt (the whole chain
    /// when there is none). An integrity failure stores this so re-entry can recognise itself.
    pub prefix_digest: String,
    /// Any shape violation found while reducing: a wrong ordinal, a closure of nothing, of the
    /// wrong attempt, of a different identity, a duplicate closure, or an attempt of 0.
    pub malformed: bool,
    /// Whether every receipt after the last marker is a code-owned transition.
    pub tail_code_owned: bool,
}

/// The reducer's verdict for one claimed replan carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplanAcquisition {
    /// Branch A: append `replan_started(attempt)` and author.
    Initial {
        attempt: u32,
    },
    /// Branch B: one open marker, only code-owned transitions after it — resume it, no marker.
    Resume {
        attempt: u32,
    },
    /// Branch C: a closed failure and a bound retry control — append `replan_started(attempt)`.
    Retry {
        attempt: u32,
    },
    Blocked(ReplanBlock),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplanBlock {
    /// The run is not awaiting a replan.
    NotAwaiting,
    /// The chain already carries an integrity failure: terminal, never re-entered.
    IntegrityAlreadyFailed,
    /// A closed failure without the verified retry control bound to this claim.
    RetryNotBound,
    /// Any other chain shape. The digest is the malformed prefix's, for the integrity receipt.
    Mismatch { chain_digest: String },
    /// An integrity receipt exists but its stored digest is not the prefix's: not a re-entry.
    /// Terminal all the same; nothing more is written.
    IntegrityRecordMismatch,
}

fn is_code_owned(event: HorizonLifecycleEvent) -> bool {
    matches!(
        event,
        HorizonLifecycleEvent::WakeStarted | HorizonLifecycleEvent::Recovered
    )
}

/// Reduce a verified lifecycle chain into what the replan branch needs to know.
pub fn reduce_replan(receipts: &[HorizonLifecycleReceipt]) -> ReplanChain {
    let mut markers: Vec<ReplanMarker> = Vec::new();
    let mut latest_substantive = None;
    let mut latest_awaiting = None;
    let mut integrity_failed = None;
    let mut malformed = false;
    let mut prefix = Sha256::new();
    let mut last_marker_index: Option<usize> = None;
    for (i, r) in receipts.iter().enumerate() {
        // The prefix ends immediately BEFORE the first integrity receipt, never including it.
        if integrity_failed.is_none() && r.event != HorizonLifecycleEvent::ReplanIntegrityFailed {
            prefix.update(r.receipt_sha256.as_bytes());
        }
        let detail = r.replan.as_ref();
        let attempt = detail.and_then(|d| d.attempt);
        let identity = detail.and_then(|d| {
            Some(ReplanIdentity {
                assumption_id: d.assumption_id.clone()?,
                target_revision: d.target_revision?,
            })
        });
        match r.event {
            HorizonLifecycleEvent::AwaitingReplan => {
                latest_awaiting = detail.and_then(|d| d.assumption_id.clone());
            }
            HorizonLifecycleEvent::ReplanStarted => {
                let expected = markers.len() as u32 + 1;
                match (attempt, identity) {
                    (Some(a), Some(identity)) if a == expected => {
                        // A new marker while one is still open is itself a shape violation.
                        if markers.iter().any(|m| m.closed_by_success.is_none()) {
                            malformed = true;
                        }
                        markers.push(ReplanMarker {
                            attempt: a,
                            identity,
                            closed_by_success: None,
                        });
                    }
                    _ => malformed = true,
                }
                last_marker_index = Some(i);
            }
            HorizonLifecycleEvent::Replanned | HorizonLifecycleEvent::Failed => {
                let success = r.event == HorizonLifecycleEvent::Replanned;
                match attempt {
                    // A plain (pre-E.F2) failure closes nothing and is not a violation.
                    None if !success => {}
                    Some(a) if a >= 1 => {
                        // Closes only the currently open marker with this attempt; a `replanned`
                        // must also carry the marker's identity.
                        let open = markers.iter_mut().find(|m| m.closed_by_success.is_none());
                        match open {
                            Some(m)
                                if m.attempt == a
                                    && (!success || identity.as_ref() == Some(&m.identity)) =>
                            {
                                m.closed_by_success = Some(success);
                            }
                            _ => malformed = true,
                        }
                    }
                    _ => malformed = true,
                }
            }
            HorizonLifecycleEvent::ReplanIntegrityFailed => {
                if integrity_failed.is_none() {
                    integrity_failed = detail.and_then(|d| d.chain_digest.clone());
                }
            }
            _ => {}
        }
        if !is_code_owned(r.event) {
            latest_substantive = Some(r.event);
        }
    }
    let tail_code_owned = match last_marker_index {
        Some(i) => receipts[i + 1..].iter().all(|r| is_code_owned(r.event)),
        None => false,
    };
    ReplanChain {
        markers,
        latest_substantive,
        latest_awaiting,
        integrity_failed,
        prefix_digest: format!("{:x}", prefix.finalize()),
        malformed,
        tail_code_owned,
    }
}

impl ReplanChain {
    /// The markers that were started and never closed, in chain order.
    pub fn open_markers(&self) -> Vec<&ReplanMarker> {
        self.markers
            .iter()
            .filter(|m| m.closed_by_success.is_none())
            .collect()
    }

    /// The three branches, or a block. `awaiting` is the checkpoint's status; `expected` is the
    /// claimed carrier's identity; `retry_bound` says the caller verified a retry control receipt
    /// bound to this checkpoint digest and to the failed→pending transition that produced the
    /// current claim.
    pub fn acquire(
        &self,
        awaiting: bool,
        expected: &ReplanIdentity,
        retry_bound: bool,
    ) -> ReplanAcquisition {
        if !awaiting {
            return ReplanAcquisition::Blocked(ReplanBlock::NotAwaiting);
        }
        if self.integrity_failed.is_some() {
            return ReplanAcquisition::Blocked(ReplanBlock::IntegrityAlreadyFailed);
        }
        let mismatch = || {
            ReplanAcquisition::Blocked(ReplanBlock::Mismatch {
                chain_digest: self.prefix_digest.clone(),
            })
        };
        if self.malformed {
            return mismatch();
        }
        // The carrier must be the one the parking receipt named.
        if self.latest_awaiting.as_deref() != Some(expected.assumption_id.as_str()) {
            return mismatch();
        }
        let open = self.open_markers();
        match open.as_slice() {
            [marker] => {
                if self.tail_code_owned && marker.identity == *expected {
                    ReplanAcquisition::Resume {
                        attempt: marker.attempt,
                    }
                } else {
                    mismatch()
                }
            }
            [] => {
                let next = self.markers.len() as u32 + 1;
                match self.latest_substantive {
                    Some(HorizonLifecycleEvent::AwaitingReplan) => {
                        ReplanAcquisition::Initial { attempt: next }
                    }
                    Some(HorizonLifecycleEvent::Failed) => {
                        // The attempt that failed must have been on this carrier's identity.
                        let last = self.markers.last();
                        match last {
                            Some(m)
                                if m.closed_by_success == Some(false)
                                    && m.identity == *expected =>
                            {
                                if retry_bound {
                                    ReplanAcquisition::Retry { attempt: next }
                                } else {
                                    ReplanAcquisition::Blocked(ReplanBlock::RetryNotBound)
                                }
                            }
                            _ => mismatch(),
                        }
                    }
                    _ => mismatch(),
                }
            }
            _ => mismatch(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::horizon::{assumption_id, HorizonLifecycleReceipt, ReplanDetail};

    const STATE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn price() -> ReplanIdentity {
        ReplanIdentity {
            assumption_id: assumption_id("price"),
            target_revision: 1,
        }
    }

    struct Chain(Vec<HorizonLifecycleReceipt>);
    impl Chain {
        fn new() -> Self {
            Chain(Vec::new())
        }
        fn prev(&self) -> Option<String> {
            self.0.last().map(|r| r.receipt_sha256.clone())
        }
        fn push(
            &mut self,
            event: HorizonLifecycleEvent,
            prev_q: Option<&str>,
            next_q: Option<&str>,
            reason: Option<&str>,
            replan: Option<ReplanDetail>,
        ) -> &mut Self {
            let t = 1_000 + self.0.len() as u64;
            let r = HorizonLifecycleReceipt::issue_with_replan(
                "goal:horizon:1",
                event,
                t,
                Some(STATE.to_string()),
                prev_q.map(str::to_string),
                next_q.map(str::to_string),
                reason.map(str::to_string),
                self.prev(),
                replan,
            )
            .unwrap_or_else(|e| panic!("{event:?} rejected: {e:?}"));
            assert!(r.verify());
            self.0.push(r);
            self
        }
        fn scheduled(&mut self) -> &mut Self {
            self.push(
                HorizonLifecycleEvent::Scheduled,
                None,
                Some("pending"),
                None,
                None,
            )
        }
        fn wake(&mut self) -> &mut Self {
            self.push(
                HorizonLifecycleEvent::WakeStarted,
                Some("pending"),
                Some("running"),
                None,
                None,
            )
        }
        fn recovered(&mut self) -> &mut Self {
            self.push(
                HorizonLifecycleEvent::Recovered,
                Some("running"),
                Some("pending"),
                None,
                None,
            )
        }
        fn awaiting_key(&mut self, key: &str) -> &mut Self {
            self.push(
                HorizonLifecycleEvent::AwaitingReplan,
                Some("running"),
                Some("pending"),
                None,
                Some(ReplanDetail::awaiting(assumption_id(key))),
            )
        }
        fn awaiting(&mut self) -> &mut Self {
            self.awaiting_key("price")
        }
        fn started_as(&mut self, attempt: u32, key: &str, revision: u32) -> &mut Self {
            self.push(
                HorizonLifecycleEvent::ReplanStarted,
                Some("running"),
                Some("running"),
                None,
                Some(ReplanDetail::started(assumption_id(key), attempt, revision)),
            )
        }
        fn started(&mut self, attempt: u32) -> &mut Self {
            self.started_as(attempt, "price", 1)
        }
        fn replanned_as(&mut self, attempt: u32, key: &str, revision: u32) -> &mut Self {
            self.push(
                HorizonLifecycleEvent::Replanned,
                Some("running"),
                None,
                None,
                Some(ReplanDetail::started(assumption_id(key), attempt, revision)),
            )
        }
        fn replanned(&mut self, attempt: u32) -> &mut Self {
            self.replanned_as(attempt, "price", 1)
        }
        fn failed_attempt(&mut self, attempt: u32) -> &mut Self {
            self.push(
                HorizonLifecycleEvent::Failed,
                Some("running"),
                Some("failed"),
                Some("replan_validation_failed"),
                Some(ReplanDetail::attempt_only(attempt)),
            )
        }
        fn failed_plain(&mut self) -> &mut Self {
            self.push(
                HorizonLifecycleEvent::Failed,
                Some("running"),
                Some("failed"),
                Some("segment_execution_failed"),
                None,
            )
        }
        fn acquire(&self, retry_bound: bool) -> ReplanAcquisition {
            reduce_replan(&self.0).acquire(true, &price(), retry_bound)
        }
        fn is_mismatch(&self, retry_bound: bool) -> bool {
            matches!(
                self.acquire(retry_bound),
                ReplanAcquisition::Blocked(ReplanBlock::Mismatch { .. })
            )
        }
    }

    #[test]
    fn branch_a_initial_acquisition_numbers_the_first_attempt_one() {
        let mut c = Chain::new();
        c.scheduled().wake().awaiting().wake();
        assert_eq!(c.acquire(false), ReplanAcquisition::Initial { attempt: 1 });
        assert_eq!(
            reduce_replan(&c.0).acquire(false, &price(), false),
            ReplanAcquisition::Blocked(ReplanBlock::NotAwaiting)
        );
    }

    #[test]
    fn branch_b_resumes_the_one_open_attempt_after_a_crash_without_a_new_marker() {
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started(1)
            .recovered()
            .wake();
        assert_eq!(reduce_replan(&c.0).open_markers().len(), 1);
        assert_eq!(c.acquire(false), ReplanAcquisition::Resume { attempt: 1 });
    }

    #[test]
    fn branch_c_retry_after_a_closed_failure_needs_the_bound_control_and_takes_the_next_ordinal() {
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started(1)
            .failed_attempt(1)
            .wake();
        assert!(reduce_replan(&c.0).open_markers().is_empty());
        assert_eq!(
            c.acquire(false),
            ReplanAcquisition::Blocked(ReplanBlock::RetryNotBound)
        );
        assert_eq!(c.acquire(true), ReplanAcquisition::Retry { attempt: 2 });
    }

    #[test]
    fn a_later_awaiting_after_a_closed_attempt_is_initial_with_the_next_ordinal() {
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started(1)
            .replanned(1)
            .scheduled()
            .wake()
            .awaiting()
            .wake();
        assert_eq!(c.acquire(false), ReplanAcquisition::Initial { attempt: 2 });
    }

    #[test]
    fn a_closure_must_close_the_currently_open_marker_and_nothing_else() {
        // Close without any open marker: FAILED(attempt) before any REPLAN_STARTED.
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .failed_attempt(7)
            .wake();
        assert!(
            c.is_mismatch(true),
            "close-before-start must not become a retry"
        );
        // Attempt 0 is not an attempt.
        let mut c = Chain::new();
        c.scheduled().wake().awaiting().wake().started(1);
        assert!(HorizonLifecycleReceipt::issue_with_replan(
            "goal:horizon:1",
            HorizonLifecycleEvent::Failed,
            5,
            Some(STATE.to_string()),
            Some("running".into()),
            Some("failed".into()),
            Some("replan_validation_failed".into()),
            c.prev(),
            Some(ReplanDetail::attempt_only(0)),
        )
        .is_err());
        // Unknown attempt: open marker is 1, closure names 2.
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started(1)
            .failed_attempt(2)
            .wake();
        assert!(c.is_mismatch(true));
        // Duplicate close.
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started(1)
            .failed_attempt(1)
            .failed_attempt(1)
            .wake();
        assert!(c.is_mismatch(true));
        // A pre-E.F2 plain failure closes nothing and breaks nothing.
        let mut c = Chain::new();
        c.scheduled().wake().failed_plain();
        assert!(!reduce_replan(&c.0).malformed);
    }

    #[test]
    fn identity_is_reduced_and_compared_on_every_branch() {
        // The parking receipt named another assumption than the claimed carrier.
        let mut c = Chain::new();
        c.scheduled().wake().awaiting_key("weather").wake();
        assert!(c.is_mismatch(false));
        // A resumed marker for another assumption / revision is not this carrier's.
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started_as(1, "price", 2)
            .recovered()
            .wake();
        assert!(c.is_mismatch(false));
        // A REPLANNED with different detail does not close the marker.
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started(1)
            .replanned_as(1, "weather", 1);
        assert!(reduce_replan(&c.0).malformed);
        // A retry after a failure on another identity is not this carrier's retry.
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started_as(1, "price", 2)
            .failed_attempt(1)
            .wake();
        assert!(c.is_mismatch(true));
    }

    #[test]
    fn any_other_shape_is_a_mismatch_with_the_prefix_digest_and_is_terminal_once_recorded() {
        // Two open markers.
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started(1)
            .recovered()
            .wake()
            .started(2);
        let chain = reduce_replan(&c.0);
        let ReplanAcquisition::Blocked(ReplanBlock::Mismatch { chain_digest }) =
            chain.acquire(true, &price(), true)
        else {
            panic!("two open markers must be a mismatch");
        };
        assert_eq!(chain_digest, chain.prefix_digest);
        // A wrong ordinal.
        let mut c = Chain::new();
        c.scheduled().wake().awaiting().wake().started(2);
        assert!(c.is_mismatch(false));
        // Once the integrity receipt exists, the prefix digest excludes it and the goal is
        // terminal — re-entry recognises itself by that stored digest.
        let mut c = Chain::new();
        c.scheduled()
            .wake()
            .awaiting()
            .wake()
            .started(1)
            .recovered()
            .wake()
            .started(2);
        let before = reduce_replan(&c.0).prefix_digest.clone();
        c.push(
            HorizonLifecycleEvent::ReplanIntegrityFailed,
            Some("running"),
            Some("failed"),
            Some(REPLAN_LIFECYCLE_MISMATCH),
            Some(ReplanDetail::integrity(before.clone())),
        );
        let after = reduce_replan(&c.0);
        assert_eq!(after.prefix_digest, before);
        assert_eq!(after.integrity_failed.as_deref(), Some(before.as_str()));
        assert_eq!(
            after.acquire(true, &price(), true),
            ReplanAcquisition::Blocked(ReplanBlock::IntegrityAlreadyFailed)
        );
    }

    #[test]
    fn a_receipt_without_replan_detail_digests_exactly_as_before_the_amendment() {
        let mut c = Chain::new();
        c.scheduled();
        let r = &c.0[0];
        assert!(r.replan.is_none());
        let json = serde_json::to_string(r).unwrap();
        assert!(
            !json.contains("replan"),
            "old receipts serialise without the new field"
        );
        let back: HorizonLifecycleReceipt = serde_json::from_str(&json).unwrap();
        assert!(back.verify());
    }
}
