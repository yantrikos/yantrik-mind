# Proposal — thinking as a LEVEL, per model, with a timeout sized to it

**Status: proposal. Nothing implemented.** Rewritten 2026-09-04 after measurement. The first draft
rested on a causal chain that has since been **retracted** (E.THINK2), and its timeout table was
left blank on purpose; the blanks are still blank, and this version explains why that is the correct
outcome rather than an omission.

## What was retracted, and what replaced it

The first draft argued the Mind fails to recognise its own ollama gateway, falls back to `/v1`, and
discards `config.think`. **False.** That URL sniffing lives in `ApiLLM::new`, which the local lane
never calls; the lane uses `GenericOpenAIBackend::for_provider("ollama", …)` — provider **declared**,
native `/api/chat`, `think` honoured — and the comment above that call names the TLS-gateway hazard
explicitly, because someone already hit it and fixed it. So there is no detection bug to fix here,
and the 312 s that disqualified three legs of reading 8 was **genuine generation**.

What survived is measured, and it is enough on its own.

## The measurements (E.THINK1, E.THINK3, E.THINK5)

n=5 per cell, native `/api/chat`, one authoring prompt, `num_predict` 4000:

| model | `think` | med_s | max/min | thinking chars | files per run |
| --- | --- | --- | --- | --- | --- |
| gpt-oss:20b | `false` | 29.2 | 1.58x | 16094 | **[0,0,0,4,4]** |
| gpt-oss:20b | `low` | **7.5** | **1.22x** | 484 | **[3,3,4,4,4]** |
| gpt-oss:20b | `high` | 29.4 | 1.01x | 17328 | [0,0,0,0,0] |
| qwen3.8:27b | `false` | 136.7 | 1.84x | **0** | [1,2,3,4,4] |
| qwen3.8:27b | `low` | 147.3 | 1.82x | 1417 | [3,3,3,3,3] |
| qwen3.8:27b | `high` | 149.3 | 1.35x | 1224 | [3,3,3,3,3] |

**Four facts follow, and each is the reason for one part of the design.**

1. **The same value means different things per model.** `think:false` suppresses completely on qwen
   (0 chars, 5/5) and not at all on gpt-oss (16094). A shared default is provably wrong for one of
   these two whichever value is chosen. → **per-model descriptor.**
2. **A level can be catastrophic on one model and harmless on another.** `high` yields **0 files in
   all five runs** on gpt-oss and a steady 3 on qwen. → **per-model, and never a blind default.**
3. **Within one model, speed and completeness disagree.** On qwen, `false` is fastest but least
   complete ([1,2,3,4,4]); `low`/`high` are slower and consistently complete. So "best" depends on
   what the caller wants. → **per-workload, which `think_for(role, …)` already expresses.**
4. **We cannot say any of this today.** `GenerationConfig.think` is `Option<bool>`, and ollama
   native already accepts `"low"`/`"high"` — the capability is missing from **our type**, not from
   the wire. → **the level enum.**

## The proposal

```rust
pub enum Think { Off, Low, Medium, High }
```

- `think_for(role, default) -> Think`, keeping today's shape. `YM_THINK_<ROLE>` gains
  `off|low|medium|high`; `off/false/0/no` → `Off` and `on/true/1/yes` → `YM_THINK_DEFAULT_ON`
  (default `Medium`), so **no existing env file changes meaning**. Staging currently sets
  `YM_THINK_DISPATCH=off`, `YM_THINK_REASONING=on`, `YM_THINK_PLAN=on`; all three must keep behaving
  exactly as they do now.
- A **capability descriptor per (provider, model)**: which levels that model honours, and which
  level each workload gets. Two models already disagree on every axis, so this is not speculative
  generality — it is the minimum that can express what was measured.
- Wire mapping, transport-aware: native `/api/chat` takes `think` (bool **or** level string);
  `/v1` takes `reasoning_effort`, and **`Off` must not map to `"none"`** — measured as the worst
  available value on both models (E.THINK1). The Anthropic body carries no thinking field at all
  and needs `thinking: {type, budget_tokens}` to stop silently dropping the request.

**Back-compat is a hard requirement.** `Option<bool>` → `Think` is ~75 call sites. `None` and
`Some(false)` must keep their exact present behaviour, landing as a **pure refactor first**, before
any call site adopts a new level. That ordering is the whole safety argument and is the same one the
`ctl` registration proposal makes.

## The timeout, and why there is still no table

`timeout_for(level)` should replace the hardcoded `timeout_global(300 s)` (`llm/api.rs:175`). What
the numbers support:

- **300 s is marginal-to-insufficient for qwen-class authoring.** Worst probe draw 195 s, and the
  real T1 prompt measured **312 s** — the probe underestimates the real workload by ~1.6x.
- **300 s is comfortable for gpt-oss:20b.** Worst probe draw 30.4 s; real T1 179.7 s.
- So one global constant is generous for one model and short for the other. **The timeout must be
  per-model, not merely per-level.**

What the numbers do **not** support is a specific number. These cells span 1.01x–1.84x, but a
separately observed **227.5 s** draw for qwen `false` lies *outside* the [89.9, 165.0] range of the
five samples — a rare slow draw that five repeats did not capture. Writing a duration into code from
data that already missed a known outlier would produce the fifth entry on `DECISIONS_WAITING`'s list
of *bounds that are judgment, not measurement*. **What is needed first: repeats on the real T1
workload rather than the probe**, since the probe is ~1.6x short.

## The one change the evidence supports today

**gpt-oss authoring should use `low`, not off.** Four times faster, the tightest spread in the table,
and files on every run instead of two in five. It cannot be made without the level enum, which is
the practical argument for doing the refactor first.

## What this does not fix

It does not make a 27B model quick at authoring, and it does not touch the fail-closed privacy guard,
which behaved correctly under a real timeout and must keep doing so.

## Configuration surface — what is tunable today, and what this adds

Pranab asked whether all of this is configurable, with defaults, overridable by config. **Today it
is half true**, and the half that is missing is the half that matters.

**Already configurable, verified in the source:**

| knob | domain | effect |
| --- | --- | --- |
| `YM_THINK_<ROLE>` | `on` / `off` | per-workload thinking, via `think_for(role, default)`. Live on staging: `DISPATCH=off`, `REASONING=on`, `PLAN=on` |
| `YM_LOCAL_THINK` | `on` / `off` | the local lane's default |
| `YM_PROVIDER_DEADLINE_S` | seconds | what the authoring budget clamp sizes against |
| `YM_LOCAL_OLLAMA_URL` / `_MODEL`, `YM_BRAIN_POOL`, `YM_ROLE_*` | — | which provider and model a lane uses |

**NOT configurable today — each hardcoded, each measured to matter:**

| thing | where | why it matters |
| --- | --- | --- |
| the **300 s ollama timeout** | `llm/api.rs:175`, `timeout_global` | **zero** env references near it. Insufficient for qwen-class authoring, generous for gpt-oss |
| — *status* | **shipped as `YM_LLM_TIMEOUT_S` (E.TIMEOUT2, `38b8d69`)**, default 300 s, on BOTH clients incl. the local lane's; deployed and gate-verified on staging | |
| the **level** | `GenerationConfig.think: Option<bool>` | `low` is the best setting on gpt-oss and cannot be expressed at all |
| **per-model behaviour** | `disable_thinking()`, compiled into the family template | the same value suppresses on qwen and not on gpt-oss |
| the `reasoning_effort` **value** | hardcoded `"none"` | measured as the worst available value on both models |

**Proposed surface — everything gets a default and an override, and precedence is explicit.**

```text
YM_THINK_<ROLE>        off | low | medium | high      # extends today's on/off
YM_THINK_DEFAULT_ON    low | medium | high            # what a legacy "on" means (default: medium)
YM_THINK_MODELS        "<model>=<level>; <model>=<level>"   # per-model override
YM_LLM_TIMEOUT_S       seconds                        # global default, replaces the constant
YM_LLM_TIMEOUT_<LEVEL>_S   seconds                    # per level
YM_LLM_TIMEOUT_MODELS  "<model>=<seconds>; ..."       # per model, most specific
```

`YM_THINK_MODELS` and `YM_LLM_TIMEOUT_MODELS` use the `;`-separated `key=value` shape
`YM_BRAIN_POOL` already uses, because model tags (`qwen3.8:27b-q4_K_M`) contain characters an
environment variable name cannot hold — which is the reason a per-model knob cannot simply be
`YM_THINK_<MODEL>`.

**Precedence, most specific wins:** per-model override → per-role (`YM_THINK_<ROLE>`) → global
default → the compiled default for that (provider, model). Every layer optional; every layer has a
default; nothing is required for existing deployments to keep behaving exactly as they do.

**Two rules the config must obey, both learned the hard way today:**
1. **A default that came from a measurement must record which model it was measured on.**
   `reasoning_effort:"none"` was verified on qwen3.6, written down as verified, and is now wrong on
   both models we run — E.MODEL1's shape, a tuning constant outliving its model. A compiled default
   should carry the model it was measured against so its staleness is visible.
2. **No duration ships until it is measured on the real workload.** The probe underestimates T1 by
   ~1.6x and n=5 already missed a known outlier, so `YM_LLM_TIMEOUT_*` should ship with the
   **current 300 s** as its default — a pure refactor of a constant into a knob — and the number
   changed only when repeats on the real workload justify it.

## The architecture Pranab described already exists — and it shrinks this proposal

Pranab: *"llm should be the interface and then it should inherit and implemented by each different
provider level. Same for models: base model and actual model."* Inspected rather than assumed:

**The provider interface exists.** `LLMBackend` has **nine** implementations, and `provider/` holds
genuine per-provider ones — `AnthropicBackend`, `GoogleGeminiBackend`, `GenericOpenAIBackend`, with
a `ProviderRegistry`. The local lane already goes through `GenericOpenAIBackend::for_provider(…)`.

The debt is that the **older** `llm/api.rs` still multiplexes with booleans — `is_ollama`,
`is_anthropic`, branched at five sites — and it is that file which holds the URL-sniffing and the
hardcoded 300 s. So the direction of travel is right and half-travelled; `ApiLLM` is the legacy path.

**The model split exists too**, and this is the part that changes the proposal:

- `ModelFamily::from_model_name()` — the **base model**, selecting the chat template.
- `ModelCapabilityProfile::from_model_name()` — the **actual model**, already carrying
  `tier`, `estimated_params_b`, `max_tools_per_prompt`, `tool_call_mode`, `slot_mode`,
  `use_family_routing`, `max_agent_steps`, `multi_step_capable`, `max_effective_context`,
  `ambient_context_budget` — **and `supports_repair_loop` / `max_repair_attempts`**.

**So the "capability descriptor per (provider, model)" this proposal asked for is not a new
structure. It is two missing fields on a structure that is already there**, already keyed by actual
model name, and already used for exactly this kind of per-model behavioural tuning.

| what the measurements say is per-model | where it lives today | where it belongs |
| --- | --- | --- |
| thinking level | `disable_thinking()` on the **family** template — too coarse: qwen3.8 and gpt-oss differ, and qwen3.6 → qwen3.8 changed under a constant tuned on the older one | `ModelCapabilityProfile` |
| call timeout | a hardcoded 300 s constant at three sites | `ModelCapabilityProfile` |

That is a materially smaller change than "add a descriptor", and it is better placed: a profile keyed
on the **actual model name** is precisely what E.MODEL1 needs, because a value tuned on qwen3.6
stops applying the moment the tag changes, instead of silently outliving it.

**One more thing the profile already has that E.REPAIR1 speaks to.** `supports_repair_loop` and
`max_repair_attempts` exist per model. E.REPAIR1 measured the build lane's review repairing 45% of
the time when told, 15% when not. Whether to spend a second repair round is therefore already
expressible per model rather than as a global recipe decision — the recipe simply does not consult
it. Worth checking before any second-round design is drawn.


**Status 2026-09-04 (E.PROFILE1):** the per-model layer is built in yantrik-ml — `YM_THINK_MODELS` and `YM_LLM_TIMEOUT_MODELS` (substring match on the model name) resolve at the backend at send time, above the caller's per-role/global value, above `ModelCapabilityProfile` (`think_default`, `call_timeout_s`), above the compiled default. Per-role `YM_THINK_<ROLE>` stays in the mind. Levels (low/medium/high) remain a proposal: the knob today is a switch, because only the native `/api/chat` path honours a level and only for some models.
