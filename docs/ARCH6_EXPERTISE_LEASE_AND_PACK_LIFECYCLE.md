# ARCH-6 — Attachable, measured expertise: implementing `VISION.md`

*2026-08-25. Implementation map for `docs/VISION.md` (the three-part AGI-foundation / Packs /
compiled-experience conversation). Written after five parallel code sweeps over this workspace,
`../yantrikdb` (engine 0.16.0 + `packs/` authoring pipeline), `yantrikdb-mcp`, `yantrikdb-server`
and `yantrikdb-marketplace`. Every claim below carries a code reference; where the vision assumes
something the code does not do, the code wins and the design bends. Verified against HEAD
`aff4655`.*

---

## 0. The vision's proposals, each with a verdict

`VISION.md` is not one plan; it is fourteen proposals of very different sizes. Verdicts:

| # | Proposal (VISION.md line) | Verdict | Slice |
|---|---|---|---|
| 1 | `PackView` + five ops as a thin adapter in `mind-memory`, no `mind-packs` crate (:477-510) | **ADOPT** — 80% exists as `MemoryFacade` pack methods; extend `PackBrief`, add catalog + identity | P.1, P.3 |
| 2 | Packs stay OUT of the world model; domain knowledge ≠ current situation (:455-473) | **ALREADY TRUE structurally** — pin it with a test (§B.1) | P.1 |
| 3 | Coverage examples route pack selection before any LLM is asked (:514-570) | **ADOPT** — no engine API exists; build host-side, abstain on ties, shadow first | P.3 |
| 4 | Mounting is a lease: `PackLease{pack_id, reason, task_id, mounted_at, expires_at}` (:574-641) | **ADAPT** — lease = *visibility*, not engine mount; engine mount state is shared across concurrent turns (§B.5) | P.4 |
| 5 | `KnowledgeCapability` with `published_efficacy` / `local_efficacy` / `local_n` kept separate (:644-711) | **ADOPT** — published from the signed `certify.py` certificate, local from the flight recorder | P.2, P.5 |
| 6 | Capability-gap ladder: existing capability → existing pack → new skill → new tool → self-build (:715-775) | **ADOPT** — today both gap detectors jump straight to rung 5 (§B.6); rung 4 is propose-to-human | P.6 |
| 7 | Pack provenance flows into world-state lineage as `pack:… record:… digest:…` (:779-827) | **ADAPT** — belief `Evidence.source_event` now; typed `mind-world` source when the world log is REACHABLE (it is BENCHMARKED) | P.9 |
| 8 | Pack rules never outrank governance (:831-870) | **ALREADY TRUE for the walls** (they never read prompts); pin with a test; there is no standing-rules layer to order, so build no rule engine (§B.8) | P.4 |
| 9 | Sub-agents = same mind + expertise lease + memory scope + tools + budget (:873-907) | **ADOPT** — `SubAgent::new` is five positional args and carries no `AccessContext`; this slice also closes an ARCH-4 hole | P.8 |
| 10 | Clustered packs for a fleet (:910-931) | **DEFER** — single box; server RFC 031 exists when needed | — |
| 11 | Pack evolution v1→v2, v1 historical, quarantine (:1301-1342, :1721-1742) | **ADOPT** — via a lifecycle registry with `supersedes`; nothing today versions a Skill, Recipe, Procedure or pack | P.7 |
| 12 | Pack index hierarchy / graph relations / four pack kinds (:1344-1444) | **DEFER / REJECT for now** — the vision itself says "don't explode the taxonomy"; one manifest, `supersedes` only | — |
| 13 | Promotion ladder raw experience → … → proven pack; never seal from one success (:1748-1784) | **ADOPT** — with `runs ≥ 5` (`Prior::is_trustworthy`) and independently-authored evals | P.7 |
| 14 | Phases 4–7 (imagination, long-horizon goals, abstraction, self-development) (:310-366) | **SEQUENCE, don't start** — Phase 4/5 need world-state + executive past BENCHMARKED; the pack line IS the substrate for 6/7 (§E) | — |

---

## A. What exists (as built, not as documented)

### A.1 Substrate — `yantrikdb` 0.16.0 (`../yantrikdb/crates/yantrikdb-core/src/engine/pack.rs`)

Complete and well-tested (27 tests, `tests/pack_mount.rs`):

- `PackManifest` (`pack.rs:132`): `name, version, origin, description, embedder, content_digest,
  corpus_rows, namespace, publisher_pubkey, signature, reembedded_from, constitution: Vec<String>,
  coverage: Vec<String>, recommended_top_k, recommended_min_similarity`. `pack_id()` = `origin@version`
  (`:225`). **No efficacy field of any kind** — efficacy lives only in the detached, signed
  `yantrikdb.pack.cert.v1` JSON written by `packs/certify.py` (`:48`, `:96-110`) and in the
  marketplace DB.
- `mount_pack :948`, `unmount_pack :1373` (drops an `Arc`; zero host writes — `unmount_leaves_host_byte_identical`),
  `install_pack :1732` (copies beside the db, auto-remounted at open), `mounted_packs :1389`,
  `pack_context :1431` (constitution + coverage rendered as prose with an authority ceiling
  emitted LAST, `:1485-1492`), `seal_pack :809`, `sign_pack :1053`, TOFU trust tiers
  `Signed 0.85 / Unsigned 0.75 / Unverified 0.60` (`:83-90`).
- Recall: pack candidates merge into the host pool at `recall.rs:2804` ("step 3.45") and compete
  for MMR/ordering; score = `composite * tier` (`pack.rs:1609`). **Provenance on a hit is one string**
  `why_retrieved: "pack:{manifest.name}"` (`:1594`) — the name, not the id, not the digest; the
  pack's `rid` is carried but indistinguishable from a host rid.
- **Not in the engine:** any coverage-matching API (coverage is prose in `pack_context` only;
  `docs/PACKS.md:507` says the similarity floor "stands in for it"); application of
  `recommended_top_k` / `recommended_min_similarity` (signed, readable via `read_manifest :2196`,
  never applied, not on `PackInfo`); pack-only recall; per-pack telemetry of any kind.
- The measured attach-harm result (`docs/PACKS.md:477`): unconditional top-5 injection took a
  control set from **12/12 to 5/12**; gating on *similarity* (not composite score) restored it.
  The engine leaves that gate to the consumer.
- Authoring is Python in `../yantrikdb/packs/`: `build.py` (seal), `lint_pack.py`,
  `sweep_retrieval.py` (joint top_k × min_similarity), `evaluate.py` (deterministic string match,
  never an LLM judge), `certify.py` (holdout baseline vs mounted + attach-harm control, signed by
  an *evaluator* key distinct from the publisher key), `compile.py`/`bundle.py` (the second carrier:
  constitution → LoRA `.ycap`), `candidates/*.json`. Not reachable from the `yantrikdb pack` CLI
  (`info|install|list|keygen|sign|trust|remove` only) nor from the MCP `pack` tool (`tools.py:2969`).

### A.2 The mind

**Two unrelated things are both called "pack" in this repo.**

| | Knowledge pack (`.ydbpack`) | Capability pack (`ym pack install <json>`) |
|---|---|---|
| Type | `PackBrief { id, name, version, origin, trust, rows, namespace, installed }` `mind-types/src/memory.rs:367` | `PackDoc { pack, title, …, skills, evals }` `mind-conversation/src/pack.rs:77`; `InstalledPack { doc, certified, attestation }` `:105` |
| Ops | `MemoryFacade` `mount_pack :524`, `install_pack :545`, `unmount_pack :549`, `uninstall_pack :535`, `mounted_packs :553`, `recall_from_packs :561`, `pack_context :567`, `seal_learned_pack :541` | `pack_install :305`, `pack_certify :363` (PackEval all-must-pass; empty evals = fail; demotes to disabled), `attest_verdict :400` (Weft), `pack_draft :468` (self-authorship from skills) |
| Reaches the model at | `pack_context` → `build_prompt` (`lib.rs:6980`), agent loop (`:8092/:8179`), page recipe (`delegate.rs:763/:192`); `recall_from_packs` → `turn_grounding` top-5 (`lib.rs:7908`, labelled "third-party reference"), bounded-loop procedures (`cognitive.rs:350-364`) | skills banked as `<pack>.<skill>` in `mind_skills` |
| Sealed by | `seal_learned_pack` (`mind-memory/src/lib.rs:2891`): `learned-craft` namespace only, table allowlist scrub `:444`, `looks_private` PII lexicon `:585`, version passed as the literal `"0.1.0"` (`lib.rs:6298-6308`) | never sealed |
| Certified by | never | `pack_certify` |

The seal path and the certify path share no type, and neither knows the other exists (§B.4).

**Where the mind's own recall goes.** `recall_typed` / `beliefs_matching*` / `hydrate_working_set`
(~40 call sites) go through `Cmd::RecallTyped` → `recall_beliefs(&db, …)`, which scores the typed
**belief graph** and never touches the engine's vector index — stated verbatim at
`lib.rs:7900-7902`. `recall_from_packs` (`mind-memory/src/lib.rs:2090-2115`) is the only path to
a pack's corpus: `db.recall_text(query, top_k*4)` post-filtered by the mounted packs' namespaces,
mapped to bare `(text, score)`. No similarity floor. The rid and pack id are discarded.

**Reliability lives in three unjoined ledgers.** `mind_skills.runs/successes` (SQLite; `Skill::success_rate()`
returns **1.0 at zero runs**, `memory.rs:356`; auto-quarantine `runs>=4 && successes*2<runs`,
`mind-memory/src/lib.rs:1675`, duplicated at `procedure.rs:68`, `surface.rs:743`, `pack.rs:476` with a
*different* threshold; no rehabilitation path; `candidate → active` never implemented — every
creator but `skills.rs:397` writes `"active"` directly); the per-tool Beta bandit (`guards.rs:117-128`
is the one write site; `tool_track_record` `mind-memory:2245`); the hash-chained flight recorder
(`mind-observability/src/lib.rs:33`, `DecisionEvent` with `predicted/outcome/brier/semantic_success`).
Exactly one learning signal is closed (BUILD.md's rule): `tool_catalog::search_lines_with_evidence`
(`:437`, `capability-ranking-v1`) with `selection_flipped` events (`lib.rs:7703-7760`).
Pack-derived procedures are `Prior::declared(0.5)` forever (`cognitive.rs:359`). `PluginSpec`
(`plugins.rs:150`) carries `Provenance{Builtin,Imported,SelfAuthored}` and no reliability
(ARCH-5 gap D7).

**Gap → self-build has no intermediate rungs.** Regret clusters (`dream.rs:505-556`, n≥2 on a
subject) and the Reflex Arc (`reflex.rs:45` six-condition gate) both append a line to
`selfbuild-goals.txt`; `deploy/self_improve.sh` turns it into a source PR. Neither consults
`capability_report()`, `recall_skills()`, `tool_track_record()` or the pack library first.
`GoalSpec.missing_capabilities` (`mind-spec/src/goal.rs:37`, filled by `compile.rs:101`) is the only
"capability unavailable" signal and it never reaches `arbitrate()`, whose
`ResourceContextView.capability_available` is an anonymous bool (`mind-proactive/src/lib.rs:81`).

**Other organs the vision touches.** `mind-world`: `WorldEvent.source_id: String` (`:20`), no typed
source, `lineage_of` one hop (`:204`), crate used only by `mind-evals`. `mind-governance`: harm gate
+ `GovernedActionRuntime::decide` (`:261`), purpose lens `purpose_allows` (`mind-types/src/purpose.rs:322`),
egress broker — **no precedence type, no rule store, no standing-rules layer**; pack-constitution
containment is prompt prose in the engine (`pack.rs:1485-1492`). `SubAgent` (`mind-agents/src/lib.rs:233`)
takes five positional args, no `AccessContext`, no pack context, `max_steps` as its only budget while
`mind_spec::Budget` (`goal.rs:297`) sits unused. Maturity ladder and doctrines are prose only
(`PHASE2_EXPERIMENT_LEDGER.md:390-393`); no rung is data. `mind-evolution`, `mind-identity`,
`mind-cortex`, `mind-instincts`, `mind-perception` remain one-line stubs.

### A.3 Honest placement on the ladder, pack line

| Component | Rung today | Evidence |
|---|---|---|
| Engine mount/seal/sign | ACTIVE (in the engine's own terms) | 27 tests; installed packs remount at open |
| `pack_context` injection | ACTIVE, not outcome-validated | three live sites; no measurement of effect |
| `recall_from_packs` evidence | ACTIVE, not outcome-validated, **and unfloored** | `lib.rs:7908`; §B.2 |
| Capability-pack certification | ACTIVE for skill bundles | `pack_certify`; Weft attestation when configured |
| `seal_learned_pack` | REACHABLE (operator verb) | never certified, hardcoded version |
| Coverage routing, leases, local efficacy, gap ladder, lifecycle registry | absent | — |

---

## B. Findings that change the design

**B.1 "Keep packs out of the world model" is already true by construction.** The mind's belief
recall never reads the pack index (`lib.rs:7900-7902`); pack text enters only as labelled
third-party evidence (`:7912`) or as constitution prose. The existing test `tests.rs:3526-3543`
pins one direction (a host belief never leaks through `recall_from_packs`). The inverse — a pack
row never becomes a belief without a `pack:` provenance — has no test. Add it (P.1).

**B.2 Live defect: pack evidence is injected with no similarity floor.** `recall_from_packs`
returns the top 5 by score with no threshold, and `turn_grounding` injects all of them. That is
exactly the condition the substrate measured as harmful (`PACKS.md:477`, controls 12/12 → 5/12).
`recommended_min_similarity` is signed into every manifest for this purpose and read by nothing
in this workspace. This is a *wall before feature*: fix before any pack becomes easier to reach.

**B.3 Recall drops pack identity, so lineage is impossible today.** `(text, score)` is all that
survives `Cmd::RecallFromPacks`. The engine only offers `why_retrieved: "pack:{name}"` and the rid.
Everything downstream — local efficacy per pack, `source_event = pack:…#rid`, supersession of a
pack row by a host correction — needs `PackHit { pack_id, rid, text, score, namespace }`.

**B.4 Seal and certify are disjoint, and "pack" is overloaded.** A learned `.ydbpack` is never
evaluated; a certified capability pack is never sealed. The promotion ladder needs one lifecycle
object that both paths write to. Name them apart in code: *knowledge pack* (`.ydbpack`) and
*skill bundle* (`PackDoc`); the registry in P.7 tracks the former, and a skill bundle becomes a
stage input ("repeated success") rather than a parallel pack system.

**B.5 A lease cannot be an engine mount.** `pack_context()` is unconditional over everything
mounted (`pack.rs:1408-1431`); the memory actor is one shared engine; Telegram spawns a task per
message (`telegram.rs:1316`). Mount-for-turn-A / unmount-at-A's-end changes turn B's context
between its grounding and its prompt build. `packs/CAPABILITIES.md §8` measured the same hazard
for adapters ("two clients sharing a daemon each read a flag the other is setting… byte-identical
'compiled' and 'bare' artifacts"). Therefore: **the library stays mounted; a lease is the set of
packs a given turn/job/run is allowed to see**, applied as a filter on `pack_context` and
`recall_from_packs`. Lazy mount-on-lease is a later optimisation once the library outgrows memory.
Consequence to verify on the box: mounting the whole library does not pollute the mind's own
grounding (B.1 says it cannot; re-check after P.3, it is the load-bearing assumption).

**B.6 The escalation ladder is inverted today.** Rung 5 (source modification) is the only
implemented rung; it has the strongest gates in the system (six conditions, treasury, compile/test/
diff, governance carve-out, eval custody) while rungs 1–3 have none because they do not exist as
rungs. Building them makes self-build *rarer*, not more autonomous.

**B.7 Two ladders, no data.** The 7-rung ladder (`PHASE2:392`) and the vision's 5-rung pack ladder
(`VISION.md:1779`) do not map one-to-one and neither is a type. Reconciliation in §C.3; the pack
registry stores the 7-rung value.

**B.8 Governance precedence is structural, not textual.** The harm gate, purpose lens and egress
broker are deterministic and never read prompt text, so a pack constitution cannot alter their
verdicts — that IS the enforcement. What the vision calls "user standing rules" has no
implementation at all (`NEVER_RULE` at `tool_catalog.rs:30` is a prompt constant; "standing orders"
are recurring recipes). Do not invent a rule engine to order layers that do not exist. Do pin the
invariant: a mounted hostile pack (the engine's own `pack_context_contains_hostile_constitution`
fixture) must leave `GovernedActionRuntime::decide`, `purpose_allows` and `egress::classify`
byte-identical (P.4 test), and pack context must sit after persona and before memory in message
order with the authority ceiling last (already the engine's shape).

**B.9 Learning signals around packs are open, not closed.** `Prior::declared(0.5)` never becomes
measured; `success_rate()` fakes certainty at n=0; the quarantine rule has four copies. One
`Reliability` type in `mind-spec` with `Basis` and a single verdict function is the precondition
for anything pack-related to learn (P.5).

---

## C. Design

### C.1 Vocabulary (new types; where they live)

```text
mind-types (waist, no logic)
  PackHit        { pack_id, rid, text, score, similarity, namespace }        // replaces (String, f64)
  PackBrief      + content_digest, coverage: Vec<String>, recommended_top_k,
                   recommended_min_similarity, signer: Option<String>, stage: PackStage,
                   published: Option<PublishedEfficacy>, local: LocalEfficacy
  PublishedEfficacy { holdout_n, baseline, mounted, attach_harm_pass, evaluator_pubkey, cert_digest }
  LocalEfficacy  { impressions, used, graded, good, first_used_ms, last_used_ms }   // denominators kept
  Lease          { id, pack_id, purpose: Purpose, scope: LeaseScope, reason, granted_ms, expires_ms }
  LeaseScope     { Turn(trace_id) | Job(id) | Run(run_id) | Standing }
  PackStage      { Draft, Candidate, Sealed, Probation, Proven, Quarantined, Superseded }
  CapabilityGap  { id, subject, evidence: Vec<String>, opened_ms, rung: Rung, attempts: Vec<Attempt>, status }
  Rung           { ExistingCapability, ExistingPack, NewSkill, NewTool, SelfBuild }

mind-spec (model-free decisions; must never gain an inference dep)
  reliability::{ Reliability { runs, successes, basis }, verdict(&Reliability) -> Verdict{Untested,Candidate,Active,Discredited} }
  coverage::rank(query_vec, &[(pack_id, Vec<vec>)], floor, margin) -> Option<Vec<(pack_id, sim)>>   // abstains on ties
  lease::policy(ranked, caps, standing) -> Vec<pack_id>                                             // pure, capped
  lifecycle::transition(stage, &LocalEfficacy, &PublishedEfficacy, thresholds, now) -> Option<PackStage>
  escalation::next_rung(&CapabilityGap, &Inventory) -> Remedy

mind-memory (adapter + tables; sole writer)
  tables: mind_pack_registry(pack_id PK, stage, content_digest, sealed_ms, supersedes, superseded_by, cert_json)
          mind_pack_stats(pack_id PK, impressions, used, graded, good, …)
          mind_pack_coverage(pack_id, content_digest, phrase, vec)          // cache
          mind_capability_gaps(id PK, subject, rung, status, opened_ms, closed_ms, closed_by_rung, json)
  facade: available_packs(), inspect_pack(id), recall_from_packs_scoped(query, top_k, leased: &[pack_id]) -> Vec<PackHit>,
          pack_context_for(&[pack_id]), record_pack_impression/used/graded, leases CRUD

mind-conversation (glue, `ym` verbs, recorder emits)   |   mind-agents (Mission)   |   yantrikdb (substrate asks)
```

DAG respected: `mind-spec` depends only on `mind-types`; `mind-memory` is the only crate touching
`yantrikdb-core`; `mind-conversation` composes. No new crate. `mind-evolution` stays a stub until
something needs a home the DAG forbids elsewhere — nothing here does.

### C.2 Substrate asks (small PRs to `../yantrikdb`; each has a host-side fallback for v1)

1. `PackInfo` gains `content_digest, coverage, recommended_top_k, recommended_min_similarity,
   publisher_pubkey` (today only `read_manifest(path)` exposes them; the Python binding already
   omits `namespace`, `py_engine/pack.rs:161-169`). Fallback: host calls `read_manifest` per mounted path.
2. `RecallResult` provenance = `pack:{pack_id}@{content_digest[..8]}` and a `pack_id: Option<String>`
   field, not `pack:{name}`. Fallback: host maps rid → pack via `PackFilters`-style namespace matching (weaker).
3. `pack_context_for(&[&str]) -> Option<String>` — the existing renderer with a pack filter, so
   the authority ceiling and `sanitize_pack_prose` are not duplicated host-side. Fallback: host
   renders from `read_manifest` with its own ceiling text (accept the duplication, note it).
4. Optional, later: `coverage_match(query) -> Vec<(pack_id, sim)>` in the engine, where the
   embedder and index already are. v1 is host-side.

### C.3 Ladder reconciliation (one ladder, stored as data)

| 7-rung (project) | Pack-line meaning | Vision's 5-rung |
|---|---|---|
| DEFINED | manifest sealed, lint clean (`lint_pack.py`) | CREATED |
| TESTED | `evaluate.py` runs; holdout separate from corpus author | TESTED |
| BENCHMARKED | signed `certify.py` cert with attach-harm control | BENCHMARKED |
| REACHABLE | in the library, coverage-routable, at least one lease granted | LOCALLY_USED (a) |
| SHADOWED | surfaced into grounding and *graded* while another source stayed authoritative | LOCALLY_USED (b) |
| ACTIVE | evidence used in answers; procedures ranked by measured prior | — |
| OUTCOME-VALIDATED | local efficacy lower bound ≥ floor over n ≥ N, censoring reported | OUTCOME-VALIDATED |

`PackStage` is the lifecycle (what the registry does to a pack); the rung is the epistemic claim
(what we are entitled to say about it). A pack can be `Proven` in stage only when its rung is
OUTCOME-VALIDATED; the transition function refuses otherwise.

---

## D. Slices

Order rationale: walls first (P.1), instrumentation before adaptation (P.2 before P.4/P.5), the
router shadowed before it leases (P.3 → P.4), learning closure (P.5) before anything escalates
on it (P.6), lifecycle last because it consumes every earlier signal (P.7). Each slice is one ledger
entry with the fields below, KEEP/KILL decided on the box.

**P.1 — Pack identity + similarity floor (the wall).**
Objective: no pack evidence enters a prompt below the pack's own floor; every hit knows its pack.
Crates: mind-types, mind-memory, mind-conversation. Types: `PackHit`; `PackBrief` extended via
`read_manifest`. Change: `Cmd::RecallFromPacks` returns `PackHit` with `similarity` from the engine
result and applies `recommended_min_similarity` (default floor when absent — take the value
`sweep_retrieval.py` found, not a guess) *on similarity, not composite*; grounding label carries
`pack_id`. Tests: `:memory:` sealed-pack fixture (reuse `craft.ydbpack` round-trip tests,
`mind-memory:3107-3149`): a control query below floor yields nothing; a covered query yields hits
with ids; inverse of `tests.rs:3526` — a pack row can never be asserted as a belief without
`provenance == "pack"`. Kill criterion: none (plumbing) — but reachability must be shown: a box
trace with a floored-out hit on the default chat path. Risk: low. Rollback: revert; floor default 0.
Rung earned: pack evidence goes from "ACTIVE, unfloored" to ACTIVE with the measured wall.

**P.2 — Pack telemetry (measure before adapt).**
Objective: know, per pack, impressions / used / graded / good, with censoring explicit. Crates:
mind-observability (event kinds `pack_surfaced`, `pack_evidence_graded`), mind-memory
(`mind_pack_stats`), mind-conversation (emit on the **default** agent loop and `chat_as`, not only
the bounded loop — E.R2's recorder was dark for exactly that reason). "Used" v1 = deterministic
n-gram overlap between surfaced pack text and the reply, declared a proxy; "graded" joins the
existing turn grade (`pace_ledger.rs:67`) when one exists, else `Pending`/`Censored` — never 0.
`ym packs` renders published (from the cert) beside local, each with its n. Doctrine 2 audit built
into the report: `P(graded | used)` vs `P(graded | unused)`; if they differ materially, the
report says so above the rates. Doctrine 3: counts come from the hash-chained recorder AND are
recounted by SQL over `mind_pack_stats`; the render shows both and flags divergence. Tests: event
schema, censoring never counted as failure, pending retirement rule (window + margin → instrumentation
defect, the E.D4 rule). Kill: if after 14 days the two witnesses disagree, the slice is KILL until
the recorder is proven live. Rung: instrumentation, no behaviour change.

**P.3 — Pack catalog + coverage router (SHADOWED).**
Objective: answer "which expertise matches this need?" without an LLM, and abstain honestly.
Crates: mind-spec (`coverage::rank`), mind-memory (`available_packs()` over `YM_PACK_LIBRARY` +
installed; manifests read without mounting; coverage phrase embeddings cached by
`content_digest` using the embedder `recall_skills` already uses, `mind-memory:1622`),
mind-conversation (router runs every turn, emits `pack_route_shadow{query, ranked, would_lease}`,
leases nothing). Policy: top-1 sim ≥ floor AND margin over #2 ≥ δ, else abstain. Tests: three
fixture packs with disjoint coverage; right pack chosen; overlapping coverage → abstain;
determinism. Kill criterion (pre-registered): on a hand-labelled set of ≥ 30 real household/work
queries, top-1 agreement with the human label ≥ 0.80 and abstention on the ≥ 10 "no pack applies"
queries ≥ 0.90 — otherwise the coverage lists are too thin (author more coverage; that is a pack
fix, not a router fix) or the router is wrong. Rung: REACHABLE + SHADOWED for routing.

**P.4 — ExpertiseLease (ACTIVE on the chat path).**
Objective: a turn sees only the packs leased to it; leases have purpose, scope and expiry.
Crates: mind-types (`Lease`, `LeaseScope`), mind-spec (`lease::policy`, capped at 2 packs/turn —
each constitution is up to 1500 tokens, `CONSTITUTION_TOKEN_BUDGET`), mind-memory
(`pack_context_for`, `recall_from_packs_scoped`, lease store), mind-conversation (`ym pack lease
<id> [--standing|--turn]`, `ym leases`, release at the end of `ConversationEngine::turn`
`cognitive.rs:596`, expiry sweep on the poll loop next to the other `last_*` cursors). Every grant
and release is a recorder event with the router's ranked list attached. Product decision for
Pranab (§G): auto-lease from the router on day one, or standing leases only until P.3's kill
criterion is met — recommendation: the latter for one week. Tests: (a) attach-harm control —
a pinned corpus of ≥ 20 household questions unrelated to any pack answers identically with the
library mounted vs empty (the `control_when_mounted.py` pattern, moved into `mind-evals`);
(b) hostile-pack invariant (§B.8): mount the engine's hostile fixture, assert
`GovernedActionRuntime::decide`, `purpose_allows`, `egress::classify` unchanged; (c) a lease
scoped to trace A is invisible to trace B; (d) expiry releases. Kill: any regression in (a) on the
box → KILL the auto-lease policy, keep standing leases. Risk: medium (touches prompt assembly at
three sites). Rollback: env flag falls back to global `pack_context()`. Rung: leases ACTIVE.

**P.5 — Close the learning signals (KnowledgeCapability).**
Objective: measured pack evidence changes a future decision; published and local never mix.
Crates: mind-spec (`reliability::{Reliability, verdict}` — one function replacing the four
copies of `runs>=4 && successes*2<runs`; `success_rate()` at n=0 becomes `Basis::Untested`, not
1.0), mind-memory (load `<pack>.cert.json` beside the pack, verify with
`yantrikdb::verify_bytes`, store as `PublishedEfficacy`; `Prior::measured(runs)` for pack procedures
once `local.graded ≥ 5`, replacing `cognitive.rs:359/:393`), mind-conversation (router ranking
blends local efficacy with the `capability-ranking-v1` formula, `tool_catalog.rs:117-121`, and
emits `selection_flipped` when the blend changes the lease — same audit trail as tools;
`surface.rs:408 CapabilityEntry` and catalog lines gain `(ok 87% n=31)` — this is ARCH-5 G.7).
Tests: verdict table; cert signature failure → published shown as "unverified", never as a number;
flip audit fixture. Kill: over a window, leases flipped by local evidence must not be graded worse
than the unflipped baseline more often than better (denominator: flips with a grade). Rung:
learning ACTIVE for packs.

**P.6 — Capability-gap ladder.**
Objective: a repeated failure tries the cheap rungs before a source PR, and records why.
Crates: mind-types (`CapabilityGap`, `Rung`), mind-spec (`escalation::next_rung` — pure over an
`Inventory { enabled_tools+reliability, skills+verdict, packs+coverage_sim, mcp_available }`),
mind-memory (`mind_capability_gaps`), mind-conversation (both detectors — `dream.rs:505`,
`reflex.rs:253` — open/advance a gap instead of writing a goal line; a goal line is written only
when the gap reaches `SelfBuild` with rungs 1–3 recorded as tried or inapplicable; rung
`NewTool` = a proposal to the operator naming an MCP server, never autonomous; `NewSkill` =
a bounded-loop / delegation goal with `bank_procedure`, `cognitive.rs:416`). Gap closure =
the subject stops recurring for a window; `regret→capability latency` (NIGHT_SHIFT_CHARTER metric
#3) computed per rung. `ym gaps`. Tests: ladder is monotone; a gap with a covering pack never
reaches SelfBuild without a recorded pack attempt and its grade; goal-file output unchanged for
gaps that legitimately reach rung 5. Kill: if in 4 weeks no gap is closed at rungs 1–3 while gaps
keep reaching 5, the ladder is decoration — report it, do not keep it. Rung: ACTIVE; this is the
first place self-build gets *less* frequent by design.

**P.7 — Lifecycle registry + promotion ladder.**
Objective: one object that seal, certify, use and quarantine all write to; versions that supersede.
Crates: mind-memory (`mind_pack_registry`; `seal_learned_pack` derives the version from the
registry — `0.1.0 → 0.2.0`, `supersedes` = previous; the `"0.1.0"` literal at `lib.rs:6298-6308`
goes away), mind-spec (`lifecycle::transition` with pre-registered thresholds: `Candidate`
requires the source skills at `Verdict::Active` with `runs ≥ 5` — `pack_draft`'s `min_runs=1`
retired; `Sealed → Probation` requires a certificate whose `evaluator_pubkey ≠ publisher_pubkey`
AND an eval corpus whose author is recorded as not the corpus author — the independence rule the
`uk-statutory-rates` pack established and `asset-craft` 0.1.0 followed (a different model wrote the
exam without seeing the corpus); `Probation → Proven` requires `local.graded ≥ N` and Wilson lower
bound ≥ published − tolerance; `→ Quarantined` on upper bound < floor; `Superseded` when a newer
version reaches `Probation`), mind-conversation (`ym pack stage`, the skill `candidate → active`
producer — the first ever — and quarantine re-certification on a schedule, ARCH-5 G.9).
Tests: a fixture with faked counters is refused; same-author evals cannot certify; v1 stays
readable after v2 is preferred; router prefers the highest non-quarantined stage. Kill: no pack
may reach `Proven` in the first N days by construction; if one does, the thresholds are wrong.
Rung: lifecycle DEFINED→TESTED→REACHABLE in one slice; OUTCOME-VALIDATED only by living.

**P.8 — Sub-agent `Mission` with lease and scope.**
Objective: "the same mind delegating work to a worker temporarily equipped with expertise".
Crates: mind-agents (`Mission { task, access: AccessContext, leases: Vec<pack_id>, tools,
act_tools, budget: mind_spec::Budget }` replacing the five positional args of `SubAgent::new
:246`; leased `pack_context_for` injected into the prompt at `:295-315`; **sub-agent reads carry
`access` through the purpose gate** — closing the ARCH-4 residual that they run outside it),
mind-conversation (`delegate.rs` passes the job's lease). Tests: a mission without a lease sees no
pack; scope isolation; budget honoured. Rung: REACHABLE for delegation packs.

**P.9 — Provenance into beliefs (world-state deferred).**
Objective: a belief derived from pack evidence says so forever. Change: consolidation/research
writes `Evidence.source_event = Some("pack:{pack_id}#{rid}@{digest8}")` (`memory.rs:100`) and
`provenance = "pack"`; `explain_belief` renders it; a `SourceRef::parse` helper in mind-types so the
same grammar becomes a typed `mind-world` source variant when the world log leaves BENCHMARKED
(its own slice, not this line). A host correction that supersedes a pack row uses the engine's
`pack_row_ns_status` edge (`pack.rs:2227`) so the correction survives remount. Tests: round trip;
`ym explain` shows the pack. Rung: DEFINED→REACHABLE.

---

## E. Relation to the other lines

- **Executive line.** EX4-LIVE-A is KEEP at `aff4655` and its site cannot disagree by
  construction; the next steps there are choosing a site that can (EX4-LIVE-C), then EX5, with
  EX6-E owed a contract. That line now needs *wall-clock* (one digest opportunity per day) more
  than code. The pack line touches memory/skills/evals/agents and never `arbitrate()`, so it is the
  right build line while shadow opportunities accumulate. The one touch point: P.6's `Inventory`
  is exactly what `ResourceContextView.capability_available` should become (a named capability
  with a remedy), and it is how a knowledge gap eventually enters the executive — but not before
  EX5.
- **ARCH-5 roadmap.** P.5 ⊇ G.7 (capability evidence join); P.7 ⊇ G.9 (skill rehabilitation);
  the flight recorder G.4 exists since E.R2 and P.2 builds on it; G.3 (belief supersession
  producer) is the same shape as pack `Superseded` and should share the tombstone-reason pattern.
  G.1/G.5/G.6/G.8 are untouched by this line.
- **Phases 4–7 of the vision.** Phase 4 (imagination) needs prediction–outcome pairs at action
  granularity (ARCH-5 gap D5; the recorder already has the fields) and a REACHABLE world log —
  not this line. Phase 5 (long-horizon) needs a durable plan object (ARCH-5 C, Predict→Plan
  MISSING) — not this line. Phase 6 (abstraction/transfer) has a concrete first form here: a pack
  sealed from one domain's experience and *proven on a second domain* is transfer evidence, and
  the registry can record the domains it was graded in. Phase 7 (self-development) is P.6 + P.7 +
  the existing self-build shell. So the pack line is the substrate for 6/7 and orthogonal to 4/5.
- **The second carrier.** `.ycap` compiled capabilities (constitution → LoRA, `CAPABILITIES.md`)
  are real in the substrate and out of scope for the mind until it runs a local weights lane; when
  it does, the rule is already written there: name the adapter per request, never mount state.

---

## F. Metrics (each with the denominator an adversary would accept) and doctrines

| Metric | Numerator ÷ denominator | Reported with |
|---|---|---|
| Lease precision | leases whose pack evidence was *used* ÷ leases granted | used-proxy caveat |
| Attach harm | control answers changed ÷ pinned control questions, library mounted vs empty | per pack, per run |
| Local efficacy (per pack) | graded-good ÷ graded | impressions, used, graded shown; censoring = 1 − graded/used |
| Router agreement | top-1 = label ÷ labelled queries; abstain = label ÷ no-pack queries | labelled set pinned in evals |
| Gap closure | gaps closed at rung r ÷ gaps opened | regret→capability latency by rung |
| Flip audit | flips graded better ÷ flips graded | window |
| Ladder census | packs per `PackStage`, days in `Probation` | — |

Doctrine 1 (reachability): every slice ends with a recorder trace from the **default** path on the
box, not from a test and not from the bounded loop. Doctrine 2 (selective observation): turn
grades exist only when the person responds; every pack rate is shown with its observation rate
and P.2's `P(graded|used)` vs `P(graded|unused)` audit sits above the numbers. Doctrine 3
(independent witness): recorder chain vs SQL recount for local stats; publisher key vs evaluator
key for published efficacy (already the certify design); a pack's own `content_digest` is the
join key so a re-sealed pack can never inherit its predecessor's evidence.

---

## G. Residuals, non-goals, decisions owed

**Decisions for Pranab before P.4:** (1) auto-lease from the router on day one vs standing leases
only until P.3's criterion is met (recommendation: standing only for one week); (2) the per-turn
lease cap (recommendation: 2); (3) where the library lives (`YM_PACK_LIBRARY`, default beside the
state dir) and whether marketplace search (`client/yantrik_pack_client.py:98`) is a P.3 source or
later (recommendation: later — local library first, the marketplace is a catalog source not a
router); (4) whether learned packs may be auto-*certified* by the mind's own evals — the
independence rule says no; a human or a different model authors the exam.

**Honest residuals this line does not fix:** `seal_learned_pack`'s PII gate is a lexicon
(`looks_private`) and the staged rows carry no scope tag check — a sealed learned pack could carry
household-derived craft that names a person; adding a scope-tag refusal on staging is cheap and
belongs in P.7. `pack_context` still reaches the private inference lane (fine — packs are public
text; the leak direction that matters is guarded by the namespace-only seal). The engine's
provenance string (`pack:{name}`) is a substrate ask; until it lands, rid→pack mapping is by
namespace and two packs sharing a namespace are indistinguishable — refuse to lease two packs
with the same namespace in P.4.

**Non-goals, stated so they are not drifted into:** no `mind-packs` crate; no rule engine or
standing-rules layer; no rewrite of `mind-conversation`; no pack taxonomy; no engine mount per
turn; no LLM in the router, the grader, or the lifecycle transitions; no Phase 4/5 machinery;
no marketplace publishing from the mind (a `Proven` pack becomes *publishable* by the operator —
`bundle.py`/`certify.py` remain the publishing path, outside the daemon).
