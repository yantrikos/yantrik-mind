# E.CB2 — three-task exploratory bakeoff, Mind vs Hermes (frozen harness, v2)

Everything a run may do is fixed in `MANIFEST.json` before either system starts, and every
execution — generation and artifact checks — happens on one host inside containment: a Docker
network whose only egress is the owned model gateway (`net/cb2net.sh` proves it with a probe),
read-only images, unprivileged users, resource limits, one writable task directory. Wall time
is recorded per leg and never used as a ranking.

Order, all on the staging box as root:

1. `docker build -t cb2-hermes -f docker/hermes.Dockerfile .` and
   `docker build -t cb2-check -f docker/check.Dockerfile .` (network needed at build time only).
2. `bash net/cb2net.sh` — must end with `gateway ok / internet blocked / dns blocked`.
3. `bash selftest/selftest.sh` — six fixtures must agree with their expected verdicts.
4. Per task T1, T2, T3: `python3 run/mind_leg.py <T> <out>` (fresh scratch instance, cancel on
   cap or timeout, teardown receipt) and `bash run/hermes_leg.sh <T> <out>` (fresh home,
   max_turns 8, contained). Receipts are counts only; artifacts are read-only and tree-hashed.
5. `bash run/check.sh <t> <artifact> <verdict.json>` on each artifact (writable copy, contained).
6. Hashes and verdicts to the reviewer; artifacts shuffled A/B for the blind viewer test.

Artifacts, receipts and homes never enter the repository. The preregistration, the stated
prior, and the results live in `docs/PHASE2_EXPERIMENT_LEDGER.md`.
