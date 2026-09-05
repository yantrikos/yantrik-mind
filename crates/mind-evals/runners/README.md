# Reading runners (outside the derived fixtures, by design)

`run_all.sealed.sh` — the sequence that graded readings 6 and 7 on staging (`/root/cb2n/run_all.sh`), kept byte-for-byte for reproducibility; its `FIX` points at the `cb2n-ed03255` checkout.

`run_all_local.sh` — the same sequence for the synced fixtures (`FIX=/root/cb2n/fixtures`) with E.CB2-SKIP1: a leg whose receipt is valid **and** has a verdict is skipped; a valid receipt without a verdict (a declared rerun) gets the frozen checker run for it. Used for reading 8 (E.CB2-R8b–R8e). `diff run_all.sealed.sh run_all_local.sh` is the whole difference.
