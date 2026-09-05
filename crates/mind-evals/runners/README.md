# Reading runners (outside the derived fixtures, by design)

`run_all.sealed.sh` — the sequence that graded readings 6 and 7 on staging (`/root/cb2n/run_all.sh`), kept byte-for-byte for reproducibility; its `FIX` points at the `cb2n-ed03255` checkout.

`run_all_local.sh` — the same sequence for the synced fixtures (`FIX=/root/cb2n/fixtures`) with E.CB2-SKIP1: a leg whose receipt is valid **and** has a verdict is skipped; a valid receipt without a verdict (a declared rerun) gets the frozen checker run for it. Used for reading 8 (E.CB2-R8b–R8e). `diff run_all.sealed.sh run_all_local.sh` is the whole difference.

## Syncing the fixtures to the box (E.CB2-NS1, 2026-09-05)

The box's `/root/cb2n/fixtures` is the committed `fixtures/cb2n` tree plus one thing the repository never holds: the shipped Hermes archive under `docker/`. So a sync is an OVERLAY, never a replace:

1. `bash fixtures/cb2n/scratch/rederive.sh` must print "re-derives exactly" first.
2. Locally: `tar --force-local -czf cb2n-tree.tgz --exclude=__pycache__ -C crates/mind-evals/fixtures/cb2n .` and `scp` it to `/tmp` on the box.
3. On the box, with nothing attached to `cb2net`/`cb2egress`: `cp -a /root/cb2n/fixtures /root/cb2n/fixtures.bak.$(date +%s)`, then `tar --no-same-owner -xzf /tmp/cb2n-tree.tgz -C /root/cb2n/fixtures`, then `echo commit=<sha> > /root/cb2n/SOURCE` (the runner prints it in `sequence.log`).
4. `FIX=/root/cb2n/fixtures bash /root/cb2n/fixtures/selftest/selftest.sh` — every line "agree".
5. Before the first graded run after any harness change: one UNGRADED leg into its own out dir (`CB2_PROFILE=… CB2_OUT=/root/cb2n/out-<name>-preflight`, `net/cb2net.sh` then `run/mind_leg.sh T1 $OUT`) and read the receipt and the Mind's container log. Four readings died from harness defects whose first run was a graded one.
