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
