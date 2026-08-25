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
 
## E.W3 — Epistemic state: 7/9 GREEN

| Field | Entry |
|---|---|
| Hypothesis | state_at can distinguish Known/Unknown/Conflicted/Stale/Expired without becoming last-write-wins: conflicts persist per I4; only NAMED registered rules resolve them; staleness and expiry are judged against the query's cuts, never wall clock. |
| Baseline | E.W2: 4/9. Pre-W3 patch per review: AccessContext moved INTO WorldQuery now (A6 boundary from day one) — omission structurally impossible; no context-free world.current() can grow. |
| Change | Per-source newest-claim bucketing; latest Supersede retires ALL earlier-occurred claims of the proposition across sources (scorecard caught the same-source stale-email hole); >1 live distinct value = Conflicted unless a registered ResolutionRule picks a winner (carrier-delivered-scan-overrides-estimate/v1 as first rule); Stale when known_at − last_verified > freshness policy; Expire kind → Expired at later cuts only. |
| Expected | CONFLICT (Room4/Zoom stays Conflicted), STALE (weather at 75h age), EXPIRY (flight Expired after T, Known before T — inverse catches wall-clock implementations), SUPERSESSION preserved through the new retire-rule. |
| Actual | mind-world 8/8 tests; oracle **7/9**: +CONFLICT, +STALE, +EXPIRY, SUPERSESSION survived its own regression test (the retire-across-sources rule was FOUND because the scorecard flipped it red mid-slice). Remaining RED: INVALIDATION (W4 derivations), PURPOSE (W5 gate enforcement). |
| Regressions | None outstanding; concurrent trading work in tree left alone. |
| Decision | **KEEP — W3 complete.** Next: W4 derivation+lineage+invalidation (the defining 3A test), then W5 purpose enforcement, W6 replay equivalence, W7 oracle expansion to ~75 events + trajectories + metamorphic tests. |


## E.W4-W6-R — oracle flips land; scorecard reaches 9/9; panic retires

| Field | Entry |
|---|---|
| Hypothesis | Wiring INVALIDATION/PURPOSE scoreboard arms to real WorldQuery semantics (derivation-on-demand + purpose gate) yields 9/9 without touching mind-world internals. |
| Change | INVALIDATION: warranted_early cut day(23,10)->day(22,18); PURPOSE re-cut to day(23,10), operator expectation corrected Friday->Thursday. |
| Expected | First-run 9/9 GREEN. |
| Actual (attempt 1) | 7/9 — two genuine findings, no mind-world defects: (1) flight input at day(23,10) had aged 50h > 48h freshness => Stale => the rule CORRECTLY refused warrant off stale input. A derived claim is warranted only inside the INTERSECTION of its inputs' freshness windows — here [day22:15, day23:08]. (2) PURPOSE expected Known("Friday") but the stream supersedes to THURSDAY — expectation written from prose memory instead of the stream, exactly what dumb-oracle discipline exists to catch. |
| Actual (attempt 2) | 9/9 GREEN; assert_eq!(green, 9) retired itself as designed in E.W0. Workspace: 42 suites, 908 passed, 0 failed (Phase-2 freeze was 891). |
| Semantics learned | Freshness is part of WARRANT, not presentation: staleness of any consumed input silently voids a derivation at later cuts — which is also what kills zombies without a sweeper. Oracle expectations must be transcribed from the event stream, never from narrative memory. |
| Decision | KEEP. Commits 5ebef6f (machinery), 6bc259d (flips). |

## E.W7 — metamorphic invariances at scale (~78 transitions)

| Field | Entry |
|---|---|
| Hypothesis | The four metamorphic laws (duplicate-, order-, restart-invariance; termination semantics) hold over generated volume, not just hand-picked events. |
| Change | w7_metamorphic_tests in mind-world: 5 entities x 15 scrambled/late asserts + Supersede/Retract/Expire terminations (commit 04e331b). |
| Expected | All four green first run. |
| Actual | Two REAL findings: (1) default freshness fires on multi-day gaps — metamorphic tests pin freshness explicitly to isolate variables (terminations test runs with_freshness_ms(MAX); staleness stays proven in w3/oracle). (2) recorded_seq is ARRIVAL bookkeeping: a resumed log numbers post-restart ingests by arrival while one-shot replay renumbers canonically; restart equivalence is judged on the canonical (occurred_at, observed_at, source_event_id) projection + answer equality across cuts — the precise content of I6 "same event set yields one history". Both encoded as documented semantics in the test module. |
| Result | 15/15 mind-world green (11 prior + 4 W7). KEEP. |
| Boundary | ORACLE-file expansion to ~75 hand-written adversarial events with trajectory checkpoints remains OPEN — Phase 3A completion NOT yet claimed; machinery + laws in place for that expansion to land as pure fixture work. |


## E.W7-b — adversarial month lands: 65 hand-authored events, 11/11 GREEN; world-state-v1 frozen

| Field | Entry |
|---|---|
| Change | Oracle expanded 16->65 hand-authored interacting events across days 20-28: Alice-doc continuation (upload/withdraw/expiry/post-expiry confirmation), compound derivation chain visa+passport->clear->trip_ready/go, knowledge-time conflict evolution (venue Room4->Conflicted), late weak ETA email vs delivered scan, invoice/payroll corroboration, cold-chain out-of-order witnesses, credential rotation/revoke/re-issue, DNS stale-refresh, expense supersession chain, mid-stream RESTART at ops:deadline. Scorecard extended to 11 properties (+REPLAY_EQUALITY, +LINEAGE); per-leg FIXTURE diagnostics retained. Compound-chain recursion added to mind-world (depth-capped input resolution through derived entities) after fixture design exposed single-hop limitation - classified MISSING SEMANTIC before coding; unit test two_hop_derivation_loses_warrant_transitively green. |
| Classified findings | (1) ORACLE ERROR x2: six arcs authored attr "state" while queries said "status" (empty-relevant Unknowns); Alice expectations ignored live week-one claims (upload JOINS a conflict, never replaces it). Fixed fixtures only. (2) DRIVER ERROR: restarted log carried NO POLICY - replay restores history, not the lens (rules/freshness/derivations are deployment configuration); build_log closure now shared by primary and resumed instances. (3) MISSING SEMANTIC, FIXED IN MACHINERY: agreeing witnesses RE-VERIFY a proposition - collapse of same-value claims now keeps the FRESHEST observation as representative, so new corroboration can un-stale a fact; previously freshness was judged on the first-encountered row and a fresh witness could not refresh anything. Ranking untouched (I4). OPEN QUIRK recorded, NOT changed: a Retract buried under later asserts leaves that source's older claim alive in its bucket (per-source retraction semantics undecided) - flagged for a deliberate decision before Phase 3B. |
| Result | 11/11 GREEN; mind-world 16/16; workspace 42 suites / 917 passed / 0 failed (891 at Phase-2 freeze). All eight Phase-3A completion criteria met: trajectory suite, metamorphics, restart equivalence, zero knowledge-time leakage, zero purpose leakage, zero duplicate duplication, zero zombie derivations, full regression. |
| Decision | FREEZE checkpoint tag world-state-v1. Phase 3A complete: "Given what had happened and what I had learned by time T, reconstruct what was warranted, uncertain, stale, contradicted or expired, and explain why." NO further world-state intelligence. Phase 3B begins exactly like 3A: red executive oracle first (goals/commitments/predictions -> attention candidates -> IGNORE/MONITOR/ACT), AttentionBoard unbuilt until it earns green. |


## E.W7-c — Phase 3A FINAL VALIDATION under the adversarial-month directive; world-state-v1 re-pointed to validated commit

| Field | Entry |
|---|---|
| Baseline | HEAD 77dc66c; mind-world 16/16; mind-evals 19/19(+1 gated). Machinery frozen throughout. |
| Fixture count | 76 hand-authored events (was 65): +corroboration complication (A/B Thursday, C Friday, B retracts, D confirms), +asymmetric freshness windows (cruise window vs marine forecast), +differentiated privacy scopes (comp.band/medical.clearance restricted, public.notice open). |
| Checkpoints | 8 trajectory checkpoints t1..t8 spanning days 21-29 incl. post-restart t7/t8; expected Known/Unknown/Conflicted/Stale sets authored per checkpoint; 8/8 GREEN. |
| Failures discovered + classified | ORACLE_ERROR x3: (a) t7 package probe cut past freshness (fixed by realistic carrier re-confirmation event - witnesses re-verify their own claims); (b) t7 alice conflict-set over-specified: her own confirmation replaces her promise INSIDE the source bucket -> honest conflict is 3 values not 4; (c) earlier session attr mismatches (recorded in E.W7-b). NO implementation bugs found this slice. MISSING_SEMANTIC evidence ADDED: defense.sched checkpoint t8 encodes current global-retract-latest semantics (B's retraction silences A/C/D claims) - SECOND independent instance of per-source retraction granularity gap; recorded as amendment evidence for a deliberate pre-3B decision. Expectation primitive NOT added (#16); awkwardness noted only where global-retract forces it. |
| New capabilities proven fixture-side, zero machinery change | PRIVACY_INHERITANCE: derived comp.risk invisible to members purely because gated inputs resolve Unknown through evaluation - no value can cross the gate (purpose_leakage=0 for direct AND derived facts). WHY_LINEAGE: evaluator reconstructs E<-rule2<-C<-rule1<-{A,B} and tags passport VALUE SUPERSEDED / visa_status INPUT UNWARRANTED from lineage_of + live cuts. Advisory-intersection: marine stale while window fresh => advisory unwarranted (not displayed-as-stale). |
| Final metrics | 14/14 scored properties GREEN (added CHECKPOINTS, PRIVACY_INHERITANCE, WHY_LINEAGE); separate metrics table printed (conflict/supersession/retraction/expiry/stale/bitemporal/lineage/invalidation/duplicate/corroboration/restart/purpose all OK); knowledge_time_leakage=0 purpose_leakage=0 duplicate_semantic_duplication=0 restart_replay_divergence=0 zombie_derivations=0 historical_lineage_loss=0. Workspace: 42 suites / 919 passed / 0 failed. |
| Regressions | None. KEEP. Tag world-state-v1 re-pointed from 0c6a7f1 to this commit so the tag certifies the VALIDATED state. STOP per directive #21: no AttentionBoard, no executive cognition, no counterfactual simulation, no planning. Phase 3B opens under a new RED benchmark. |


## E.W8 — Phase 3A.1: RETRACT refined to evidence-targeted withdrawal; world-state-v1.1 frozen (immutable)

| Field | Entry |
|---|---|
| Trigger | Two independent fixture instances (alice upload-withdrawal quirk E.W7-b; defense.sched global-silencing E.W7-c) earned the semantic per methodology. Directive: refine RETRACT, no new primitive, red test first. |
| Red spec | Five unit tests written BEFORE code (retraction_targeting_tests): one-witness-withdrawal leaves others in conflict; pair-with-one-withdrawal stays Known via the other; solo self-retract goes Unknown; participation is bi-temporal around the retraction; a source may speak again after withdrawing. Confirmed RED under prior semantics (2 hard failures), then GREEN after the amendment. |
| Change (machinery, minimal) | state_at: latest-action-per-source now decides participation - a source whose LATEST action on a proposition is a retraction contributes nothing; empty live-witness set returns Unknown; SUPERSEDE keeps cross-source retirement of earlier-occurred claims; EXPIRE stays proposition-level (validity period ended). The three kinds remain distinct: RETRACT=withdraw my evidence, SUPERSEDE=my/newer state replaces prior state, EXPIRE=this stopped being valid at end of period. |
| Classified findings this slice | IMPLEMENTATION_BUG found by t8: Conflicted was built from raw per-source claims, so corroborating witnesses of the SAME value inflated conflict breadth ("Friday" reported twice). Fixed to distinct live values + focused regression test (conflict_breadth_tests). ORACLE_AMENDMENT x3 (sanctioned): alice withdrawn/revived and t7 expectations updated because cloud-drive's upload is now honestly WITHDRAWN (2-value conflict remains); iam k3 re-authored as authoritative rotation Supersede from iam itself (a third-party retract with no prior claim is inert under evidence-targeted semantics); t8 defense.sched now expects the honest Conflicted(Thursday,Friday) with A/C/D still speaking. |
| Result | Oracle 14/14 GREEN; checkpoints 8/8 GREEN; mind-world 22/22; workspace 42 suites / 927 passed / 0 failed. Hard constraints all zero unchanged. |
| Decision | KEEP. Tag world-state-v1 left untouched at 00379c5 (evidentiary checkpoint, never re-pointed again); world-state-v1.1 created for the validated retraction-refined state; future checkpoints append (v1.x), never move. Phase 3A now genuinely complete. STOP before Phase 3B: it must open as a RED executive oracle scoring ACT/MONITOR/IGNORE with positive credit for correct silence - missed interventions, unnecessary/premature interventions, interruption cost - not an AttentionBoard implementation. |


## E.EX0 — Phase 3B RED executive baseline: 37/37 decisions UNREPRESENTABLE against world-state-v1.1

| Field | Entry |
|---|---|
| Deliverable | crates/mind-evals/src/executive_oracle.rs - INDEPENDENT dumb oracle only. No AttentionBoard, no executive controller, no scheduler change, no LLM planning, no counterfactual simulation, no new primitive (Expectation explicitly NOT added; waiting-on-someone modeled as fixture facts + awkwardness note). Gated YM_EXEC_3B=1; ungated suite green. |
| Benchmark shape | 31 hand-authored situations -> 37 scored decisions across families A-ignore(5), B-monitor(4), C-act(2), F-resolved(3), G-deadline-curves(6: T-21d Ignore / T-14d,T-10d Monitor / T-2d act-internal / T-4h act+interrupt / T+1h late-recovery), H-commitments(3: promise beats opportunity), I-competing-sets(2 sets x 4 candidates incl. resource-blocked demotion ACT->MONITOR), J-resource(2), K-receptivity(2), L-waiting-on-Alice(3). Per-situation hand-authored outcome tables [ignore,monitor,act] with want=outcome-minimum enforced by pre-run ORACLE_ERROR self-check (caught 1 real fixture inconsistency before scoring). Costs modeled as interrupt/execution/risk, not collapsed (#5); ACT carries requires_user_interrupt flag (#6). |
| Baseline result | representable 0/37 | recall-only fragments 0 | UNREPRESENTABLE 37. ACT precision 0/0 recall 0/10; MONITOR 0/0 recall 0/13; IGNORE 0/0 recall 0/14; correct_silence=0; unnecessary_action=0 (nothing can act at all); missed_intervention=10/10. Confusion matrix: every actual row lands entirely in the UNREPRESENTABLE column. Intentional assert retires only when coverage is earned (E.W0 discipline). |
| Capability verification (verified, not assumed) | Executive choice abstraction ABSENT; central arbitration ABSENT; silence credit ABSENT; monitor semantics ABSENT; cross-organ prioritization ABSENT; mind-proactive Detect->Generate->Score->Deliver pipeline + commitment ledger = doc-comment STUB ONLY (one line). Only existing door probed: belief-recall fragments (none matched even that). Existing proactive/reflex organs in mind-conversation are transcript-scoped Phase-2 learning chains, not posture arbiters. |
| Failure classification | EXECUTIVE_SEMANTIC_MISSING: posture vocabulary+arbitration seam (all 37); outcome/cost comparison surface (37); silence-as-decision credit (14 IGNORE cases); deadline-curve escalation state (6); commitment ledger with convergence ranking (mind-proactive stub, 3); resource-conditioned demotion (3); receptivity context input (2); set-of-decisions scoring (8). WORLD_STATE_DEFECT: none - Phase 3A inputs were sufficient everywhere the fixtures referenced them (conflicted/stale/resolved facts expressed cleanly through v1.1 semantics). ORACLE_ERROR: 1 pre-run self-check catch (set-anchor outcome table), fixed in-fixture. |
| Decision | FREEZE as Phase 3B baseline. Smallest seam the failure distribution justifies (proposal only, NOT built): a single typed arbitration point consuming (a) WorldQuery reads from mind-world and (b) a commitment/deadline register (the missing half of mind-proactive), emitting Posture{IGNORE,MONITOR,ACT}+requires_user_interrupt with outcome-table scoring already defined here. Nothing more until this oracle starts turning green decision-family by decision-family. STOP per directive #18/#21. |


## E.EX1 - posture vocabulary + one typed arbitration seam; first 10/37 decisions earned GREEN

| Field | Entry |
|---|---|
| Isolation rule (binding) | The production executive consumes OBSERVABLE candidate variables ONLY. Oracle outcome tables are evaluation ground truth and never cross the seam. Fixture->candidate mapping uses facts (deadline, window, internal_capability, resolved) - never costs-of-postures. |
| Delivered | mind-proactive (was a one-line stub) now holds: Posture{IGNORE,MONITOR,ACT}; ExecutiveCandidate (observable waist input); ExecutiveDecision{posture, requires_user_interrupt, reason_code, monitor:Option<MonitorPlan>, evidence_refs}; MonitorPlan with WakeCondition{DeadlineWithin,StateChangeOf,SourceFresh} - every MONITOR answers "what would cause me to reconsider". arbitrate() is total/deterministic/side-effect-free. Formal semantics encoded: IGNORE=no justified future cognition obligation; MONITOR=no action now + future obligation exists; ACT=justified intervention inside open window. 5 seam unit tests green. |
| Scope earned | EX1_SCOPE = 10 of 37: 4x resolved->IGNORE(already_resolved), 2x too-early->MONITOR(wake condition asserted), 4x window-open->ACT incl one ACT-internal and one ACT+user-interrupt. Oracle scores scope separately from the still-red remainder; regression assert added (scope stays green while coverage expands). Overall baseline honestly unchanged: 27/37 still UNREPRESENTABLE via no door; intentional red retires only at full coverage (EX2..EX7 earn them). |
| Classified findings | ORACLE_ERROR x2 fixed in fixtures: doc_due_2h_dependency and passport_window_open left requires_user_interrupt=false contradicting their own facts ("only Pranab can renew"); the seam correctly demanded interrupt - oracle wrong, production right. No new EXECUTIVE_SEMANTIC_MISSING beyond E.EX0 catalogue; no WORLD_STATE_DEFECT. Boundary type conversion (oracle Posture vs proactive Posture) kept explicit at the scoring edge to preserve the isolation wall. |
| Result | mind-proactive 5/5; gated oracle: EX1 10/10 GREEN, overall red by design; workspace 42 suites / 935 passed / 0 failed ungated (a gate-variable leak in my own chained command caused one false failure - re-ran clean). |
| Decision | KEEP (commit b1300b4). Roadmap unchanged: EX2 temporal escalation -> EX3 commitments as a VIEW over authoritative organs (no new registry) -> EX4 resource/receptivity -> EX5 set arbitration -> EX6 outcome measurement -> EX7 adversarial executive trajectory -> executive-control-v1; then shadow mode beside live behavior before any real action wiring. STOP: nothing beyond EX1 built. |


## E.EX3 - obligations outrank opportunities; waits honor grace; 6 more decisions earned (22/37)

| Field | Entry |
|---|---|
| Red spec first | ex3_commitment_tests failed structurally (no CommitmentView, no obligation fields) before implementation. |
| Change | CommitmentView = NORMALIZED VIEW referencing authoritative organs (ref_id + source_organ) - the executive owns NO obligation registry. ExecutiveCandidate += commitment / converging_obligation_due_ms (observable environment fact) / wait_grace_until_ms. Deterministic rules: fulfilled -> IGNORE(commitment_fulfilled); converging + actionable + internal -> ACT(obligation_deadline_converging); distant -> MONITOR(commitment_tracked, wake scheduled); optional opportunity while an obligation converges elsewhere -> IGNORE(yields_to_commitment); waiting_on_someone with grace alive -> MONITOR(waiting_grace_open) EVEN IF a window technically exists; grace elapsed -> ACT(dependency_wait_elapsed). |
| Seam correction during RED->GREEN | Initial ladder let intervention_window_open override an alive grace period; the red test caught it - grace now governs its own phase. Production forced the semantic precision, not the benchmark bending. |
| Result | mind-proactive 13/13; EX3 SCOPE 6/6 GREEN; EX1 10/10 and EX2 6/6 held; overall red honestly retained at 15/37; workspace 42 suites / 943 passed / 0 failed. Fixture amendments: two L-wait ACT cases gained requires_user_interrupt=true per their own facts (same class as E.EX1 corrections). |
| Decision | KEEP (commit 9565f32). Coverage 22/37. Next: EX4 resource/receptivity, then EX5 set arbitration, EX6 outcome measurement, EX7 adversarial trajectory -> executive-control-v1, then shadow mode. STOP: nothing beyond EX3 built. |
