# Yantrik Mind — Build Doc

The most powerful companion we can build, ground-up, on the **YantrikDB typed-memory moat**. This is the working build doc; the full rationale is in `C:/Users/sync/.claude/plans/jazzy-seeking-porcupine.md`, and the task board is **saga project `yantrik-mind` (id 3)**, epics 43–49.

## The bet
Everyone else (OpenClaw, hermes) stores memory as flat markdown + vector/FTS RAG. We build on YantrikDB's **typed cognitive graph** — beliefs with Bayesian revision, contradiction detection, consolidation, working-set attention, a shared identity chain — so the companion's memory *thinks*. The job is to **cash in a moat that's already built** (in `yantrikdb-core`) and wrap it in a clean architecture, taking the proven transport/UX patterns from OpenClaw/hermes. Telos: ASI who is Pranab's friend + extension, governed by ONE inviolable harm-gate ("no rope as long as it's not harming anyone").

## Quick start
```
cd c:/Users/sync/codes/yantrik-mind
cargo build        # workspace skeleton (15 mind-* crates) compiles green
cargo test
```
Rust 1.91 (edition 2021), multi-thread tokio. Reuse crates are path-deps to `../yantrik-companion/crates/*`; `[patch]` redirects the `yantrik-ml.git`/`yantrikdb.git` URLs to those local crates (yantrikdb-core git-deps yantrik-ml).

## Reuse, don't rewrite
| Concern | Crate (path dep) | Use |
|---|---|---|
| Memory substrate | `yantrikdb-core` | the typed cognitive engine, behind a facade |
| Inference | `yantrik-ml` | LLMBackend/Embedder + ProviderRegistry + model-adaptive profiles |
| System events | `yantrik-os` | SystemObserver/EventBus (crossbeam, sync, **lossy**) |
| Chat transports | `yantrik-chat` | ChatProvider + history store |
Copy+adapt (decouple from the `CompanionService` god-object): SilencePolicy, TrustModel, Bond, cortex pulse/entity/baseline/pattern, a curated instinct subset. **Author new**: harm-gate, proactive pipeline, orchestration. Do NOT depend on the companion-* god-object crates.

## Architecture — the narrow waist (`mind-types`, no logic)
Every module talks ONLY through these:
1. `Event` — normalized envelope (BodyForAgent / CommandBody / RawBody + source + trace_id + ts).
2. `MemoryFacade` (async, Send+Sync) — firewall over the **`!Sync`** YantrikDB.
3. `Candidate` (+`ActionIntent`) — the unit of "something to do/say"; one 7-axis scoring path.
4. `HarmGate` (leaf) — `evaluate(&ActionIntent)->Decision`. Deterministic, no-LLM, deny-by-default, un-rewritable.
5. `TurnContext` — per-turn value threaded perceive→cognize→decide→act→learn.
6. `ActionRuntime` — the Effect boundary (decide/execute, capabilities, confirmation, idempotency, receipts).
Plus a `Clock` seam (deterministic time). Reuse yantrik-ml `ChatMessage`/`GenerationConfig`/`LLMResponse` as the inference waist.

**DAG (acyclic):** governance(leaf) ← memory ← {perception, inference} ← {cortex, instincts, tools} ← {proactive, conversation, evolution} ← core. Back-edges are review-blocks.

## Concurrency rules (non-negotiable — forced by ground truth)
- **Memory = single-owner actor.** YantrikDB is `!Sync` (RefCell + rusqlite Connection). One dedicated thread owns it; the facade is an async mpsc+oneshot client. **Mitigate head-of-line blocking now:** priority lanes (Interactive≫CommitmentDue≫Background≫Bulk); no heavy work inside the actor (snapshot→compute outside→commit); bounded requests; read-cache outside; **ban the `memory→inference→memory` cycle** (legal: `cortex → memory snapshot → inference → memory commit`). Instrument queue depth.
- **Inference = bounded blocking pool + semaphore** (permits=1 for a local single-model backend); all calls via `spawn_blocking`. Cost/latency governance lives in this queue.
- **Perception = one async bus** fed by a bridge task draining yantrik-os's crossbeam; delivery-critical events on a separate at-least-once mpsc (off the lossy broadcast).
- **`mind-core` holds only `Arc<dyn Trait>` handles + the loop — zero domain state** (≤8 fields). The mechanical guard against a new god-object.
- `mind-memory` is the **sole writer** to the cognitive graph; everyone else proposes.
- Consolidation is **provenance-preserving** — derived beliefs never erase raw evidence.

## Module map (crates/)
`mind-types` · `mind-observability` (trace log + replay) · `mind-evals` (golden demos, harm corpus) · `mind-governance` (harm-gate, bond, silence, trust) · `mind-memory` (actor+facade, working-set, consolidation, privacy) · `mind-inference` (pool, health, fallback, cost, cache, prompt) · `mind-perception` (bus, bridge, inbound, scheduler) · `mind-cortex` (thin coordinator, proposes) · `mind-instincts` (Instinct trait + InstinctContext + curiosity) · `mind-proactive` (Detect→Generate→Score→Deliver + commitments) · `mind-conversation` (channels, 3-tier prompt, consolidation-as-context, mirror, untrusted-wrap) · `mind-tools` (registry, MCP, ActionRuntime exec) · `mind-evolution` (thin calibration) · `mind-identity` (profile/self-model surface) · `mind-core` (orchestrator + binary).

## Build order (saga epics 43–49)
- **Phase 0 (43):** scaffold ✓ · BUILD.md+saga ✓ · `mind-types` contracts+Clock · observability/evals skeletons · **Spike A** memory actor · **Spike B** inference pool · **harm-gate stub** · **Spike C** sync-serialization check.
- **Phase 1 (44):** `mind-memory` — the moat (actor, facade, working-set, consolidation, privacy, DB-style tests).
- **Phase 2 (45):** `mind-inference` robustness + `mind-conversation` MVP → **E2E demo: contradiction-aware grounded chat** (the visible milestone).
- **Phase 3 (46):** perception bus + cortex + full harm-gate (property + adversarial corpus) + governance walls.
- **Phase 4 (47):** instincts + proactive pipeline + commitment ledger.
- **Phase 5 (48):** tools + ActionRuntime (gated execution).
- **v1 = Phases 0–5.** **v2 (49):** evolution loop, central identity-chain sync, voice/robustness.

## Testing
Injectable `ScriptedLLM` (LLMBackend) → ~90% of orchestration is deterministic. Golden-transcript replay in CI. Memory tested like a DB (`:memory:`): belief posteriors, contradiction, consolidation, recall ranking. **Harm-gate = the most important suite**: property tests + a checked-in jailbreak/injection corpus that MUST stay denied (failing any = build break). Actor concurrency stress (no deadlock/lost-writes, bounded queue). Contract tests on each waist type.

## Conventions
- Edition 2021, `version.workspace`/`edition.workspace`. Common deps in root `[workspace.dependencies]`.
- New crate that must reach memory → depend on `mind-types` and take a `MemoryFacade`, never `yantrikdb-core` directly (except inside `mind-memory`).
- Keep the DAG acyclic; keep `mind-core` stateless.
