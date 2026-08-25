# Phase 2 — Experimental Ledger

*Every Phase-2 slice obeys the same epistemology it is supposed to give the mind: hypothesis →
baseline → change → expected metric → actual metric → regressions → decision (KEEP / REVISE /
REVERT). An entry without a measured baseline does not get a decision. This ledger is the
development process's own flight recorder.*

---

## E.0 — Discovery fit-threshold (Phase-1 carry-over, 2026-08-24)

| Field | Entry |
|---|---|
| Hypothesis | A multi-word `discover_tools` query matching a catalog line on ONE weak description word is a false fit; requiring ≥2 overlaps (or a name match) removes the false positive without losing true discovery. |
| Baseline | `cognitive_loop_behavioral_suite_passes` RED: scenario "honest empty results are barren steps" scored 3/4 — `barren_steps` 0 instead of ≥3, because each false-fit result entered the capsule as evidence and reset the stall counter. |
| Change | `tool_catalog.rs:search_lines` threshold: multi-token queries require score ≥2; single-token queries unchanged; explicit tool names always qualify. |
| Expected metric | Eval suite green with failures==0 preserved; existing `search_finds_a_gated_tool_by_description` stays green ("track a price drop" has price+drop = 2 overlaps). |
| Actual metric | Suite green (18/18 in mind-evals lib); workspace 867 passed / 0 failed; watch_price discovery fixture unaffected. |
| Regressions | None observed. Pre-existing rustfmt drift across repo left untouched deliberately. |
| Decision | **KEEP** |

## E.1 — Memory actor scheduling (G.1): lanes REJECTED, off-thread execution KEPT

| Field | Entry |
|---|---|
| Hypothesis | Bulk memory commands head-of-line-block interactive reads on the single-owner actor; a second lane drained interactive-first bounds the delay. |
| Baseline | Seeded file-backed store (~8.5k transcript rows). Single FIFO, bulk op on-actor: an interactive `recent_messages` issued mid-VACUUM waited **65.5 ms of the 70.0 ms** background total — the stall is real and large. |
| Change A (lanes, implemented first) | Two crossbeam channels, drain-interactive-first pump, command-property classification (`Cmd::lane()`), per-lane depth/high-water gauges. Consistency invariant documented: background whitelist = pure reads / file copies / idempotent sweeps; all causal-chain writes stay FIFO; same-caller ordering by await-per-command regardless of lane. |
| Measured A | **FAILED its own goal**: interactive read still waited **65.5 / 70.0 ms**. Lanes reorder *queued* work; nothing preempts a *running* command. The actor was busy inside VACUUM INTO when the read arrived — queue arrangement cannot fix service-time blocking. |
| Change B (off-thread execution) | `SnapshotTo` moved off the actor thread (it already opened its own read-only connection and touches no actor state; fallback-to-inline if thread spawn fails). |
| Measured B | Two lanes + off-thread: interactive **7.3 ms** during a 79.4 ms copy. Forced single-FIFO + off-thread: interactive **17.4 / 44.4 ms**, also passes with no lane machinery. Final config (single FIFO + off-thread): interactive **7.7 ms** during a 74.3 ms copy. |
| Conclusion | The measured win came entirely from *not occupying the actor with self-contained bulk work*, not from lane ordering. Keeping lanes would be sunk-cost reasoning. |
| Regressions | None: 45+1 mind-memory tests green incl. read-your-writes-through-the-queue, same-caller ordering determinism, concurrent-load exactly-once + backlog drain, and the latency experiment (kept as an `#[ignore]`d explicit tool). Dependency footprint unchanged (crossbeam dep added then removed). |
| Decision | **REVISE → simpler design.** REJECT two-lane queues. KEEP: (a) off-thread self-contained bulk execution (the pattern to copy when another command measures as a stall); (b) single-queue backlog gauge + high-water mark exposed via facade `backlog_depth()` — the tripwire that justifies future off-thread splits; (c) the scheduling doctrine written into the source where the next person will look before reintroducing lanes. BUILD.md's "priority lanes" requirement should be read as intent (protect interactive latency) — satisfied by (a)+(b), not by queues. Follow-up candidates if ever measured necessary: off-thread Export via read-only conn; two-phase RetroDedup (detect off-thread, apply small batch on-actor). |

## E.4 — Flight Recorder v1 — BUILT

| Field | Entry |
|---|---|
| Hypothesis | A hash-chained append-only decision log keyed by trace_id makes cognition reconstructable ("what did it know / predict / choose / learn") from persisted evidence, without becoming another source of truth and without failing cognition when observability fails. |
| Baseline | trace_id plumbed end-to-end, consumed by nothing (`mind-types/src/event.rs:51`; observability crate was a one-line stub). Debugging cognition = journalctl archaeology; prediction-vs-outcome existed only inside the foresight/judgment ledgers with no cross-organ causal path. |
| Change | `mind-observability` revived as the flight recorder: `DecisionEvent` (every ARCH-5 field optional), `DecisionLog` (JSONL, `chain = sha256(prev ++ event_json)`, genesis-rooted — same discipline as receipts.rs/immune ledger), redaction via the SAME secret detector that guards memory writes + per-field truncation budgets, fail-sticky `record()` (first write failure disables the log with one warning; can never fail its caller), `for_db()` path convention (`YM_DECISION_LOG` > `<db>.decisions.jsonl` > disabled). Emit sites wired: cognitive runs (incl. compile-time refusals) under `run-<ts>` traces; packet created/resolved/expired under the packet id as trace; reflex enqueues carry cluster evidence + predicted metric + rollback condition; foresight predictions graded hit/miss carry made-confidence vs outcome under the judgment-ref trace. Read side: `DecisionLog::read_trace(prefix)` + `render_trace`, exposed as `ym why [prefix]` / REPL `:why [prefix]`. |
| Expected metric | Full test battery green (append/read, chain integrity, tamper detection, deletion breakage, reopen continuation, multi-event reconstruction, truncation, secret redaction, disabled/failure tolerance); one end-to-end wiring proof (packet create→reject reconstructable through `why()`, chain verifies on disk); workspace suite stays green. |
| Actual metric | 8/8 observability tests pass; wiring test passes (create+resolve share trace `pkt:<hex>`, rendered fields all present, `verify_log == Ok(2)`); **workspace 879 passed / 0 failed** (was 867 pre-phase). Clippy clean on the new crate. |
| Regressions | None observed. Recorder is observe-only; no authoritative store reads it. Eval harnesses get `disabled()` by default so no test writes files. |
| Decision | **KEEP** |

### Phase-2 target-state check (flight-recorder slice)

For a cognitive run, a prepared packet, a queued self-improvement, and a graded forecast, the
following are now answerable from persisted evidence via `ym why <trace>`: what triggered it,
what goal was active, which evidence ids backed it, what alternatives/rejections were recorded,
which policy/budget lines ran, what was predicted and with what confidence, what actually
happened, and what lesson/calibration changed because of it. Remaining gaps toward full closure
(deliberately not in this slice): tool-call-level prediction stamps (§2/§3 packet schema v2),
baseline-vs-candidate behavioral gates (§6), executable rollback evaluation (§7), AttentionBoard
shadow events (§8 — now unblocked, since the recorder exists).

---

## E.R1 — Recorder tightening (spans + health), 2026-08-24

| Field | Entry |
|---|---|
| Hypothesis | (a) Span linkage (`event_id`/`parent_event_id`/`object_id`) added now costs nothing and later turns traces into causal trees; (b) fail-sticky disabling is the wrong failure mode — backoff-retry keeps observability alive across transient disk errors without ever risking cognition. |
| Baseline | Flat trace labels only; one transient write failure silenced the recorder for the rest of the process. |
| Change | Three optional serde fields + `DecisionEvent::span()` constructor + render lines; `RecorderHealth` (failure → exponential backoff window 30s→10min cap → silent no-op inside window → automatic retry after → success resets healthy). Chain anchoring deliberately deferred per review. |
| Expected metric | All recorder tests green incl. new span-tree test; blocked-path test still passes with revised semantics; no cognition impact. |
| Actual metric | 9/9 observability tests pass; workspace stays green. |
| Regressions | None. |
| Decision | **KEEP** |

## E.6 — Continuity capture hoisted above the loop fork

| Field | Entry |
|---|---|
| Hypothesis | Taught-belief capture ("remember that X") and commitment capture ("remind me to X tomorrow") sat below the `agent_primary` early-return — dead under default config — so continuity depended on the model choosing `remember`/`add_reminder`. Hoisting both above the fork makes capture loop-independent while preserving exactly-once via existing dedup (proposition-keyed beliefs; jaccard/cosine task dedup). |
| Baseline | Verified again in source: extractors at lib.rs:9205–9220 executed ONLY when `YM_AGENT=off`; both agent paths returned at :8979–8994. Under defaults, "remind me tomorrow" survived only as long as prompt compliance held. |
| Change | Capture block moved to immediately before the fork (after all early-returning intercepts, preserving prior semantics); old inline block deleted (no double-capture); each capture emits a flight-recorder event (`belief_taught`, `commitment_captured`) carrying its object id (`belief:<rid>`, `task:<id>`) so follow-through can parent onto it later. |
| Expected metric | Paired fixtures: same turns through BOTH sides of the fork produce identical durable effects with a deliberately useless model (capture must not need model cooperation); repeated promise leaves exactly ONE open task; full suite green. |
| Actual metric | 4/4 fixtures pass (`commitment_is_captured_on_the_default_agent_path`, `taught_belief_is_captured_on_the_default_agent_path`, `capture_effects_are_identical_across_loops`, `repeated_commitment_does_not_duplicate_tasks`); workspace **886 passed / 0 failed**. |
| Regressions | None observed. Note: turns matching early intercepts (courier acks etc.) still skip generic capture exactly as before — parity preserved by placement. |
| Decision | **KEEP** |

## E.L1 — First closed learning chain: tool calls with empirical priors

| Field | Entry |
|---|---|
| Hypothesis | One narrow action class can carry the full chain OBSERVE→PREDICT→ACT→EXPECTED→OBSERVED→ERROR→LESSON, with prediction confidence taken ONLY from empirical history (the per-tool Beta bandit), never invented by a model. |
| Baseline | Tool calls recorded outcomes only (`record_tool_outcome(tool, ok)`) — no prediction existed before dispatch, so "it worked" could never be distinguished from "I thought it would work". |
| Change | `EngineBus::call`: pre-dispatch event `tool_predicted` {predicted proposition, confidence = bandit posterior mean, policy line declares n and low-N status}; post-dispatch `tool_observed` {five-way verdict, prediction_error = observed(0/1) − prior where meaningful, lesson}; Unavailable/Denied excluded from error grading by design (capability gaps are not wrong predictions). Refusals record verdict=denied with no error number. |
| Expected metric | Call 1 on a virgin tool: prior 0.5 labeled n=0/low-N. After one observed success: prior moves to the bandit's Beta-smoothed mean 2/3. Chain verifies on disk. |
| Actual metric | Both tests green. Mid-slice correction caught BY ITS OWN TEST: my first implementation stacked an extra shrinkage layer on top of the bandit's already-Beta(1,1)-smoothed mean (got 5/9 instead of 2/3) — double-counting the uniform prior. Removed; the posterior mean is used directly. The test that failed is the one that found it. |
| Regressions | None; evals unaffected (recorder disabled by default in harnesses; extra track-record read is one indexed query on :memory: in tests). |
| Decision | **KEEP** — with the honest scope note: prediction/observation events currently root their own `toolcall-<ts>` traces (object_id carries the call signature). Threading the run-level trace id through the Bus trait so tools parent under their cognitive run is next, now trivial via the span fields. |

**Running totals since Phase-2 start:** tests 867 → 886 passing, 0 failing. Slices kept: G.0 fit-threshold, off-thread bulk + backlog gauge, Flight Recorder (+spans/health), E.6 continuity hoist, E.L1 tool-call learning chain. Rejected by measurement: two-lane memory scheduling.

---

## E.T1 — Trace tree + Brier calibration + the serde_json scar

| Field | Entry |
|---|---|
| Hypothesis | (a) Tool spans parented under their run's trace make "did this call serve this goal?" answerable; (b) Brier loss is the calibration metric — signed error alone conflates "confidently right" with "accidentally right"; (c) `semantic_success` recorded where determinable keeps execution-success from hardening into capability definition. |
| Change | `Bus::declare_trace` (default no-op) — `Cognition::run` mints `run-<ts>` before acting, `Outcome.trace_id` returned; `EngineBus` roots prediction/observation spans under it and chains observed→predicted via `parent_event_id`. New fields: `brier`, `semantic_success` (Ok=true / Empty=false / else None). `ym why calibration`: confidence-decade buckets pairing predictions to outcomes through span linkage, with OVERCONFIDENT/underconfident flags at ±0.15. |
| **Falsification found by its own test** | The chain-verify test failed `Err(3)` on a four-line log. Diagnosis: **serde_json's default f64 formatting is not round-trip stable** (`0.11111111111111113` → `...112`, 1 ulp). Any hash-chained ledger signing floats had latent breakage. Fix: workspace-wide `features = ["float_roundtrip"]`, documented in root Cargo.toml. Pinned forever by `floats_survive_write_read_verify_without_ulp_drift`. |
| Metric | 10/10 observability incl. span-tree + float-scar tests; learning-chain tests assert shared trace_id, observed→predicted parentage, brier=(prior−observed)², semantic flags; workspace **890 passed / 0 failed**. |
| Decision | **KEEP** |

## E.G7 — Capability evidence joins selection (the closure milestone, first instance)

| Field | Entry |
|---|---|
| Hypothesis | Per the doctrine now in BUILD.md ("a learning signal is not closed until it affects a future decision"): the tool bandit must change a FUTURE choice, not just keep score. discover_tools ranking is a real decision surface — code-ranked, model-consumed. |
| Baseline | `search_lines`: pure keyword relevance (post-G.0 bar). History invisible → a failing tool with a well-worded description outranks a reliable one forever. |
| Change | `search_lines_with_evidence`: among lines already above the relevance bar, bonus = `1.5 × (rate − 0.5) × min(n,20)/20` ∈ [−0.75, +0.75]; no history = zero bonus = unchanged behavior. Handler passes live bandit rows AND annotates surfaced lines with `· measured ok {r}% (n=…)` when n≥3, so the model sees WHY near-ties rank as they do. |
| Experiment (deterministic) | Two-tool catalog: alpha holds a 3-vs-2 token edge but measures 0.20 n=20; beta measures 0.95 n=20. Baseline ranks alpha first; with evidence beta FIRST. Bounds proven: perfect-history n=100 cannot rescue a below-bar fit; n=2 perfect history (~+0.075) cannot flip even a small gap; the capped full bonus (+0.75) still loses to one extra token of relevance. |
| Expected vs actual | Expected: flip occurs, bounds hold. Actual: all four ranking tests green (`without_evidence_semantics_alone_rank_the_candidates`, `measured_history_overturns_a_semantic_edge`, `evidence_is_bounded_and_cannot_dominate_relevance`). |
| Pending metric (named honestly) | The LIVE outcome delta — funnel success rate of discover_tools-surfaced tools after this join vs before — accumulates via the existing scoreboard/funnel once running on the box. If Y ≤ X, the blend weight or gate gets revised per this ledger's own rules. |
| Also recorded | Three-success contract + P(available)×P(permitted)×P(success) factorization written into `tool_outcome.rs` as design law: Denied/Unavailable feed availability/permission learning, never the success rate; goal_contribution lands where ExpectedOutcomes exist. |
| Decision | **KEEP** (deterministic behavior change proven; live-metric review scheduled by this entry). |

**Totals:** tests 867 → 890 passing, 0 failing. The chain is closed for one action class end-to-end:
*prior → predicted event → dispatch → five-way observation → brier + signed error → bandit update → NEXT SELECTION CHANGES (proven) → surfaced with its measured evidence.*

---

## E.C1 — Counterfactual cohort + goal contribution + the self-build epistemic gate

| Field | Entry |
|---|---|
| Change 1 — policy-disagreement cohort | When measured history changes discover_tools' top pick vs the legacy semantic-only ranking, a `selection_flipped` event records selected vs would-have-been, with reason. Not causal A/B, but across decisions it grades exactly: *"when my learned policy overruled my old policy, how often was it right?"* |
| Change 2 — goal contribution (third success kind) | Graded at the RUN BOUNDARY via new optional `Bus::grade_goal` (Cognition owns its capsule and contract verdict): per evidence-producing tool, `tool_goal_graded` {contributed = a finding CITED its evidence | ran_unused}, parented under the run trace. `ym why contribution` aggregates per-tool rates, declaring young numbers too-young-to-rank below n=3. Mid-slice correction: grading was first placed in the turn wrapper; its own integration test failed (drove `Cognition::run` directly) — moved to the run boundary where it structurally belongs. |
| Change 3 — self-build epistemic gate | `deploy/self_improve.sh`: an automerge candidate's GOAL must carry the falsifiable contract (**hypothesis** / **predicted metric** / **rollback**) or it goes EPISTEMY-DRAFT for human. Reflex goal lines relabeled `REFLEX HYPOTHESIS…` so gated sources qualify; looser generators now face the same bar tests face. Symmetry enforced: Yantrik cannot modify itself under a lower epistemic standard than it uses for tools. |
| Expected | Flip events only when evidence actually overrode; one grade per producing tool under run trace; gate blocks unlabeled goals without breaking reflex flow. |
| Actual | Integration test green (fetch→cited finding→`contributed`, `goal_met=true`, same trace, chain verifies); workspace **891 passed / 0 failed**; `bash -n` clean. |
| Deliberately NOT done | Blend-weight tuning (collect reality first — retrospective weight-fitting once the disagreement cohort has outcomes); conditional bandits P(success|tool,domain) (sparse-data soup before samples exist); AttentionBoard; shadow-execution of legacy picks (design noted, read-only tools later). |
| Decision | **KEEP** |

**The milestone, stated precisely:** Yantrik can now show, from persisted evidence — *this goal needed a tool → I predicted from my own track record → I acted → I observed → I was wrong/right by this much (brier) → my estimate updated → my next choice CHANGED (flip recorded) → the changed choice's goal contribution gets graded → and I cannot merge self-modifications that lack this exact epistemic contract.* What remains before the claim "learned behavior measurably performs better": accumulate the live Y-vs-X delta from the cohort — the instrumentation for which now exists by construction.

---

## E.C2 — Proxy honesty, cohort report, and the frozen baseline

| Field | Entry |
|---|---|
| Change 1 — terminology discipline | Today's `contributed` verdict renamed **`evidence_used`**: rung three of the ladder (execution → semantic → **evidence_utilized** → goal_contribution → goal_outcome), written into `tool_outcome.rs` as design law. Citing is not causing; letting rung three masquerade as causal contribution teaches "my search gets cited, therefore my search causes goals" — precisely the proxy optimization this architecture forbids. Rung 4 arrives only via disagreement-cohort/shadow comparison; rung 5 only where ExpectedOutcomes exist. |
| Change 2 — cohort report | `ym why flips`: frequency by legacy→learned pair + by chosen-prior band with n≥10 backing called out. Outcome join pending trace linkage; the table exists so accumulation starts now. |
| Change 3 — baseline frozen | Commit `60a5cab`, annotated tag **`cognitive-closure-v1`**: Yantrik before broader autonomous arbitration, but after the first closed experiential-learning loop. 891 tests / 0 failures. Every future architecture experiment (AttentionBoard, adaptive capability policies, deeper self-build) now has something concrete to beat. |
| Next protected loops (spec'd, not built) | **(a) Changed-choice → better-outcome:** shadow-execute legacy picks for READ-ONLY tools only (learned→B real, legacy→A shadow, no external side effects; mutating tools never shadowed), then report Y vs X overall / disagreements-only / by confidence / by tool-pair. **(b) Self-build → measure:** autonomous promotion requires baseline-vs-candidate metric vectors over pinned scenarios — target metric improves by threshold AND protected dimensions (security/policy/memory/quality floor) non-regressing AND resources within bounds. **Protected invariants are constraints, not weights: no scalar fitness may compensate for violating one.** |
| Decision | **KEEP** — and stop here. No AttentionBoard, no policy tuning, until these two loops close on live evidence. |

*Phase-2 final state: 891/0 · tag `cognitive-closure-v1` · ledger entries E.0–E.C2: six KEEP, one REVISE-to-simpler, zero unproven claims carried forward.*

---

## E.C3 — Policy identity on disagreement records + the shadow-safe predicate (final pre-freeze addition)

| Field | Entry |
|---|---|
| Change 1 | Every `selection_flipped` event now carries full POLICY IDENTITY, so "Y improved on X" stays attributable when anything changes: `policy=capability-ranking-v1/1`, build commit (`YM_BUILD_COMMIT`, else unknown), formula version + verbatim formula, semantic scores for BOTH picks, reliability rate/n/bonus for both, and an FNV fingerprint of the catalog snapshot ranked against. Module-level constants (`RANKING_*`, `EVIDENCE_WEIGHT`, `SAMPLE_CAP`) are shared by ranker and recorder — one source of truth. |
| Change 2 — terminology correction | Shadow-eligibility is **shadow-safe / observationally safe**, a policy predicate — NOT "read-only". Candidate conditions: no external mutation · no user-visible effect · no irreversible effect · bounded cost · bounded resource use · same privacy/purpose authorization · no additional disclosure. A technically-read-only paid API query, rate-limited endpoint, inbox fetch with audit effects, or large crawl is NOT shadow-safe. Reserved experiment identity for future shadow runs: `experiment_id` · `baseline_policy_id` · `candidate_policy_id` · `request_trace_id` · `real_pick` · `shadow_pick`. Not built — no shadow runs exist yet. |
| Metric | mind-conversation 391/0 with enriched records; workspace unchanged elsewhere. Committed post-baseline (the tag remains the frozen comparison point; this commit is instrumentation for evidence collection, not behavior change). |
| Decision | **KEEP — and FREEZE.** No further architecture until the two protected loops close on live data: (a) learned-vs-legacy outcome delta Y > X over the disagreement cohort, (b) self-build baseline→candidate metric vectors with constraints-not-weights promotion. The next meaningful number is not 892 tests; it is **Y > X**. |
 
## E.W0 — PHASE 3A RED BASELINE (the contract's first deliverable)

| Field | Entry |
|---|---|
| Deliverable | `mind-evals/src/world_oracle.rs` ONLY — the dumb oracle (E3): explicit expected states per bi-temporal checkpoint, no twin algorithm. No world-model production code written, per the frozen instruction. |
| Harness | 16 adversarial events (duplicate ingestion email:501×2; corroborating calendar:88 kept distinct per E2; supersession email:923; LATE stale email arriving after supersession; carrier delay occurred-before/learned-late; ETA guess overridden by delivered-scan per I4; Alice promised→inbox-retraction vs "sent-yesterday" contradiction; Room4-vs-Zoom unresolved conflict; stale weather; flight expiry invalidating a dependent derivation) × 9 oracle expectations incl. the knowledge-time non-leakage pair and a purpose-denied read. |
| Baseline result | **0/9 representable — every expectation FAIL:UNREPRESENTABLE.** No typed-transition ingestion seam (events fed via transcript as the only door). Restart leg impossible on :memory:, no durable world snapshot exists. Bi-temporal cuts: NO API. Conflicted/stale/expired representation: NONE. Purpose-scoped world reads: absent (scope wall exists; world-cut does not). HARD CONSTRAINTS unmeasurable: knowledge-time leakage · purpose leakage at world layer · replay divergence. |
| Semantics today's architecture cannot represent | current-state cuts at (valid_at, known_at); Unknown/Conflicted/Stale/Expired as first-class values; supersession-with-reason over free-form names; duplicate-by-identity vs corroboration distinction; derived-lineage invalidation; purpose-scoped entity queries; replay-equivalent persistence of world state. |
| Decision | RED BY DESIGN, pinned by an intentional panic that retires only when the world model answers these checkpoints. Suite remains green ungated (env `YM_WORLD_3A=1` runs the oracle). Phase 3A implementation may now begin — earning each semantic against this baseline. |
 
## E.W1 — Temporal spine (mind-world) — 2/9 GREEN

| Field | Entry |
|---|---|
| Hypothesis | The smallest typed-event → typed-transition → append-only-log → deterministic-replay spine makes DUPLICATE_ID and CORROBORATION representable without any current-state semantics. |
| Baseline | W0: 0/9, all UNREPRESENTABLE. |
| Change | New crate `mind-world` (W1 scope only): WorldEvent{source_event_id, source_id, kind, occurred_at, observed_at, entity, attr, value} → WorldLog.ingest (identity dedup via source_event_id; corroboration = count of prior independent witnesses of same proposition; stable transition_id + recorded_seq) → WorldLog.replay with canonical order (occurred_at, observed_at, source_event_id) per I6. Append-only by construction (I7 shape). Oracle converted from monolithic RED to a 9-case scoreboard. |
| Expected | DUPLICATE_ID GREEN (email:501 twice = one transition); CORROBORATION GREEN (distinct sources survive as separate witnesses); other 7 stay RED/UNREPRESENTABLE. |
| Actual | mind-world 3/3 tests green; oracle **2/9** — DUPLICATE_ID ✅, CORROBORATION ✅ after its own check forced an E2 refinement: the LATE stale email (email:old-late) is a THIRD independent Tuesday witness, so corroboration had to be scored as distinct-SOURCES-preserved, not exact row lists. Exactly the kind of semantic sharpening the slice process exists to produce. Remaining 7 RED/UNREPRESENTABLE (no WorldQuery API yet). Ungated suite unaffected (env-gated oracle). |
| Regressions | None from this slice. NOTE: unrelated concurrent edits appeared in the working tree during this slice (trades.rs, watch.rs, a `grade_due_trades` call without definition) — not mine, left untouched, excluded from this commit; they currently break `cargo test -p mind-conversation` until their author completes them. |
| Decision | **KEEP — W1 complete.** Next: W2 bi-temporal cuts (the no-hindsight-leakage property), only after the tree is quiet again. |
 
## E.W2 — Bi-temporal cut: 4/9 GREEN

| Field | Entry |
|---|---|
| Hypothesis | state_at(entity, attr, WorldQuery{valid_at, known_at}) over the replayed log answers asserted/superseded state with ZERO hindsight leakage, and a late-arriving old fact cannot resurrect a superseded proposition. |
| Baseline | E.W1: 2/9; bi-temporal cuts had no API. |
| Change | `WorldLog::state_at` — knowledge filter first (observed_at <= known_at), then world-time selection (latest occurred_at among non-retracted); `WorldQuery` type exists now so no context-free consumer API can take root (W5 adds AccessContext); `StateAt` enum carries all five epistemic values from day one (Conflicted/Stale/Expired populated in W3). |
| Expected | Leakage pair passes both ways (early=Unknown, later=Known(delayed)); late Tuesday email does NOT resurrect superseded interview. |
| Actual | mind-world 5/5 (incl. the two new properties as permanent tests); oracle **4/9**: DUPLICATE_ID, CORROBORATION, **BITEMPORAL**, **SUPERSESSION** GREEN — scored through real queries, not assertion. |
| Regressions | None. Concurrent trading work in tree (trades.rs/watch.rs/grade_due_trades) landed separately and compiles; untouched here. |
| Decision | **KEEP — W2 complete.** Next: W3 epistemic state (Conflicted preserved per I4, named RESOLVE_BY_RULE for carrier-vs-ETA, Stale/Expired). |
