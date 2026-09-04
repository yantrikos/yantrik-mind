# ARCH8 — the execution loop: what a delivery loop needs that ours does not

Written 2026-09-04 at Pranab's ask: *"Is our recipe engine enough or does it need improvement?
Think about yourself, how do you execute something from prompt to delivery end to end."*

Grounded in one day's record — including the failures, which are the load-bearing part.

## 1. What the engine already has

`RecipeStep`: `Tool · Think · ThinkCited · Validate · Render · JumpIf · Notify · AskUser · Act ·
WaitUntil · WaitForCondition · Schedule`. `ErrorAction`: `Fail · Skip · Retry · JumpTo · Replan`.
And a **planner** that emits a JSON array of `RecipeStep` from a goal.

That is a capable engine. Three things are missing, and only one of them is a missing feature.

## 2. The three gaps, in order of what they cost

**(a) Nothing executes what it built.** `Validate` validates citations. The build lane authors
files, reviews them with a model, and ships. The codebase states the consequence in its own review
step: *"a model checking its own work is weaker than executing it, and this may simply not catch
what a failing test would."* Reading 6's single Mind failure was exactly this — a correct tracker
and an incorrect test suite for it, written in one pass, never seen to fail.

This is the largest gap and the hardest, because running generated code is a security decision, not
a plumbing one. Inside the benchmark container it was tried and abandoned for good reasons
(`unshare -rn` refused; generated code there can reach the run proxy). Outside it, on a box that
already trusts the mind, it is a different question and has never been asked.

**(b) The build lane never plans.** `build_recipe` is a fixed step list authored in Rust. The
planner exists and is used elsewhere. Every T1 outcome measured on 2026-09-03/04 was decided by
DECOMPOSITION — multi-file layouts scored 11/11, monoliths were cut and scored 2/11 — and that
choice was made implicitly inside a single authoring call, by a model that had not been told its
budget. A plan step makes the decomposition explicit, checkable against a measured budget, and
visible in the receipt.

**(c) Conditions are a fixed enum of four.** `VarExists · VarEmpty · VarContains ·
VarIsPublishableDocument`. "Did this step achieve its purpose?" is only askable in those shapes. The
completion pass added in E.CB2-B2 is a hand-rolled instance of a question the engine cannot express
generally: *what happened, and does it change what I do next?*

## 3. The loop this session actually ran, and what to take from it

Not offered as a gold standard — it produced four instances of the same defect in one day. What is
worth copying is that it **corrects**, and the engine cannot.

1. **Write the expectation and the kill criteria BEFORE acting.** This is not ceremony: it is what
   stops a result being reinterpreted once it arrives. It fired twice today — once catching that a
   gate was gameable by the very failure it gated, and once forcing an inconclusive run to be
   reported as inconclusive instead of quietly re-run with a friendlier number.

2. **Act small, then observe the ARTIFACT — not the report.** Every T1 defect was found by reading
   `RESULT.md` and the file bytes. The score said "2/11" three times and never once said why.

3. **Verify each claim before building on it.** "Is this reachable?" "Is this dead code?" "Did the
   mutant actually apply?" Three "SURVIVED" verdicts today were meaningless because a shell quoting
   bug meant the mutation never applied, and one more because the mutation was not the one named.

4. **When the trace disagrees with the observation, instrument — do not re-reason.** A mutant
   survived that "should have panicked". Reasoning harder produced nothing; printing the value
   revealed the fix had been applied to one of three call sites and would not have fixed the bug
   that motivated it.

5. **Bound every loop, and make the bound visible.** Three model calls, one declared rerun, six
   legs. A bound nobody can see is not a bound.

6. **Correct the record plainly and keep going.** Four corrections today, each cheaper than the
   claim it replaced.

## 4. The guideline

Steps 1, 3, 4 and 6 are a single cycle: **observe, compare against what you expected, and let the
difference change what you do next.** The engine can already act, branch and replan. What it cannot
do is notice that it was wrong.

A recipe that plans perfectly and never checks fails exactly the way the token clamp did: correctly
implemented, tested green, and wrong in production on the first case it had never been made to meet.
Planning is not the differentiator. The observe-and-correct cycle is.

## 5. Order of work

1. **Tell the author its measured budget** — done (E.CB2-P), no extra call.
2. **Plan then author in budget-sized groups** — preregistered; the planner already exists.
3. **Verify by executing, where it is safe to do so** — the largest gap, and a security question
   before it is an engineering one. It must not be answered inside the benchmark container.
4. **A condition that can ask a real question** of a step's output, so recoveries stop being
   hand-rolled per lane.

## 6. Bounding a loop — the rule, and the three ways we got it wrong

Every loop in this system that can fail must be able to STOP. The forge and foresight both had
loops that could not, and both were found the same way: a comment promising a retry, sitting next
to code that counted nothing. When you write "will retry next tick", you are making a claim about
termination, and the claim needs a number behind it.

**The class.** *The guard's firing condition is destroyed by the very failure it exists to catch.*
Four instances so far:
- `forge_due` bounds on `now - updated_ms > 900_000`, but `updated_ms` is refreshed after the
  match on every tick INCLUDING the failures — a stuck venture always looks freshly updated.
- `resolve_predictions` retried while `status == "open"` and the deadline was past, which is
  exactly what a failed judge preserves.
- E.CB2-B's recovery path was unreachable in precisely the case that needed it.
- E.CB2-B's gate never exercised the failure the change introduced.

**Three checks, in order, when you add a bound:**

1. **Is the bound's own input mutated by the failure path?** If the failure refreshes the clock or
   preserves the predicate, the bound can never fire. Read the failure path, not the success path.
2. **Is the RESET reachable on every path that makes progress?** This is the one that nearly
   shipped a worse bug than it fixed. The forge's strong builder advances build→test and `return`s
   early, skipping the tick's bookkeeping — so a venture that failed twice and then succeeded
   would have carried two failures into the next stage and died there. **A bound whose reset is
   unreachable kills the work that is going well**, which is strictly worse than not bounding at
   all. Fix it by keying the counter to what it counts (the stage), never by patching the one call
   site you happened to find; the next early return reintroduces it.
3. **Does anything TELL?** A retry with a diagnostic is a design decision; a retry with neither a
   counter nor a report is a stall waiting to happen. This is what separated the two real findings
   from the two negatives — `consolidate_with_min` can stall forever, but `memory-baseline` names
   that exact condition, so it is a considered fail-closed and not an oversight.

**How to stop.** Follow the precedent already in the file rather than inventing a second idiom:
`max_iter` bounds the forge's iterate loop and exits honestly — *"shipped AS-IS at 6/10 after
exhausting 2 iterations — honest ceiling, artifacts kept for review."* Say which stage gave up and
after how many attempts. And pick a terminal state the rest of the code already understands: the
forge's non-terminal test is `st != "shipped" && st != "killed"` in three places, so a new stage
name would have kept the venture due forever — the bug the bound was added to fix.

**A terminal state is not a verdict.** Foresight closes an ungradeable prediction as `unjudged`,
which is deliberately never scored as a hit or a miss: no calibration evidence may be written from
a judgment that never happened. Before adding the state, every reader of that field was checked —
each either tests `== "open"` (so the new state correctly drops out) or matches `hit`/`miss`
explicitly (so it is ignored). Adding a state to a field other code branches on is a schema change;
treat it as one.

## 7. Gap (c), costed: why the completion pass still matches prose

E.LOOP-L made the truncation signal a shared constant so the publisher and the recipe cannot
drift. That removes the fragility but not the underlying shape, and it is worth being precise
about why, because the constant looks like a fix and is really a patch.

**The signal exists structurally and is thrown away.** `publish_file_set` knows exactly which files
were cut and refused — it returns them. The recipe needs precisely that fact. But `RecipeStep::Tool`
stores only `Value::String(out)`: the host's `call_tool` returns a `String`, so a tool's only
channel back into the recipe's variables is its human-readable message. Every structured outcome a
tool knows must therefore be re-encoded as English and re-parsed by a `VarContains`. That is why
the guard reads prose — not an oversight at the call site, a limit of the interface.

`mind-recipes` already knows this hurts: `VarIsPublishableDocument` exists because
`VarContains { "</html>" }` is satisfied by prose that merely MENTIONS `</html>`. That variant is a
point fix for one such question. The completion pass is a second. Each new "did this step achieve
its purpose?" gets its own bespoke answer.

**The real fix, and its price.** Let a tool return a result plus structured metadata, and store it
as side variables (`{store_as}__cut_files`, `{store_as}__stop_reason` — the `Think` arm already
does exactly this for stop reasons). Conditions then test facts instead of sentences, and gap (c)
closes generally rather than one variant at a time.

The price is the `RecipeHost::call_tool` signature, which every tool in the system implements. That
is a wide, mechanical change — the kind that is safe with review and unwise without it, and it
would touch the same interface the capability lanes and the twin-lane shadowing fix run through.
**Not started unilaterally.** Recorded here so the choice is visible and costed rather than
rediscovered: the constant holds the line today, and the interface change is what actually closes
the gap.
