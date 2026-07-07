# THE NIGHT SHIFT — Operations Kernel Charter

*Co-designed Claude (Fable 5) × GPT-5.5, 2026-07-06. Three-round debate, converged.
This is the build contract for the next 30 days. Pre-registered before building, per house culture.*

## The leap, in one sentence

> Yantrik-mind stops being a very good memory assistant and becomes a **sleeping household
> staff that wakes up with finished, evidence-backed work** — it simulates the near future,
> prepares proof-carrying work before being asked, measures what it missed, and turns
> repeated misses into new capabilities it builds for itself.

The product moment: the owner wakes to **one** message —
*"I worked the night shift. 11 packets done. 4 need your confirmation. 2 beliefs changed.
1 regret logged and turned into a regression test. 1 capability built and deployed.
Skipped 3 low-value scans (budget); stayed silent on 2 subjects (nothing changed)."*

Not reminders. Not suggestions. A **done board** — including the negative space
(what it chose NOT to do). Restraint made visible is half the wow.

## The kernel loop (nightly)

1. **Future scan** — walk FutureNodes (events, trips, deadlines, obligations, risks) for the next 14 days; rank by deadline × severity × preparability.
2. **Mission dispatch** — spawn/resume bounded emissaries for the fragile nodes.
3. **Budget allocation** — Treasury grants each emissary its share; skip-with-log when dry.
4. **Packet compilation** — emissaries produce proof-carrying ActionPackets (drafts, checklists, carts, plans, fallbacks) to the **last safe inch**; the irreversible click is always human.
5. **Verification pass** — grounding check (facts only), privacy-lane check, harm-gate, budget, expiry.
6. **Morning done board** — one message, ranked: ready-for-confirmation / prepared / blocked-on-one-fact / beliefs-changed / regrets / what-I-didn't-do. One-tap buttons: approve, reject, show-evidence, snooze, never-again.
7. **Outcome logging** — accepted / rejected / ignored / expired per packet.
8. **Regret replay** — every owner ask is classified: anticipated / missed-but-foreseeable / unforeseeable. Missed-but-foreseeable → RegretRecord → "what signal, 72h earlier, should have triggered a packet?" → regression test.
9. **Self-extension** — regret clusters become typed tensions become **auto-enqueued self-build goals** (the existing, proven build loop authors the Rust, gates it, self-deploys). Human queue keeps priority.

## The six objects (2 new, 4 mapped onto existing code)

| Object | Status | Maps to |
|---|---|---|
| EvidenceClaim | **exists** | typed beliefs (2,400+ live: confidence, evidence trails, contradiction links) |
| EmissaryRun | **extend** | recipe engine (persistent, detached, WaitForCondition) + charter blob (mission/budget/deadline/allowed-tools/escalation) |
| BudgetLedger | **extend** | the Treasury (owner-declared envelope, static shares v1, spill-to-reserve, skip-with-log) |
| RegretTension | **extend** | open_tensions + counterfactual fields (what-could-have-been-prepared, cluster_id, selfbuild_link) |
| **FutureNode** | **NEW** | generalizes festival/trip/event ledgers into one queryable forward store (title, window, participants, dependencies, readiness criteria, linked claims, status) |
| **ActionPacket** | **NEW** | formalizes ActionIntent + hard-grounded drafting + adversarial self-check (action, reason, evidence links, confidence, cost, risk, reversibility, privacy scope, harm-gate class, confirmation-required, expiry, alternatives-rejected, status) |

## The privacy wall (built FIRST — walls before features)

Every inference request carries a **privacy scope**:

- **private** — family memories, names, photos, sensitive facts → owned-hardware models only, or deterministic template-fill, or NO model call. Hard-blocked from cloud.
- **household** — semi-private operational data → only providers the owner explicitly allowlists; logged; revocable.
- **public** — web research, generic scaffolding, Rust code, public papers → cloud freely.

**Scaffold/fill is the standard pattern:** cloud writes the generic renderer ("a birthday-card
template"), local code fills the private details (her name, the memory, the joke). Cloud never
sees the archive. This is a compliance repair, not an optimization — today's prompt routing
leaks family context to cloud providers and that stops in week one.

## The 30-day plan (against the REAL calendar)

Forcing functions: **Rath Yatra D+10 (Jul 16) · Brishti's birthday D+17 (Jul 23) · Branson D+18–21 (Jul 24–27)**

- **D1–2 — walls + baseline.** Privacy facade (scope enum at the inference chokepoint, private hard-block). Regret log ON — the baseline week measures preventable-asks BEFORE the kernel can prevent anything (untreated baseline, then show the curve drop).
- **D2–4 — the two new objects.** ActionPacket + FutureNode types; seed FutureNodes from existing festival/trip/event ledgers.
- **D4–8 — Night Shift compiler v0 + FestivalOps.** First emissary ships the **Rath Yatra packet set** (readiness checklist · logistics+weather+fallback · child-story for Aadrisha + family-message draft via scaffold/fill) with 2 days of slack before the festival. Done board renders. One-tap buttons live.
- **D10–17 — BirthdayOps.** Gift packets fueled by the taste model (8,411 analyzed photos + gift intel), card draft grounded in stored memories only, low-stress plan options, confirmation-ready cart.
- **D17–18 — TripOps + the twin's birth certificate.** Branson packets (packing personalized to family+weather, docs, route, rain alternatives, child items, night-before + return checklists). The cross-event insight — *"the trip starts the day after the birthday; reject birthday plans that create packing stress"* — is the first output no single-event system could produce. That collision IS the world twin being born.
- **D19–30 — the loops close.** Regret replay grades all three operations. First regret-cluster auto-derived self-build goal flows into the (already proven) build loop. Treasury report. Curves rendered.

## Pre-registered success metrics (kill criteria culture)

Three curves, logged from D1:

1. **Preventable-ask rate** = owner asks matching a FutureNode within its horizon that had no live packet ÷ all FutureNode-linked asks. **Must decline week over week.**
2. **Packet acceptance** = confirmed ÷ (confirmed + rejected + expired). Target: no collapse below ~40% (spam guard).
3. **Regret→capability latency** = days from tension-cluster formation to self-built capability deployed.

Day-30 hard criteria: ≥20 packets · ≥8 owner-judged useful · ≥3 confirmation-ready external actions · **0 harm-gate bypasses · 0 private-data cloud calls post-facade** · ≥5 RegretRecords · ≥1 regret→deployed capability · nightly spend within envelope · the owner says, at least once, **"I was going to ask about that."**

## Deferred (deliberately, not forgotten)

- **Voice (LAN WebRTC)** — emotional surface, not cognition; post-30.
- **The Institution** (councils/voting) — governance follows *measured* conflict between emissaries; not before.
- **Market treasury** (bidding, credit ratings) — after static-shares v1 accumulates ROI data.
- **Autonomous purchasing** — never; prepared-to-the-last-safe-inch is the permanent boundary.
- **Shadow-self policy mutation** — dead as proposed; replaced by counterfactual replay (no engagement-metric reward hacking).

## Standing doctrine (conserved from the house culture)

- The model proposes, deterministic code disposes.
- The harm-gate is human-only, inviolable, and the CI wall stays.
- Speak only when something changed; the novelty gate extends to every emissary.
- Honest gaps beat confident fabrication — packets flag what they don't know.
- Every claim traceable to evidence; every action traceable to a packet; every packet expirable.
