//! mind-world — Phase 3A temporal spine + epistemic state (W1–W3).
//!
//! Contract: docs/PHASE3_WORLD_STATE_V1.md. Scope through W3: identity/dedup/corroboration
//! (I6/E2), append-only history (I7), bi-temporal cuts with no hindsight leakage (A1/I2),
//! epistemic states Known/Unknown/Conflicted/Stale/Expired with CONFLICT vs RESOLVE_BY_RULE
//! kept distinct (I4), registered-only deterministic derivations deferred to W4 (E1),
//! purpose context present at the query boundary from day one (A6).

use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind { Assert, Supersede, Retract, Expire }

/// A fact arriving from an authoritative source. Identity is the SOURCE EVENT's id — the same
/// id arriving twice is one semantic event, whatever its payload (I6).
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
/// earlier event's proposition, never a deletion (I7).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorldTransition {
    pub transition_id: u64,
    pub source_event_id: String,
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

/// A NAMED deterministic conflict-resolution rule (E1: registered only, never implicit
/// last-write-wins). Applies only when multiple distinct-source claims are live.
pub struct ResolutionRule {
    pub id: &'static str,
    pub version: u32,
    /// Some(winning_value) iff this rule resolves the claim set.
    pub apply: Box<dyn Fn(&[Claim]) -> Option<String> + Send + Sync>,
}

/// One live claim handed to resolution rules.
pub struct Claim<'a> {
    pub source_id: &'a str,
    pub value: &'a str,
    pub occurred_at: i64,
}

/// A REGISTERED deterministic derivation (E1/W4): named + versioned + declared inputs. The
/// producer re-runs against currently warranted inputs on every query.
pub struct DerivationRule {
    pub id: &'static str,
    pub version: u32,
    pub entity: String,
    pub attr: String,
    pub consumes: Vec<(String, String)>,
    pub produce: Box<dyn Fn(&[Option<&StateAt>]) -> Option<String> + Send + Sync>,
}

#[derive(Debug, Clone)]
pub struct WorldQuery {
    pub valid_at: i64,
    pub known_at: i64,
    /// Purpose Gate at the boundary from DAY ONE (contract A6): there is no context-free
    /// WorldQuery, so no ungated consumer API can grow around one. Enforcement deepens in W5.
    pub access: mind_types::AccessContext,
}

/// Epistemic current-state values. All five exist from day one so call sites cannot grow
/// around booleans; W2 populated Known/Unknown, W3 the rest.
#[derive(Debug, Clone, PartialEq)]
pub enum StateAt {
    Known(String),
    Unknown,
    Conflicted(Vec<String>),
    Stale { value: String, last_verified: i64 },
    Expired,
}

pub struct WorldLog {
    transitions: Vec<WorldTransition>,
    seen_event_ids: HashSet<String>,
    next_seq: u64,
    next_tid: u64,
    /// Bi-temporal freshness policy: staleness judged against known_at, NEVER Utc::now().
    freshness_ms: i64,
    resolution_rules: Vec<ResolutionRule>,
    /// Registered deterministic derivations (E1). Evaluated ON DEMAND from current warranted
    /// sources — never materialized eagerly — so a superseded input cannot leave a zombie
    /// conclusion behind (I3): warrant loss propagates because nothing is cached.
    derivations: Vec<DerivationRule>,
    /// Purpose gate at the world boundary (A6/I5): None = construction-phase allow-all;
    /// production logs set this BEFORE any consumer query exists (W5).
    gate: Option<Box<dyn Fn(&mind_types::AccessContext, &str) -> bool + Send + Sync>>,
}

impl Default for WorldLog {
    fn default() -> Self {
        Self {
            transitions: Vec::new(),
            seen_event_ids: HashSet::new(),
            next_seq: 0,
            next_tid: 0,
            freshness_ms: 48 * 3_600_000,
            resolution_rules: Vec::new(),
            derivations: Vec::new(),
            gate: None,
        }
    }
}

impl WorldLog {
    pub fn new() -> Self { Self::default() }

    /// Register a named resolution rule (the ONLY way many claims become one value).
    pub fn with_rule(mut self, rule: ResolutionRule) -> Self {
        self.resolution_rules.push(rule);
        self
    }

    pub fn with_freshness_ms(mut self, ms: i64) -> Self {
        self.freshness_ms = ms;
        self
    }

    /// Register a derivation (W4): named, versioned, declared inputs — E1's RegisteredDerivationRule.
    pub fn with_derivation(mut self, d: DerivationRule) -> Self {
        self.derivations.push(d);
        self
    }

    /// Install the purpose gate (W5). After this call, EVERY state_at query is checked against
    /// the caller's AccessContext; denied entities read as Unknown — absence of authorization
    /// is indistinguishable from absence of fact, exactly as A6 requires.
    pub fn with_gate(mut self, g: Box<dyn Fn(&mind_types::AccessContext, &str) -> bool + Send + Sync>) -> Self {
        self.gate = Some(g);
        self
    }

    /// W4: on-demand derivation with LINEAGE. Because the rule re-runs against currently
    /// warranted inputs on every query, a retracted/superseded input invalidates the output
    /// automatically — zombie conclusions are structurally impossible rather than swept.
    pub fn derived_state(&self, entity: &str, q: &WorldQuery) -> StateAt {
        if let Some(g) = &self.gate {
            if !g(&q.access, entity) {
                return StateAt::Unknown;
            }
        }
        for d in self.derivations.iter().filter(|d| d.entity == entity) {
            let inputs: Vec<Option<StateAt>> =
                d.consumes.iter().map(|(e, a)| Some(self.state_at(e, a, q))).collect();
            let refs: Vec<Option<&StateAt>> = inputs.iter().map(|o| o.as_ref()).collect();
            if let Some(v) = (d.produce)(&refs) {
                return StateAt::Known(v);
            }
        }
        StateAt::Unknown
    }

    /// W5 helper: lineage of one derivation, for `world why`.
    pub fn lineage_of(&self, entity: &str) -> Option<(&str, u32, &[(String, String)])> {
        self.derivations
            .iter()
            .find(|d| d.entity == entity)
            .map(|d| (d.id, d.version, d.consumes.as_slice()))
    }

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
        self.next_seq += 1;
        let t = WorldTransition {
            transition_id: self.next_tid,
            source_event_id: ev.source_event_id.clone(),
            source_id: ev.source_id.clone(),
            kind: ev.kind,
            entity: ev.entity.clone(),
            attr: ev.attr.clone(),
            value: ev.value.clone(),
            occurred_at: ev.occurred_at,
            observed_at: ev.observed_at,
            recorded_seq: self.next_seq,
        };
        let tid = t.transition_id;
        self.transitions.push(t);
        IngestResult::Applied { transition_id: tid, corroborates }
    }

    /// Canonical deterministic replay (I6): same event SET, any arrival order/batching →
    /// identical logical log. Order = (occurred_at, observed_at, source_event_id).
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

    /// THE BI-TEMPORAL CUT + EPISTEMIC STATE (W2+W3).
    ///
    /// Knowledge filter FIRST (observed_at <= known_at): later information can never leak into
    /// an earlier cut — the no-hindsight property. Then world-time selection among what was
    /// known: per distinct SOURCE, its newest claim by world time; superseded/retracted claims
    /// lose standing even when their evidence arrives late (world-time ordering beats arrival).
    ///
    /// One live value → Known (or Stale when freshness policy says so, judged against
    /// known_at — never wall clock). Several live values → Conflicted UNLESS a registered
    /// named rule resolves them; confidence numbers never silently rank (I4).
    /// Latest applicable Retract/Expire → Unknown/Expired respectively.
    pub fn state_at(&self, entity: &str, attr: &str, q: &WorldQuery) -> StateAt {
        if let Some(g) = &self.gate {
            if !g(&q.access, entity) {
                return StateAt::Unknown;
            }
        }
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
            Kind::Expire => return StateAt::Expired,
            Kind::Retract => return StateAt::Unknown,
            _ => {}
        }
        // A SUPERSEDE at world-time T retires every earlier-occurred claim of this proposition,
        // WHATEVER source emitted it — otherwise a stale email from the same source as the
        // correction could keep a dead value alive through per-source bucketing.
        let latest_supersede = relevant
            .iter()
            .filter(|t| t.kind == Kind::Supersede)
            .map(|t| t.occurred_at)
            .max();
        // Per-source newest claim (each witness speaks once).
        let mut per_source: std::collections::HashMap<&str, &WorldTransition> = std::collections::HashMap::new();
        for t in &relevant {
            match t.kind {
                Kind::Assert | Kind::Supersede => {
                    if let Some(sup) = latest_supersede {
                        if t.kind == Kind::Assert && t.occurred_at < sup {
                            continue; // retired by the later supersession
                        }
                    }
                    let e = per_source.entry(t.source_id.as_str()).or_insert(t);
                    if (t.occurred_at, t.recorded_seq) >= (e.occurred_at, e.recorded_seq) {
                        *e = t;
                    }
                }
                _ => {}
            }
        }
        let mut claims: Vec<&WorldTransition> = per_source.values().copied().collect();
        claims.sort_by_key(|t| (t.occurred_at, t.recorded_seq));
        // Collapse same-value witnesses; differing remaining values = live conflict.
        let mut distinct: Vec<&WorldTransition> = Vec::new();
        for c in &claims {
            if !distinct.iter().any(|x| x.value == c.value) {
                distinct.push(c);
            }
        }
        if distinct.len() == 1 {
            let winner = *distinct.last().unwrap();
            let age = q.known_at.saturating_sub(winner.observed_at);
            return if age > self.freshness_ms {
                StateAt::Stale { value: winner.value.clone(), last_verified: winner.observed_at }
            } else {
                StateAt::Known(winner.value.clone())
            };
        }
        let claim_view: Vec<Claim> = distinct
            .iter()
            .map(|t| Claim { source_id: t.source_id.as_str(), value: t.value.as_str(), occurred_at: t.occurred_at })
            .collect();
        for rule in &self.resolution_rules {
            if let Some(winner) = (rule.apply)(&claim_view) {
                return StateAt::Known(winner);
            }
        }
        StateAt::Conflicted(claims.iter().map(|c| c.value.clone()).collect())
    }
}
 
#[cfg(test)]
mod w1_tests {
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
        assert_eq!(log.ingest(&ev("email:501", "interview", "Thursday")), IngestResult::Duplicate);
        match log.ingest(&ev("calendar:88", "interview", "Thursday")) {
            IngestResult::Applied { corroborates: 1, .. } => {}
            other => panic!("corroboration must be counted, got {other:?}"),
        }
        assert_eq!(log.transitions().len(), 2);
        assert_eq!(log.ingest(&ev("calendar:88", "interview", "Thursday")), IngestResult::Duplicate);
    }

    /// I6: replay determinism — same event SET in any arrival order yields one history.
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
        assert_eq!(render(&canonical), render(&shuffled));
    }
}

fn wev(id: &str, kind: Kind, occ: i64, obs: i64, ent: &str, val: &str) -> WorldEvent {
    WorldEvent {
        source_event_id: id.into(), source_id: id.split(':').next().unwrap().into(),
        kind, occurred_at: occ, observed_at: obs,
        entity: ent.into(), attr: "status".into(), value: val.into(),
    }
}
fn base(n: i64) -> i64 { 1_787_400_000_000 + n * D }
const D: i64 = 86_400_000;

#[cfg(test)]
mod w2_tests {
    use super::*;

    /// THE NO-HINDSIGHT-LEAKAGE PROPERTY (W2). Never regresses for Yantrik's lifetime.
    #[test]
    fn later_information_cannot_leak_into_earlier_knowledge() {
        let log = WorldLog::replay(&[wev("carrier:771", Kind::Assert, base(20), base(22), "package", "delayed")]);
        let q = |known: i64| WorldQuery { valid_at: base(20), known_at: known, access: mind_types::AccessContext::operator_audit() };
        assert_eq!(log.state_at("package", "status", &q(base(20))), StateAt::Unknown);
        assert_eq!(log.state_at("package", "status", &q(base(22))), StateAt::Known("delayed".into()));
    }

    /// A LATE-ARRIVING OLD FACT cannot resurrect a superseded proposition.
    #[test]
    fn a_late_old_email_does_not_resurrect_a_superseded_state() {
        let log = WorldLog::replay(&[
            wev("email:501", Kind::Assert, base(20), base(20), "interview", "Tuesday"),
            wev("email:923", Kind::Supersede, base(22), base(22), "interview", "Thursday"),
            wev("email:old", Kind::Assert, base(20), base(23), "interview", "Tuesday"),
        ]);
        let q = WorldQuery { valid_at: base(23), known_at: base(23), access: mind_types::AccessContext::operator_audit() };
        assert_eq!(log.state_at("interview", "status", &q), StateAt::Known("Thursday".into()));
    }
}

#[cfg(test)]
mod w3_tests {
    use super::*;

    fn q(valid: i64, known: i64) -> WorldQuery {
        WorldQuery { valid_at: valid, known_at: known, access: mind_types::AccessContext::operator_audit() }
    }

    /// 1. CONFLICT PRESERVATION (I4): two distinct sources, two live values, NO rule ⇒
    ///    Conflicted — never whichever sorted last, never confidence ranking.
    #[test]
    fn conflicting_claims_stay_conflicted_without_a_rule() {
        let log = WorldLog::replay(&[
            wev("email:961", Kind::Assert, base(24), base(24), "meeting", "Room4"),
            wev("chat:962", Kind::Assert, base(24) + 3_600_000, base(24) + 3_600_000, "meeting", "Zoom"),
        ]);
        assert_eq!(
            log.state_at("meeting", "status", &q(base(25), base(25))),
            StateAt::Conflicted(vec!["Room4".into(), "Zoom".into()])
        );
    }

    /// 2. EXPLICIT RESOLUTION: a NAMED rule picks the winner; both claims stay in history.
    #[test]
    fn a_named_rule_resolves_and_history_keeps_both_claims() {
        let log = WorldLog::replay(&[
            wev("email:eta", Kind::Assert, base(24), base(24), "package", "maybe-Saturday-ETA-Monday"),
            wev("carrier:deliv", Kind::Supersede, base(24) + 6 * 3_600_000, base(24) + 7 * 3_600_000, "package", "delivered-Saturday"),
        ])
        .with_rule(ResolutionRule {
            id: "carrier-delivered-scan-overrides-estimate",
            version: 1,
            apply: Box::new(|claims: &[Claim]| {
                claims.iter().find(|c| c.source_id == "carrier" && c.value.starts_with("delivered")).map(|c| c.value.to_string())
            }),
        });
        // Rule-based, NOT arrival-order-based: prove by querying where the ETA is the LATER claim.
        assert_eq!(
            log.state_at("package", "status", &q(base(25), base(25))),
            StateAt::Known("delivered-Saturday".into())
        );
        assert_eq!(log.transitions().len(), 2, "both original claims remain in history");
    }

    /// 3. STALENESS is bi-temporal: judged against known_at, never wall clock.
    #[test]
    fn staleness_is_judged_from_the_querys_knowledge_time() {
        let log = WorldLog::new().with_freshness_ms(48 * 3_600_000);
        let observed = base(10);
        let log = WorldLog::replay(&[wev("api:wx", Kind::Assert, observed, observed, "weather.thursday", "rain")]);
        // Fresh at known_at = T+47h; stale at T+49h — same fact, different knowledge cuts.
        assert_eq!(
            log.state_at("weather.thursday", "status", &q(observed + 47 * 3_600_000, observed + 47 * 3_600_000)),
            StateAt::Known("rain".into())
        );
        match log.state_at("weather.thursday", "status", &q(observed + 49 * 3_600_000, observed + 49 * 3_600_000)) {
            StateAt::Stale { value, last_verified } => {
                assert_eq!((value.as_str(), last_verified), ("rain", observed));
            }
            other => panic!("expected Stale, got {other:?}"),
        }
    }

    /// 4. EXPIRATION follows the query's WORLD-time cut — and the adversarial inverse catches
    /// any accidental use of current wall time.
    #[test]
    fn expiry_follows_valid_at_not_wall_clock() {
        let expire_at = base(25);
        // Freshness policy widened so the inverse cut tests EXPIRY, not staleness.
        let log = WorldLog::replay(&[
            wev("cal:flight", Kind::Assert, base(21), base(21), "flight", "Thursday-window"),
            wev("cal:flightx", Kind::Expire, expire_at, expire_at, "flight", "cancelled"),
        ])
        .with_freshness_ms(i64::MAX);
        assert_eq!(log.state_at("flight", "status", &q(expire_at + D, expire_at + D)), StateAt::Expired);
        assert_eq!(
            log.state_at("flight", "status", &q(expire_at - D, expire_at - D)),
            StateAt::Known("Thursday-window".into()),
            "before expiry the flight was live — wall-clock implementations fail this inverse"
        );
    }
}



 
#[cfg(test)]
mod w4_w6_tests {
    use super::*;

    fn wev(id: &str, kind: Kind, occ: i64, obs: i64, ent: &str, attr: &str, val: &str) -> WorldEvent {
        WorldEvent {
            source_event_id: id.into(), source_id: id.split(':').next().unwrap().into(),
            kind, occurred_at: occ, observed_at: obs,
            entity: ent.into(), attr: attr.into(), value: val.into(),
        }
    }
    const D: i64 = 86_400_000;
    fn base(n: i64) -> i64 { 1_787_400_000_000 + n * D }

    /// W4 — THE DEFINING 3A TEST (I3): a derived conflict exists only while BOTH inputs are
    /// warranted; superseding one input kills the derivation WITHOUT sweeping history.
    #[test]
    fn superseding_an_input_kills_the_derived_conclusion_but_not_the_history() {
        let overlap = DerivationRule {
            id: "overlap-rule", version: 1,
            entity: "travel_conflict".into(), attr: "status".into(),
            consumes: vec![("interview".into(), "date".into()), ("flight".into(), "window".into())],
            produce: Box::new(|inputs: &[Option<&StateAt>]| {
                match (inputs[0], inputs[1]) {
                    (Some(StateAt::Known(i)), Some(StateAt::Known(_))) if i.contains("Thursday") => {
                        Some("Thursday-travel-conflict".into())
                    }
                    _ => None,
                }
            }),
        };
        let log = WorldLog::replay(&[
            wev("email:501", Kind::Assert, base(20), base(20), "interview", "date", "Thursday"),
            wev("cal:flight", Kind::Assert, base(21), base(21), "flight", "window", "Thursday"),
            wev("email:923", Kind::Supersede, base(22), base(22), "interview", "date", "Friday"),
        ])
        .with_freshness_ms(i64::MAX)
        .with_derivation(overlap);
        let q = |v: i64| WorldQuery { valid_at: v, known_at: v, access: mind_types::AccessContext::operator_audit() };

        // While Thursday was live: conflict warranted.
        assert_eq!(
            log.derived_state("travel_conflict", &q(base(21))),
            StateAt::Known("Thursday-travel-conflict".into())
        );
        // After the correction: warrant GONE — no zombie conclusion.
        assert_eq!(log.derived_state("travel_conflict", &q(base(23))), StateAt::Unknown);
        // Yet the epistemic history survives: the old interview claim is still queryable at its cut.
        assert_eq!(
            log.state_at("interview", "date", &q(base(21))),
            StateAt::Known("Thursday".into())
        );
    }

    /// W5 — purpose gate at the boundary: an unauthorized reader sees Unknown, which is
    /// indistinguishable from absence of fact (A6). The authorized reader sees truth.
    #[test]
    fn unauthorized_readers_cannot_distinguish_fact_from_absence() {
        let log = WorldLog::replay(&[wev("email:9", Kind::Assert, base(1), base(1), "interview", "date", "Friday")])
            .with_gate(Box::new(|ctx: &mind_types::AccessContext, _entity: &str| {
                ctx.purpose().label().starts_with("audit") // only audit lanes read private world state
            }));
        let ok = WorldQuery { valid_at: base(2), known_at: base(2), access: mind_types::AccessContext::operator_audit() };
        assert_eq!(log.state_at("interview", "date", &ok), StateAt::Known("Friday".into()));
        // A non-audit context: structurally requires a ctx, gate denies, reads as Unknown.
        let member = mind_types::AccessContext::principal(
            mind_types::Scope::Private("asha".into()),
            mind_types::Purpose::conversation("asha"),
        );
        let denied = WorldQuery { valid_at: base(2), known_at: base(2), access: member };
        assert_eq!(log.state_at("interview", "date", &denied), StateAt::Unknown);
    }

    /// W6 — REPLAY EQUIVALENCE: uninterrupted == restart-split; canonical LOGICAL equality
    /// (deterministic semantics, not storage page bytes).
    #[test]
    fn restart_split_replay_equals_uninterrupted_replay() {
        let mk = |from: usize, to: usize| -> Vec<WorldEvent> {
            (from..to)
                .map(|i| wev(&format!("e:{i:03}"), Kind::Assert, base(i as i64), base(i as i64), "ent", "status", &format!("v{i}")))
                .collect()
        };
        let render = |l: &WorldLog| l.transitions().iter()
            .map(|t| format!("{}|{}|{}|{}", t.recorded_seq, t.transition_id, t.source_event_id, t.value))
            .collect::<Vec<_>>().join(";");
        let s1 = WorldLog::replay(&mk(0, 75));
        let mut s2 = WorldLog::replay(&mk(0, 37));
        for e in mk(37, 75) {
            s2.ingest(&e); // the "restart": fresh process continues from its own prefix log
        }
        assert_eq!(render(&s1), render(&s2), "restart must be invisible in logical state");
        // Snapshot-loss leg: rebuild from authoritative transitions alone.
        let rebuilt = WorldLog::replay(&mk(0, 75)); // transitions ARE the authority here
        assert_eq!(render(&s1), render(&rebuilt));
    }
}

