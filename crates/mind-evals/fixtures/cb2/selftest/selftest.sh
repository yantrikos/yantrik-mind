#!/bin/bash
# Positive and negative self-tests of the checkers, inside the checker image (no network).
# Each fixture must produce a verdict (an infrastructure failure is a FAIL of the self-test,
# never a pass) whose set of failed checks equals the expected set exactly.
set -u
FIX="$(cd "$(dirname "$0")/.." && pwd)"; BAD=0
declare -A EXPECT=(
  [t1_pass]=""
  [t1_fail]="appended_record_matches_submission_and_schema,dashboard_json_block_present,dashboard_per_day_exact_14_bins,dashboard_recent_five_exact_order,dashboard_total_exact,submit_appends_exactly_one_record"
  [t2_pass]=""
  [t2_fail]="contact_section_reachable,projects_section_has_4_articles,relative_links_resolve"
  [t3_pass]=""
  [t3_fail]="done_prints_done_1,pytest_passes"
)
for f in t1_pass t1_fail t2_pass t2_fail t3_pass t3_fail; do
  T=${f%_*}; V="/tmp/cb2-self-$f.json"
  bash "$FIX/run/check.sh" "$T" "$FIX/selftest/$f" "$V" "/tmp/cb2-self-$f.excerpts.txt" >/dev/null 2>&1
  FAILED=$(python3 -c "import json,sys
try:
    d=json.load(open('$V')); print(','.join(sorted(k for k,v in d['checks'].items() if not v['pass'])))
except Exception:
    print('NO-VERDICT')" 2>/dev/null)
  if [ "$FAILED" = "${EXPECT[$f]}" ]; then RES=agree; else RES=DISAGREE; BAD=1; fi
  echo "$f: $RES failed=[$FAILED] expected=[${EXPECT[$f]}]"
done
exit $BAD
