# E.CB2 — three-task exploratory bakeoff, Mind vs Hermes (frozen harness)

Everything a run may do is fixed in `MANIFEST.json` before either system starts: the two systems
and their exact configuration, the single model and endpoint, the caps (one invocation per
system per task, 1800 s, 8 model requests, no manual edits, no downloads, network limited to
loopback and the owned endpoint), the three briefs (hashed), the T1 seed and its 14-bin UTC
semantics, the output-directory contract and the canonical tree hash, the checkers, the fixed
browser, the cost read per system, and the environment confound (different hosts: wall time is
recorded, never compared). `docs/PHASE2_EXPERIMENT_LEDGER.md` carries the preregistration row,
the stated prior, and the results.

Run order: `run/scratch_up.sh` on the staging box (scratch instance, fresh state, its own ports),
`run/mind_leg.py <T>` ON the box (console API is loopback-only), `run/hermes_leg.sh <T>` on the
workstation with `CB2_OUT` set to a run root outside the repo, then `checks/check_web.mjs` /
`checks/check_t3.py` on the frozen artifact copies, `tools/tree_hash.py` on each, and
`run/scratch_down.sh` for the teardown receipt. Artifacts and receipts never enter the repo;
only counts, hashes and the grader's verdicts go on the ledger.
