#!/bin/bash
# E.MODEL1: the run refuses to start on a model that no longer exists. These cases prove the
# LOADER's five guarantees; the classifier's own table lives in verdict_cases.py.
#
# OFFLINE BY CONSTRUCTION. The loader calls `curl` unqualified, so a shell function shadows it and
# returns a canned status — which makes the refusal path testable without a retired model to hand.
# That matters: this whole feature exists because openai/gpt-oss-120b was retired mid-reading, and
# the one day the refusal could be verified against a genuinely dead model was the day it shipped.
# The stub also COUNTS requests, which is how "a reading probes once, not once per leg" is checked.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"; BAD=0
T=$(mktemp -d /tmp/cb2-model-self-XXXX); trap 'rm -rf "$T"' EXIT
mkdir -p "$T/fixtures/profiles" "$T/out"
cp -r "$HERE/../run" "$T/fixtures/run"
printf 'k' > "$T/key"   # never read: the stub curl replaces the request entirely
cat > "$T/fixtures/profiles/synthetic.profile" <<PROF
CB2_UPSTREAM=model.test.invalid
CB2_UPSTREAM_IP=203.0.113.9
CB2_UPSTREAM_IPS=203.0.113.9
CB2_UPSTREAM_RESOLVE=0
CB2_MODEL=test/retired-model
CB2_KEY_FILE=$T/key
PROF

# The loader validates the key file's ownership before it probes. Under a self-test there is no
# proxy user to own it, so that check is neutralised HERE, in the copy, and nowhere else.
sed -i 's|\[ "$(stat -c .%u %a. "$CB2_KEY_FILE")" = "10002 400" \]|true|' "$T/fixtures/run/profile.sh"

curl() {  # $CB2_TEST_CODE / $CB2_TEST_BODY drive it; every call is counted.
  echo x >> "$T/curlcount"
  local out=""; local a
  for a in "$@"; do [ "${prev:-}" = "-o" ] && out=$a; prev=$a; done
  [ -n "$out" ] && printf '%s' "${CB2_TEST_BODY:-}" > "$out"
  printf '%s' "${CB2_TEST_CODE:-200}"
}

say() { if [ "$2" = "$3" ]; then echo "$1: agree [$2]"; else echo "$1: DISAGREE got=[$2] want=[$3]"; BAD=1; fi; }
load() { # $1 = out subdir -> exit code, with stdout in $T/say
  export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/out/$1.json"
  rm -f "$T/out/$1.json"; : > "$T/curlcount"
  ( . "$T/fixtures/run/profile.sh"; cb2_profile_load "$T/fixtures" >"$T/say" 2>&1; echo $? ) | tail -1
}
field() { python3 -c "import json;print(json.load(open('$T/out/$1.json')).get('$2'))" 2>/dev/null; }
reqs() { wc -l < "$T/curlcount" | tr -d ' '; }

# 1. A retired model REFUSES, quoting the provider, and leaves NO state pinned behind it — the
#    same ordering rule the key check follows, so a later corrected load resolves afresh.
export CB2_TEST_CODE=410
export CB2_TEST_BODY='{"detail":"The model has reached its end of life on 2026-09-03T08:00:00Z"}'
say "gone_refuses" "$(load gone)" "1"
grep -q "is GONE" "$T/say" && r=y || r=n;             say "gone_says_gone" "$r" "y"
grep -q "test/retired-model" "$T/say" && r=y || r=n;  say "gone_names_the_model" "$r" "y"
grep -qi "end of life" "$T/say" && r=y || r=n;        say "gone_quotes_the_provider" "$r" "y"
grep -q "refusing to start" "$T/say" && r=y || r=n;   say "gone_refuses_not_warns" "$r" "y"
[ -f "$T/out/gone.json" ] && r=y || r=n;              say "gone_pins_no_state" "$r" "n"

# 2. A live model passes, costs exactly ONE request, and the answer is RECORDED — the probe's
#    latency included, because it is the cheapest baseline a leg's own timing can be read against.
export CB2_TEST_CODE=200 CB2_TEST_BODY='{"choices":[]}'
say "alive_loads" "$(load ok)" "0"
say "alive_costs_one_request" "$(reqs)" "1"
say "alive_recorded" "$(field ok model_alive)" "alive"
r=$(python3 -c "import json;d=json.load(open('$T/out/ok.json'));print('y' if len(d.get('model_alive_at',''))==20 else 'n')" 2>/dev/null)
say "alive_time_recorded" "$r" "y"
r=$(python3 -c "import json;d=json.load(open('$T/out/ok.json'));print('y' if 0<=d.get('model_probe_ms',-1)<60000 else 'n')" 2>/dev/null)
say "alive_latency_recorded" "$r" "y"

# 3. Later loads INHERIT the verdict. A reading that re-probed per leg would spend a request for
#    every script that sources this file and could report two different answers within one run.
export CB2_PROFILE=synthetic CB2_RUN_STATE="$T/out/ok.json"; : > "$T/curlcount"
got=$( . "$T/fixtures/run/profile.sh"; cb2_profile_load "$T/fixtures" >/dev/null 2>&1; echo "$CB2_MODEL_ALIVE" )
say "reload_makes_no_request" "$(reqs)" "0"
say "reload_inherits_the_verdict" "$got" "alive"

# 4. A bad minute is NOT death. Refusing here would cancel readings that would have been fine;
#    this is the failure direction opposite to the one that caused the incident.
export CB2_TEST_CODE=429 CB2_TEST_BODY='slow down'
say "rate_limited_loads" "$(load slow)" "0"
grep -q "inconclusive" "$T/say" && r=y || r=n;  say "rate_limited_reported" "$r" "y"
grep -q "GONE" "$T/say" && r=y || r=n;          say "rate_limited_not_death" "$r" "n"
say "rate_limited_recorded" "$(field slow model_alive)" "inconclusive"

# 5. The probe can be declined, and a declined probe says "unchecked" rather than claiming health.
export CB2_TEST_CODE=410 CB2_MODEL_PROBE=0
say "probe_off_loads" "$(load off)" "0"
say "probe_off_costs_nothing" "$(reqs)" "0"
say "probe_off_is_not_alive" "$(field off model_alive)" "unchecked"
unset CB2_MODEL_PROBE

exit $BAD
