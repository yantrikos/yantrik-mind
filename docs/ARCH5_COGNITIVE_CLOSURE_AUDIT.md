# ARCH-5 — Cognitive Closure Audit

*2026-08-24. Evidence-backed map of what exists in code, what the closed loop is missing, and the
road to closing it. Written after full-repo archaeology: every claim below carries a code
reference; where a doc claims something the code does not do, the doc loses.*

Method: all 19 crates inspected; six end-to-end flows traced through source (interactive turn,
task/commitment, recipe/planner, sub-agent, self-build, always-on tick); full workspace test run;
`git log` archaeology on regressions. Test state at audit time: **867 tests passing, 0 failing**
(one red cognitive-loop eval found on HEAD was diagnosed and fixed — §G.0).

---

## A. Current architecture (as built, not as documented)

### The real shape

BUILD.md's DAG (`governance ← memory ← {perception, inference} ← … ← core`) is **half-real**.
Six of its crates are one-line stubs with zero dependents:

| Stub crate | BUILD.md claims | Reality |
|---|---|---|
| `mind-perception` | event bus + bridge | absent; events arrive via the Telegram poll loop |
| `mind-cortex` | thin coordinator | absent; coordination is hand-rolled in `mind-conversation` |
| `mind-instincts` | Instinct trait + curiosity | absent |
| `mind-proactive` | Detect→Generate→Score→Deliver + commitment ledger | absent; proactive lives in `mind-conversation/src/proactive.rs`, `decisions.rs`, etc. |
| `mind-evolution` | thin calibration | absent; "evolution" actually = `fitness.rs` + deploy scripts + `ym evolution` scorecard in `lib.rs:5979` |
| `mind-observability` | trace log + replay | absent; `trace_id` is plumbed (`mind-types/src/event.rs:51`) and consumed by nothing |

What actually exists (lines counted under `crates/*/src`):

| Crate | ~Lines | What it really is |
|---|---|---|
| `mind-conversation` | 42k (61 files) | **the mind**. `lib.rs` alone is 9.3k lines; `ConversationEngine` (~60 fields, lib.rs:3237) spans chat, photos, finance, smart-home, delegation, cognition flags. Every module `impl ConversationEngine`. |
| `mind-tools` | 12.8k | sandbox (userns), coder, workers, MCP, browser/voice/mail/github tools |
| `mind-core` | 3.6k | engine() god-constructor, REPL command surface, Telegram poll loop (2.2k), setup |
| `mind-evals` | 3.2k | immune trials on DB snapshots, loop_eval / cognition_eval paired suites, loop_compare promotion gate |
| `mind-memory` | 4.2k | single-owner memory actor + facade, scope wall → purpose lens, receipts ledger |
| `mind-agents` | 3.0k | SubAgent ReAct, bounded Cognition loop (NBA/capsule/controller consumer), compile |
| `mind-spec` | 1.9k | model-free half: GoalSpec/Capsule/Controller (used by mind-agents + evals only) |
| `mind-recipes` | 1.9k | persistent recipe engine, SQLite store, idempotent recovery |
| `mind-inference` | 1.8k | provider chain, private lane fail-closed, survival mode |
| `mind-governance` | 1.7k | harm gate (+property/adversarial corpus), egress broker ARCH-3A, device trust, weft attestations |
| `mind-types` | 1.6k | waist types: Event, MemoryFacade, Candidate(+dead scorer), Purpose, TurnContext |

The two cognition loops (both dispatch through the same `run_agent_tool_as` and guard pipeline):

1. **Legacy agent loop** (`conversation/lib.rs:7852`) — grounding → relevance-gated catalog →
   bounded step loop with guards (`guards::pre/post`, barren counting at lib.rs:7940–8355).
   Default path (`agent_primary=true`).
2. **Bounded control loop** (`YM_COGNITION=on`; `cognitive.rs:410` → `mind_agents::Cognition`) —
   deterministic `Controller::decide` first each iteration (`mind-spec/control.rs:186`), one NBA
   call over a flat Capsule, contract-driven completion, terminal-delivery verbatim pass-through,
   procedure recall/bank. Off by default.

Governance walls that are real: harm gate deterministic + monotonic + execute()-time re-check
(`governance/lib.rs:281`); scope wall then purpose lens in fixed order inside the one facade impl
(`memory/lib.rs:2332`); hash-chained read-receipt ledger (`memory/receipts.rs`); egress broker +
HMAC receipts (ARCH-3A); GitHub-side CI job blocking any diff touching `crates/mind-governance`
(`.github/workflows/ci.yml`). CI runs **only** that git-diff job — no cargo build/test exists in CI
(the ../yantrikdb path deps make hosted builds impossible), so **the box is the only test runner**.

Always-on: one Telegram long-poll loop (`core/telegram.rs:1206`) pacing ~40 jobs, each behind a
deterministic `*_due()` gate (clock + profile-KV date keys); heavy work detached via spawn; quiet
hours wraparound-aware; treasury passes/day envelope (`treasury.rs:99–157`); interruption escrow +
funnel kill-site instrumentation on the knock path.

Self-build: lives mostly in `deploy/*.sh`, not Rust. Reflex Arc drafts goals behind a six-condition
gate (`reflex.rs`), regret wire enqueues from night-shift misses (`dream.rs:505`), human queue via
`selfbuild-goals.txt`; builder = headless Claude Code/codex/qwen (`self_improve.sh:93–98`); gates:
governance carve-out → compile → sensitive-path/diff-size/test-presence → draft PR → optional gated
automerge after CI green; deploy with health-probe auto-rollback (`self_deploy.sh:90–98`); two-tier
fitness (fast gate, slow target `0.70·skill + 0.20·tool_reliability + 0.10·urge_discharge`,
`fitness.rs:58`).

---

## B. Original-vision coverage

| Original concept | Current implementation | Maturity | Gap |
|---|---|---|---|
| Communications (CLI/TG/voice/WG chat/desktop) | REPL (`core/main.rs:188`), Telegram member+operator surfaces (`telegram.rs`), voice fast-path w/ refusal escalation (`lib.rs:8474,8626`), WireGuard `/chat` member socket (ARCH-2, fail-closed binding), clients/ dir | **Strong** | desktop/mobile clients thin; channel-declared turn identity exists (`TurnIdentity`) but latency/risk axes unmeasured (scoreboard.rs:70 declares them NOT_INSTRUMENTED) |
| Tasks | typed-graph tasks w/ jaccard+cosine dedup (`memory/lib.rs:874–940`), reminder_loop 60s (`telegram.rs:424`), escalating nudges (`proactive.rs:701`), followthrough lifecycle (`followthrough.rs`) | **Partial-strong** | tasks are reminder rows, not executable jobs; no goal linkage (`goal_id` always None, memory/lib.rs:916); commitment extractor dead under default config (§E) |
| Recipe / planned execution | `mind-recipes`: Think/Act(gated)/AskUser/WaitUntil/WaitForCondition/Schedule, SQLite, intent-hash tamper check, visible-failure recovery (`recipes/lib.rs:479`, tests :1181,:1540) | **Strong mechanics** | NL `plan:` entry sits AFTER the `agent_primary` early-return (lib.rs:9085 vs 8945) so the planner is unreachable under default config; RunRecord has no goal/task FK |
| Cron/Scheduler | per-job date keys in the poll loop; recipes sleep/wake via `resume_due` (`onboarding.rs:750`) | **Strong discipline, no center** | ~30 subsystems self-schedule; nothing arbitrates across them (§C, §D1) |
| YantrikDB memory | actor/facade exactly as designed (`memory/lib.rs:1726–1781`); working-set hydration w/ decay + uncertainty causes (`:2617`); consolidation cursor (`lib.rs:3792`); scope+purpose walls; receipts | **Strong core** | priority lanes + queue depth instrumentation MISSING (single unbounded mpsc, :1749) despite BUILD.md non-negotiable #38; `MemoryFacade::consolidate()` is an admitted no-op (:2679) while the real consolidator lives in conversation |
| Always-On Tick | poll-loop nervous system, cheap-before-expensive gates everywhere, DMN idle tick ≤1 call (`proactive.rs:94`) | **Strong** | silence unaccounted outside knock/escrow/funnel; most silent ticks leave no trace |
| Planner | bounded-loop plan-from-recalled-procedures (`cognition.rs:136`) + replan (`:448`); NL plan: (unreachable, above) | **Partial** | no durable plan object linking goal→steps→outcome; capsule plans are ephemeral |
| Researcher | `research.rs`: priors→live cited evidence→Bayesian revise→contradict edges; attribution guard born of a real incident (:20–45); deep fan-out + adversarial fact-check | **Strong** | supersession never produced (see C-Learn); research answers written with unscoped `append_message` (:8953,:8991) unlike scoped writes elsewhere |
| Builder | deploy-side self-build pipeline (above) + forge ventures (`code.rs:527`) + delegate iterate-until-good (`delegate.rs:786`) | **Strong shell** | fixture RED-verification is prompt-instruction only; no per-change benchmark A/B (`brain_bench` compares models, not diffs); post-deploy revert is prose in a goal line (`reflex.rs:143`) |
| Web Search | keyless DuckDuckGo + SSRF-guarded fetch; ScriptedFetcher for evals | **Strong** | — |
| Sub-Agent System | read-only allow-listed ReAct (`agents/lib.rs:233–427`), shared-scope reads only (`lib.rs:9336`), act-tools propose-only (:334–363); delegations w/ scratch quarantine + explicit `ym jobs keep` promotion; orphan reconciliation (`delegate.rs:561`) | **Strong** | mission contracts informal for delegations (ledger row + brief); GoalSpec contracts used only by bounded loop; sub-agent findings can bypass scoping via unscoped transcript writes (above) |
| Sandbox | userns+net-ns+prlimit tmpfs-masked scratch (`tools/sandbox.rs:96–161`); skills always sandboxed; worker-side same shape; Windows fails closed honestly (:14–15) | **Strong (Linux)** | no seccomp/landlock hardening; self-build containment is clone+cargo-allowlist, not the sandbox; output caps exist, network grants (domain-scoped egress for sandboxed code) don't |
| Self-Improvement | reflex six-condition gate w/ structural fields (`reflex.rs:45–68`), regret clusters → goals (`dream.rs:505`), dream self-ideation mining own logs, GOAL sanity gate (junk-PR scar), fitness two-tier | **Partial-strong** | evidence bar enforced at enqueue, but repro-test verification soft (fixture name checked to contain "test", reflex.rs:52); improvement measured only ~14d later via fitness scalar; rollback condition never executed automatically |
| Self-Healing | recipe idempotent recovery, delegation orphan close-out, ledger-lock steal (`immune.rs:800`), vigilance_scan log-signature watchdog (`proactive.rs:10`), provider chain failover + survival mode (`inference/lib.rs:734,1004`), corrupt-device-store fail-closed (`devices.rs:376`) | **Strong detect, weak remediate** | remediation is observe-and-escalate; no poisoned-queue/backlog detection; no automated post-deploy regression revert |

---

## C. Closed-loop analysis

```
Observe → Interpret     PARTIAL-STRONG
  Per-turn grounding: hydrate_working_set (memory/lib.rs:2617) with half-life decay,
  uncertainty classification, open contradictions, commitments; pinned exact-match entity
  beliefs defeat ranking lottery (lib.rs:9233); honesty wall for novel entities (:9227).
  Missing: no perception bus (stubs); episodes recorded but temporal miners unwired;
  context assembly has no inclusion reasons (§D).
  Refs: conversation/lib.rs:7724, 9210–9285; mind-memory/src/lib.rs:2617–2677.

Interpret → Predict     PARTIAL
  Foresight stores calibrated predictions at emission with immutable judgment ledger
  (foresight.rs:314–366, 1067–1113); knock pre-commits graded engagement predictions
  (proactive.rs:477–485). Missing: ordinary actions/tasks/tool calls carry NO prediction —
  the predict stage exists for weather-like forecasts only.
  Refs: foresight.rs:314; proactive.rs:477; scoreboard.rs; judgment_trend.rs (Brier skill).

Predict → Plan          MISSING (bounded loop excepted)
  No arbitration point converts predicted value into chosen work. Candidates are decided by
  whichever cron gate fires first; the one cross-subsystem precedence rule lives inside the
  YM_PROACTIVE block only (telegram.rs:2044–2122). The 7-axis Candidate scorer
  (mind-types/candidate.rs:18–48) has ZERO consumers — the designed unit of choice is dead
  code. Inside the bounded loop, Controller+NBA+contract IS a predict→plan seam, flag-gated off.
  Refs: telegram.rs:2044–2122; mind-types/src/candidate.rs; mind-spec/control.rs:186.

Plan → Execute          PARTIAL-STRONG
  Recipes persist and resume idempotently; Act is harm-gated + intent-hash stamped
  (recipes/lib.rs:817,909); ActionRuntime re-checks the gate independently
  (governance/lib.rs:281). Charter's ActionPacket exists as informal KV JSON
  (decisions.rs:135–180) MISSING cost, risk, reversibility, harm-class, privacy-scope;
  alternatives_rejected hardcoded empty (:159). Emissaries are synchronous functions, not
  EmissaryRun missions (emissary.rs:7,174,298).
  Refs: decisions.rs:135–180; dream.rs:357–560; emissary.rs.

Execute → Measure       PARTIAL-STRONG
  Five-way tool outcome classifier feeding bandits, Denied/Unavailable excluded from
  reliability (tool_outcome.rs:75–138); turn-grade reward channel (pace_ledger.rs:67);
  packets resolve proposed→confirmed/rejected/expired (decisions.rs:209–327). Missing:
  charter metric #2 (packet acceptance rate) computed nowhere; no expected-vs-actual triple
  per action; risk/channel/latency axes declared unmeasured (scoreboard.rs:70).
  Refs: tool_outcome.rs; pace_ledger.rs; decisions.rs:296–327.

Measure → Learn         PARTIAL
  Strong: Bayesian belief revision with negative evidence + contradiction edges
  (research.rs:208–309); procedure ledger banked ONLY on contract-met success
  (cognition.rs:396–411); bandit updates; isotonic calibration + source reliability
  (memory/lib.rs:2201–2208); skill auto-quarantine <50% over ≥4 (memory/lib.rs:1648).
  Missing: supersession producer — BeliefStatus::Superseded defined (types/memory.rs:51),
  set nowhere, to_belief_dto hardcodes "active" (memory/lib.rs:267); quarantined skills have
  no rehabilitation path; learnings don't route back into capability/skill selection weights
  beyond the bandit.
  Refs: research.rs:295–296; mind-types/src/memory.rs:51; docs/ARCH4_PURPOSE_GATE_V1.md:107.

Learn → Self-Improve    PARTIAL-STRONG (shell)
  Reflex arc: clustered corrected turns → six-condition draft gate → single-line contract into
  goals file; regret wire ≥2 misses/subject → capability-gap goal; dream self-ideation;
  builder fleet; compile/test/diff gates; draft-PR default. This is genuinely
  evidence-gated at ENQUEUE time.
  Refs: reflex.rs (whole); dream.rs:505–556; deploy/self_improve.sh.

Self-Improve → Measure  WEAK
  Deploy-time health probe auto-rollback exists (self_deploy.sh:90–98); fitness stamps grade
  ~14d later into a scalar target. But the reflex's OWN rollback condition is prose
  (reflex.rs:143–147) executed by nothing; there is no candidate-vs-baseline benchmark delta
  gate before merge; a green build + passing tests is still the operative definition of
  "worked" at merge time. A successful build is treated as evidence more often than the
  charter admits.
  Refs: self_improve.sh:119–232; fitness.rs; reflex.rs:143.
```

**Loop verdict:** observe/execute edges are the strong ones; the *cognitive* middle
(predict→plan) and the *accountability* tail (self-improve→measure) are where closure fails.
The system measures outcomes richly but rarely commits to predictions beforehand, and almost
never closes the last edge mechanically.

---

## D. Top 10 architectural gaps

Scored as `impact × frequency × strategic importance ÷ (risk × cost)`, 1–5 each.

1. **No attention economy (Predict→Plan missing).** ~30 independently-paced cron gates decide what
   the mind does next; only the proactive block has internal precedence. Interactive work is fine
   today only because quiet hours and caps accidentally prevent pile-ups. Impact 5 × freq 5 × strat
   5 ÷ (risk 3 × cost 4) = **4.2**. Slice, not rewrite: one deterministic AttentionBoard pass that
   collects due items from existing gates and orders them by urgency/importance/receptivity before
   dispatch — the gates keep their keys, the board gets the veto.
2. **Memory actor has one unbounded lane.** BUILD.md calls priority lanes non-negotiable; grep
   finds zero implementation. A bulk consolidation or export can head-of-line-block an interactive
   recall mid-turn. Impact 4 × freq 4 × strat 5 ÷ (risk 2 × cost 2) = **5.0** — highest score;
   small, testable (actor-level: two lanes, depth gauges, starvation assertion).
3. **Flight recorder absent.** trace_id plumbed end-to-end, collected by nothing; observability is
   a stub. Debugging cognition means journalctl archaeology. Impact 4 × freq 4 × strat 4 ÷ (risk 2
   × cost 2) = **4.0**. A receipts-style JSONL decision log (what was known/goal/alternatives/
   prediction/outcome) reuses the proven hash-chain pattern.
4. **Belief lifecycle has no producers for superseded/quarantined.** Revision creates parallel
   nodes + contradict edges forever; hydration must derive staleness heuristically. Impact 4 ×
   freq 4 × strat 4 ÷ (risk 2 × cost 2) = **4.0**. `research_revise` already knows old⟂new — it
   should mark the loser superseded-by-new (one producer, tombstone reason preserved).
5. **No prediction-outcome pairs at action granularity.** Forecasts and proactive sends carry
   falsifiable confidence; tasks, tool-heavy plans, and packets do not — so learning cannot
   distinguish "predicted this would work" from "it worked". Impact 4 × freq 5 × strat 4 ÷ (risk 2
   × cost 3) = **3.7**. Extend packet/task creation to stamp expected outcome + confidence;
   resolution paths already exist (decisions.rs:209–327).
6. **Dead-under-default region of handle_turn_as.** Everything after the agent_primary early-return
   (commitment extraction :9168, taught-belief extraction :9155, watch-monitors, briefing trigger,
   plan: :9085) executes only with YM_AGENT=off; default behavior silently depends on prompt
   compliance. Two near-identical sandbox intercepts drift (:8898/:9124). Impact 4 × freq 5 × strat
   3 ÷ (risk 3 × cost 2) = **2.9**. Hoist the deterministic extractors above both loops.
7. **Capability registry lacks measured evidence.** PluginSpec carries security/enabled/tools but
   success rates live elsewhere (tool_track_record, skill counters); planning consults presence,
   not performance. Impact 3 × freq 4 × strat 4 ÷ (risk 2 × cost 2) = **3.0**. Join at render:
   catalog lines + NBA prompt get `(reliability n=…)` from the existing bandit tables.
8. **ActionPacket schema incomplete + acceptance metric unshipped.** The charter's spam-guard
   metric (#2) is computable from existing rows and computed nowhere; packets lack
   risk/reversibility/cost fields needed for pre-action simulation (mission §11). Impact 3 × freq
   3 × strat 4 ÷ (risk 1 × cost 1) = **4.0** on its own scale — cheapest slice in this list.
9. **Governance immutability conventional outside the CI job.** Box-side protection of
   mind-governance/mind-evals is prompt filters (dream.rs:241,297; fitness.rs:9–12); the CI
   harm-gate-guard covers governance but NOT mind-evals custody on the GitHub side. Impact 2 ×
   freq 2 × strat 5 ÷ (risk 2 × cost 1) = **2.5**. One-line CI extension + a staged-diff check in
   self_improve.sh matching the evals-custody rule it already declares.
10. **Skills demote but never rehabilitate; treasury units are wrong-grained.** Quarantine is
    automatic SQL; recovery from quarantine is manual/absent. Treasury counts passes/day while
    gift-scout/mail-sweep/profile-refresh bypass the envelope entirely. Impact 3 × freq 3 × strat 3
    ÷ (risk 2 × cost 2) = **2.25**. Rehabilitation = re-run certification pack on a schedule;
    treasury = token-metered accounting at the inference chokepoint (already instrumented).

(Resolved during this audit: discovery false-fit corrupting the stall signal — see §G.0.)

---

## E. Duplication / dead-code audit

**Dead code**
- `mind-types::Candidate`/`ScoreAxes` 7-axis scorer — defined, tested, zero consumers
  (candidate.rs:18–48; the `.priority()` hits in capsule.rs are unrelated).
- Six stub crates advertised as architecture (§A table); `mind-evolution` has no dependents.
- `TurnContext` waist constructed only for ActionRuntime paths; the loops thread ad-hoc values.
- YantrikDB engines imported-but-unwired: causal, replay, analogy, narrative, hawkes, agenda,
  query_dsl, counterfactual (re-implemented locally as `tools/shadow.rs:161`);
  `BeliefPattern` dead import at memory/lib.rs:27.
- `MemoryFacade::consolidate()` no-op vs real consolidator in conversation (two names, one job).
- Deterministic commitment extractor dead under default config (§D6).

**Same conceptual state in multiple places ("things in flight" have four homes)**
1. Tasks — typed graph rows (SQLite via memory actor).
2. Delegation ledger — profile-KV JSON blob capped at 50 (`delegate.rs:15`).
3. Recipe runs — separate RecipeStore SQLite (`recipes/store.rs`).
4. Reminder-dedup — flat file `reminded` set (`telegram.rs:424`).
Plus nudges/grades/knock ledgers as profile KV keys. Consequence: "what is the mind currently
obligated to?" requires reading four stores; drop_sweep exists precisely because of this
(lib.rs:8677–8702).

**Idiom drift (regression hazards)**
- Scoped vs unscoped transcript writes coexist (scoped at most exits; unscoped at :8953,:8991,
  :9043,:9077,:9100,:9118,:9141,:9149,:9297) — invites an ARCH-1/ARCH-4 regression.
- Two near-identical raw-sandbox intercepts (:8898 vs :9124).
- `watch.rs` is media perception; WaitForCondition watchers live in mind-recipes — the naming
  misdirects every new reader (this audit included).

**God-object note:** the ≤8-field guard was applied to mind-core while ConversationEngine grew to
~60 fields one crate downstream. The anti-god-object doctrine needs a seam (trait split), not a
rewrite; the bounded loop + EngineBus already demonstrates the extraction pattern.

---

## F. Proposed target architecture (evolution, not replacement)

Keep: the DAG, the memory actor, both loops sharing one dispatch/guard pipeline, the poll-loop
nervous system with date-key gates, the deploy-side self-build shell, governance as-is.

Strengthen eight seams:

1. **AttentionBoard** (new module, `mind-conversation::attention`): a deterministic collector that
   polls the EXISTING due-gates and emits one ordered work list per tick; the poll loop consumes
   the list instead of running 30 independent if-blocks. Gates keep authorship of candidates; the
   board owns ordering/veto. Interactive turns remain a separate always-first lane.
2. **Memory lanes**: two mpsc channels into the same actor thread (interactive drained before
   background), queue-depth gauges exported to pulse/scoreboard. No API change.
3. **Flight recorder**: hash-chained JSONL per meaningful decision (goal, retrieved ids, declared
   purpose, considered alternatives, chosen action, policy verdicts, prediction, actual, lesson)
   keyed by trace_id; writer behind the existing receipts pattern; rendered by `ym why <trace>`.
4. **Supersession producer** in `research_revise` + reflection: mark losers, preserve tombstones;
   hydration stops deriving what history now states.
5. **Packet schema v2** + acceptance-rate computation + expected-outcome stamping at creation
   (feeds gap 5 and mission §10/§11).
6. **Capability evidence join**: catalog/NBA renders annotated with measured reliability from the
   bandit tables; discover_tools ranking blends the same prior (skills already do this —
   memory/lib.rs:1597 blends 0.1×success_rate — tools should match).
7. **Deterministic extractors hoisted** out of the dead-under-default region so commitment/belief
   capture runs on BOTH loops (they are pure string functions today; hoisting is mechanical).
8. **Closure of the last edge**: reflex rollback conditions become executable checks (fitness
   snapshot deltas already computed — add a threshold comparison that opens a revert goal), and
   self_improve gains a baseline-vs-candidate behavioral score gate using the existing
   loop_eval/loop_compare harness against a pinned scenario corpus.

Everything above reuses existing types/stores; nothing replaces a working organ.

---

## G. Concrete implementation roadmap

Each slice: objective / crates / types / tests / risk / benefit / rollback.

**G.0 — DONE 2026-08-24. Discovery fit-threshold (stall-signal integrity).**
- Objective: a multi-word discover_tools query must not treat one weak description-word overlap as
  a fit; honest empties must stay barren steps in the bounded loop.
- Affected: `mind-conversation/tool_catalog.rs` (search_lines only; gate_catalog untouched).
- Types: none added; threshold `required = if tokens >= 2 { 2 } else { 1 }`.
- Tests: `one_weak_word_overlap_is_not_a_fit`, `a_single_word_still_finds_its_tool`,
  `naming_the_tool_always_qualifies` (tool_catalog.rs); red→green proof: the previously failing
  `cognitive_loop_behavioral_suite_passes` now passes (barren_steps 0 → 3, failures stay 0,
  StepBudget stop preserved).
- Risk: low — single function, three consumers audited (discover_tools handler, evals); existing
  `search_finds_a_gated_tool_by_description` (needs 2 overlaps: price+drop ✓) unchanged.
- Benefit: retrieval honesty (no more "Native tools that may fit" lies); the controller's stall
  signal works again; wasted budget on confident dead ends drops.
- Rollback: revert one commit.
- Root cause recorded: 81b42b0 ("write the catalog in the words people ask in") introduced the
  word "drive" into browse's line; CI cannot run tests (path-deps), so the regression shipped
  silently — argues for G.9's box-side suite gating.

**G.1 — Memory lanes + queue depth (gap D2).** Actor-internal: split Cmd channel into
interactive/background lanes; drain interactive first; expose depth counters. Crates:
mind-memory, mind-types (facade trait default). Tests: actor stress — bulk consolidation cannot
delay an interactive recall beyond N commands. Risk: low (ordering within a lane preserved).
Benefit: interactive latency bounded under load; BUILD.md's non-negotiable finally true.
Rollback: feature-flagged lane selection.

**G.2 — Packet schema v2 + acceptance metric (gap D8).** Extend packet_add with
risk/reversibility/cost/expected fields (serde default = backward compatible); compute charter
metric #2 into scoreboard. Tests: schema roundtrip + metric math on fixtures. Risk: minimal.
Benefit: pre-action simulation gets its input; spam-guard measurable.

**G.3 — Supersession producer (gap D4).** research_revise marks revised-old beliefs superseded
(with tombstone reason + successor id); reflection marks conflict members. Tests: revise flow
leaves status=superseded + explain shows lineage; red-team corpus unaffected. Risk: medium-low
(read paths must include superseded deliberately, not hide by accident). Benefit: lifecycle real;
ARCH4's honest-limit retired.

**G.4 — Flight recorder v1 (gap D3).** DecisionLog JSONL (hash-chained like receipts.rs) written
at: cognitive_turn exit, packet create/resolve, selfbuild enqueue/deploy. `ym why <trace_id>`
renders. Tests: chain tamper test (pattern exists); render test. Risk: low; append-only sidecar.
Benefit: debuggable cognition; feeds G.8 comparisons.

**G.5 — AttentionBoard v0 (gap D1).** Collect (source, due_ms, urgency, importance, class) from
existing gates into one ordered list; poll loop iterates the list; per-tick speak-budget enforced
centrally (spoke flag generalizes). No gate changes required v0. Tests: ordering properties
(interactive > due-commitment > opportunistic), starvation bounds, determinism. Risk: medium —
touching the main loop; mitigate by keeping every existing gate callable and falling back to
legacy order behind env flag. Benefit: one place answers "why did the mind do this now?"; double-
fires impossible; budget coherent.

**G.6 — Hoist deterministic extractors (gap D6).** Move commitment/taught-belief capture above the
loop fork; delete the duplicate sandbox intercept. Tests: both loops produce identical extraction
on fixture transcripts. Risk: low-medium (extraction firing twice for legacy path — dedupe by
cursor). Benefit: promises captured regardless of which brain answered.

**G.7 — Capability evidence join (gap D7).** Render `(ok 87% n=31)` next to tool lines when bandit
n ≥ threshold; blend into search_lines ranking like recall_skills does. Tests: render + ranking
fixtures. Risk: low. Benefit: planning consults evidence, not existence.

**G.8 — Close the self-improve→measure edge (gap D9 + C tail).** (a) CI: extend harm-gate-guard to
also block `crates/mind-evals` changes from self-authored PRs; (b) self_improve.sh: run
pinned-scenario corpus (loop_eval/cognition_eval suites) on baseline AND candidate, require
non-regression for automerge; (c) reflex rollback condition becomes a checked threshold on the
fitness snapshot that files a revert goal automatically. Tests: script-level (bash -n + dry-run
mode) + a Rust helper for the threshold comparison. Risk: medium (deploy scripts are production);
mitigate with YM_DRYRUN. Benefit: "a successful build" stops being the definition of improvement.

**G.9 — Skill rehabilitation + treasury metering (gap D10).** Scheduled re-certification of
quarantined skills against their pack evals; token-based treasury accounting from the inference
meter (already tracked) alongside passes. Tests: rehab happy path + failure stays quarantined;
meter aggregation. Risk: low. Benefit: capability library self-heals; budget reflects reality.

Order rationale: G.1–G.4 are small, isolated, and each removes a documented lie from the system;
G.5 is the strategic centerpiece and deliberately comes after instrumentation (G.4) so its
behavior is observable from day one; G.8 hardens the optimizer boundary before the loop gets any
more autonomous.

---

## Appendix: evidence base

- Full test run at audit time: 867 passed / 0 failed (was 866/1 before G.0).
- Flows traced with line refs: interactive turn (main.rs:188 → handle_line_as → conv.turn
  cognitive.rs:371 → handle_turn_as lib.rs:8648 → agent_loop :7852 | cognitive_turn :410);
  tasks (memory/lib.rs:874–940 → telegram reminder_loop :424 → nudges proactive.rs:701);
  recipes (skills.rs:236 → recipes/lib.rs:437 → run_with :700–890 → resume_incomplete :479);
  sub-agents (agents/lib.rs:233–427; delegate.rs; conversation/lib.rs:9336 scope);
  self-build (reflex.rs → selfbuild-goals.txt → deploy/self_build_tick.sh → self_improve.sh →
  self_deploy.sh → fitness.rs); tick (telegram.rs:1193–2122, ~40 gated jobs).
- Regression archaeology: `81b42b0` introduced the "drive" false-fit 47 commits before the red
  eval; window between red-introduction and detection ≈ 6 weeks because no CI test runner exists.
