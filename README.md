# yantrik-mind

**A ground-up Rust AI companion built on the YantrikDB typed-memory moat.**

Most AI assistants store memory as flat text and retrieve it with keyword or vector search. yantrik-mind doesn't. It stores *beliefs* — typed, revisable nodes with Bayesian confidence scores, evidence trails, and contradiction edges — in YantrikDB's cognitive graph. Every conversation makes the memory smarter, not just longer.

---

## Why it's different

| Flat-RAG companions | yantrik-mind |
|---|---|
| Memory = markdown + vector index | Memory = typed cognitive graph (beliefs, evidence, contradictions) |
| Recall = approximate keyword/embedding search | Recall = semantic search blended with a confidence prior |
| Old context is truncated or summarised | Consolidation distils turns into durable typed beliefs that never expire |
| Research answers a question then forgets | Research recalls prior beliefs, revises them with live evidence (Bayesian), and flags contradictions |
| No notion of confidence | Every belief carries a calibrated confidence score that updates as evidence accumulates |
| No contradiction detection | YantrikDB's contradiction engine detects conflicting beliefs; the companion asks rather than asserting either side |

**Observed in the wild (2026-06-27, live system):**
```
:remember + the latest stable Rust version is 1.70
→ confidence now 0.81, 1 evidence item

research and update the latest stable Rust compiler version
→ revised: "the latest stable Rust version is 1.70" → "The latest stable Rust version is 1.96.0."
   prior confidence dropped 0.81 → 0.50 (Bayesian, evidence trail grew 1→2)
   contradiction detected: old ⟂ new
   sources: github.com/rust-lang/rust/releases, releases.rs, doc.rust-lang.org/stable/releases.html …
```

This is the move flat-RAG companions structurally cannot make: there is no revisable typed belief to update, no evidence trail to grow, no contradiction engine to fire.

---

## Features

### Typed memory that compounds
- **Beliefs** are keyed by their proposition. Asserting evidence raises or lowers confidence via Bayesian log-odds update; contradicting beliefs get a `contradicts` edge drawn between them.
- **Contradiction detection** runs against the full cognitive graph. Conflicting beliefs are surfaced with severity scores; the companion hedges rather than asserting either side.
- **Evidence trails**: every belief records its provenance (`told`, `inferred`, `extracted`, `consolidated`) and every piece of evidence that shaped its confidence.
- **Consolidation** (`consolidate` / `:consolidate`): distils recent conversation turns into durable typed beliefs and future commitments in a single LLM pass. Runs on a cursor so it never re-chews the same turns. The memory grows and compounds; it doesn't summarise and shrink.
- **Belief inspection** (`:beliefs [query]`): lists stored beliefs ranked by confidence, with optional semantic filtering — see what the mind actually knows and how sure it is.
- **Semantic recall** (YantrikDB 0.9.0, bundled model2vec, dim 64): paraphrases retrieve the right belief with no shared keywords — no external server, no download.

### Research that revises its own beliefs
- `research and update X` / `update your knowledge on X`: recalls prior beliefs near the topic, researches live with citations (keyless DuckDuckGo + SSRF-guarded fetch), reconciles findings against priors, asserts new facts, applies negative Bayesian evidence to stale beliefs, draws contradiction edges.
- `deep dive on X` / `thoroughly research X`: decomposes the topic into focused sub-questions, fans out to parallel sub-agents, synthesises, then runs an adversarial fact-check pass to flag unsupported claims.

### Multi-LLM routing
Providers: NanoGPT, Ollama Cloud, MiniMax, OpenRouter, Grok — all OpenAI-compatible. Adding a provider is config-only. Features:
- **Resilient chain**: errors and empty replies fall over to the next link automatically.
- **Per-function routing**: pin each role (`chat`, `research`, `util`, `verify`, `code`, `consolidate`) to a different provider and model via `YM_ROLE_<ROLE>`.

### Agentic coder on Claude
`code: X` / `write a script to X` dispatches Claude Code (driven by MiniMax's Anthropic-compatible endpoint) in an isolated scratch directory with a secret-stripped child environment. The coder synthesises real code and reports the result; outward effects still require your confirmation.

### Parallel sub-agents
A bounded ReAct loop (think → call tool → observe → … → finish) over a granted read-tool subset. The step budget prevents runaway loops. `fan_out` runs many tasks concurrently via the inference pool. Act-capable agents can only *propose* outward actions — they cannot self-confirm anything that requires confirmation.

### Commitments and persistent delegation
- Spoken commitments (`"I'll do X by tomorrow"`, `"remind me to X"`) are extracted by consolidation and become tracked tasks with due dates.
- **Monitors** set up long-running WaitForCondition recipes: `watch my inbox for X`, `watch my github for X`, `watch <url> for X`. They poll in the background and notify you when the condition is met.
- Recipes survive restarts (SQLite-backed, idempotent — a non-idempotent step is failed-visibly on recovery, never double-executed).

### NL planner
`plan: X` / `automate X` / `set up a task to X` → the mind authors and runs a recipe from a natural-language goal. Recipes support Think (LLM drafting), Act (gated outward action), AskUser (pause and resume), WaitUntil (time-based), and WaitForCondition (poll until true).

### Code sandbox and skill library
- `run python: …` / `run shell: …` / `run rust: …` executes in an isolated sandbox (user namespaces, no network) that masks the mind's own state directory.
- `save that as skill <name>`: banks a green run as a semantic skill. Recalled by meaning, not just name. Auto-quarantined if success rate drops below 50% over ≥4 runs.
- Remote execution: `worker python: …` / `worker shell: …` fans work out to a pool of SSH workers and returns results.

### Deterministic, property-tested harm-gate
One inviolable rule, deterministic (no LLM in the loop — an LLM-evaluated gate is injectable), deny-by-default for governed capabilities, not overridable at runtime:
- **Categorical denials**: weapons synthesis instructions, self-harm facilitation, malware deployment.
- **Protected paths**: `.ssh`, `.env`, `/etc/`, `/proc/`, credentials, key material.
- **Secret exfiltration** blocked on every outward channel (email, network, filesystem writes).
- **Mass-targeting wall**: messages to more than 5 recipients denied.
- **Obfuscation-resistant**: two normalisation passes — whitespace/zero-width collapse *and* leet-folded squeeze — so `b0mb`, `b-o-m-b`, `b​omb` all still match.
- **Monotonic toward safety**: adding text to a denied intent can only deepen the denial, never open it. Prompt injection cannot talk the gate open.
- **Defence in depth**: `execute()` re-checks the gate independently of `decide()`, so a bypassed `decide` call cannot sneak a harmful action through.
- Adversarial corpus of known jailbreaks and injections is checked in and must stay denied — any regression is a build break.

### Bounded self-improvement
The mind can open bounded self-build pull requests against its own codebase: it compiles the change (no build break → no PR), stages the diff, and posts a draft PR via the GitHub API. Harm-gate carve-outs prevent it from touching its own governance code (`crates/mind-governance`). The `:beliefs` command was itself authored and merged by this self-build pipeline.

### Always-on, Telegram-native
Runs as a systemd service on any Linux host (no GPU or local model required for API-backed providers). The Telegram bot interface maps `/command` slash syntax to the same REPL commands available locally. Quiet hours are configurable.

---

## Quickstart

See [BUILD.md](BUILD.md) for prerequisites, build steps, and the full crate architecture.

See [deploy/DEPLOY.md](deploy/DEPLOY.md) for running the mind always-on as a Linux service.

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines, the testing conventions, and the harm-gate rules.

```sh
# build (Rust 1.91+, edition 2021)
cargo build

# run the REPL (needs at least one provider key set — see BUILD.md)
cargo run -p mind-core

# in-REPL commands
:remember + Pranab prefers terse replies   # assert a belief
:beliefs                                    # list all beliefs ranked by confidence
:beliefs rust                              # filter to beliefs about Rust
:conflicts                                  # list open contradictions
:explain <belief statement>                 # show evidence trail + confidence
:tasks                                      # open commitment ledger
:consolidate                                # distil recent turns into typed beliefs
:quit
```

---

## Contributing

See [BUILD.md](BUILD.md) for the architecture, module map, concurrency rules, and testing conventions. See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

The harm-gate adversarial corpus (`crates/mind-governance`) must never be weakened — any new denial test passing `Allow` is a build break.

Do not submit changes to `crates/mind-governance` without a paired adversarial corpus extension and a passing `deny_is_stable_under_perturbation` run.
