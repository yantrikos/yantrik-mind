# ARCH7 — Loop engineering: the mind's loops, measured, then made one

*Written 2026-09-02 on Pranab's ask: "check the loop engineering as well … make sure we are the best
in loop engineering that fits the mind." This document is the census and the diagnosis. Every
change it proposes is preregistered on the ledger before code, per house rule.*

## 1. The census — every loop the mind runs today

| Loop | Host | Cadence / trigger | Budget it respects | Exit rule | Recorded? | Runs headless? |
|---|---|---|---|---|---|---|
| Turn: `agent_loop` (tool-using reply) | every frontend | per turn | `agent_budget().max_steps` (~5), compaction | steps exhausted or model stops calling tools | tool predictions/observations (E.AGI-A2) | yes |
| Turn: bounded cognition (`cognitive_turn`, Controller + NBA + contract) | every frontend | per turn, `YM_COGNITION` | `max_steps`, `YM_MAX_MODEL_CALLS`, `YM_MAX_USD`, `YM_MAX_WALL_SECS`, stall counter | contract met, budget, or stall | full trace (goal_compiled, tool_*, graded) | yes — but **off by default** |
| Headless heartbeat (`tick_delegations` + world shadow) | `run_headless` | 30 s | none of its own; recipes carry theirs | never | horizon receipts; world_shadow | yes (only this) |
| Recipes: `resume_due` (standing orders) | inside the heartbeat / poll loop | on due `__wake_at` | recipe step guard (`guard`), intent hash | steps end, wait, or fail | run rows, notes | yes |
| Horizon: `resume_due_horizons` | inside the heartbeat / poll loop | on due job | `HorizonBudget` (actions, cost, elapsed, replans) | one segment per due goal; park on drift | hash-chained lifecycle receipts (E.HOR) | yes |
| DMN / dreaming (`dmn_tick`, 3 phases; `dream`) | Telegram poll loop only | `YM_DMN_SECS` when idle ≥ `YM_DMN_IDLE_SECS` | **one model call per tick** (global) | per phase | dreaming ring (E.WEB12) | **no** |
| Calibrated knock (`maybe_knock`) | Telegram poll loop only | when idle, ≤ 1 / day | engagement prediction committed first | nine dispositions (E.G2a) | world_shadow + knock_disposition | **no** |
| Proactive digest + executive shadow (EX4-LIVE-A) | Telegram poll loop only | `YM_PROACTIVE_SECS` (24 h), idle, quiet hours | receptivity gate | one message per tick | ex4_shadow store (n = 5 on prod) | **no** |
| Ask-drive (`proactive_ask`) | Telegram poll loop only | `YM_ASK_SECS` (2 h) | one question outstanding | — | — | **no** |
| Home watch, resolve, profile refresh, family, follow-ups, price watch, ICS, lease sweep, member beat, patterns, mail sweep, twitch, whois, trad-prep | Telegram poll loop only | one `last_x` timer each (`YM_*_SECS`, 120 s … 3 d) | none shared | each its own | mostly journal lines | **no** |
| Browse (`ym browse`) | operator command | on demand | bounded observe → decide → act; commit boundary in the driver | goal met, refusal, or steps | notes | yes |
| Self-build / reflex arc | cron on the box | nightly | six-condition draft gate; compile/test/diff | draft PR | goals file, evolution log | yes |

Eighteen background cadences live as independent `last_x` timers inside one function
(`telegram.rs`, the poll loop). Two more run in the headless heartbeat. None of them knows about the
others.

**L1d status (2026-09-02 13:00Z):** L1d-A (`bb2914c`) and L1d-B (`9b0a921`) SHIPPED — all
thirty-five loops record under loop-ledger-v5 in code; witnessed on staging only for the six loops
that run there (process-hosted and headless). The sixteen poll-loop speakers are unwitnessed by
construction until a Telegram-hosted box carries this binary (Pranab's word). One declared deviation
on the ledger: the forge gate names the venture it acts on.

**Correction (2026-09-02, L1d finding):** the table above was every loop that had a name, not every
loop the mind runs. A source census of the poll body found sixteen more proactive speakers with no
loop id and no ledger record — the morning briefing, foresight, the evening look-ahead, event prep,
anticipate, the birthday then-and-now, the dream note, forge, work watch, work radar, book ask,
event ask, the support nudge, gift scout, the weekly report and the news digests; eleven claim
engagement, nine run detached. They are ledgered by L1d (two rows: synchronous, then detached with
a two-phase lifecycle) before any of them moves.

## 2. The diagnosis — five faults, one cause

1. **Timers, not choices.** Whatever gate fires first, runs. ARCH5 §C already found the
   Predict → Plan seam MISSING: "candidates are decided by whichever cron gate fires first"; the
   seven-axis `Candidate` scorer in `mind-types/candidate.rs` — the designed unit of choice — has
   **zero consumers**. The mind never decides what its idle time is for.
2. **Loops live where the phone is.** DMN, knock, digest, ask, home watch — every judgement loop is
   hosted by the Telegram poll loop. A headless box (the canary, any console-only install) runs
   none of them. E.D2 named this censoring pattern for the executive; E.G1c found it again for the
   world shadow and hoisted one record out. It is not a bug in one loop; it is the architecture.
3. **No shared economy.** The DMN's one-call-per-tick budget is the only cross-feature budget that
   exists. Every other loop spends model calls, wall time and attention as if it were alone. The
   purpose gate (ARCH4) governs *what may be read*; nothing governs *what is worth doing now*.
4. **Idle work leaves no decision record.** A knock that stayed silent, a DMN phase that found
   nothing, a digest declined by receptivity — most of these are journal lines, not events in the
   hash-chained decision log. The flight recorder shows the mind's turns and shows nothing of its
   nights. The completeness gate (E.AGI-A5) cannot even see them.
5. **Continuation is count-based.** The turn loops stop on step count or a stall counter; neither
   asks whether the last step produced anything the mind did not already have. A run of five
   redundant searches is five steps.

One cause: the loops were built one feature at a time, each with its own timer, and no loop is
responsible for the others.

## 3. What "best loop engineering that fits the mind" means here

Not more loops. One **attention loop** that owns the mind's idle time and treats every background
activity as a *candidate* competing for a budget, decided by the roadmap's own Phase D rule —
utility = expected information gain − cost − risk — recorded as a decision every tick, hosted by
the process, delivered by whichever channel exists. The frontends become delivery surfaces; the
mind's nights become part of its record; the dead scorer becomes the live arbiter.

Built the way this house builds: measure first, shadow second, activate only past a gate.

## 4. The sequence (each preregistered on the ledger before code)

- **L1 — the loop ledger (measurement).** Every background iteration — each timer gate in the poll
  loop, the headless beat, DMN, knock, digest, ask, watches — emits one `loop_tick` decision event:
  loop id, host (`telegram` / `headless`), cadence, what it considered, what it did or why it
  skipped (`idle-gate`, `quiet-hours`, `cadence`, `nothing-due`, `budget`), model calls and wall
  spent, and the budget it consulted. No behaviour change. `ym why loops` renders the last 24 h per
  loop; the cockpit gains a *Loops* instrument. Kill: any change to what any loop sends; a tick that
  records twice or not at all (fixture per loop).
- **L2 — the attention loop, in shadow.** One `attention_tick` (hosted by the process, not a
  frontend) builds the candidate set from the same producers the timers use, scores them with the
  seven axes plus the Phase D utility, and records what it *would* have chosen — beside what the
  timers actually did (L1). Shadow only, E.PK3 discipline: it ranks, it does not choose. Gate: ≥ 14
  days of paired records, agreement and disagreement counted, the disagreements read one by one.
- **L3 — hosting moves** (split in practice into L3a: the loops that speak to nobody; L3b: a
  delivery contract, then the informational speakers; L3c: the engagement loops once the console
  surface has receptivity and reply grading — see §4a). DMN, knock, digest, ask and the watches are evaluated by the attention
  loop on every box; delivery routes to Telegram, the console notice, or the journal. The poll loop
  keeps only what is Telegram: reading updates and sending. Gate: byte-identical sends on a Telegram
  box over a replayed day (fixture), and the canary running every judgement loop it never ran.
- **L4 — the attention budget and novelty-gated continuation.** A per-hour budget (model calls,
  USD, wall) shared by every loop, spent by the arbiter; turn loops continue only when the last step
  produced new evidence (observation hash unseen this turn), else stop with a receipt that says so.
  Gate: same answers on the turn corpus with fewer steps; idle spend within budget on both boxes.
- **L5 — activation.** The attention loop chooses. Only after L2's gate, with rollback.

## 4a. Status (2026-09-02 11:05Z; every item cites its ledger row)

- **L1 — FOLDED** (`3096637`, v3 schema; `95be2ae` L1b-v3 typed gates). Every timer and cadence
  site decides through a `Gated` kind; one reduced record per opportunity; `ym why loops` and the
  cockpit's Loops card read it. Legacy race found and fixed on the way: the detached mail sweep
  could double-spawn (`OpportunityGate::take_act`).
- **L2 — PARTLY BUILT, nothing wired** (`8a3f385` v6 + L1 v4 amendment, `cfbce2a` renumbers it to
  loop-ledger-v5 with per-host wake ids; ledger correction `35fec1d` moves the shadow's base from
  v4 to v6 = current v5 + one label, on Codex's terms). Two pure slices exist and decide nothing:
  **L2-A** (`1082088`, plus Codex's grid correction `1cc9ed1`) is the attention policy's arithmetic
  — the seventeen-loop constant table, both urgency formulas, the exact integer score with its
  corrected bound, and the total ranking rule, proven equivalent to the live float scoring path on
  the frozen grid. **L2-B** (`09605f9`) is the wake identity: `cycle:<process_start_ms>:<wake_no>`,
  strict in both directions, because a shadow paired against a row of unknown wake is evidence
  about nothing. Neither is called from any loop; no row carries a cycle label yet.
  Still ahead: the v6 writer and reader, the per-wake signal set, and the one `attention_shadow`
  append. Evidence needs a Telegram box: the prod batch.
- **L3a — SHIPPED** (`38f672e`, `6684e16`, Codex's `bf0e05c`; ledger 724c0d6 → 3aaeff2 → d2c647a).
  The three loops that speak to nobody — DMN, ICS refresh, lease sweep — run in a process-hosted
  runner on every box, behind an engine-owned turn exclusion taken inside all three reply
  surfaces. Witnessed: the offline-cognition pass ran on the canary for the first time
  (`[dmn] rehearsed 8 memories`, 07:16:19Z). The lesson that cost ninety minutes: the cockpit's
  automatic JSON refreshes are turns, not people — origin is now explicit at the entry seam
  (`cli_dispatch_view`, nine polled routes, fail-closed allowlist). loop-ledger-v4 = v3 + the
  `process` host.
- **L3b — SHIPPED** (`d85b434`; prereg 297850f with Codex's notice accounting). One `Delivery`
  seam: Telegram when a chat is pinned outside quiet hours, else a durable console notice queue
  (queued → leased → shown receipts, one `shown` per notice, leased and acknowledged by the
  cockpit's Notices card), else `undelivered`. Only Telegram acceptance is delivered; a queued
  notice can mark nothing as spoken. Resolve, ProfileRefresh and Patterns run in the process
  runner on every box with explicit one-call budget lines; quiet hours now queue instead of
  drop; the headless heartbeat's notes reach the cockpit. `ym why deliveries` reads the ledger.
  Engagement loops (knock, digest, ask) wait for L3c.
- **L3c — SHIPPED in two reviewed halves** (`e42401b` the accounting, `d16b5c1` the moves; prereg
  af5a593 with Codex's answers and six amendments; E.P3 fixed inside it). Presence is the cockpit's
  own polling; an engaging line goes only where someone is and expires unshown; the prediction's
  clock starts at the cockpit's `shown` acknowledgement; one displayed line earns one claim; every
  side effect converges after a crash. Knock, digest and ask run in the runner behind the engaging
  door on every box. Witnessed on the canary: the ask's first opportunity `held:no-presence`. The
  open-cockpit half of the witness waits for a paired cockpit. Watches (HomeWatch … Whois) are L3d.
- **E.F3 — SHIPPED** (`9c142be`; ledger 769d960, witness 06f537d). An expired commitment is
  one `expired` receipt (terminal, from the verified chain, nothing appended after it) and one
  bounded notice; the sweep and the claim are one transaction; expired goals list first, outside
  the active heading. Witnessed on the canary one second after deploy.
- **L4, L5 — not started.**

## 5. What this is not

Not a rewrite of the recipes engine or the horizon scheduler — those already have budgets, receipts
and exit rules, and are the model the other loops should meet. Not a new model call per tick: L1
adds none; L2's scoring is arithmetic over existing signals. Not a change to any wall: purpose,
scope, harm and egress gates sit below every loop and stay there.
