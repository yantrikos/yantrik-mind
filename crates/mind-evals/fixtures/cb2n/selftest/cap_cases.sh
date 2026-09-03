#!/bin/bash
# The request cap is ONE number carried by the run state. These cases prove the three things that
# make that true, using a synthetic profile in a temporary fixtures tree so nothing touches a real
# profile, a real key file or a real output root. Exit non-zero on any disagreement.
#
# They exist because the cap used to be five independent literals. A number duplicated in five
# places is not a parameter, it is five parameters that happen to agree today.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"; BAD=0
T=$(mktemp -d /tmp/cb2-cap-self-XXXX); trap 'rm -rf "$T"' EXIT
mkdir -p "$T/fixtures/profiles"
cat > "$T/fixtures/profiles/synthetic.profile" <<'PROF'
CB2_UPSTREAM=cap.test.invalid
CB2_UPSTREAM_IP=203.0.113.7
CB2_UPSTREAM_IPS=203.0.113.7
CB2_UPSTREAM_RESOLVE=0
CB2_MODEL=cap/test-model
CB2_MIND_LANE=roles
CB2_MIND_PROVIDER=nim
CB2_MIND_KEY_ENV=NVIDIA_API_KEY
PROF
. "$HERE/../run/profile.sh"

say() { if [ "$2" = "$3" ]; then echo "$1: agree [$2]"; else echo "$1: DISAGREE got=[$2] want=[$3]"; BAD=1; fi; }

# 1. Unset means 8 — the existing profiles are unchanged in effect.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s1.json"; unset CB2_CAP
  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo "$CB2_CAP" || echo LOADFAIL ) > "$T/o1"
say "cap_defaults_to_8" "$(cat "$T/o1")" "8"

# 2. A profile that names a cap gets that cap, and it lands in the run state.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s2.json" CB2_CAP=24
  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo "$CB2_CAP" || echo LOADFAIL ) > "$T/o2"
say "cap_24_is_honoured" "$(cat "$T/o2")" "24"
say "cap_is_in_the_run_state" \
    "$(python3 -c "import json;print(json.load(open('$T/s2.json')).get('cap'))" 2>/dev/null)" "24"

# 3. A run state written at one cap REFUSES a load asking for another, AND SAYS SO. The exit code
#    alone is not the observable: `cb2_profile_load` returns 1 for an unknown profile, a bad key
#    file, an unreadable state and an unresolved upstream too, so a case that only distinguishes
#    LOADED from REFUSED reports "the cap wall works" for any failure at all. A reviewer running
#    this suite on a host where several other cases were failing watched this one pass for a reason
#    that had nothing to do with the cap. Assert the message.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s2.json" CB2_CAP=8
  out=$(cb2_profile_load "$T/fixtures" 2>&1) && echo LOADED || echo "$out" ) > "$T/o3"
if grep -q 'was written at cap 24, not 8' "$T/o3"; then R3=cap-refusal; else R3="other: $(head -c 80 "$T/o3")"; fi
say "a_reading_cannot_change_its_own_budget" "$R3" "cap-refusal"

# 4. ...and the same cap reloads cleanly, so the refusal is about the cap and not about reloading.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s2.json" CB2_CAP=24
  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo LOADED || echo REFUSED ) > "$T/o4"
say "the_same_cap_reloads" "$(cat "$T/o4")" "LOADED"

# 5. A cap that is not a positive integer is refused rather than coerced to something.
for bad in 0 -1 abc 3.5 ""; do
  ( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/s5-$RANDOM.json" CB2_CAP="$bad"
    cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo LOADED || echo REFUSED ) > "$T/o5"
  # An EMPTY CB2_CAP is the unset case and correctly becomes 8; the rest must be refused.
  want=REFUSED; [ -z "$bad" ] && want=LOADED
  say "cap_rejects_${bad:-empty}" "$(cat "$T/o5")" "$want"
done

# 6. NO CONSUMER READS THE CAP BEFORE THE LOADER THAT EXPORTS IT.
#    This is the defect that killed reading 4: hermes_leg.sh had `CAP=${CB2_CAP:-8}` one line ABOVE
#    `cb2_profile_load`, so the proxy enforced 24 while the leg checked against 8 and disqualified a
#    run that had finished inside its budget. The instance is fixed; this case is here for the CLASS,
#    because the next script to read a run-state variable can make exactly the same mistake.
for f in run/hermes_leg.sh run/mind_leg.sh run/smoke_mind.sh run/smoke_hermes.sh; do
  src="$HERE/../$f"; [ -f "$src" ] || continue
  load=$(grep -n 'cb2_profile_load' "$src" | head -1 | cut -d: -f1)
  first=$(grep -n 'CB2_CAP' "$src" | head -1 | cut -d: -f1)
  if [ -z "$first" ]; then
    continue   # nothing to order; a row here would be a constant compared with itself
  elif [ -z "$load" ]; then
    say "cap_read_after_load_in_$(basename "$f")" "reads-cap-without-loading" "impossible"
  elif [ "$first" -gt "$load" ]; then
    say "cap_read_after_load_in_$(basename "$f")" "after" "after"
  else
    say "cap_read_after_load_in_$(basename "$f")" "line $first is BEFORE the loader on line $load" "after"
  fi
done

# 7. No literal 8 is left deciding the budget in any consumer.
F="$HERE/.."
# run/verdict.py is in this list because it USED to hold `CAP = 8` and was not scanned — the one
# file the case's own regex matched and its file list omitted. The cap is a required argument there
# now, so nothing in it can decide a budget by itself.
for f in run/hermes_leg.sh run/mind_driver.py run/proxy.sh run/verdict.py; do
  # COMMENTS ARE NOT CODE. The first version matched the whole line and so flagged verdict.py for
  # the sentence explaining that its `CAP = 8` had been REMOVED — a scan failing on its own
  # changelog. Everything from the first `#` is stripped before matching (both shell and Python use
  # it), so only a literal that could still decide a budget counts.
  # WHAT THIS CASE ACTUALLY PROTECTS, restated because its first two versions got it wrong. The
  # old regex matched only a bare CAP=8, and every budget literal actually present is written as a
  # DEFAULT for the unset case, so changing the driver's `else 8` to `else 0` did not move the
  # observable - a check that does not exist. But a default is LEGITIMATE and deliberate: unset
  # means 8, which is what keeps the existing profiles unchanged in effect. The rule worth
  # enforcing is narrower: a line may carry an 8 only as the FALLBACK of the run-state value. So
  # flag a cap-context 8 on a line mentioning NEITHER CB2_CAP nor argv - a literal deciding alone.
  n=$(sed 's/#.*//' "$F/$f" 2>/dev/null | grep -iE 'cap' | grep -E '8' | grep -cvE 'CB2_CAP|argv' || true)
  say "no_hard_coded_cap_in_$(basename "$f")" "$n" "0"
done


# ── E.CB2-W: the WALL is one number, for the same reason ───────────────────────────────────────
# It was a literal in three consumers (hermes_leg, mind_leg, mind_driver). Same three claims as the
# cap, driven the same way — including asserting the REFUSAL MESSAGE, because `cb2_profile_load`
# returns 1 for half a dozen unrelated reasons and a LOADED/REFUSED case would report "the wall
# wall works" for any of them. That lesson was paid for once already, in case 3 above.

# W1. Unset means 1800 — every profile written before today is unchanged in effect.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/w1.json"; unset CB2_WALL
  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo "$CB2_WALL" || echo LOADFAIL ) > "$T/ow1"
say "wall_defaults_to_1800" "$(cat "$T/ow1")" "1800"
say "the_default_wall_is_in_the_run_state"     "$(python3 -c "import json;print(json.load(open('$T/w1.json')).get('wall'))" 2>/dev/null)" "1800"

# W2. A profile that names a wall gets it, and it lands in the run state.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/w2.json" CB2_WALL=3600
  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo "$CB2_WALL" || echo LOADFAIL ) > "$T/ow2"
say "wall_3600_is_honoured" "$(cat "$T/ow2")" "3600"
say "wall_is_in_the_run_state"     "$(python3 -c "import json;print(json.load(open('$T/w2.json')).get('wall'))" 2>/dev/null)" "3600"

# W3. A reading may not change its own timeout mid-run, and the refusal says which number it is.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/w2.json" CB2_WALL=1800
  out=$(cb2_profile_load "$T/fixtures" 2>&1) && echo LOADED || echo "$out" ) > "$T/ow3"
if grep -q 'was written at wall 3600s, not 1800s' "$T/ow3"; then RW3=wall-refusal; else RW3="other: $(head -c 80 "$T/ow3")"; fi
say "a_reading_cannot_change_its_own_timeout" "$RW3" "wall-refusal"

# W4. ...and the same wall reloads, so the refusal is about the wall and not about reloading.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/w2.json" CB2_WALL=3600
  cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo LOADED || echo REFUSED ) > "$T/ow4"
say "the_same_wall_reloads" "$(cat "$T/ow4")" "LOADED"

# W5. A wall that is not a plausible number of seconds is refused rather than coerced. 30 is the
#     interesting one: it parses, it is positive, and a run at it would void every leg.
for bad in 0 30 abc 3.5 ""; do
  ( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/w5-$RANDOM.json" CB2_WALL="$bad"
    cb2_profile_load "$T/fixtures" >/dev/null 2>&1 && echo LOADED || echo REFUSED ) > "$T/ow5"
  want=REFUSED; [ -z "$bad" ] && want=LOADED
  say "wall_rejects_${bad:-empty}" "$(cat "$T/ow5")" "$want"
done

# W6. And no consumer may keep a wall of its own. This is the case that would have caught the
#     original defect: three files agreeing on 1800 by coincidence, with nothing checking.
for f in hermes_leg.sh mind_leg.sh mind_driver.py; do
  n=$(grep -cE 'WALL *= *1800' "$HERE/../run/$f")
  say "no_second_wall_in_$f" "${n:-0}" "0"
done
# The driver must be HANDED the wall, not left to guess it.
n=$(grep -c 'sys.argv\[4\]' "$HERE/../run/mind_driver.py")
say "the_driver_takes_the_wall_as_an_argument" "${n:-0}" "1"
n=$(grep -c '"$CB2_CAP" "$WALL"' "$HERE/../run/mind_leg.sh")
say "the_leg_hands_it_down" "${n:-0}" "1"

# W7. Found by mutation, and the FIRST version of this case did not catch it either: it exported
#     CB2_WALL itself before calling the loader, so the child saw the test's own export and the
#     loader's was redundant. The observable has to be a wall the loader alone could have exported,
#     which is also how a real profile uses it — nim-ds sets CB2_WALL in the profile FILE, with a
#     clean environment. Deleting the loader's `export` reaches every leg (separate processes),
#     drops them all to 1800, and shows nothing in the output. This closes the same hole for the
#     cap and the model, which had it too.
cp "$T/fixtures/profiles/synthetic.profile" "$T/fixtures/profiles/synthwall.profile"
echo 'CB2_WALL=3600' >> "$T/fixtures/profiles/synthwall.profile"
( export CB2_PROFILE=synthwall CB2_RUN_STATE="$T/w7.json"; unset CB2_WALL CB2_CAP
  cb2_profile_load "$T/fixtures" >/dev/null 2>&1
  bash -c 'echo "${CB2_WALL:-unset}/${CB2_CAP:-unset}/${CB2_MODEL:-unset}"' ) > "$T/ow7"
say "a_profile_set_wall_reaches_a_child_process" "$(cat "$T/ow7")" "3600/8/cap/test-model"

# W8. And the DEFAULTED wall, which is the case the explicit export actually carries. Line 15 of
#     the loader sources the profile under `set -a`, so anything the profile FILE sets is exported
#     as a side effect — W7 passes even with the export deleted. A default is a plain assignment
#     outside that block, so the export on the last line is the only thing that puts 1800 in front
#     of a leg. That is the majority case: qwen, nim and nim-cap24 all name no wall.
( export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/w8.json"; unset CB2_WALL CB2_CAP
  cb2_profile_load "$T/fixtures" >/dev/null 2>&1
  bash -c 'echo "${CB2_WALL:-unset}/${CB2_CAP:-unset}"' ) > "$T/ow8"
say "a_defaulted_wall_reaches_a_child_process" "$(cat "$T/ow8")" "1800/8"

# W9. THE MANIFEST IS A CONSUMER TOO, and the one that matters most: it is the document that
#     decides what disqualifies a run. It said `"wall_clock_seconds": 1800` and killed "any run
#     ... over 1800 s", so a leg at 3600 would have been declared dead by the manifest while every
#     script happily ran it. W6 did not catch it because W6 enumerates three files I thought of --
#     a completeness check that is only as complete as the list. This was found by the PREFLIGHT,
#     on the box, minutes before a graded leg would have run; four readings have already died to
#     harness defects whose first run was a graded one.
m="$HERE/../MANIFEST.json"
say "the_manifest_kills_by_no_literal_wall" "$(grep -c '1800 s' "$m")" "0"
r=$(python3 -c "
import json
w = json.load(open('$m'))['caps']['wall_clock_seconds']
k = ' '.join(json.load(open('$m'))['kill'])
print('ok' if (\"run state\" in str(w) and \"run state's wall\" in k) else 'literal')")
say "the_manifest_wall_names_the_run_state" "$r" "ok"

# W10. THE SAME CHECK, TAKING NO LIST. W6 and case 6 each grep three files; the MANIFEST was on
#      neither, and it is what nearly killed a reading. `scan_literals.py` walks the whole tree
#      instead, so a file that does not exist yet is already covered.
#
#      Its FIRST version was a heredoc inside `$( )`: python never received it and the case
#      reported zero hits for a clean tree and for two deliberately broken ones alike. It is a
#      separate file now precisely so it can be run alone and made to fail on demand -- a check
#      that cannot fail does not report nothing, it reports success.
hits=$(python3 "$HERE/scan_literals.py" "$HERE/..")
say "no_budget_or_timeout_is_a_bare_literal_anywhere" "$(printf '%s' "$hits" | grep -c .)" "0"
[ -n "$hits" ] && printf '    %s
' "$hits"

# W11. A COMMENT INSIDE A LINE CONTINUATION SILENTLY TRUNCATES THE COMMAND, and `bash -n` does not
#      care: what is left is still valid syntax, just a different command. Adding a note above the
#      `-e YM_CTL_PORT=` line of `docker run ... \` cut the invocation in half; `bash -n` passed
#      both scripts, the smoke then failed with `"docker run" requires at least 1 argument`. Syntax
#      checking is not command checking, so check the shape directly.
bad=0
for f in "$HERE"/../run/*.sh "$HERE"/../net/*.sh "$HERE"/*.sh; do
  [ -f "$f" ] || continue
  n=$(awk 'prev ~ /\\$/ && $0 ~ /^[ 	]*#/ { print FILENAME ":" NR } { prev=$0 }' "$f")
  [ -n "$n" ] && { bad=$((bad+1)); echo "    $n"; }
done
say "no_comment_sits_inside_a_line_continuation" "$bad" "0"

# W12. And the console lane keeps off other surfaces' defaults. CTL was pinned to 8078, which is
#      YM_FRAME_PORT's default, so every Mind leg since reading 3 booted with a COLLISION line.
#      Inert -- the driver drives 8091 -- but a false alarm in the one log a reviewer reads as
#      evidence. The product's own five defaults do not collide; the harness had moved CTL onto
#      FRAME. Assert the harness pins nothing onto a default that is not its own.
clash=0
for f in "$HERE/../run/mind_leg.sh" "$HERE/../run/smoke_mind.sh"; do
  for pin in $(grep -o 'YM_[A-Z_]*PORT=[0-9]*' "$f" | sort -u); do
    var=${pin%%=*}; val=${pin##*=}
    # the product's defaults, from mind-core telegram.rs
    case "$val" in
      8077) other=YM_CTL_PORT;; 8078) other=YM_FRAME_PORT;; 8079) other=YM_CHAT_PORT;;
      8088) other=YM_WEB_PORT;; 8090) other=YM_WEBUI_PORT;; *) other="";;
    esac
    [ -n "$other" ] && [ "$other" != "$var" ] && { clash=$((clash+1)); echo "    $(basename "$f"): $var=$val is $other's default"; }
  done
done
say "no_pinned_port_lands_on_another_surfaces_default" "$clash" "0"

exit $BAD
