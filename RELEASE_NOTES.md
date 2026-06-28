# Release notes — v0.1.1

**yantrik-mind** is a ground-up Rust AI companion built on the YantrikDB typed-memory moat — typed beliefs with Bayesian revision, contradiction detection, research that revises its own memory, multi-LLM routing, persistent delegation, an NL planner, a parallel worker pool, a code sandbox, and a deterministic harm-gate — all in one binary. This release adds belief introspection to the REPL and was itself opened as a bounded self-build PR.

## What's new in v0.1.1

### `:beliefs [query]` — inspect the cognitive graph from the REPL

The new `:beliefs` REPL command (and its Telegram `/beliefs` equivalent) lets you see exactly what the mind knows and how confident it is:

```
:beliefs
• The latest stable Rust version is 1.96.0. (0.81)
• Pranab prefers terse replies (0.74)
• ...

:beliefs rust
• The latest stable Rust version is 1.96.0. (0.81)
```

- Lists the top 10 stored beliefs ranked by Bayesian confidence score, highest first.
- Accepts an optional query argument for semantic filtering — paraphrases work, not just keywords.
- Empty store returns `(no beliefs stored)` rather than a blank screen.
- This command was authored and merged by yantrik-mind's own bounded self-build pipeline (PR #2), making it the first capability the system added to itself.

---

## Full capability summary

### Typed memory that compounds
Beliefs are stored as typed nodes in YantrikDB's cognitive graph — not flat text. Each belief carries a Bayesian confidence score, a full evidence trail (`told` / `inferred` / `extracted` / `consolidated`), and contradiction edges to conflicting beliefs. Positive and negative evidence update confidence via log-odds revision. Consolidation (`consolidate` / `:consolidate`) distils recent conversation turns into durable typed beliefs and tracked commitments in a single LLM pass — the memory compounds instead of truncating. Semantic recall (bundled model2vec, dim 64, no external server) retrieves the right belief from a paraphrase with no shared keywords.

### Research that revises prior beliefs
`research and update X` recalls what the mind already believes near a topic, researches it live with cited sources (keyless DuckDuckGo + SSRF-guarded fetch), reconciles findings against priors, asserts new facts, applies negative Bayesian evidence to stale beliefs, and draws contradiction edges — all in one turn. `deep dive on X` fans multiple sub-agents in parallel, then runs an adversarial fact-check pass over the synthesis. Flat-RAG companions have no revisable typed belief to update and no evidence trail to grow.

### Multi-LLM resilient routing
Five OpenAI-compatible providers out of the box: NanoGPT, Ollama Cloud, MiniMax, OpenRouter, Grok. A `ChainBackend` falls over past errors and empty replies automatically. A per-function `Router` lets each role (`chat`, `research`, `util`, `verify`, `code`, `consolidate`) be pinned to a different provider and model via `YM_ROLE_<ROLE>` — without touching code.

### Agentic coder on Claude
`code: X` dispatches Claude Code via MiniMax's Anthropic-compatible endpoint into an isolated scratch directory with a secret-stripped child environment. Outward effects still require explicit confirmation.

### Parallel sub-agents
Bounded ReAct loops (step budget enforced) over a granted read-tool subset. `fan_out` runs tasks concurrently via the inference pool. Act-capable agents propose but cannot self-confirm outward actions — confirmation is always routed back to the user.

### Persistent delegation and monitors
Spoken commitments are extracted by consolidation into tracked tasks with due dates. Long-running monitors (`watch my inbox for X`, `watch <url> for X`, `watch my github for X`) persist across restarts via SQLite-backed recipes (idempotent; failed-visibly on recovery, never double-executed).

### NL planner (recipe engine)
Natural-language goals (`plan: X`, `automate X`) are turned into typed recipes with Think, Act, AskUser, WaitUntil, and WaitForCondition steps. AskUser pauses mid-execution and resumes with the next message.

### Code sandbox and semantic skill library
Isolated execution (user namespaces, no network, state dir masked) for Python, shell, and Rust. Green runs can be saved as named skills recalled by meaning rather than exact name. Skills are auto-quarantined on repeated failures. Remote execution (`worker python: …` / `worker shell: …`) fans work out to a pool of SSH workers.

### Deterministic, property-tested harm-gate
A single inviolable gate: no LLM in the loop, deny-by-default for governed capabilities, not overridable at runtime. Blocks weapons synthesis instructions, self-harm facilitation, malware deployment, credential exfiltration on any outward channel, writes to protected paths (`.ssh`, `/etc/`, `.env`, …), and mass-targeting (>5 recipients). Two normalisation passes resist obfuscation (whitespace/zero-width collapse + leet-folded squeeze). Monotonic toward safety: adding text to a denied intent can only deepen the denial. A checked-in adversarial corpus of jailbreaks and injections must stay denied — any regression is a build break. `execute()` re-checks the gate independently of `decide()` for defence in depth.

### Bounded self-build
The mind can open bounded draft pull requests against its own codebase: it compiles first (no build break → no PR), stages the diff, and posts via the GitHub API. `crates/mind-governance` is excluded from self-modification. v0.1.1 itself is the first release to include a capability shipped this way.

---

## Architecture

17 `mind-*` crates with a narrow-waist design around six contracts in `mind-types`: `Event`, `MemoryFacade`, `Candidate`/`ActionIntent`, `HarmGate`, `TurnContext`, `ActionRuntime`. The DAG is acyclic and enforced by review; `mind-core` holds only handles and the main loop — zero domain state.

The `MemoryFacade` runs on a dedicated thread (YantrikDB is `!Sync`) behind an async mpsc+oneshot client, with priority lanes (Interactive ≫ CommitmentDue ≫ Background ≫ Bulk) to bound head-of-line latency. The inference pool uses `spawn_blocking` + a semaphore to keep the async executor free during synchronous LLM calls.

Requires Rust 1.91+, edition 2021, multi-thread tokio. No GPU required for API-backed providers.

---

## Upgrade from v0.1.0

No schema migrations required. Pull and rebuild:

```sh
git pull
cargo build
```

The `:beliefs` command is available immediately after rebuild.
