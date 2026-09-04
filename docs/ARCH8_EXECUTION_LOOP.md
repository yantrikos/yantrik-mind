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
