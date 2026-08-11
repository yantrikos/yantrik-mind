#!/usr/bin/env bash
# verify — the gate to run before committing or deploying.
#
# WHY THIS EXISTS. I was using `cargo clippy … | grep -cE "^error"` as a clean-build check and
# reporting "0 errors" while the build was failing. With --message-format short, an error line STARTS
# WITH A FILE PATH ("crates/x/y.rs:12:5: error[E0063]: …"), so `^error` only ever matches the final
# summary line — and if compilation dies before that line is printed, it matches nothing at all. The
# check could not fail in the way the build was wrong, which is the same defect as an availability
# probe that cannot say no.
#
# So this gate reads EXIT CODES, never greps for the word "error". Exit codes cannot be phrased
# differently by a future toolchain.
#
# It also checks the SIBLING path-dependencies first. yantrik-mind path-depends on ../yantrikdb and
# ../yantrik-companion, so uncommitted work-in-progress in either makes every local build fail with
# errors that look like they belong to this repo but do not. That happened once and cost real time
# suspecting the wrong change.
set -uo pipefail

cd "$(dirname "$0")/.." || exit 1
fail=0
step() { printf '%-34s' "$1"; }
ok() { echo "ok"; }
bad() {
  echo "FAILED"
  fail=1
}

# ── Siblings first: their state explains errors that appear to be ours. ──────────────────────────
for sib in ../yantrikdb ../yantrik-companion; do
  [ -d "$sib/.git" ] || continue
  step "sibling $(basename "$sib")"
  dirty=$(git -C "$sib" status --porcelain 2>/dev/null | wc -l | tr -d ' ')
  if [ "$dirty" != "0" ]; then
    echo "$dirty uncommitted file(s) — a build error here is probably THEIRS, not ours"
  else
    ok
  fi
done

step "cargo build --workspace"
if cargo build --workspace --quiet 2>/tmp/ym_build.log; then ok; else bad; tail -20 /tmp/ym_build.log; fi

step "cargo test --workspace"
if cargo test --workspace --quiet 2>/tmp/ym_test.log 1>/tmp/ym_test.out; then
  # Report the count so a suite that silently stops running tests is visible. Zero tests passing is
  # not a pass.
  passed=$(grep -oE '[0-9]+ passed' /tmp/ym_test.out | awk '{s+=$1} END {print s+0}')
  if [ "$passed" -lt 100 ]; then
    echo "FAILED (only $passed tests ran — the suite is not executing)"
    fail=1
  else
    echo "ok ($passed passed)"
  fi
else
  bad
  grep -E 'panicked at|^test .* FAILED|error\[' /tmp/ym_test.out /tmp/ym_test.log 2>/dev/null | head -20
fi

step "cargo clippy --all-targets"
if cargo clippy --workspace --all-targets --quiet 2>/tmp/ym_clippy.log; then ok; else bad; tail -20 /tmp/ym_clippy.log; fi

# ── The desktop client: syntax + its own build. ──────────────────────────────────────────────────
DESK=../yantrik-mind-desktop
if [ -d "$DESK" ]; then
  if command -v node >/dev/null 2>&1; then
    step "desktop js syntax"
    syn=0
    for f in "$DESK"/dist/*.js; do node --check "$f" >/dev/null 2>&1 || { syn=1; node --check "$f"; }; done
    if [ "$syn" = "0" ]; then ok; else bad; fi

    # The renderer turns UNTRUSTED text into DOM, so its escaping tests are the one part of this client
    # that must not regress silently. Exit code is the verdict.
    if [ -f "$DESK/test/render.test.js" ]; then
      step "desktop renderer tests"
      if (cd "$DESK" && node test/render.test.js >/tmp/ym_render.log 2>&1); then
        echo "ok ($(grep -c '^  ok' /tmp/ym_render.log) checks)"
      else
        bad
        grep -A1 FAIL /tmp/ym_render.log | head -20
      fi
    fi
  fi
  # The client has a Rust half too, and it was NOT covered here — I edited src/main.rs and the gate
  # said VERIFIED without ever compiling it. A gate that checks only some of what changed is the
  # earlier clippy mistake in a new place.
  if [ -f "$DESK/Cargo.toml" ]; then
    step "desktop cargo check"
    if (cd "$DESK" && cargo check --quiet 2>/tmp/ym_desk.log); then ok; else bad; tail -20 /tmp/ym_desk.log; fi
  fi

  step "desktop wiring"
  if python3 "$(dirname "$0")/check_ui.py" "$DESK" >/tmp/ym_ui.log 2>&1; then ok; else bad; cat /tmp/ym_ui.log; fi
fi

echo
if [ "$fail" = "0" ]; then
  echo "VERIFIED — safe to commit and deploy."
else
  echo "NOT VERIFIED — do not deploy."
fi
exit "$fail"
