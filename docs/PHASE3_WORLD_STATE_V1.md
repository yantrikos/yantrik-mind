# PHASE 3 — World State v1 ("Situation Model")

*Commissioned 2026-08-25, after `cognitive-closure-v1`. Direction: Yantrik becomes the substrate
that gives interchangeable LLMs continuity, state, agency, experience and consequences — not
another planner. This file is the build contract; the epistemology is the Phase-2 ledger's.*

## The question this answers

Memory answers *"what happened before?"*. The world model answers **"what is true NOW?"**.
Prediction: *"what will probably be true next?"*. Executive: *"given all that, what should I do —
including nothing?"*.

## v1 scope — five primitives, no ontology

```text
Entity · State · Goal · Event · Relation
+ confidence, valid_from, valid_until, source, last_verified
```

Events update state via TYPED TRANSITIONS with evidence — never free-form LLM rewrites:

```text
Event "interview moved Tue→Thu"
  → supersedes state interview.date=Tuesday (tombstone reason: superseded-by-event)
  → current state interview.date=Thursday
  → derived consequences (deterministic): Tuesday-prep deadline demotes; Thursday travel conflict
    candidate; Wednesday opens.
  → executive candidates: reschedule prep / check calendar / DO NOTHING (first-class output)
```

## Mapping existing organs IN (no rewrite)

| Existing organ | Becomes |
|---|---|
| FutureNodes KV (`decisions.rs`) | Goal/Event primitives keep their store; world model REFERENCES by id |
| Tasks + commitments | Things-in-motion states |
| Beliefs (YantrikDB) | Beliefs section: known/uncertain/contradicted/stale — already derived |
| Capability evidence + bandits | Self section |
| Forecasts + judgment ledger | Predictions section |
| Poll-loop gates | Event SOURCES feeding transitions |

Rule: the world model is a VIEW + transition log over authoritative stores (flight-recorder
discipline again — observe, don't duplicate truth). One new store max: `world_transitions` JSONL,
hash-chained like the rest.

## Milestone (pre-registered)

A simulated week/month harness: 50–100 seeded events (mail arrives, deadline moves, package
delays, promises made, replies arrive, tool unavailable, weather changes, contradictions land).
Pass = continuously answer from persisted state, without inventing obligations, pestering, or
losing causality: *what changed / true now / waiting-on / becoming-risky / preparable / do next /
do NOT bother user about / why.* Safe actions allowed and scored. Red test FIRST: the harness
fails against today's organs before v1 exists.

## Executive attention comes AFTER v0 world state

AttentionBoard graduates from cron-sorting to: WORLD STATE → goals+predictions →
opportunities/risks → decide {ignore | monitor(wait) | act}. "Nothing deserves attention" must be
a measured, creditable output.

## Explicitly deferred (order matters)

Long-horizon goal reasoning → counterfactual simulation → broader perception surfaces → deeper
autonomous capability acquisition. Not before the world model exists; #6 is partially underway
already (capability evidence loop).

---

# Amendments (2026-08-25, ratified before any implementation)

**A1 — Two clocks, always.** World time vs knowledge time: `occurred_at` (when it was actually
true) is distinct from `observed_at` (when Yantrik learned it). A delay that happened Aug 20 but
was learned Aug 22 must never replay as known-on-Aug-20. Add `valid_from/until`, `recorded_at`
where meaningful; event-time vs knowledge-time is non-negotiable.

**A2 — Typed transitions BEFORE elaborate primitives.** Vocabulary ≥ {ASSERT, UPDATE, SUPERSEDE,
RETRACT, EXPIRE, LINK, UNLINK, ACTIVATE/BLOCK/UNBLOCK/SATISFY/ABANDON_GOAL, START_WAIT,
RESOLVE_WAIT}. **No world-state mutation without saying what KIND of change occurred and why** —
"Tue→Thu" alone could be reschedule, correction, confirmation, confusion, or contradiction.

**A3 — Open-world rule.** Absence of a known state is not evidence of its opposite. Current-state
API represents `Known(value) | Unknown | Conflicted(candidates) | Stale(last_verified) | Expired`.
Reuse the belief machinery's epistemic philosophy; do not invent another confidence model.

**A4 — Lineage on every derived item.** Each derived fact carries `derived_from: [event/
transition ids] + rule_id + rule_version`. "Thursday travel conflict" must traverse back to email
id 892 via named rules — `ym why` answers from evidence, never retrospective narration.

**A5 — Store invariant restated.** Not "one new store max" but: **exactly one authoritative
transition history; every materialized world-state representation is disposable and reproducible**
from authoritative sources + transitions. Corrupted snapshot = rebuild, not incident.

**A6 — Purpose Gate at the boundary from day one.** WorldQuery, WorldTransition, and
DerivedConsequence all carry AccessContext. The question is never "what is true now?" but
"**what am I authorized to know is true now, for this purpose?**" ARCH4 intact or nothing ships.

**A7 — Harness = oracle-scored, nasty by design.** Maintain Oracle(t); score separately:
current-state precision/recall · stale false-positives · supersession correctness · lineage
accuracy · waiting-on accuracy · risk detection + FALSE risk rate · unnecessary/missed
interventions · restart continuity · out-of-order correctness · duplicate idempotency. Required
fixtures include the late-old-email sequence, the Saturday-delivery-overrides-Monday-ETA
sequence, and the promised-document-conflict sequence.

**A8 — Restart/replay is a hard requirement.** Kill mid-stream at event 37, restart, feed 38–60:
answers identical to uninterrupted execution. Duplicate, out-of-order, late, retracted events;
corrupted derived state — all explicit cases.

**A9 — Watch for `Expectation`, do not build it.** If "waiting on Alice's document by Friday"
stays awkward across fixtures, that is evidence for a sixth primitive (expected transition /
obligation). Evidence, not aesthetics, promotes it.

**A10 — WORLD STATE IS NOT A SUMMARY.** In giant letters. Authoritative records + typed
transitions + deterministic derivations = current state; the LLM reasons OVER it. An LLM may
PROPOSE consequences ("maybe Thursday creates a travel conflict") only as candidate hypotheses,
promoted by deterministic/tool-backed verification. This is how the world avoids being slowly
hallucinated.

**Milestones split — no sneaking 3B into 3A:**

```text
Phase 3A — Situation awareness:
  At arbitrary simulated T, after arbitrary valid H (duplicates, delays, contradictions,
  supersessions, restarts): reconstruct authorized present state, identify unresolved
  transitions + imminent risks, explain every non-source fact through causal lineage.

Phase 3B — Executive cognition (separate milestone):
  Given that state → ignore / monitor / act → outcome → learning.
  Intervention improves outcomes over doing nothing.
```

Arc: Phase 2 taught *“was my action a good choice?”* → Phase 3 teaches *“what situation am I
actually in?”* → executive asks *“what deserves attention?”* → counterfactuals ask *“what
situation would I create?”*. Experience → situation → attention → imagination.
 
# Phase 3A authorized - five implementation invariants (2026-08-25)

**I1 - Stable entity identity.** Transitions target stable `entity_id`s, never free-form names.
Minimal: `EntityRef { entity_id, entity_type_hint?, aliases[], source_refs[] }`. Alias resolution
is evidence-backed; ambiguous identity STAYS ambiguous (no silent merges of Alice / Alice Smith /
alice@example.com; no silent split of two Johns).

**I2 - Bi-temporal query API.** A1 through to the surface:
`WorldQuery { valid_at /* world time */, known_at /* knowledge time */, access_context }`.
"What was true Aug 20" is NOT "what had Yantrik LEARNED by Aug 20". Later information must never
leak backward into past cognition - this is what makes future regret/counterfactual evaluation fair.

**I3 - Derivation invalidation (hard 3A test).** When a source transition is retracted or
superseded, dependent derivations lose their warrant: find dependents via lineage, re-evaluate,
then retain / supersede / retract. No zombie conclusions whose evidence no longer exists.

**I4 - Conflict is never secretly ranking.** .91 > .87 does NOT turn Conflicted into Known.
Conflicts persist until an explicit NAMED deterministic resolution rule justifies choosing
(e.g., carrier-delivered-scan > estimated-delivery email). Confidence is not authority.

**I5 - Derived knowledge inherits privacy conservatively.** provenance_scope,
purpose_constraints, subject scope and sensitivity derive from lineage: Private(A) + Private(B)
means derived C stays protected. Computation must not launder restricted information into
unrestricted world state.

## Engine shape (small on purpose)

Authoritative sources -> events -> normalize/identify -> typed transitions -> THE transition log ->
{replay | derivations} -> materialized world view -> Purpose Gate -> WorldQuery.
NO WorldBrain. NO world-model LLM loop. NO ten managers. Intelligence comes from correct state
evolution. LLM-proposed consequences enter only as candidate hypotheses (A10).

## Red benchmark (built FIRST, brutal by design)

~75 events with Oracle(t), deliberately including: normal changes, duplicates, late events,
out-of-order, corrections, contradictions, retractions, expiration, entity ambiguity, restart,
snapshot loss, derived-state invalidation, purpose-denied queries, unknown states, stale states.
Checkpointed at t=10/25/37 --RESTART-- 38/52/75 (a break at event 17 must not be accidentally
corrected by event 43).

**Precision beats recall in 3A:** current-state precision, false-current rate, false-obligation
rate and false-risk rate outrank recall. Architectural principle: UNKNOWN IS PREFERABLE TO
INVENTED CERTAINTY. "I don't know whether it was delivered" is useful; "it's delayed until Monday"
after Saturday delivery happened is dangerous.

**Knowledge-time non-leakage test (validates all of A1):** delay occurred Aug 20; decision D made
Aug 20 without knowledge; informing email arrives Aug 22.
Query(valid_at=Aug20, known_at=Aug20) => Unknown.
Query(valid_at=Aug20, known_at=Aug22) => Known(delayed).
Decision D may never be graded using facts learned two days later.

**Situation inference vs executive judgment, separated religiously:** "deadline in 18h and
document unresolved" is 3A. "This deserves interrupting the user" is 3B. Derived facts stay
disentangled from prior actions so 3B receives a trustworthy situation.

## Surfaces

ym world now                    CURRENT / STALE / CONFLICTED sections - evidence rendering
ym world diff <time|transition> + moved Tue->Thu / - Tuesday prep urgency / + delivered / ! overdue
ym world why <state-id>         lineage tree: rule <- state <- transition <- email msg-492

Evidence rendering, never LLM narration.

## Success, stated

After 3A Yantrik can say: "X used to be true, Y replaced it, Z is unresolved, Q is stale, this
conflict exists because of these two events, and this was all I knew at the time." Then 3B asks
the genuinely Jarvis question over that trustworthy situation - and counterfactuals become
simulation over the real model: WorldState(now) apply-A vs apply-B. Every additional primitive
beyond these five is earned from failing scenarios, exactly as Phase 2 earned its architecture.
