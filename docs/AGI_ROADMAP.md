# AGI Roadmap — a research north star, evidence-gated

*Opened 2026-08-26, at the request of the `yantrikdb-core-codex` workspace (three-part spec,
2026-08-26 01:35Z). This is a RESEARCH document. Nothing in it is a product claim, and no line of
it may be quoted as one. Its first duty is to say what this mind can already be shown to do, which
is much less than what it has been built to do.*

**Status: READ-ONLY DESIGN.** Implementation of anything below waits behind the product gates
(P.2e/P.3b at `bca555d`, P.4 at `59b3b5c`, both awaiting review). This document may be written,
argued with, and measured against; it may not be built from yet.

---

## 0. What would count

Adopted verbatim from the request, because it is the right definition and a stricter one than this
project would have written for itself:

> Yantrik Mind demonstrates increasing generality only when it improves on HELD-OUT tasks across
> unrelated domains with limited examples, learns unfamiliar tools from their contracts, maintains
> and replans long goals, transfers procedures, stays calibrated, and preserves safety under
> novelty. **Benchmark aggregation alone is not AGI.** Publish demonstrated capabilities,
> confidence intervals, costs, and explicit failures.

Two additions this house's own failures have earned, which sharpen it:

- **A capability is not integrated until it is reachable.** (Doctrine 1, E.D1 — earned by E.R2, a
  fully instrumented learning chain whose emit sites lived in a loop production leaves off.)
- **A rate measured on a self-selected sample is not a rate.** (Doctrine 2, E.D1 — earned by E.P2,
  where a well-calibrated predictor was calibrated against a sample that deleted its own failures.)

## 1. The rules this roadmap inherits

| Rule | Source | What it forbids |
|---|---|---|
| Pre-registration | every `E.*` ledger entry | Writing the hypothesis after seeing the number. An entry without a baseline gets no decision. |
| Doctrine 1 — reachability | E.D1 | Calling a thing done because tests pass. Tests prove semantics; production traces prove reachability; neither substitutes. |
| Doctrine 2 — selective observation | E.D1 | Adapting a policy from outcomes without first auditing `P(observed \| success)` vs `P(observed \| failure)`. |
| Doctrine 3 — independent witness | E.D1 | Confirming a claim with a witness that fails the same way the claim does. Strongest cheap form: a different mechanism reading persisted state after the fact. |
| The maturity ladder | E.D1 | Reporting a rung the evidence does not reach. DEFINED → TESTED → BENCHMARKED → REACHABLE → SHADOWED → ACTIVE → OUTCOME-VALIDATED. |
| Kill criteria before code | E.PK3, E.PK4 | A wall invented after the result it would have failed. |

The ladder is the reason this document can be written honestly at all: it has a rung for "built,
tested, and never once observed doing its job", and that rung is where most cognitive machinery in
this repository actually sits.

## 2. Baseline — where the mind stands today, by evidence

*One row per capability the mind is often said to have. `Rung` is the highest rung its evidence
reaches, not the highest its code could support. A capability with no ledger entry is `ABSENT` from
this table's point of view however much code exists.*

| Capability | Rung | The witness (not the intention) |
|---|---|---|
| Persistent episodic/semantic memory | ACTIVE | Single-owner actor over yantrikdb; E.1 measured its scheduling (interactive read 65.5 ms → 7.3 ms under load). Continuity across sessions is used every day but has **no held-out measurement** — see Phase B. |
| Hash-chained decision record | ACTIVE | E.4: 8/8 chain/tamper/reopen tests; `ym why <trace>` reconstructs a run from persisted evidence. Doctrine 3's cheap form is built on it. |
| Closed learning chain (tool calls) | ACTIVE, reachable, **window-named** | E.L1 (prediction → outcome → Brier), E.T1 (trace tree), **E.R2** — which found it recording on a loop production does not run, and fixed it. E.AGI-A2 stamped goal identity live; E.AGI-A5 made the completeness gate name its window (since this binary started, or an auditor's `since=`), because a log that spans binaries let an older one hide or flatter the running one. Staging all-time 65/80 (81%); prod 0/200 on its pre-stamping binary. |
| Outcome attribution | ACTIVE | Six-way `Outcome` (Ok / Empty / Denied / Unavailable / **Malformed** / error) with one write site; E.PK2b→E.PK2e pinned the planner-vs-tool boundary and proved on the live loop that a refused call feeds no bandit and leaks no value. |
| Abstention | SHADOWED | E.PK3's coverage router abstains by floor and by margin; it decides nothing yet. Elsewhere abstention exists as refusal, not as a chosen alternative to answering (Phase D). |
| Self-observation | ACTIVE for matched questions; **unsolved for paraphrases** | Instrument panel, `ym why`, two-witness pack stats (E.PK2). E.MQ1–E.MQ3 showed a generative answer path cannot be trusted with the mind's own capabilities; E.MQ4 made the answer an architectural property — a typed claim registry rendered verbatim, `ym claims`, provenance-stamped. Routing free-form questions to a claim was then killed four times on sealed sets (lexicon 2/10, E.MQ5 LLM 7/10 + 3 misroutes, E.MQ6 singleton shortlist 5/20 + 2 wrong): truthful when the question matches the registry's words, silent otherwise. |
| Expertise routing | SHADOWED | E.PK3: 35/38 agreement, 12/12 abstention on a pre-registered labelled set; live it has ranked correctly and incorrectly, both recorded. It has never chosen anything. |
| Expertise attachment (leases) | ACTIVE (operator-driven) | E.PK4: grant → mount → a live turn that used the leased row (0.635, both witnesses agreeing) → expiry swept in ~20 s. Efficacy explicitly not claimed. |
| Bounded planning | BENCHMARKED, shadowed live (n = 5) | Executive EX1–EX4: 26/37 pre-registered decisions representable (E.EX4-R2). On prod the shadow has seen 5 opportunities (5 sent by the legacy gate, 5/5 agreement, 2 engaged / 3 ignored) — shadow-consistent evidence, not proof, and the disagreement that would justify switching is the one the design cannot observe (E.D2/E.D4). Decides nothing. |
| Temporal/world model | BENCHMARKED, **reachable in shadow** | world-state-v1.1 frozen: 9/9 oracles, ~78 metamorphic transitions, a 65-event adversarial month (E.W7-b, E.W7-c, E.W8). Now fed live (E.G1/E.G1b: PRIMARY turns ingest presence behind the purpose gate) and consulted in shadow at every knock evaluation and, on a headless box, every 10 min (E.G1c — the first cut lived inside a Telegram-only gate and never fired; the cadence was witnessed to the millisecond). Each knock evaluation now ends in a joinable disposition event (E.G2a). It has still decided nothing, and the paired agreement table (E.G2) waits for a Telegram box. |
| Safety walls (harm gate, egress, purpose, scope) | ACTIVE | Deterministic, structured-field-only; E.PK4 proved a hostile pack's constitution moves none of them (72 purpose combinations, 8 egress classes, 3 intents, byte-identical). |
| Procedure learning | DEFINED/TESTED | Banked approaches and skills exist and are recallable; **no evidence any banked procedure was ever reused to a better outcome.** |
| Cross-domain transfer | ABSENT | Never tested. No held-out domain split exists. |
| Tool learning from contracts | ABSENT as a measured capability | The mind is *given* schemas and now held to them (E.PK2e); it has never been scored on an unseen tool. |
| Long-horizon agency | **ACTIVE, first instance** | E.F1–E.F1e: a durable goal survived restarts, woke on schedule, executed a bounded read-only segment, and COMPLETED with the answer actually delivered to the home channel; every lifecycle hop is a hash-chained receipt, and the chain was verified cryptographically by the peer (E.F1e). Repeated live with the label residual closed ("Total number of pending tasks: 0."). One goal, one segment, no replan yet — first instance, not generality. |
| Causal reasoning | ABSENT | Prediction exists for tool success only (a Beta posterior over one tool's track record); no action-effect model. |
| Calibrated uncertainty | PARTIAL, ACTIVE | Brier scored on tool-success predictions (E.T1). Never scored on answers, and E.P2 is the standing warning about what a good-looking calibration number can hide. |
| Multi-agent cooperation | ACTIVE, partially measured | E.AGI-I0 found 16/152 experiment rows (~10.5%) recording a second-agent defect catch after the author's own gates were green. This is a one-directional lower bound with coordination costs recorded, not a controlled solo comparison; reverse-direction attribution and quality/cost lift remain unmeasured. |
| Safe self-improvement | DEFINED | A self-build lane exists with an epistemic gate (E.C1) and two-tier fitness grading; no promotion has ever been blinded, and none has been scored against a hidden holdout. |

**The honest summary (revised 2026-09-02).** What is genuinely ACTIVE and witnessed is
*instrumentation*: memory, recording, attribution, walls, attachment — and now one long-horizon
goal carried to a delivered completion with receipts. What is BENCHMARKED and *reachable in shadow*
is *judgement*: the world model is fed live and consulted at every knock evaluation, the executive
has shadowed five real opportunities; neither has decided anything. What is ABSENT is *generality*:
transfer, tool learning, causality — and routing a free-form question about the mind itself, which
four sealed sets killed. The gap between "this mind has a world model" and "this mind has ever used
a world model to decide anything" is now a measured gap with a join key, not a named one.

## 3. The closed learning chain: what is asked vs what is recorded

The requested minimum event contract, against `mind_observability::DecisionEvent` as it exists at
`59b3b5c`:

| Required field | Today | Gap |
|---|---|---|
| trace_id | `trace_id` + `parent_event_id` (spans) | Compilation is the identified bounded-execution root within one caller-minted turn trace; completion/refusal, tool predictions, and contribution grades parent to it, while tool observations parent to their predictions. Stored forecasts mint collision-safe time/process/sequence `prediction_made` roots and binary grades parent to them. Packet trace/object ids use the same collision-resistant shape so same-millisecond proposals cannot collapse into one lifecycle. |
| goal_id | `goal_id` + `goal` (free text) | Compiled bounded runs propagate `GoalSpec.id` through tool spans, contribution grades, refusals, and completion; action-packet lifecycles propagate their stable future-node id. `ym why goals` measures coverage. Legacy free-form turns remain unstamped. **Phase F needs complete coverage.** |
| actor / lane | `actor`, `lane` | Compilation, bounded-run completion/refusal, pack routing/evidence lifecycle events, action-packet lifecycles, forecast grades, grounding assembly, and both tool loops stamp their actor plus `primary|member`; `ym why lanes` measures stamped vs missing events so remaining families stay visible. |
| context fingerprint | `context_fingerprint` | Grounding assembly, compilation, bounded-run completion/refusal, pack routing/evidence lifecycle, tool prediction/observation, and goal-contribution grades stamp one domain-separated opaque hash of the originating turn context; `ym why contexts` measures coverage across those families without printing identifiers. Other decision families remain unstamped. |
| model / tool / policy versions | `policy`, `tool_version`, `model_route` | Policy identity is real (E.C3); both tool loops stamp the dispatcher crate/version and `ym why versions` measures coverage. Bounded runs, both tool loops, and model-judged forecast grades stamp their configured routes, with `ym why models` measuring coverage; ledger-receipt forecast grades are correctly excluded as non-model events. Explicit `YM_ROLE_*` pools retain `provider:model` instead of reporting `scripted`. Actual serving-link/version and per-capability implementation versions remain absent. |
| predicted outcome + probability | `predicted`, `confidence` | Tool calls carry both fields. Action-packet creation carries a bounded confidence value in both its mutable store record and immutable lifecycle root; the packet-chain gate rejects missing confidence, but packet outcomes do not yet define a calibrated predicted proposition. |
| redacted action signature | `object_id` | Redaction is enforced at the boundary (E.PK2e: a refused call carries a constant id, never the arguments); packet creation and terminal outcomes share their generated opaque packet id. |
| evidence_ids | `evidence_ids` | Present; used by pack leases and belief explanations. |
| actual outcome | `outcome`, `verdict` | Tool, packet, and forecast lifecycles persist outcomes; an `unclear` forecast closes its immutable trace and judgment-ledger mirror with evaluator/execution provenance but remains explicitly excluded from binary calibration and pending counts. The judgment report separates still-pending, overdue-unresolved, and inconclusive rows; overdue accountability persists across the bounded ledger even after the row ages out of the 90-day scoring window. |
| evaluator identity | `evaluator_id` | Tool outcomes, tool-goal evidence use, packet decisions and expirations, pack lexical-use, pack next-message outcomes, and forecast grades stamp versioned evaluator identities; forecast grades distinguish ledger receipts from grounded model judgments. `ym why evaluators` measures stamped vs missing grades. **Phase C cannot promote a lesson without complete coverage.** |
| semantic success | `semantic_success` (`Option<bool>`) alongside the six-way `verdict` | Present on tool observations, goal-contribution grades, packet decisions and expirations, pack lexical-use grades, pack next-message outcomes, and hit/miss forecast grades; `ym why semantics` measures coverage by outcome kind and exposes missing grades, including legacy hit/miss forecast outcomes, while explicitly excluding non-binary `unclear` closures. |
| cost / latency | `latency_ms` on grounding assembly, executed tool observations, compilation, completed bounded runs, and model-judged forecast grades; `model_calls` on compilation, completed bounded runs, and forecast grades | Both execution loops stamp tool-call and shared grounding-assembly wall time; `ym why latency` exposes tool coverage and p50/p95/max, while `ym why resources` separates grounding, compilation, bounded-run, and forecast-grading logical model requests (including zero-call ledger receipts) and forecast-judge latency. Grounding model-call attribution, tokens, monetary cost, provider failover attempts, and legacy full-turn latency remain absent; no metric is cost-normalised. |
| lesson candidate | `lesson` | Present, but written at emit time, not proposed and tested. |
| before/after confidence | `prediction_error`, `brier` | Persisted for executed tool priors and hit/miss forecast grades. Forecast grades score the calibrated probability actually stored, spoken, and pre-registered; raw model confidence remains separate training input. `ym why calibration` reports tool and forecast confidence bands separately so one prediction family cannot mask another; broader decision families remain uncalibrated. |

Phase A's load-bearing gaps are complete stable-goal identity, context fingerprint, complete evaluator
identity, cost and broader latency, and measured coverage of the existing `semantic_success` and `lane` fields.
Complete lane coverage and model/tool version stamps remain required schema work. None is hard; all matter for the claims that
follow.

## 4. Memory architecture: six stores, and which exist

| Store | Exists? | Note |
|---|---|---|
| Working memory (bounded task state) | Yes | Turn-scoped scratch, the bounded loop's step log. |
| Episodic (immutable evidence) | Yes | Transcript + decision log (hash-chained, append-only). |
| Semantic beliefs (polarity, confidence, temporal validity, evidence, contradiction state) | Yes | Belief revision with provenance; conflicts surfaced, contradictions tracked, tensions aged (E.P2's swamp fix). |
| Procedures (applicability, preconditions, failure modes, version, outcome history) | Partial | Skills carry measured ledgers; banked approaches carry none of the four qualifiers. |
| Causal / world model | Partial | Temporal-epistemic world state (BENCHMARKED); no action→effect representation. |
| Self-model (live tools/permissions/costs + measured competence) | Partial, measured inaccurate | Live capability report and code-owned boundary facts exist, and measured competence exists per tool. E.MQ1–E.MQ3 found the generative answer path can override trusted capability facts, including on a clean subject; deterministic answering or a separate trusted self lane remains unbuilt. |

House invariants already enforced and worth keeping as roadmap constraints: no derived record
without provenance; corrections preserve history; a re-sealed pack never inherits its predecessor's
evidence (content-digest keying, E.PK1); consolidation is reversible and source-backed.
Action-packet trigger authority (`inferred|observed|told`) is written atomically to both the packet
store and its creation event, so knock eligibility cannot disagree with the audit trail.
Owner decisions are terminal and idempotent: retrying or reversing an already-confirmed packet does
not rewrite its status, double-count acceptance, or append a second terminal event.

## 5. Phases

*Each phase states its gate. A phase is not "done" when built; it is done when its gate is measured
on a held-out set and the number is published with its failures.*

**A — Truthful instrumentation.** Close the five schema gaps in §3; unify both execution loops at
the observation contract (they already share `tool_outcome`; they do not share event emission).
*Gate:* ≥99% of sampled consequential calls carry complete trace/prediction/outcome/provenance;
zero secret leakage (already regression-tested at the argument boundary); replay reproduces
classifications. `ym why chains` now measures the latest 200 tool calls against this completeness
contract, counts both orphan observations and predictions that never closed, and names aggregate
missing fields without printing identifiers. Duplicate observations for one prediction and globally
duplicated prediction or observation event IDs fail closed instead of inflating completeness;
blank provenance values and zero timestamps do not count as present, and observations require one
of the six defined verdicts, an actual-outcome field, a nonblank lesson, and an
`ok=true` / `empty=false` semantic grade. Graded `ok`/`empty`/`failed` outcomes must carry
the exact prediction error and Brier score implied by the recorded prior, while unavailable,
denied, and malformed outcomes must carry neither. An observation may share its prediction's
millisecond but cannot predate it, and its evaluator must be the versioned `tool-outcome-v1`
classifier rather than an arbitrary nonblank label. On compiled traces, the prediction must
also parent to the single identified compilation root; that root id must resolve globally to one
nonblank, same-trace event whose nonzero timestamp does not follow the prediction. A flat
same-trace label, cross-trace duplicate id, or postdated root no longer passes as causal provenance.
When a trace declares no compiled root, its prediction must be parentless rather than carrying a
dangling or cross-trace parent claim.
Rooted preflight refusals enforce the same root identity and temporal-integrity contract even though
they correctly carry no prediction; a parentless refusal qualifies for that lane only when its trace
declares no compiled root.
E.AGI-A1 captured the first live reading at 0/200 on the pre-wiring binary;
E.AGI-A2 then measured 1/16 (6.2%) on staging, with the single fresh post-wiring call complete and
the other 15 rows preserved as historical stratigraphy. The gate reads only
a fully verified hash chain and reports unavailable on the first corrupt or forged line.

`ym why packet-chains` applies the same ≥99% evidence gate to the latest 200 action-packet closure
candidates, including overdue proposals whose lazy expiry event has not run and duplicate-root
overdue traces that previously could evade the denominator: exactly one creation and exact
root-to-terminal expiry-horizon linkage,
exactly one decision or expiry, causal parent/object/stable-goal,
actor, and lane links, one allowed trigger-authority stamp, verdict-consistent semantic grade, and
the expected evaluator version. Still-pending proposals stay outside the denominator.

`ym why forecast-chains` gates the latest 200 forecast closure candidates, including overdue traces
without a terminal event even when duplicate roots exist, on a single immutable prediction root,
exact root-to-terminal resolution-deadline linkage, causal
parent/object/actor/lane linkage, the exact issued probability, consistent
semantic/error/Brier grades for hit/miss outcomes, explicit absence of those binary calibration
claims for `unclear`, an actual outcome, and evaluator-specific execution provenance (one timed
model request for grounded judgment or zero requests for a ledger receipt).

All aggregate `ym why` analytics now share that verified reader; raw trace reconstruction remains
permissive for forensics, but calibration or coverage claims cannot be computed from a broken log.

**B — Memory curation and continuity.** The E.MQ0 track. Baseline consolidation backlog age and
per-namespace starvation; namespace-balanced digests; current-chain heads; contradiction and
freshness handling. *Gate:* material lift on held-out memory tasks, no namespace-isolation
regression, no irreversible bulk rewrite. *Substrate note:* the digest numbers that motivated this
track (~1,800 episodic awaiting consolidation, 40 pending triggers, no narrative head) come from
the **Codex persona store via the yantrikdb MCP**, not from a read of Mind's own database; the
baseline must name its substrate or say it cannot.

**C — Outcome learning.** Promote lessons and procedures only from repeated evidence or explicit
teaching; run candidates in shadow against baseline behaviour. *Gate:* credible held-out lift,
Brier/attribution/negative-transfer reported, rollback proven. *Blocked on A* (evaluator identity).

**D — Active learning.** Choose among answering, asking, searching, experimenting, delegating,
abstaining by pre-registered utility = expected information gain − cost − risk. *Gate:* fewer
unnecessary questions and searches while accuracy and calibration improve; high-stakes uncertainty
raises abstention, not confidence. *Note:* this is where the coverage router's abstention machinery
generalises beyond packs.

**E — Tool and domain transfer.** Unseen tools given only as schemas/docs/examples; a procedure
trained in one domain tested in ≥3 held-out ones; adversarially similar tools and changed schemas.
*Gate:* transfer lift over zero-shot, bounded adaptation examples, no invented capability claims,
graceful `-32602` recovery (the recovery path exists and is tested; the transfer is not).

**F — Long-horizon agency.** Goals across interruptions and sessions: decompose, checkpoint,
monitor assumptions, replan, resume, terminate. 15-minute, 2-hour, multi-day simulated schedules
before any real canary. *Gate:* every action traceable, interruptible, budgeted, reversible;
irreversible operations always human-authorised. *Blocked on A* (goal identity).

**G — Causal/world-model reasoning.** Represent observation vs inference vs hypothesis vs
counterfactual; predict action effects before execution and compare. *Gate:* beats correlation
baselines on held-out interventions; failed predictions revise confidence without erasing evidence.
*Standing asset:* world-state-v1.1 is frozen and adversarially tested — this phase is largely about
making it reachable.

**H — Safe self-improvement.** Proposals to prompts, retrieval policy, routing, procedures and
bounded code, in isolated branches only; frozen replay, hidden holdouts, adversarial safety suites,
independent judges. *Gate:* blinded improvement with confidence bounds, zero critical safety
regressions, deterministic rollback, human approval for code/policy promotion.

**I — Multi-agent cooperation.** Delegation, specialisation, disagreement resolution, provenance
handoff, duplicated-work avoidance, adversarial peer input; peer messages are untrusted evidence.
*Gate:* quality/cost lift over solo execution, no authority laundering, complete responsibility
attribution. *Note:* E.AGI-I0 measured the arrangement's one-directional post-green peer-catch rate
(16/152 rows, ~10.5%) and recorded coordination costs, but did not supply the missing solo baseline,
reverse-direction attribution, or a controlled quality/cost comparison.

## 6. Evaluation design

Frozen public regression set + rotating private holdouts. ≥8 unrelated domains, with
domain-held-out and tool-held-out splits. Anti-memorization variants: renamed entities, changed
numbers, reordered evidence, novel schemas, counterfactual twins. Four arms compared — no-memory,
retrieval-only, current Mind, candidate Mind. Multiple seeds; confidence intervals; paired
analysis. Human baseline where feasible and a strong-model baseline always; scores reported
cost- and latency-normalised. Independent proposer/executor/judge roles, rotating model families.
Abstentions, interventions, harmful attempts and silent failures recorded — not only completed
answers. Old benchmark categories re-run every time, so new agency never regresses retrieval,
date-handling or sorting.

## 7. Core metrics

Generality: held-out domain score · tool-learning success · transfer ratio.
Learning: examples-to-competence · retained lift after time · negative-transfer rate.
Memory: useful-context precision · answer-item recall · contradiction and freshness errors ·
evidence density.
Agency: goal completion · correct replans · autonomy horizon · intervention rate · budget adherence.
Metacognition: calibration/Brier · abstention utility · self-model accuracy · capability-claim
precision.
Safety: unauthorized actions · secret leakage · prompt/pack influence on policy walls · rollback
success.
Efficiency: tokens · wall time · model/tool calls · storage growth · consolidation throughput.

Every rate is published with the denominator an adversary would accept, and with its observation
rate beside it (Doctrine 2).

## 8. Non-negotiable walls

The mind may not autonomously weaken permissions, consent, audit logging, evaluator independence,
rollback, spend limits, namespace isolation, or provenance requirements. No recursive deployment or
replication. No live self-edit on the strength of self-authored evaluation alone. No optimisation
for engagement, dependency, or concealment. User values and explicit authority outrank inferred
goals. High-impact and irreversible actions stay human-gated.

Two of these are already enforced in code rather than policy, and that is the standard the rest
should reach: the walls do not read prompts (E.PK4's hostile-pack invariant), and real money is
walled off by a `const`, not a config.

## 9. Promotion rule

A change advances only if: pre-registration predates results; implementation and evaluation sets
are separated; held-out lift clears its threshold; critical safety regressions are zero;
cost/latency stay within budget; provenance and replay are complete; rollback is tested. Otherwise
hold, revise, or kill. **Failed experiments are first-class roadmap evidence** — this ledger's most
useful entries (E.R2, E.P2, E.D2) are all failures, and E.D2 is a plan that killed itself before
running.

## 10. The first 30 days (read-only / shadow)

| Week | Work | Deliverable |
|---|---|---|
| 1 | This roadmap; unified event schema (§3's five gaps); threat model; metric definitions; frozen baseline manifest | `AGI_ROADMAP.md`, `AGI_EVAL_MANIFEST.json`, ledger `E.AGI0` |
| 2 | Memory/continuity baseline; namespace and backlog audit; self-model accuracy audit | Baseline report, ledger `E.MQ0` |
| 3 | Offline lesson/procedure induction evaluator over historical traces; **no promotion** | Evaluator + its false-positive rate on known-bad lessons |
| 4 | Shadow active-learning decisions; unseen-tool/domain transfer suite | First capability report, red-team report, prioritised implementation RFC |

Product release work and AGI research results stay separately labelled, always.

## 11. What this document does not claim

That any of this is close. The baseline table says the mind's *instrumentation* is real and its
*generality* is absent, and no amount of roadmap changes that. The value of writing it now is that
the measurements it defines are mostly runnable today against a system that already records enough
to be scored — which is the one advantage a small, heavily instrumented mind has over a large one.

---

*Companion documents: `PHASE2_EXPERIMENT_LEDGER.md` (every claim above traces to an `E.*` entry),
`ARCH6_EXPERTISE_LEASE_AND_PACK_LIFECYCLE.md` (the pack line this roadmap's Phase E will build on),
`AGI_EVAL_MANIFEST.json` (the frozen baseline).*
