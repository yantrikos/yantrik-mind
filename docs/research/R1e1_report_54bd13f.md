# R1 — Multidimensional Grammar (MDG): report

Status: PARTIAL. Written 2026-10-05. Prior art incomplete; experiment designed, not run.

## 1. Prior art
NOT COMPLETE. Searches for "MDG" returned unrelated results (MDG Financial, Millennium
Development Goals). Relevant literature (AMR/UMR, latent reasoning such as Coconut,
graph-to-text) has not yet been verified against the spec's claims. No citations are
asserted here, because a claim without evidence counts against the report.

## 2. Experiment design (falsifiable)
Hypothesis (spec sections 25, 32): a model working on MDG solves the same tasks with
substantially fewer sequence positions than natural language.

Measure: sequence positions (tokens) to solve a fixed task set, MDG vs natural language,
at matched accuracy.

Support: MDG reaches equal task accuracy with materially fewer positions.

Kill condition: MDG needs the same or more positions at equal accuracy, OR its accuracy
collapses when position count is held equal to the natural-language baseline.

Confound controls (ways the experiment could flatter MDG):
- Unequal task difficulty between arms -> use identical underlying tasks.
- Tokenizer advantage -> count positions, not bytes; report both.
- Training-data advantage -> matched model, matched fine-tuning budget.
- Selective reporting -> pre-register the task set and the accuracy threshold.

## 3. Results
NONE. Not run. CPU-only machine, no cloud model wired for this task this turn.

## 4. Sources
None verified yet.
