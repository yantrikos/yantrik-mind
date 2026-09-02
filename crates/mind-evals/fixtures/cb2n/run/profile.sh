#!/bin/bash
# Profile loader (sourced): `cb2_profile_load <fixtures dir>` exports the CB2_* upstream/model/key
# settings of profiles/${CB2_PROFILE:-qwen}.profile. With CB2_UPSTREAM_RESOLVE=1 the upstream's IPv4
# addresses are resolved HERE, once per invocation (CB2_UPSTREAM_IPS; the first -> CB2_UPSTREAM_IP)
# and CB2_RESOLVED_AT records when. A named key file must exist and be non-empty; its content is
# read by nothing in these scripts — the proxy container gets it by bind mount, and the leak
# scan hands the FILE to grep as a pattern file, so the key never enters a variable.
cb2_profile_load() {
  local fix=$1 name=${CB2_PROFILE:-qwen}
  [ -f "$fix/profiles/$name.profile" ] || { echo "profile: unknown profile '$name'"; return 1; }
  set -a; . "$fix/profiles/$name.profile"; set +a
  CB2_RESOLVED_AT=""
  if [ "${CB2_UPSTREAM_RESOLVE:-0}" = 1 ]; then
    CB2_UPSTREAM_IPS=$(getent ahosts "$CB2_UPSTREAM" | awk '$1 ~ /^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$/ {print $1}' | sort -u | tr '\n' ' ' | sed 's/ $//')
    [ -n "$CB2_UPSTREAM_IPS" ] || { echo "profile: could not resolve $CB2_UPSTREAM"; return 1; }
    CB2_UPSTREAM_IP=${CB2_UPSTREAM_IPS%% *}; CB2_RESOLVED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  fi
  [ -n "${CB2_UPSTREAM_IP:-}" ] && [ -n "${CB2_UPSTREAM_IPS:-}" ] || { echo "profile: upstream address unset"; return 1; }
  if [ -n "${CB2_KEY_FILE:-}" ]; then
    [ -s "$CB2_KEY_FILE" ] || { echo "profile: key file missing or empty"; return 1; }
    [ "$(stat -c %a "$CB2_KEY_FILE")" = 400 ] || { echo "profile: key file must be mode 0400"; return 1; }
  fi
  export CB2_PROFILE=$name CB2_UPSTREAM CB2_UPSTREAM_IP CB2_UPSTREAM_IPS CB2_RESOLVED_AT CB2_MODEL CB2_KEY_FILE CB2_MIND_LANE CB2_MIND_PROVIDER CB2_MIND_KEY_ENV
}
# Key-leak COUNT over the given paths (files, recursive): grep takes the key file itself as its
# pattern file; nothing here reads the key. Empty key file setting -> 0.
cb2_key_leak_hits() {
  [ -n "${CB2_KEY_FILE:-}" ] || { echo 0; return; }
  local n=0 p
  for p in "$@"; do [ -e "$p" ] && n=$(( n + $(grep -rlFf "$CB2_KEY_FILE" "$p" 2>/dev/null | wc -l) )); done
  echo "$n"
}
# Environment leak COUNT for a container (its configured env, joined): 0 or 1.
cb2_env_leak_hits() {
  [ -n "${CB2_KEY_FILE:-}" ] || { echo 0; return; }
  docker inspect "$1" --format '{{join .Config.Env "\n"}}' 2>/dev/null | grep -cFf "$CB2_KEY_FILE" || true
}
# Void / rerun bookkeeping: `cb2_rerun_prepare <system> <task> <out>` returns 0 when the leg may
# start: either nothing exists for it, or exactly one prior receipt exists, is VOID, and no _void1
# archive exists yet — in which case every prior output of the leg is renamed *_void1 (preserved).
cb2_rerun_prepare() {
  local sys=$1 t=$2 out=$3 rec="$3/receipts/${1}_${2}.json" x
  [ -e "$rec" ] || [ -e "$out/artifacts/${sys}_$t" ] || return 0
  [ -e "$out/receipts/${sys}_${t}_void1.json" ] && { echo "refusing: ${sys} $t already used its one rerun"; return 1; }
  python3 -c "import json,sys; d=json.load(open('$rec')); sys.exit(0 if d.get('void') is True else 1)" 2>/dev/null || { echo "refusing: ${sys} $t exists and is not void (one invocation per task)"; return 1; }
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
