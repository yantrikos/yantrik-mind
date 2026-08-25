# EX4 review — the two open flags are a production defect, not oracle over-constraint

Reviewed from the Claude Code workspace against commit `e4adffc` (EX4 wip).
Verify everything below yourself before acting on it; the evidence is reproducible.

## Verdict

**Do not classify these as ORACLE_ERROR. Do not relax the fixtures.**

A proposed reading of the two open EX4 cases is that `quiet_hours` and
`user_unavailable` are simultaneously present, the executive picks one
deterministic primary blocker, and the oracle is over-constrained for insisting
on a particular one. That reading is wrong, and one fixture disproves it.

`low_urg_during_meeting` sets `user_receptive = Some(false)` and leaves
`quiet_hours = false`. It has exactly ONE blocker. There is no ambiguity for a
precedence rule to resolve. It returns the correct posture, the correct reason
code and a valid wake condition — and it still fails.

## Evidence

`arbitrate` called in isolation, outside the oracle:

```
low_urg_during_meeting (1 blocker) posture=Monitor  interrupt=true   reason=user_unavailable     wake=1
user_sleeping (2 blockers)         posture=Monitor  interrupt=true   reason=quiet_hours          wake=1
refresh_network_down (control)     posture=Monitor  interrupt=false  reason=resource_unavailable wake=1
```

Both failures are `requires_user_interrupt == true` on a `MONITOR`: a decision
to defer that simultaneously says "interrupt the user". The control line shows
the same function clearing that flag on the resource path.

## Cause

`crates/mind-proactive/src/lib.rs`, the EX4 conditioning pass. The `quiet_hours`
and `user_receptive == Some(false)` branches are entered *because*
`d.requires_user_interrupt` is true, set `d.posture = Posture::Monitor`, and
never clear the flag. The resource-block branch fifteen lines above does clear
it — the function is inconsistent with itself.

Fix: `d.requires_user_interrupt = false;` in each of the two branches.

This is EX4 §13 as written: receptivity is supposed to change delivery strategy.
Here the posture deferred and the delivery strategy did not move.

## Fix the harness that hid it — first

Two things concealed this, and both should be closed before the defect itself,
so the fix is demonstrated rather than assumed.

1. `executive_oracle.rs` prints only `dec4.reason_code` on failure, while `ok4`
   is a conjunction of posture, interrupt flag and wake condition. Given only
   `got "quiet_hours"` / `got "user_unavailable"` as evidence, "the reason codes
   are ambiguous" is a reasonable inference — which is exactly why the
   diagnosis went there. **Print the field that actually mismatched.**
2. The `mind-proactive` unit tests `user_only_action_with_unreceptive_user_defers`
   and `quiet_hours_defer_non_urgent_but_critical_window_overrides` assert
   posture and reason code only. **Add `requires_user_interrupt` to both.**
   Neither the unit tests nor the oracle could catch this as written.

## Then finish EX4

- Keep isolated fixtures pinning each semantic separately:
  `quiet_hours=true, user_receptive=true` and
  `quiet_hours=false, user_receptive=false`.
- Keep ONE overlapping fixture (`user_sleeping_low_urgency`) that accepts either
  deterministic primary blocker, with a wake condition matching the chosen
  reason. Precedence between genuinely simultaneous blockers IS undefined today
  and a single-blocker reason code is the right representation for now — but
  settle it only AFTER the interrupt-flag defect is fixed, or you cannot tell
  whether the fixture passes for the right reason.
- Add the re-arbitration case: quiet hours end, user still unavailable →
  `MONITOR(user_unavailable)`. Waking means reconsider, not act.
- Do NOT add `Vec<Blocker>` or a multi-blocker explanation model yet.
- Run the FULL workspace, not just `mind-proactive`, before marking EX4 KEEP.

## Cleanup: the EX0 block now lies

The gated oracle prints `representable today: 0 | unrepresentable: 37` two lines
above `EX1 10/10 / EX2 6/6 / EX3 6/6 / EX4 2/4`. Those contradict each other in
one report and the false one is on top. The EX0 probe queries an unseeded
`:memory:` store with a needle built from the fixture's own id, so it can only
ever return UNREPRESENTABLE. Delete it or relabel it explicitly as the frozen
EX0 baseline.

## Before authoring more receptivity fixtures

The live receptivity organ changed on 2026-08-25.

`proactive_receptivity()` was being computed on a biased subsample: the old
single-slot resolver kept one outstanding proactive send in a scalar key while
the ledger logged a claim per send, so any beat sent before the previous
resolved was orphaned. 650 of 932 claims were stuck past deadline. The loss was
biased — an ignored send occupies the slot for its full 90-minute window while
an engaged one clears on the next user turn — so failures were preferentially
destroyed and the surviving third read **43% engagement against a true 31%**.

628 orphaned claims have been settled from the transcript record, and the gate
moved off its absolute `>= 0.35` threshold onto one relative to the person's own
baseline (on corrected data, four of five time bins sit at 23–31% and an
absolute 0.35 would have muted the mind everywhere but Night).

Implications for EX4:

- **Do not hard-code receptivity thresholds in fixtures.** Any number near 0.35
  encodes an assumption that has been retired.
- `user_receptive`, `quiet_hours` and `user_in_meeting` all have real
  authoritative organs available today: the world-model receptivity bins,
  the quiet-hours gate in the Telegram loop, and the ICS feed refreshed every
  6h. At least one EX4 case should be sourced from a real organ rather than a
  fixture literal — otherwise ResourceContextView is scored entirely on values
  the test set by hand.

## One standing note on what the score means

`ex1_candidate` and its EX2–EX4 equivalents build each `ExecutiveCandidate` from
a per-fixture `match id`, setting flags like `already_resolved = true` directly.
That is a conclusion about the fixture text, not an observable fact in it — the
derivation that `mind-world` exists to perform is being done by hand in a match
arm. This is a reasonable stage-1: the arbiter's semantics have to be pinned
before perception can be wired into it. But "24/37 representable" measures
whether `arbitrate` maps flags to postures as intended, not whether the system
can decide these situations. Worth labelling as such in the ledger so it is not
read on the same scale as the Phase 3A greens.

## Stop before EX5

The EX5 framing is right — yielding rather than ranking, POSTURE /
EXECUTION SELECTION / ORDER kept distinct, no aging or starvation logic until an
oracle scenario actually demonstrates the failure. Nothing to change there.
Just do not start it until EX4 is KEEP on a clean full-workspace run.
