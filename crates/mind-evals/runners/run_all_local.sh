#!/bin/bash
# E.CB2-N graded sequence (outside the fixtures; it only CALLS the cb2n scripts in the
# preregistered order). Usage: CB2_PROFILE=<qwen|nim> run_all.sh [out dir]
# One invocation per system per task; after each valid leg the frozen checker runs once.
# Stops on: a disqualified or malformed receipt (report), a VOID receipt (report; a human declares
# the single rerun by invoking the leg again, then re-runs this script — legs with a valid,
# non-void receipt are skipped), or any leftover network attachment. Everything stays on the box.
set -u
FIX=/root/cb2n/fixtures; export CB2_PROFILE=${CB2_PROFILE:-qwen}; OUT=${1:-/root/cb2n/out-$CB2_PROFILE}; export CB2_OUT="$OUT"
mkdir -p "$OUT/verdicts" "$OUT/receipts" "$OUT/raw"
LOG="$OUT/sequence.log"
say() { echo "$(date -u +%Y-%m-%dT%H:%M:%SZ) $*" | tee -a "$LOG"; }
state() { python3 -c "import json,sys
try:
    d=json.load(open('$1'))
    if 'disqualified' not in d or 'void' not in d: print('malformed')
    elif d['void'] is True: print('void')
    elif d['disqualified'] is True: print('dq')
    elif d['disqualified'] is False: print('ok')
    else: print('malformed')
except Exception:
    print('malformed')"; }
say "sequence start; profile $CB2_PROFILE; source $(cat /root/cb2n/SOURCE 2>/dev/null)"
# the run state is written by the FIRST loader call: the network script, so the allowlist and every
# later consumer share one resolved set
bash "$FIX/net/cb2net.sh" >> "$OUT/raw/cb2net.txt" 2>&1 || { say "STOP: containment not proven (see raw/cb2net.txt)"; exit 8; }
say "containment proven; run state $(sha256sum "$OUT/run_state.json" | cut -c1-16) $(python3 -c "import json; d=json.load(open('$OUT/run_state.json')); print(d['profile'], d['upstream'], d['upstream_ips'], d['resolved_at'], d['model'])")"
for T in T1 T2 T3; do
  t=$(echo "$T" | tr 'A-Z' 'a-z')
  for SYS in mind hermes; do
    REC="$OUT/receipts/${SYS}_$T.json"
    if [ -f "$REC" ] && [ "$(state "$REC")" = ok ]; then
      # E.CB2-SKIP1: a valid receipt produced OUTSIDE this runner (a declared rerun) has no verdict yet;
      # the frozen checker runs for it here instead of being skipped with the leg.
      if [ -f "$OUT/verdicts/${SYS}_$T.json" ]; then say "== $SYS $T already has a valid receipt and a verdict; skipped"; continue; fi
      say "== $SYS $T valid receipt, no verdict; check only"
      bash "$FIX/run/check.sh" "$t" "$OUT/artifacts/${SYS}_$T" "$OUT/verdicts/${SYS}_$T.json" "$OUT/verdicts/${SYS}_$T.excerpts.txt"; CRC=$?
      say "== $SYS $T check rc=$CRC verdict=$(sha256sum "$OUT/verdicts/${SYS}_$T.json" 2>/dev/null | cut -c1-16)"
      continue
    fi
    say "== $SYS $T start"
    if [ "$SYS" = mind ]; then bash "$FIX/run/mind_leg.sh" "$T" "$OUT" >> "$OUT/raw/sequence_${SYS}_${T}.txt" 2>&1; RC=$?; else bash "$FIX/run/hermes_leg.sh" "$T" "$OUT" >> "$OUT/raw/sequence_${SYS}_${T}.txt" 2>&1; RC=$?; fi
    S=$(state "$REC")
    say "== $SYS $T leg rc=$RC state=$S receipt=$(sha256sum "$REC" 2>/dev/null | cut -c1-16)"
    ATT=$(docker network inspect cb2net --format '{{len .Containers}}')$(docker network inspect cb2egress --format '/{{len .Containers}}')
    say "attached after leg: $ATT"
    [ "$ATT" = "0/0" ] || { say "STOP: $SYS $T left an attachment — sequence halted"; exit 9; }
    case "$S" in
      void) say "STOP: $SYS $T VOID (infrastructure) — first receipt preserved; a human may declare the single rerun, then re-run this script"; exit 7;;
      ok) ;;
      *) say "STOP: $SYS $T $S — sequence halted"; exit 9;;
    esac
    bash "$FIX/run/check.sh" "$t" "$OUT/artifacts/${SYS}_$T" "$OUT/verdicts/${SYS}_$T.json" "$OUT/verdicts/${SYS}_$T.excerpts.txt"; CRC=$?
    say "== $SYS $T check rc=$CRC verdict=$(sha256sum "$OUT/verdicts/${SYS}_$T.json" 2>/dev/null | cut -c1-16)"
  done
done
say "sequence complete"
