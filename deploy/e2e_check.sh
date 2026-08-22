#!/usr/bin/env bash
# E2E — does the mind actually COMPLETE things, end to end, on the deployed box?
#
# Every failure of 2026-08-20/21 was an integration failure that ~600 green unit tests could not
# see, and every one was found by a person hitting it:
#
#   - the agent loop halted on the FIRST repeated tool call, so every multi-step task died at step 1
#   - the planner had no model behind it at all (role pools never inherited the private lane)
#   - the fast path denied capabilities it held, then promised work it could not do
#   - a spoken reply arrived as a 95-second markdown briefing
#
# Unit tests check that a function is right. Nothing checked that a TASK FINISHES. That is the gap
# this closes: each scenario below is one thing a person would actually ask for, asserted on the
# deployed binary, with a real answer that cannot be faked from memory.
#
# Usage:  bash deploy/e2e_check.sh            (from anywhere with ssh access to the box)
#         YM_E2E_HOST=root@192.168.4.90 bash deploy/e2e_check.sh
#
# Exit code is the number of failures, so it can gate a deploy.
set -u

HOST="${YM_E2E_HOST:-root@192.168.4.90}"
KEY="${YM_E2E_KEY:-$HOME/.ssh/id_deploy}"
PASS=0
FAIL=0

say() { printf '%s\n' "$*"; }

# Run one console command on the box and echo the reply.
#
# The payload is base64'd on the way over. The first version relied on ssh passing positional
# parameters to the remote shell — it does not, it joins its arguments into one command string — so
# every message arrived EMPTY and the checks "failed" against a mind that was never asked anything.
# Fitting: the harness written to catch integration bugs shipped with one, and it failed in exactly
# the way it exists to catch — plausible output, nothing actually executed.
ym() {
  local payload secs inner
  payload=$(printf '%s' "$1" | base64 | tr -d '\n')
  secs="${2:-300}"
  inner="T=\$(cat /var/lib/yantrik-mind/console.token); echo '$payload' | base64 -d | timeout $secs curl -s -m $((secs - 10)) -H \"Authorization: Bearer \$T\" --data-binary @- http://127.0.0.1:8077/cli"
  ssh -i "$KEY" -o StrictHostKeyChecking=no "$HOST" "$inner" 2>/dev/null
}

# check <name> <expected-substring> <reply>
check() {
  local name="$1" want="$2" got="$3"
  if printf '%s' "$got" | grep -qi -- "$want"; then
    say "  PASS  $name"
    PASS=$((PASS + 1))
  else
    say "  FAIL  $name"
    say "        wanted to see: $want"
    say "        got: $(printf '%s' "$got" | head -c 220 | tr '\n' ' ')"
    FAIL=$((FAIL + 1))
  fi
}

# refute <name> <forbidden-substring> <reply>
refute() {
  local name="$1" bad="$2" got="$3"
  if printf '%s' "$got" | grep -qi -- "$bad"; then
    say "  FAIL  $name"
    say "        must NOT say: $bad"
    say "        got: $(printf '%s' "$got" | head -c 220 | tr '\n' ' ')"
    FAIL=$((FAIL + 1))
  else
    say "  PASS  $name"
    PASS=$((PASS + 1))
  fi
}

say "E2E — completion checks against $HOST"
say ""

# 1. MULTI-STEP. The one that was broken: three fetches and a comparison. The loop used to halt at
#    the first repeated call and return a single number, which is a plausible-looking wrong answer.
say "multi-step tool work"
R=$(ym 'chat Fetch these three and say which had the most downloads last week: https://api.npmjs.org/downloads/point/last-week/saga-mcp then https://api.npmjs.org/downloads/point/last-week/brainstorm-mcp then https://api.npmjs.org/downloads/point/last-week/truenas-mcp' 600)
check "all three packages reached" "truenas-mcp" "$R"
check "the comparison is stated" "brainstorm-mcp" "$R"

# 2. PLANNER. A goal has to become steps and PERSIST. This was dead for weeks behind a message that
#    blamed the user's phrasing.
say ""
say "planner takes a standing order"
R=$(ym 'schedule daily 09:30 :: e2e canary, fetch https://api.npmjs.org/downloads/point/last-week/saga-mcp and report the number' 400)
check "order accepted" "Standing order set" "$R"
R=$(ym 'orders' 200)
check "order persisted" "e2e canary" "$R"
# Clean up after ourselves: a test that leaves a daily job behind is a test that pollutes production.
ID=$(printf '%s' "$R" | grep -o 'sched:[a-f0-9-]*' | tail -1)
[ -n "$ID" ] && ym "orders cancel $ID" 200 >/dev/null

# 3. CAPABILITY HONESTY. It denied watching video while holding the tool, and denied market data
#    while holding a quote tool. Both are false statements about itself.
say ""
say "does not deny what it has"
R=$(ym 'chat Can you watch a live video stream?' 400)
refute "does not deny watching video" "can't watch" "$R"
R=$(ym 'chat What is the Nifty at right now?' 400)
refute "does not deny market data" "don't have live" "$R"
refute "does not promise instead of doing" "give me a moment" "$R"

# 4. TRADING LOOP. The universe, the filters and the decision have to survive a real run. This does
#    NOT assert that it trades — declining is a valid answer — only that the pipeline completes and
#    reports its reasoning either way.
say ""
say "trading loop completes"
R=$(ym 'hunt' 500)
check "scanned a universe" "movers scanned" "$R"
check "showed its filtering" "filtered out" "$R"

# 5. POSITIONS. Every open position must be accounted for by a rule, not by inertia.
say ""
say "open positions are managed"
R=$(ym 'follow' 300)
check "follow reports on the book" "FOLLOW" "$R"

say ""
say "-----"
say "PASS $PASS   FAIL $FAIL"
exit "$FAIL"
