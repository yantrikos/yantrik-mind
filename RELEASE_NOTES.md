# Release notes — v0.1.0

**yantrik-mind** is a ground-up Rust AI companion built on the YantrikDB typed-memory moat. This is the first public release of the full architecture across 17 `mind-*` crates.

## Highlights

### Typed memory that revises itself
Beliefs are stored as typed nodes in YantrikDB's cognitive graph — not flat text. Each belief carries a Bayesian confidence score, a full evidence trail, and a provenance tag. Positive and negative evidence update confidence via log-odds revision. YantrikDB's contradiction engine detects when two beliefs conflict and surfaces them with severity scores; the companion hedges rather than asserting either side. Consolidation (`consolidate`) distils recent conversation turns into durable typed beliefs and tracked commitments in a single LLM pass — the memory compounds instead of truncating.

### Research that revises prior beliefs (the moat move)
`research and update X` recalls what the mind already believes near a topic, researches it live with cited sources (keyless DuckDuckGo + SSRF-guarded fetch), reconciles findings against priors, asserts new facts, applies negative Bayesian evidence to stale beliefs, and draws contradiction edges — all in one turn. `deep dive on X` fans multiple sub-agents in parallel, then runs an adversarial fact-check pass over the synthesis. Flat-RAG companions have no revisable typed belief to update and no evidence trail to grow.

### Multi-LLM resilient routing
Five OpenAI-compatible providers out of the box: NanoGPT, Ollama Cloud, MiniMax, OpenRouter, Grok. A `ChainBackend` falls over past errors and empty replies automatically. A per-function `Router` lets each role (`chat`, `research`, `util`, `verify`, `code`, `consolidate`) be pinned to a different provider and model via `YM_ROLE_<ROLE>` — without touching code.

### Agentic coder on Claude
`code: X` dispatches Claude Code via MiniMax's Anthropic-compatible endpoint into an isolated scratch directory with a secret-stripped environment. Outward effects still require explicit confirmation.

### Parallel sub-agents
Bounded ReAct loops (step budget enforced) over a granted read-tool subset. `fan_out` runs tasks concurrently. Act-capable agents propose but cannot self-confirm outward actions — confirmation is always routed back to the user.

### Persistent delegation and monitors
Spoken commitments are extracted by consolidation into tracked tasks with due dates. Long-running monitors (`watch my inbox for X`, `watch <url> for X`, `watch my github for X`) persist across restarts via SQLite-backed recipes (idempotent; failed-visibly on recovery, never double-executed).

### NL planner (recipe engine)
Natural-language goals (`plan: X`, `automate X`) are turned into typed recipes with Think, Act, AskUser, WaitUntil, and WaitForCondition steps. AskUser pauses mid-execution and resumes with the next message.

### Code sandbox and semantic skill library
Isolated execution (user namespaces, no network, state dir masked) for Python, shell, and Rust. Green runs can be saved as named skills recalled by meaning (bundled semantic embedder, dim 64) rather than exact name. Skills are auto-quarantined on repeated failures.

### Deterministic, property-tested harm-gate
A single inviolable gate: no LLM in the loop, deny-by-default for governed capabilities, not overridable at runtime. Blocks weapons synthesis, self-harm facilitation, malware deployment, credential exfiltration on any outward channel, writes to protected paths (`.ssh`, `/etc/`, `.env`, …), and mass-targeting (>5 recipients). Two normalisation passes resist obfuscation (whitespace collapse + leet-folded squeeze). Monotonic toward safety: adding text to a denied intent can only deepen the denial. A checked-in adversarial corpus of jailbreaks and injections must stay denied — any regression is a build break. `execute()` re-checks the gate independently of `decide()` for defence in depth.

### Bounded self-build
The mind can open bounded draft pull requests against its own codebase: it compiles first (no build break → no PR), stages the diff, and posts via the GitHub API. `crates/mind-governance` is excluded from self-modification.

---

## Architecture

17 `mind-*` crates with a narrow-waist design around six contracts in `mind-types`: `Event`, `MemoryFacade`, `Candidate`/`ActionIntent`, `HarmGate`, `TurnContext`, `ActionRuntime`. The DAG is acyclic and enforced by review; `mind-core` holds only handles and the main loop — zero domain state.

The `MemoryFacade` runs on a dedicated thread (YantrikDB is `!Sync`) behind an async mpsc+oneshot client, with priority lanes (Interactive ≫ CommitmentDue ≫ Background ≫ Bulk) to bound head-of-line latency. The inference pool uses `spawn_blocking` + a semaphore to keep the async executor free during synchronous LLM calls.

Requires Rust 1.91+, edition 2021, multi-thread tokio. No GPU required for API-backed providers.
