# Proposal — thinking as a LEVEL, and a timeout sized to it

**Status: proposal. Nothing implemented.** Written after Pranab's observation that qwen3.8 is a
thinking model and that the mind needs *"multi level low thinking high thinking max and set timeout
accordingly."* The measurement that fixes the numbers is running; **every duration below is a blank
to be filled from it, not a guess.** Bounds chosen by judgment rather than measurement are already
one of the open items on `DECISIONS_WAITING`, and this proposal must not add a fifth.

## The problem, in one sentence

Thinking is a **boolean** everywhere in the stack, and the timeout that has to accommodate it is a
**single hardcoded constant** — so there is no way to say "think a little here, a lot there", and no
way for the timeout to follow the choice.

## What today's code actually does

Three facts, read from the source rather than assumed:

1. `yantrik-companion/crates/yantrik-ml/src/types.rs:149` — `pub think: Option<bool>`. Two states.
2. `.../llm/api.rs:175` — `timeout_global(Some(Duration::from_secs(300)))`, **hardcoded, no env
   knob**, on every ollama call regardless of what was asked of the model.
3. The two transports disagree, and the code says so. The native `/api/chat` path honours a per-call
   `config.think` (`api.rs:145`). The OpenAI-compat `/v1` path **ignores `think` entirely** — its
   own comment records this — and suppresses reasoning with `reasoning_effort: "none"` instead, but
   **only when the template family opts in** via `disable_thinking()`, never from the per-call
   config. So on `/v1` a caller asking for less thinking is silently not heard.

There is already a good extension point: **`mind_inference::think_for(role, default)`**, with a
`YM_THINK_<ROLE>` env override that accepts on/off. Call sites already pass a role
(`think_for("plan", Some(false))`, `think_for("reasoning", Some(false))`). **Levels belong there**,
not in a new parallel mechanism.

## What it cost, measured today

`E.CB2-R8`: a T1 authoring call on qwen3.8:27b took **312–316 s** and was cut at 300 s. The Mind
then failed closed — correctly — and three legs were disqualified. The same task on
`gpt-oss-backup:20b` finished in **179.7 s**.

The instructive part is that `QwenTemplate::disable_thinking()` **is** `true`, so the client already
believes it is suppressing thinking on this family. Either the suppression is not reaching the wire
on the transport in use, or 312 s is genuine generation. **The measurement now running answers
exactly that**, by reporting the reasoning/content split per level — and until it lands, the cause
is undetermined and is written here as undetermined.

## The proposal

**One enum, one policy function, one timeout function.**

```rust
pub enum Think { Off, Low, Medium, High }
```

- `think_for(role, default) -> Think`, keeping the existing shape. `YM_THINK_<ROLE>` accepts
  `off|low|medium|high`, and — for compatibility with every value already in anyone's env file —
  `off/false/0/no` → `Off` and `on/true/1/yes` → the level named by `YM_THINK_DEFAULT_ON`
  (itself defaulting to `Medium`). **No existing env file changes meaning.**
- `timeout_for(level) -> Duration` replaces the 300 s constant. One table, one place.
- Wire mapping, transport-aware, because the two transports genuinely differ:
  - native `/api/chat`: `think: false` for `Off`, `think: true` otherwise (plus the level where the
    server accepts one).
  - `/v1`: `reasoning_effort` = `none|low|medium|high` — **read from the call's level**, not only
    from `disable_thinking()`. This is the actual bug fix in the slice: a per-call request for less
    thinking is currently discarded on this path.

**Back-compat is a hard requirement.** `Option<bool>` → `Think` is 63 call sites in `mind-` and 12
in the companion crate. `None` and `Some(false)` must keep their present behaviour exactly, or a
wide mechanical change becomes a behaviour change nobody reviewed — the failure that produced the
twin-lane shadowing.

## Where the levels would be used

| call site | today | proposed | why |
| --- | --- | --- | --- |
| `build_recipe` authoring | `Some(false)` | `Off` | Its own comment: a build is a specification problem, and thinking spends budget the FILES need. |
| `build_recipe` review | `Some(false)` | `Low` | Checking your own work against a brief is the one step in the chain that is actually reasoning. |
| dispatch / tool-selection | `Some(false)` | `Off` | Latency path; the existing dual-mode split already says so. |
| `reasoning` / compose | `Some(false)` | `Medium` | Where `prefer_reasoner` routing already sends work. |

## What this does NOT fix

- It does not make a 27B model finish a 16k-token authoring call. If the measurement shows the 312 s
  was generation rather than reasoning, the honest answer is that **the model is too slow for that
  workload on this hardware**, and a bigger timeout only converts a fast failure into a slow one.
- It does not touch the fail-closed privacy guard, which behaved correctly and must keep behaving
  correctly: a timeout must still refuse to escalate private context to cloud.

## Cost and risk

One enum, one policy function, one timeout table, two wire mappings, and a mechanical sweep of ~75
call sites that must be a **pure refactor first** — every existing value mapped, nothing's behaviour
changed — before any call site adopts a new level. That ordering is the whole safety argument, and
it is the same one the `ctl` registration proposal makes for the same reason.
