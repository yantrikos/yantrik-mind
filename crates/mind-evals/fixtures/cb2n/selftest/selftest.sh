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
# The request cap: one number, carried by the run state, refusing to change mid-reading.
if bash "$FIX/selftest/cap_cases.sh"; then echo "cap_cases: agree"; else echo "cap_cases: DISAGREE"; BAD=1; fi
if bash "$FIX/selftest/model_cases.sh"; then echo "model_cases: agree"; else echo "model_cases: DISAGREE"; BAD=1; fi
# The grader must FAIL a hostile artifact, not crash on one. Structural, and it says so.
if python3 "$FIX/selftest/scan_checker_guard.py" "$FIX/checks/check_web.mjs"; then echo "checker_guard: agree"; else echo "checker_guard: DISAGREE"; BAD=1; fi
# Four fixture files are BAKED into images, so editing one changes nothing until a rebuild.
# rederive.sh proves the tree matches the patch; this proves the images match the tree.
if bash "$FIX/selftest/image_freshness.sh"; then echo "image_freshness: agree"; else echo "image_freshness: DISAGREE"; BAD=1; fi
# The receipt decision, driven through every classification without a graded leg.
if python3 "$FIX/selftest/verdict_cases.py"; then echo "verdict_cases: agree"; else echo "verdict_cases: DISAGREE"; BAD=1; fi
# The check SCORE, driven through every way a denominator can lie, without a graded leg.
if python3 "$FIX/selftest/score_cases.py" >/dev/null; then echo "score_cases: agree"; else echo "score_cases: DISAGREE"; BAD=1; fi
S=$(mktemp -d /tmp/cb2-tree-self-XXXX)
mkfifo "$S/pipe"
TREE=$(timeout -k 1 5 python3 "$FIX/tools/tree_hash.py" "$S"); TREE_RC=$?
timeout -k 1 5 bash "$FIX/run/check.sh" t3 "$S" "$S/verdict.json" "$S/excerpts.txt" >/dev/null 2>&1; CHECK_RC=$?
rm -rf "$S"
if [ $TREE_RC -eq 0 ] && [ $CHECK_RC -eq 2 ] && echo "$TREE" | grep -Eq '^[0-9a-f]{64} files=0 bytes=0 symlinks=0 specials=1$'; then
  echo "tree_special: agree detected=1 checker_refused=true"
else
  echo "tree_special: DISAGREE tree_rc=$TREE_RC checker_rc=$CHECK_RC tree=[$TREE]"; BAD=1
fi
exit $BAD
