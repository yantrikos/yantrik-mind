# Proposal — a third registration kind for the control endpoint

**Status: proposal only. Nothing implemented.** It changes how the mind decides whether anyone is
listening, which is not mine to decide. Written to make the decision cheap to take or reject.

## The problem, in one sentence

The ctl `/cli` endpoint dispatches through `cli_dispatch` — the **user** path — so a diagnostic
command is recorded as the owner doing something.

Two harms follow, both observed on staging on 2026-09-04:

1. **The belief store learns from diagnostics.** `ym patterns` produced *"Pranab's interest in `why
   roles verify` is likely driven by a need to understand the security or permission boundaries of
   the AI systems he is building…"* — from a command **I** ran while debugging. The pattern loop is
   not at fault: it requires two unique cited facts, validates citation indices, checks confidence,
   and drops free-association. It reasoned correctly over polluted inputs.
2. **Phase G, Phase D and the knock gate are unmeasurable.** `idle_ok = present && !quiet_now &&
   idle_stretch`. Every ctl command moves the activity clock, so `idle_stretch` never matures while
   anyone is driving; and ctl marks no view, so `present` is false on a headless box regardless.

## Why the obvious fix is wrong

Point ctl at `cli_dispatch_view`. I proposed this and **retract it.** A machine view does two
things, and they come apart precisely here:

| property | ctl needs it? |
| --- | --- |
| does not move the activity clock | **yes** — a diagnostic is not the owner acting |
| marks presence (`last_view_ms`) | **no** |

Presence is defined in `delivery.rs` as *"someone can see a line NOW: a pinned chat, or an open
cockpit… a queue nobody is looking at is not presence."* An open cockpit is a human watching — that
is deliberate and correct. **A script polling `loops_json` is not.** Wiring ctl through the view path
would teach the mind that an unattended poller is an audience, and it would knock — speak unprompted
— to nobody. That is worse than the pollution it fixes, and is the exact failure `has_presence`
exists to prevent.

## The proposal

`TurnExclusion::register` currently takes `moves_clock: bool`, which forces the two properties to
travel together. Replace the boolean with an explicit kind:

```rust
pub enum Registration {
    /// A person acted. Moves the activity clock. (`begin_turn_on`)
    UserTurn,
    /// The cockpit polled: a human is watching. Marks presence, leaves the clock. (`begin_view_on`)
    CockpitView,
    /// An operator tool read something. Neither. NEW.
    ToolRead,
}
```

`ToolRead` still counts as an active turn — the DMN/engagement exclusion must hold, or a diagnostic
could interleave with a background pass — but it touches **neither** `last_user_activity_ms` nor
`last_view_ms`.

Then ctl dispatches allowlisted read-only verbs as `ToolRead`. The allowlist already exists and is
exact (`is_machine_view`: `jobs json`, `orders`, `horizons_json`, `skills_json`, `claims_json`,
`loops_json`, `horizon_history_json <id>`, `chains_json`), and its own rule is the safety property:
anything not on it is dispatched as a person's line, so a mis-routed mutation can never hide from
the activity clock.

## What it fixes, and what it does not

- **Fixes**: diagnostics stop becoming evidence about the owner.
- **Fixes**: a session can drive the box without holding every idle-gated loop shut, so Phase D,
  Phase G and knock become observable — provided presence comes from somewhere real.
- **Does not fix**: presence on a headless box. That still needs a paired cockpit device
  (`DECISIONS_WAITING` item 8) or a channel. This proposal removes one of two blockers, not both.
- **Does not clean up** the belief already written. Mutating a live belief store to tidy a footprint
  is a larger risk than the footprint.

## What to check if this is taken

1. Every existing `register(..., true)` and `register(..., false)` call maps to `UserTurn` and
   `CockpitView` respectively, with **no** behaviour change — this must be a pure refactor before
   `ToolRead` is used anywhere.
2. `ToolRead` still increments `active_turns`, or the exclusion contract breaks.
3. A test that a `ToolRead` moves neither stamp, and a test that the non-allowlisted ctl path is
   still a `UserTurn` — the second is the one that keeps a mutation from hiding.
4. The idle-gated loops are watched for a session afterwards: the point is that they can now run,
   and "can now run" should be observed, not assumed. I have been wrong twice this week about what
   admits a knock opportunity.

## Cost

Small and mechanical: one enum, three call sites, plus the ctl dispatch choice. The risk is not in
the size, it is that it changes who the mind thinks is listening — which is why it is a proposal.
