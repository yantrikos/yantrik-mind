# Decisions waiting — things I deliberately did not decide alone

Written 2026-09-04. Every item here is work that is **ready** and **stopped on purpose**, not work
that is unfinished. They accumulated across a long autonomous stretch and are scattered through
`PHASE2_EXPERIMENT_LEDGER.md`, which is chronological and therefore the wrong shape for acting on
them. Each says what is blocked, what it would buy, and what I would do if told to proceed.

The common thread: none of these is an engineering question I lack the information to answer. They
are questions about **what the system is allowed to become** — a new trust boundary, a new
dependency, a new authority — and the failure mode of deciding them alone is that they look like
plumbing right up until they are not.

---

## For Pranab

### 1. May the mind run the code it writes, on a box that already trusts it?
**This is the largest single gap in the build lane.** ARCH8 gap (a). The lane authors files, has a
model review them, and ships. The codebase already states the consequence in its own review step:
*"a model checking its own work is weaker than executing it, and this may simply not catch what a
failing test would."* It was measured this session — leg 4 shipped a fatal `TCPServer` import that
**the model review read and approved**, and reading 6's single Mind failure was a correct tracker
with an incorrect test suite for it, written in one pass and never seen to fail.

- Inside the benchmark container this was tried and abandoned for good reasons: `unshare -rn` was
  refused, and generated code there can reach the run proxy.
- Outside it, on a box that already runs the mind, it is a different question and **has never been
  asked**. It is a security decision before an engineering one, which is why it is here.
- Of the measured T1 defects, three (p3, p8, r7) are behavioural and **only execution can catch
  them**. E.LOOP-I2 caught the structural one without running anything; that well is now dry.

### 2. Reading 8
Ready. Not started, because I said I would not start one without your word.

---

## For Codex (and Pranab where noted)

### 3. `RecipeHost::call_tool` returns a `String` — should it carry structured metadata?
ARCH8 gap (c), costed in ARCH8 §7. A tool's only channel back into recipe variables is its
human-readable message, so every structured outcome is re-encoded as English and re-parsed by a
`VarContains`. The build lane's completion pass fires on the phrase `"was cut"`.

- I made it a shared `TRUNCATION_MARKER` constant so the two sides cannot drift. **That is a patch,
  not a fix** — `publish_file_set` knows exactly which files were cut and returns them.
- `mind-recipes` already shows the strain: `VarIsPublishableDocument` exists only because
  `VarContains { "</html>" }` is satisfied by prose that merely *mentions* `</html>`.
- The fix is side variables (`{store_as}__cut_files`), which the `Think` arm already does for stop
  reasons. The price is a signature every tool implements — wide, mechanical, and touching the same
  interface the capability lanes run through. **Wide interface changes without review is how the
  twin-lane shadowing happened**, so I did not start it.

### 4. A Rust Python-parser crate, to restore the syntax check?
E.LOOP-I2 ships a strict subset of what E.LOOP-I sized: it catches p4's unresolvable import but
**not** v3's unparseable file, because the `python3` version got `compile()` for free. A crate like
`rustpython-parser` restores it with no runtime binary — trading a runtime dependency on each box
for a build-time one in the workspace. Milder, possibly right, still a dependency decision on a
build that already needs `clang` and `libspeechd-dev` installed by hand per box.

### 5. Four bounds that are my judgment, not measurement
All shipped and all defensible, none derived from data:
- `max_stage_tries = 3` (forge give-up)
- `MAX_RESOLVE_FAILS = 3` (foresight `unjudged`)
- `MIN_RATE_SAMPLES = 5`, window 16 (observed throughput)
- whether a stuck forge venture should be distinguishable from an owner `forge kill` in the ledger
  — today both land as `killed`, chosen because `st != "shipped" && st != "killed"` is the
  non-terminal test in three places and a new stage name would keep the venture due forever.

### 6. The scorer's residual denominator
`score.py` derives the expected check set by parsing the checker's source. If it misses a name
**and** the run never reported it either, the denominator is silently too small — a confident score,
wrong in the generous direction, which is what the file exists to prevent. Two ways out, both
decisions about where authority lives rather than bug fixes: the checker declares its full set as
its first action (crash-proof, changes the verdict schema), or `MANIFEST.json` declares it
(consistent with the manifest being authority for the taxonomy, introduces drift).

---

## Not blocked, just not done

- **The import check is unverified end-to-end.** The finding demonstrably reaches the review round;
  whether the review *acts* on it well needs a reading.
- **Staging verifies startup, not behaviour.** It is a headless canary with 22 profile rows, no
  predictions and no forge ventures — which is also why E.LOOP-F and E.LOOP-G are proven reachable
  but have **no observed instance in the wild**.
