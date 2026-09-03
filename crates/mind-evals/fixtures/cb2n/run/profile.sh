#!/bin/bash
# Profile loader (sourced): `cb2_profile_load <fixtures dir>` exports the CB2_* upstream/model/key
# settings of profiles/${CB2_PROFILE:-qwen}.profile (.profile, not .env: the repository ignores
# *.env) THROUGH THE RUN STATE: the first loader call of a run writes
# ${CB2_RUN_STATE:-${CB2_OUT:-/root/cb2n/out}/run_state.json} (profile, upstream, the upstream's
# IPv4 addresses resolved exactly once when CB2_UPSTREAM_RESOLVE=1, the first address, the
# resolution time, the model) and makes it read-only; every later call — network script, proxy,
# legs, smokes, cap test — consumes that exact set and refuses a state written for another
# profile/upstream/model. A named key file must be uid 10002 (the proxy's user) and mode 0400;
# its content is read by nothing in these scripts — the proxy container gets it by bind mount,
# and the leak scan hands the FILE to grep as a pattern file, so the key never enters a variable.
cb2_profile_load() {
  local fix=$1 name=${CB2_PROFILE:-qwen} state=${CB2_RUN_STATE:-${CB2_OUT:-/root/cb2n/out}/run_state.json}
  [ -f "$fix/profiles/$name.profile" ] || { echo "profile: unknown profile '$name'"; return 1; }
  set -a; . "$fix/profiles/$name.profile"; set +a
  # THE CAP IS ONE NUMBER. It was five independent literals — the proxy's 429, the Hermes leg, the
  # Mind driver, the Mind leg's receipt check and the manifest's prose — which can drift apart with
  # nothing noticing. It now enters the run state beside profile, upstream and model, so every
  # consumer reads the same resolved value and a state written at one cap refuses a load at another.
  # Unset means 8, so the existing profiles are unchanged in effect.
  # E.CB2-W: THE WALL IS ONE NUMBER, for the reason the cap is. It was a literal in three
  # consumers (hermes_leg, mind_leg, mind_driver) which can drift apart with nothing noticing, and
  # the deepseek reading needs a different one -- measured at 1,163 s of model time for 24 calls,
  # 65% of the old 1800. Unset means 1800, so every earlier profile is unchanged in effect.
  CB2_WALL=${CB2_WALL:-1800}
  case "$CB2_WALL" in ''|*[!0-9]*) echo "profile: CB2_WALL must be a positive integer"; return 1;; esac
  # The 60 s floor is not decoration: a wall of 30 parses, is positive, and voids every leg it
  # governs -- a setting that turns a reading into a row of timeouts is a typo, not a budget.
  [ "$CB2_WALL" -ge 60 ] || { echo "profile: CB2_WALL must be at least 60 seconds"; return 1; }
  CB2_CAP=${CB2_CAP:-8}
  case "$CB2_CAP" in ''|*[!0-9]*) echo "profile: CB2_CAP must be a positive integer"; return 1;; esac
  [ "$CB2_CAP" -ge 1 ] || { echo "profile: CB2_CAP must be at least 1"; return 1; }
  # The key is validated FIRST: a missing or wrongly-owned key must not leave a resolved run state
  # pinned behind it, or a later corrected load would silently inherit the first resolution.
  if [ -n "${CB2_KEY_FILE:-}" ]; then
    [ -s "$CB2_KEY_FILE" ] || { echo "profile: key file missing or empty"; return 1; }
    [ "$(stat -c '%u %a' "$CB2_KEY_FILE")" = "10002 400" ] || { echo "profile: key file must be uid 10002 (the proxy user) and mode 0400"; return 1; }
  fi
  if [ -f "$state" ]; then
    local got
    got=$(python3 -c "import json,sys
d=json.load(open('$state'))
print(d['profile'], d['upstream'], d['model'], d['upstream_ip'], d['resolved_at'], '|'.join(d['upstream_ips']), d.get('cap', 8), d.get('model_alive', 'unchecked'), d.get('model_probe_ms', -1), d.get('wall', 1800))" 2>/dev/null) || { echo "profile: run state unreadable: $state"; return 1; }
    read -r sp su sm sip sat sips scap salive sprobe swall <<< "$got"
    [ "$sp" = "$name" ] && [ "$su" = "$CB2_UPSTREAM" ] && [ "$sm" = "$CB2_MODEL" ] || { echo "profile: run state $state belongs to profile '$sp' ($su, $sm), not '$name' — use another out dir"; return 1; }
    [ "$scap" = "$CB2_CAP" ] || { echo "profile: run state $state was written at cap $scap, not $CB2_CAP — a reading may not change its budget mid-run; use another out dir"; return 1; }
    [ "$swall" = "$CB2_WALL" ] || { echo "profile: run state $state was written at wall ${swall}s, not ${CB2_WALL}s — a reading may not change its timeout mid-run; use another out dir"; return 1; }
    CB2_UPSTREAM_IP=$sip; CB2_RESOLVED_AT=$sat; CB2_UPSTREAM_IPS=${sips//|/ }
    # Inherited, not re-probed: one liveness answer per reading, and every leg reports the same one.
    CB2_MODEL_ALIVE=$salive; CB2_MODEL_PROBE_MS=$sprobe
  else
    CB2_RESOLVED_AT=static
    if [ "${CB2_UPSTREAM_RESOLVE:-0}" = 1 ]; then
      CB2_UPSTREAM_IPS=$(getent ahosts "$CB2_UPSTREAM" | awk '$1 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ {print $1}' | sort -u | tr '\n' ' ' | sed 's/ $//')
      [ -n "$CB2_UPSTREAM_IPS" ] || { echo "profile: could not resolve $CB2_UPSTREAM"; return 1; }
      CB2_UPSTREAM_IP=${CB2_UPSTREAM_IPS%% *}; CB2_RESOLVED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
    fi
    [ -n "${CB2_UPSTREAM_IP:-}" ] && [ -n "${CB2_UPSTREAM_IPS:-}" ] || { echo "profile: upstream address unset"; return 1; }
    # E.MODEL1 — THE MODEL MUST STILL EXIST, checked HERE: this is the first loader call of the
    # run, the one that resolves the upstream and writes the state. Every later call reads the
    # verdict back out of the state, so a healthy reading costs exactly ONE extra request no matter
    # how many legs, smokes and scripts source this file.
    #
    # It sits before the write for the reason the key check does: a model that is gone must not
    # leave a resolved state pinned behind it, or a later corrected load would inherit this
    # resolution. The harness pinned the model's NAME, the Hermes archive by hash, the checker, the
    # briefs, the cap and the upstream's addresses — and never checked the model still EXISTED.
    CB2_MODEL_ALIVE=unchecked; CB2_MODEL_PROBE_MS=-1
    if [ -n "${CB2_KEY_FILE:-}" ] && [ "${CB2_MODEL_PROBE:-1}" = 1 ]; then
      local t0 t1 code body probe verdict reason
      t0=$(date +%s%3N)
      code=$(curl -s --max-time 60 -o "/tmp/cb2_model_probe.$$" -w '%{http_code}'         -X POST "https://$CB2_UPSTREAM/v1/chat/completions"         -H "Authorization: Bearer $(cat "$CB2_KEY_FILE")"         -H 'Content-Type: application/json'         -d "{\"model\":\"$CB2_MODEL\",\"messages\":[{\"role\":\"user\",\"content\":\"ok\"}],\"max_tokens\":4}" 2>/dev/null) || code=000
      t1=$(date +%s%3N)
      body=$(head -c 400 "/tmp/cb2_model_probe.$$" 2>/dev/null); rm -f "/tmp/cb2_model_probe.$$"
      probe=$(CB2_PROBE_BODY="$body" python3 -c "
import os, sys
sys.path.insert(0, '$fix/run')
from verdict import model_liveness
v, r = model_liveness('$code', os.environ.get('CB2_PROBE_BODY', ''))
print(v, r.replace(chr(10), ' ')[:200])
") || probe="inconclusive the liveness classifier itself failed"
      read -r verdict reason <<< "$probe"
      # An unreadable classifier is not a dead model. Nothing here may turn a harness defect into
      # a verdict about the provider.
      case "$verdict" in alive|gone|inconclusive) ;; *) verdict=inconclusive; reason="unclassifiable probe";; esac
      CB2_MODEL_ALIVE=$verdict
      CB2_MODEL_PROBE_MS=$((t1 - t0))
      if [ "$verdict" = gone ]; then
        echo "profile: the model '$CB2_MODEL' is GONE — $reason"
        # ONE extra request, on the FAILURE path only. A 404 cannot tell a retired model from a
        # mistyped id -- both answer it, with the same body -- and reporting the wrong one sends an
        # operator hunting a retirement announcement that does not exist. Probing
        # `deepseek-v4-flash-0813` did exactly that: the id carried pro's date suffix, and the
        # provider had listed `deepseek-v4-flash-0731` the whole time.
        local listfile=/tmp/cb2_models.$$
        curl -s --max-time 30 -o "$listfile" -H "Authorization: Bearer $(cat "$CB2_KEY_FILE")"           "https://$CB2_UPSTREAM/v1/models" >/dev/null 2>&1 || : > "$listfile"
        echo "profile: $(CB2_LIST="$listfile" python3 -c "
import json, os, sys
sys.path.insert(0, '$fix/run')
from verdict import explain_gone
try:
    ids = [m['id'] for m in json.load(open(os.environ['CB2_LIST']))['data']]
except Exception:
    ids = None
print(explain_gone('$CB2_MODEL', ids, $code))
" 2>/dev/null || echo "the provider's model list could not be read, so retired-vs-mistyped is unresolved")"
        rm -f "$listfile"
        echo "profile: refusing to start a reading on a model that does not answer"
        return 1
      fi
      # A timeout, a 429 or an auth failure is NOT death. Refusing here would turn a bad minute
      # into a cancelled reading; calling it death would repeat the retirement mistake inverted.
      [ "$verdict" = alive ] || echo "profile: WARNING model liveness inconclusive — $reason"
    fi
    mkdir -p "$(dirname "$state")" || return 1
    python3 -c "import json,sys
json.dump({'profile': '$name', 'upstream': '$CB2_UPSTREAM', 'upstream_ips': '$CB2_UPSTREAM_IPS'.split(), 'upstream_ip': '$CB2_UPSTREAM_IP', 'resolved_at': '$CB2_RESOLVED_AT', 'model': '$CB2_MODEL', 'cap': int('$CB2_CAP'), 'wall': int('$CB2_WALL'), 'model_alive': '$CB2_MODEL_ALIVE', 'model_alive_at': '$(date -u +%Y-%m-%dT%H:%M:%SZ)', 'model_probe_ms': int('$CB2_MODEL_PROBE_MS')}, open('$state.tmp', 'w'), indent=1)" || return 1
    mv "$state.tmp" "$state" && chmod 444 "$state" || return 1
  fi
  CB2_RUN_STATE_SHA=$(sha256sum "$state" | cut -c1-64)
  export CB2_PROFILE=$name CB2_RUN_STATE=$state CB2_RUN_STATE_SHA CB2_UPSTREAM CB2_UPSTREAM_IP CB2_UPSTREAM_IPS CB2_RESOLVED_AT CB2_MODEL CB2_CAP CB2_WALL CB2_MODEL_ALIVE CB2_MODEL_PROBE_MS CB2_KEY_FILE CB2_MIND_LANE CB2_MIND_PROVIDER CB2_MIND_KEY_ENV
}
# Key-leak COUNT over the given paths (files, recursive): grep takes the key file itself as its
# pattern file; nothing here reads the key. Empty key file setting -> 0.
cb2_key_leak_hits() {
  [ -n "${CB2_KEY_FILE:-}" ] || { echo 0; return; }
  local n=0 p
  for p in "$@"; do [ -e "$p" ] && n=$(( n + $(grep -rlFf "$CB2_KEY_FILE" "$p" 2>/dev/null | wc -l) )); done
  echo "$n"
}
# Environment leak COUNT for a container (its configured env, one per line): 0 or more.
cb2_env_leak_hits() {
  [ -n "${CB2_KEY_FILE:-}" ] || { echo 0; return; }
  docker inspect "$1" --format '{{join .Config.Env "\n"}}' 2>/dev/null | grep -cFf "$CB2_KEY_FILE" || true
}
# Void / rerun bookkeeping: `cb2_rerun_prepare <system> <task> <out>` returns 0 when the leg may
# start: either nothing exists for it, or exactly one prior receipt exists that is PURELY void —
# void true, disqualified false AND dq_independent false — and no _void1 archive exists yet, in
# which case every prior output of the leg is renamed *_void1 (preserved). A leg that broke a rule
# of its own never earns a rerun, however the infrastructure behaved alongside it.
cb2_rerun_prepare() {
  local sys=$1 t=$2 out=$3 rec="$3/receipts/${1}_${2}.json" x
  [ -e "$rec" ] || [ -e "$out/artifacts/${sys}_$t" ] || return 0
  [ -e "$out/receipts/${sys}_${t}_void1.json" ] && { echo "refusing: ${sys} $t already used its one rerun"; return 1; }
  python3 -c "import json,sys
d=json.load(open('$rec'))
sys.exit(0 if (d.get('void') is True and d.get('disqualified') is False and d.get('dq_independent') is not True) else 1)" 2>/dev/null || { echo "refusing: ${sys} $t exists and is not a PURE infrastructure void (one invocation per task)"; return 1; }
  for x in "artifacts/${sys}_$t" "homes/${sys}_$t" "proxy/${sys}_$t" "state/${sys}_$t"; do
    [ -e "$out/$x" ] && { mv "$out/$x" "$out/${x}_void1" || return 1; }
  done
  for x in "receipts/${sys}_$t.json" "receipts/${sys}_${t}_teardown.txt"; do
    [ -e "$out/$x" ] && { mv "$out/$x" "$out/${x%.*}_void1.${x##*.}" || return 1; }
  done
  for x in "$out"/raw/${sys}_${t}_*.txt; do
    [ -e "$x" ] || continue
    case "$x" in *_void1.txt) ;; *) mv "$x" "${x%.txt}_void1.txt" || return 1;; esac
  done
  echo "rerun: ${sys} $t first receipt was void; prior outputs preserved as *_void1"
}
