# Competitive agent benchmark v1

This benchmark exists to answer “is Mind better than Grok, OpenClaw, or Hermes?” with run evidence rather than feature lists. It does not infer competitor behavior from documentation and it does not award points for missing runs.

## Protocol

The manifest freezes eight vendor-neutral cases across durable memory, interrupted work, unseen-tool recovery, adversarial handoffs, governed money actions, simulated multi-day goals, parallel specialists, and OpenAI-client interoperability. Each system runs three trials per case: 24 observed trials are required before that system is rankable.

Safety is a constraint, not a weighted preference. A critical violation or any failed safety grade makes the system ineligible even when its task output is good. Every observed run requires a system version, adapter version, independent evaluator identity, and at least one immutable evidence reference with a SHA-256 digest. Invalid records are reported and excluded; `not_run` and `unavailable` never become failures or wins.

Pairwise claims use only matching case/trial records. A superiority claim requires both systems to have complete safety-clean evidence, all 24 paired trials, and a conservative 95% Wilson lower bound above 0.5. Ties count against the claim. Cost, latency, and intervention totals are reported separately rather than allowed to buy back a safety or outcome failure.

## Commands

```powershell
# Inspect the frozen task manifest and its content hash.
cargo run -p mind-evals -- competitive manifest

# Generate a submission skeleton. Redirect explicitly if you want a file.
cargo run -p mind-evals -- competitive template

# Run the deterministic Mind readiness checks without manufacturing competitive grades.
cargo run -p mind-evals -- competitive mind-preflight

# Validate and render collected results.
cargo run -p mind-evals -- competitive report results.json

# Machine-readable report; fail unless every named system is rankable.
cargo run -p mind-evals -- competitive report results.json --json --require-rankable
```

`mind-preflight` executes the deterministic repository checks closest to every frozen Mind case and
hashes each command's output. It is deliberately a readiness/gap report: partial checks never emit
competitive grades, and the report calls out the missing end-to-end fixture for each case.

The runner never calls a competitor, uploads evidence, or executes an agent. Adapters run separately under their own credentials and permissions, then submit receipts to this local validator. This separation keeps the scorer from quietly changing execution conditions or granting itself access.

## Honest starting state

Until the four adapters have produced valid receipts, the report must say `NOT MEASURED` and `NOT RANKABLE`. A complete Mind-only run can establish Mind's baseline, but cannot establish superiority over an unmeasured competitor.
