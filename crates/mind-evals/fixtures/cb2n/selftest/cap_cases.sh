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
  n=$(sed 's/#.*//' "$F/$f" 2>/dev/null | grep -cE 'CAP *= *8|CB2_CAP=8|cap *= *8' || true)
  say "no_hard_coded_cap_in_$(basename "$f")" "$n" "0"
done
exit $BAD
