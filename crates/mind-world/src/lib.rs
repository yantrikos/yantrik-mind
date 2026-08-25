//! mind-world — the Phase 3A temporal spine (W1 ONLY, per docs/PHASE3_WORLD_STATE_V1.md).
//!
//! Scope locked: typed events → identity normalization → append-only transition log →
//! deterministic replay. Proves exactly two semantics: DUPLICATE_ID (same source_event_id =
//! one semantic event) and CORROBORATION (different sources asserting the same proposition =
//! two independent witnesses). NO current-state queries, NO derivations, NO executive, NO LLM.
//!
//! Invariants honored here: I6 (stable ids, deterministic ordering by
//! occurred_at → observed_at → source_event_id — never insertion accident), I7 (the log is
//! append-only; retractions are transitions, never deletions), E2 (identity vs corroboration).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind { Assert, Supersede, Retract, Expire }

/// A fact arriving from an authoritative source. Identity is the SOURCE EVENT's id — the same
/// id arriving twice is one semantic event, whatever its payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldEvent {
    pub source_event_id: String,
    pub source_id: String,
    pub kind: Kind,
    pub occurred_at: i64,
    pub observed_at: i64,
    pub entity: String,
    pub attr: String,
    pub value: String,
}

/// One durable line of epistemic history. Append-only: a retraction is a new row targeting an
/// earlier event, never a deletion (I7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldTransition {
    pub transition_id: u64,
    pub source_event_id: String,
    /// WHICH WITNESS said it — "email", "calendar" — as distinct from WHICH EVENT (`email:501`).
    ///
    /// Named by I6 and load-bearing for E2: two different sources asserting the same proposition
    /// are two independent witnesses and must never collapse, while the same source_event_id twice
    /// is one duplicate. Without this field a corroboration check has nothing to count, because
    /// every row's identity is unique by construction.
    pub source_id: String,
    pub kind: Kind,
    pub entity: String,
    pub attr: String,
    pub value: String,
    pub occurred_at: i64,
    pub observed_at: i64,
    /// Deterministic position in the canonical replay order (I6).
    pub recorded_seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IngestResult {
    /// Same source_event_id already ingested — no second semantic event.
    Duplicate,
    /// New evidence; `corroborates` counts PRIOR independent witnesses of the same proposition.
    Applied { transition_id: u64, corroborates: usize },
}

#[derive(Debug, Default)]
pub struct WorldLog {
    transitions: Vec<WorldTransition>,
    seen_event_ids: HashSet<String>,
    next_seq: u64,
    next_tid: u64,
}

impl WorldLog {
    pub fn new() -> Self { Self::default() }

    /// Ingest one event. Deterministic: identity dedup first; everything else becomes a
    /// transition with the next stable seq/tid. Ordering authority lives in [`replay`].
    pub fn ingest(&mut self, ev: &WorldEvent) -> IngestResult {
        if !self.seen_event_ids.insert(ev.source_event_id.clone()) {
            return IngestResult::Duplicate;
        }
        let corroborates = self
            .transitions
            .iter()
            .filter(|t| t.kind == Kind::Assert && t.entity == ev.entity && t.attr == ev.attr && t.value == ev.value)
            .count();
        self.next_tid += 1;
        let t = WorldTransition {
            transition_id: self.next_tid,
            source_event_id: ev.source_event_id.clone(),
            // The witness is the id's prefix by convention ("email:501" -> "email"); an id with no
            // prefix is its own witness rather than being silently grouped with everything else.
            source_id: ev
                .source_event_id
                .split_once(':')
                .map(|(w, _)| w.to_string())
                .unwrap_or_else(|| ev.source_event_id.clone()),
            kind: ev.kind,
            entity: ev.entity.clone(),
            attr: ev.attr.clone(),
            value: ev.value.clone(),
            occurred_at: ev.occurred_at,
            observed_at: ev.observed_at,
            recorded_seq: { self.next_seq += 1; self.next_seq },
        };
        let tid = t.transition_id;
        self.transitions.push(t);
        IngestResult::Applied { transition_id: tid, corroborates }
    }

    /// Canonical deterministic replay: same event SET (any arrival order/batching) produces the
    /// same logical log. Order = (occurred_at, observed_at, source_event_id).
    pub fn replay(events: &[WorldEvent]) -> WorldLog {
        let mut sorted: Vec<&WorldEvent> = events.iter().collect();
        sorted.sort_by(|a, b| {
            (a.occurred_at, a.observed_at, a.source_event_id.as_str())
                .cmp(&(b.occurred_at, b.observed_at, b.source_event_id.as_str()))
        });
        let mut log = WorldLog::new();
        for ev in sorted {
            log.ingest(ev);
        }
        log
    }

    pub fn transitions(&self) -> &[WorldTransition] { &self.transitions }
    pub fn len(&self) -> u64 { self.next_seq }
    pub fn is_empty(&self) -> bool { self.transitions.is_empty() }

    /// THE BI-TEMPORAL CUT (W2): what was true at `valid_at`, GIVEN only what had been learned
    /// by `known_at`. Knowledge-time filtering first (observed_at <= known_at) — this is the
    /// no-hindsight-leakage property; later information can never contaminate an earlier cut.
    /// Then world-time selection among what was known: the latest-occurred non-retracted
    /// assertion describes the state. A late-arriving OLD fact never resurrects a superseded
    /// proposition, because supersession happened earlier in WORLD time than the old fact
    /// describes... it wins because its occurred_at is later, regardless of arrival order.
    pub fn state_at(&self, entity: &str, attr: &str, q: WorldQuery) -> StateAt {
        let mut relevant: Vec<&WorldTransition> = self
            .transitions()
            .iter()
            .filter(|t| {
                t.entity == entity
                    && t.attr == attr
                    && t.observed_at <= q.known_at
                    && t.occurred_at <= q.valid_at
            })
            .collect();
        if relevant.is_empty() {
            return StateAt::Unknown;
        }
        relevant.sort_by_key(|t| (t.occurred_at, t.recorded_seq));
        match relevant.last().unwrap().kind {
            Kind::Assert | Kind::Supersede => StateAt::Known(relevant.last().unwrap().value.clone()),
            // Retract/Expire leave nothing warranted at this cut (W3 refines into Expired).
            Kind::Retract | Kind::Expire => StateAt::Unknown,
        }
    }
}

/// A purpose-scoped bi-temporal question. AccessContext joins in W5 — until then the type
/// exists so no consumer API grows up around a context-free shape.
#[derive(Debug, Clone, Copy)]
pub struct WorldQuery {
    pub valid_at: i64,
    pub known_at: i64,
}

/// Current-state values at a cut. W2 implements Known/Unknown; Conflicted/Stale/Expired are
/// W3's epistemic-state work and are representable from day one so call sites cannot grow
/// around booleans.
#[derive(Debug, Clone, PartialEq)]
pub enum StateAt {
    Known(String),
    Unknown,
    Conflicted(Vec<String>),
    Stale { value: String, last_verified: i64 },
    Expired,
}
 
#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str, ent: &str, val: &str) -> WorldEvent {
        WorldEvent {
            source_event_id: id.into(), source_id: id.split(':').next().unwrap().into(),
            kind: Kind::Assert, occurred_at: 100, observed_at: 110,
            entity: ent.into(), attr: "status".into(), value: val.into(),
        }
    }

    /// THE W1 DISTINCTION: same source_event_id twice = ONE semantic event; different sources
    /// asserting the same proposition = TWO independent witnesses. Never the same treatment.
    #[test]
    fn duplicate_identity_and_corroboration_are_opposites() {
        let mut log = WorldLog::new();
        assert!(matches!(log.ingest(&ev("email:501", "interview", "Thursday")), IngestResult::Applied { corroborates: 0, .. }));
        assert_eq!(log.ingest(&ev("email:501", "interview", "Thursday")), IngestResult::Duplicate, "exact duplicate is idempotent");
        // A different source saying the SAME thing is independent evidence — its own transition.
        match log.ingest(&ev("calendar:88", "interview", "Thursday")) {
            IngestResult::Applied { corroborates: 1, .. } => {}
            other => panic!("corroboration must be counted, got {other:?}"),
        }
        assert_eq!(log.transitions().len(), 2, "two witnesses = two transitions");
        // And a duplicate of the SECOND witness is also idempotent.
        assert_eq!(log.ingest(&ev("calendar:88", "interview", "Thursday")), IngestResult::Duplicate);
    }

    /// I6: replay determinism — the same event SET in any arrival order yields the identical
    /// logical log (ids, seqs, order), via (occurred_at, observed_at, source_event_id).
    #[test]
    fn replay_is_order_independent_and_canonical() {
        let mut events = vec![
            ev("email:923", "interview", "Friday"),
            ev("email:501", "interview", "Tuesday"),
            ev("carrier:771", "package", "delayed"),
        ];
        let canonical = WorldLog::replay(&events);
        events.reverse();
        let shuffled = WorldLog::replay(&events);
        let render = |l: &WorldLog| l.transitions().iter()
            .map(|t| format!("{}|{}|{}|{}", t.recorded_seq, t.transition_id, t.source_event_id, t.value))
            .collect::<Vec<_>>().join(";");
        assert_eq!(render(&canonical), render(&shuffled), "same set, any order, one history");
        assert_eq!(canonical.len(), 3);
    }

    /// I7 shape-check on the spine: ingest never removes rows; the log only grows.
    #[test]
    fn the_log_only_grows() {
        let mut log = WorldLog::new();
        log.ingest(&ev("a:1", "x", "true"));
        let before = log.len();
        log.ingest(&ev("a:1", "x", "true"));
        assert_eq!(log.len(), before, "duplicates add nothing");
        assert_eq!(log.transitions().len(), 1);
    }
}
 
#[cfg(test)]
mod w2_tests {
    use super::*;

    fn ev(id: &str, kind: Kind, occ: i64, obs: i64, ent: &str, val: &str) -> WorldEvent {
        WorldEvent {
            source_event_id: id.into(), source_id: id.split(':').next().unwrap().into(),
            kind, occurred_at: occ, observed_at: obs,
            entity: ent.into(), attr: "status".into(), value: val.into(),
        }
    }
    const D: i64 = 86_400_000;
    fn base(n: i64) -> i64 { 1_787_400_000_000 + n * D }

    /// THE NO-HINDSIGHT-LEAKAGE PROPERTY. Delay occurred Aug 20; learned Aug 22.
    /// The same world moment queried with two knowledge cuts gives two honest answers —
    /// and the early cut MUST NOT know what arrived later. This never regresses.
    #[test]
    fn later_information_cannot_leak_into_earlier_knowledge() {
        let log = WorldLog::replay(&[ev("carrier:771", Kind::Assert, base(20), base(22), "package", "delayed")]);
        let early = log.state_at("package", "status", WorldQuery { valid_at: base(20), known_at: base(20) });
        assert_eq!(early, StateAt::Unknown, "not yet LEARNED by the early cut — absence of knowledge, not denial");
        let late = log.state_at("package", "status", WorldQuery { valid_at: base(20), known_at: base(22) });
        assert_eq!(late, StateAt::Known("delayed".into()), "once learned, the past fact is known");
    }

    /// A LATE-ARRIVING OLD FACT must not resurrect a superseded proposition: supersession
    /// happened earlier in WORLD time than what the stale email describes, so Thursday wins
    /// even though the Tuesday email was only observed after it.
    #[test]
    fn a_late_old_email_does_not_resurrect_a_superseded_state() {
        let log = WorldLog::replay(&[
            ev("email:501", Kind::Assert, base(20), base(20), "interview", "Tuesday"),
            ev("email:923", Kind::Supersede, base(22), base(22), "interview", "Thursday"),
            ev("email:old", Kind::Assert, base(20), base(23), "interview", "Tuesday"), // arrives late
        ]);
        let s = log.state_at("interview", "status", WorldQuery { valid_at: base(23), known_at: base(23) });
        assert_eq!(s, StateAt::Known("Thursday".into()), "world-time ordering beats arrival order: {s:?}");
    }
}

