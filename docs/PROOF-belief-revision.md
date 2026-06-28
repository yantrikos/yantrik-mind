# Proof: online belief revision from live research

**What it shows:** yantrik-mind doesn't just *answer* a research question — it **recalls what it already
believed, researches live with citations, and revises the typed belief**, weakening the stale one
(Bayesian, with a growing evidence trail) and surfacing the contradiction. This is the YantrikDB-moat
move that flat-markdown + RAG companions (hermes-agent, OpenClaw) structurally cannot do: they have no
revisable typed belief to update, no contradiction engine, no evidence trail.

**Run:** live on CT173, real backend (NanoGPT deepseek-v4-pro), real web research (keyless DuckDuckGo
+ fetch), dim-64 semantic memory. 2026-06-27.

```
:remember + the latest stable Rust version is 1.70
→ remembered: the latest stable Rust version is 1.70 (confidence now 0.81)

:explain the latest stable Rust version is 1.70            # BEFORE
→ confidence 0.81, 1 evidence item(s), provenance Told

research and update the latest stable Rust compiler version
→ Researched "the latest stable Rust compiler version" and updated my memory:
  📚 learned: The latest stable Rust compiler version is 1.96.0.
  🔄 revised: "the latest stable Rust version is 1.70" → "The latest stable Rust version is 1.96.0."
  Sources:
   • https://github.com/rust-lang/rust/releases
   • https://releases.rs/
   • https://doc.rust-lang.org/stable/releases.html
   • https://blog.rust-lang.org/releases/
   • https://www.rust.dev/updates
   • https://versionlog.com/rust/

:explain the latest stable Rust version is 1.70            # AFTER
→ confidence 0.50, 2 evidence item(s), provenance Told     # revised DOWN; evidence trail grew 1→2

:conflicts
→ "The latest stable Rust version is 1.96.0." ⟂ "the latest stable Rust version is 1.70" (severity 0.28)
```

**The five differentiators, all in one run:**
1. **Recall** — surfaced the prior belief semantically before researching.
2. **Cited research** — 6 real source URLs (anti-confabulation: an adversarial-verify pass gates claims).
3. **Revision** — the stale belief's confidence dropped 0.81 → 0.50 via negative evidence (real
   Bayesian update, not deletion), and its evidence trail grew from 1 to 2 items.
4. **Correction asserted** — the new fact ("…1.96.0") is now a first-class, recallable belief.
5. **Contradiction detected** — the YantrikDB contradiction engine raised the old⟂new conflict.

**How it works:** `ConversationEngine::research_revise(topic)` — recall priors → `SubAgent.run` (live,
cited) → LLM reconcile (priors vs findings → `{facts, revisions}`) → for each revision: assert the
corrected belief, assert negative evidence on the stale one, draw a `contradicts` edge. Trigger:
"research and update X" / "update your knowledge on X".
