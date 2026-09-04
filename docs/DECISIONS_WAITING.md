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

### 1. ~~May the mind run the code it writes?~~ — CORRECTED: it already does
**I had this wrong, and driving staging is what showed it.** I wrote this up as an open security
question. It is not: the mind has a purpose-built sandbox (`crates/mind-tools/src/sandbox.rs`) and
already runs code in it.

`unshare` (user+net+pid+mount+uts+ipc) + `prlimit` + `timeout`: no network at all (empty net
namespace, so nothing can be exfiltrated or reached on the LAN), the mind's own state dir masked
with a tmpfs, hard cpu/memory/process/file-size/fd caps, a wall-clock kill, non-root, and code
passed as FILES rather than interpolated into a shell command line. Where user namespaces are
unavailable, `available()` is false and callers must refuse rather than fall back. Two call paths
use it today: the raw `run python/shell/rust: …` request, and the forge's test stage — which I
watched execute on staging.

So the security decision was made and implemented some time ago. What remains is narrower and
mostly an engineering question: **should the build lane run its authored files through the sandbox
the mind already has, before shipping them?** That is ARCH8 gap (a), and it is still real — the
build lane authors, has a model review, and ships, and the model review demonstrably missed a fatal
`TCPServer` import.

**One honest caveat that changes the value.** Inside the cb2n benchmark container user namespaces
are unavailable, so `Sandbox::available()` is false there and execution would be skipped. This
would improve the **real** mind and would **not** move a graded leg. That is the opposite of what I
implied when I filed it as the largest gap, and it is worth knowing before anyone spends a day on
it.

### 2. ~~Does staging get real traffic, or does Phase G wait?~~ — WITHDRAWN, it was mine
Filed because the world-model shadow reported `AGREEMENT: UNCOMPUTABLE`, which I concluded no code
change could fix. The system's own empty-state text was the answer: `ym packets` says the Night
Shift compiles them from future nodes, and `ym future` says `ym calendar add` seeds it. Three
commands later a real packet is standing by, produced entirely by the pipeline's own machinery.
Feeding the front door is not the same as forging the store. See E.G3's correction.

**Second item on this list I escalated wrongly** (item 1 was the first). Both times I reasoned from
the code to "this needs a trust or environment decision" without running the thing and reading what
it said back. Escalating feels like the careful move and therefore gets less scrutiny than acting —
but a decision filed wrongly spends Pranab's attention and parks real work.

### 3. Reading 8
Ready. Not started, because I said I would not start one without your word.

---

## For Codex (and Pranab where noted)

### 4. `RecipeHost::call_tool` returns a `String` — should it carry structured metadata?
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

### 5. A Rust Python-parser crate, to restore the syntax check?
E.LOOP-I2 ships a strict subset of what E.LOOP-I sized: it catches p4's unresolvable import but
**not** v3's unparseable file, because the `python3` version got `compile()` for free. A crate like
`rustpython-parser` restores it with no runtime binary — trading a runtime dependency on each box
for a build-time one in the workspace. Milder, possibly right, still a dependency decision on a
build that already needs `clang` and `libspeechd-dev` installed by hand per box.

### 6. Four bounds that are my judgment, not measurement
All shipped and all defensible, none derived from data:
- `max_stage_tries = 3` (forge give-up)
- `MAX_RESOLVE_FAILS = 3` (foresight `unjudged`)
- `MIN_RATE_SAMPLES = 5`, window 16 (observed throughput)
- whether a stuck forge venture should be distinguishable from an owner `forge kill` in the ledger
  — today both land as `killed`, chosen because `st != "shipped" && st != "killed"` is the
  non-terminal test in three places and a new stage name would keep the venture due forever.

### 7. The scorer's residual denominator
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
- ~~**Staging verifies startup, not behaviour.**~~ **Wrong, and corrected the same day.** I had
  verified that the service started and its ports answered, then concluded the box could not tell
  me anything about behaviour. It can: `POST` the command line to `127.0.0.1:8077/cli` with the
  token from `/var/lib/yantrik-mind/console.token`. Driving it that way immediately showed a live
  mind (197 DMN opportunities in 24 h), showed the forge was cold only because `forge start` had
  never been called, and inside ten minutes produced **two findings reading could not have given
  me**: a client-cancelled tick never reaches the give-up bookkeeping, and the forge's syntax check
  had been failing every python file it ever built (E.FORGE1). E.LOOP-F and E.LOOP-G still have no
  observed instance — but that is now a statement about what the mind has been asked to do on that
  box, not about what the box can show.

---

## What the T1 push is now blocked on (added 2026-09-04, after the "win every round" session)

Reading 7 lost 24–17 and **the entire gap is T1** — the Mind wins T2 6/6 vs 4/6 and ties T3. T1's
losses were decomposed on 32 real artifacts re-checked in the checker image. Four defects were
found and closed; what remains is not work, it is two decisions.

| defect | state |
| --- | --- |
| a duplicated path destroyed the whole build | **closed and verified** — the failure itself was the test |
| findings reached a step with no mandate to fix them | **closed and verified** — review prompt asserted |
| unresolvable import (leg p4, 2/11) | fixed and mutation-checked; **repair unverified** |
| placeholder mismatch (leg p3, 7/11) | fixed and mutation-checked; **repair unverified** |
| startup snapshot (leg p8, 8/11) | diagnosed precisely; **cannot be detected without a parser** |

**Item 3 (reading 8)** is the only way to verify the repair link for the two detectors. Staging
cannot do it: its primary brain is `ollama-local:qwen3.8:27b`, not the benchmark's `gpt-oss-20b`,
and that model narrates instead of emitting `=== FILE:` markers. I will not repoint staging's brain
to make my own test pass.

**Item 5 (`rustpython-parser`)** now gates **two** pieces of T1 value, not one: the syntax check
E.LOOP-I2 gives up (leg v3's class), and the startup-snapshot detector for p8. I probed the
regex-only version of the latter and it found nothing on the defective leg — p8's load sits at
module scope but nested inside `if`/`with`, indented exactly like a function body. Separating those
needs scope tracking.

Everything else on the T1 lane is done. Nothing here is waiting on more engineering.

---

## 8. Pair a device to staging's cockpit? (added 2026-09-04 — this one is small)

Phase G is measurable the moment this is done, and not before. The chain is fully mapped:

| link | state |
| --- | --- |
| a packet exists for knock to consider | ✅ done — `calendar add` → future node → `nightshift` → one packet standing by |
| an idle stretch | ✅ |
| **presence** | ❌ |

`idle_ok = present && !quiet_now && idle_stretch`, and `present` is
`telegram_reachable() || console_present()`. Staging is headless, so presence can only come from
`console_present()` — which needs a **machine view**, one of the cockpit's nine allowlisted
read-only GETs. Those are operator-authenticated and return **401** without a paired device.

I did not work around the 401. It is a real authentication boundary, and defeating one to make a
measurement convenient is the kind of shortcut that is invisible in a result and expensive later.

What it buys: the world-model shadow has recorded 9,865 knock evaluations that all exited before
the gate, so `AGREEMENT` has been `UNCOMPUTABLE` since E.G1 shipped. With presence, the next
evaluation reaches the receptivity gate and the shadow finally produces the number it exists for.

