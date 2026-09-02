#!/bin/bash
# Positive and negative self-tests of the checkers, inside the checker image. One line per
# fixture: expected vs observed and the failed check names. Exit non-zero on any disagreement.
set -u
FIX="$(cd "$(dirname "$0")/.." && pwd)"; BAD=0
for f in t1_pass t1_fail t2_pass t2_fail t3_pass t3_fail; do
  T=${f%_*}; EXP=${f#*_}
  bash "$FIX/run/check.sh" "$T" "$FIX/selftest/$f" "/tmp/cb2-self-$f.json" >/dev/null 2>&1; RC=$?
  OBS=$([ $RC -eq 0 ] && echo pass || echo fail)
  FAILED=$(python3 -c "import json;d=json.load(open('/tmp/cb2-self-$f.json'));print(','.join(k for k,v in d['checks'].items() if not v['pass']))" 2>/dev/null)
  echo "$f: expected $EXP observed $OBS failed_checks=[$FAILED]"
  [ "$EXP" = "$OBS" ] || BAD=1
done
exit $BAD
