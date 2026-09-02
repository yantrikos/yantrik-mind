# E.CB2 — three-task exploratory bakeoff, Mind vs Hermes (frozen harness, v3)

Everything a run may do is fixed in `MANIFEST.json` before either system starts, and every
execution — both generation legs and every artifact check — happens on one host inside
containment: an internal Docker network with no egress, a per-run request-counting proxy as
the only path to the one model (it refuses the ninth request), host services blocked from the
bridges, read-only images, unprivileged users, resource limits, one writable task directory.
`net/cb2net.sh` proves the containment with two probes before anything runs. Wall time is
recorded per leg and never used as a ranking.

Order, all on the staging box as root, from this directory:

1. `docker build -t cb2-proxy -f docker/proxy.Dockerfile .`, `docker build -t cb2-hermes -f
   docker/hermes.Dockerfile .` (needs `docker/hermes-3ce1cf2.tar.gz`, the git archive of the
   pinned commit, in the build context), `docker build -t cb2-mind -f docker/mind.Dockerfile .`,
   `docker build -t cb2-check -f docker/check.Dockerfile .`.
2. `bash net/cb2net.sh` — must end with `containment proven`.
3. `bash selftest/selftest.sh` — all six fixtures must `agree` with their expected failed sets.
4. Per task T1, T2, T3: `bash run/mind_leg.sh <T> <out>` and `bash run/hermes_leg.sh <T> <out>`
   (each brings up its own proxy, runs one invocation, tears down with a receipt).
5. `bash run/check.sh <t> <out>/artifacts/<system>_<T> <verdict.json> <excerpts.txt>` on each
   artifact (writable copy, no network).
6. Hashes and verdicts to the reviewer; artifacts shuffled A/B for the blind viewer test.

Artifacts, receipts, raw logs, homes and state volumes never enter the repository. The
preregistration, the stated prior and the results live in `docs/PHASE2_EXPERIMENT_LEDGER.md`.
