#!/usr/bin/env bash
# yantrik-mind AUTONOMOUS SELF-BUILD TICK — the cron entrypoint. Derives ONE goal, then runs
# self_improve.sh in auto-merge mode (which still gates every merge on compile + tests + small-diff
# + no-sensitive-paths, and ABORTS on any harm-gate touch).
#
# Goal source, in order:
#   1. A human-queued goal: first non-comment line of /var/lib/yantrik-mind/selfbuild-goals.txt (popped).
#   2. Self-review: Claude reads crates/mind-* and proposes ONE new improvement (avoiding recent work).
#
# Kill-switch: touch /var/lib/yantrik-mind/SELF_IMPROVE_OFF to halt all self-build.
set -euo pipefail

KILL=/var/lib/yantrik-mind/SELF_IMPROVE_OFF
[ -f "$KILL" ] && { echo "$(date -u +%FT%TZ) kill-switch present — tick skipped"; exit 0; }

# ALERT: a dead self-build loop must SPEAK, not rot silently (it once sat broken for 4 days).
# Sends a Telegram message to the active chat, at most once per failure-kind per 24h.
tg_alert() { # $1 = failure kind (slug), $2 = message
  local kind="$1" msg="$2" stamp="/var/lib/yantrik-mind/.selfbuild_alert_$1"
  if [ -f "$stamp" ] && [ "$(( $(date +%s) - $(stat -c %Y "$stamp" 2>/dev/null || echo 0) ))" -lt 86400 ]; then
    return 0
  fi
  touch "$stamp"
  local tok chat
  tok="$(. /etc/yantrik-mind.env 2>/dev/null; printf '%s' "${YM_TELEGRAM_TOKEN:-}")"
  chat="$(cat /var/lib/yantrik-mind/tg_offset.active_chat 2>/dev/null || true)"
  [ -n "$tok" ] && [ -n "$chat" ] && curl -s -m 10 "https://api.telegram.org/bot${tok}/sendMessage" \
    --data-urlencode "chat_id=${chat}" --data-urlencode "text=🛠️ self-build: ${msg}" >/dev/null 2>&1 || true
}
# Any unexpected error path (set -e) also speaks before dying.
trap 'rc=$?; [ $rc -ne 0 ] && tg_alert crash "tick crashed (exit $rc) — check selfbuild-cron.log"; exit $rc' ERR

# AUTH PREFLIGHT — before drawing a treasury pass or popping a goal. Builder-aware: the CODEX
# builder authenticates via ~/.codex (self-refreshing), so it does NOT need the Claude OAuth token.
# The Claude preflight only gates the Claude builder — a dead Claude token must not block a Codex tick.
set -a; . /etc/yantrik-mind.env 2>/dev/null || true; set +a
CODEX_AUTH_HOME="${CODEX_HOME:-${HOME:-/root}/.codex}"
if [ "${YM_BUILDER:-claude}" = "qwen" ]; then
  # The qwen builder authenticates with QWEN_API_KEY, not the Claude OAuth token, so a dead Claude
  # token must not block it. Goal GENERATION below still needs a working CLI, which is why the qwen
  # builder also overrides the generator - see the self-review block.
  : "${QWEN_API_KEY:?qwen builder selected but QWEN_API_KEY is unset}"
elif [ "${YM_BUILDER:-claude}" = "codex" ]; then
  [ -f "$CODEX_AUTH_HOME/auth.json" ] || [ -f /root/.codex/auth.json ] || { echo "$(date -u +%FT%TZ) codex builder selected but no Codex auth.json — tick skipped"; exit 0; }
else
  : "${CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN}"
  AUTH_HTTP=$(curl -s -o /dev/null -w "%{http_code}" -m 12 \
    -H "Authorization: Bearer $CLAUDE_CODE_OAUTH_TOKEN" \
    -H "anthropic-beta: oauth-2025-04-20" https://api.anthropic.com/api/oauth/usage 2>/dev/null || echo 000)
  if [ "$AUTH_HTTP" = "401" ] || [ "$AUTH_HTTP" = "403" ]; then
    echo "$(date -u +%FT%TZ) auth preflight: OAuth token rejected (HTTP $AUTH_HTTP) — tick skipped, nothing consumed"
    tg_alert token "builder OAuth token expired — self-build paused until it's refreshed (copy a fresh session token)"
    exit 0
  fi
  # transient network failure (000/5xx): proceed — the hot-window guard below degrades gracefully
fi

# TREASURY: draw one selfbuild pass from the shared daily envelope (budget.json — same file the
# Rust engine meters). Dry = skip-with-log; the goal queue is untouched, the pass runs tomorrow.
BUDGET=/var/lib/yantrik-mind/budget.json
if [ -f "$BUDGET" ]; then
  DRAW=$(python3 - "$BUDGET" <<'PY'
import json, sys, datetime
p = sys.argv[1]
try:
    b = json.load(open(p))
except Exception:
    sys.exit(0)  # unreadable -> fail open (the box-side gates still hold)
today = datetime.date.today().isoformat()
if b.get("date") != today:
    b["date"], b["spent"], b["skipped"] = today, {}, {}
cap = b.get("envelope", {}).get("selfbuild", 4)
used = b.get("spent", {}).get("selfbuild", 0)
ok = used < cap
bucket = "spent" if ok else "skipped"
b.setdefault(bucket, {})["selfbuild"] = b.get(bucket, {}).get("selfbuild", 0) + 1
json.dump(b, open(p, "w"), indent=1)
print("ok" if ok else "dry")
PY
)
  if [ "$DRAW" = "dry" ]; then
    echo "$(date -u +%FT%TZ) treasury: selfbuild envelope dry — tick skipped (goal queue untouched)"
    exit 0
  fi
fi

# Single-flight: never let a new tick stack on top of a still-running one.
exec 9>/var/lib/yantrik-mind/.selfbuild.lock
flock -n 9 || { echo "$(date -u +%FT%TZ) another tick is still running — skip"; exit 0; }

set -a; . /etc/yantrik-mind.env 2>/dev/null || true; set +a
: "${YANTRIKDB_ACC_GIT_TOKEN:?need YANTRIKDB_ACC_GIT_TOKEN}"
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL ANTHROPIC_API_KEY
export CARGO_HOME=/root/.cargo RUSTUP_HOME=/root/.rustup
# cron runs with a minimal PATH; claude lives in /usr/local/bin, cargo in /root/.cargo/bin.
export PATH="/usr/local/bin:/root/.cargo/bin:$PATH"

echo "=========================================================="
echo "$(date -u +%FT%TZ) self-build tick start"

# QUOTA GUARD: only the Claude builder shares Pranab's Max subscription. Codex and Qwen have their
# own auth/budget paths and must not dereference an absent Claude token. Runs BEFORE the goal pop,
# so nothing is consumed; cron retries in 6h.
if [ "${YM_BUILDER:-claude}" = "claude" ]; then
  HOT="${YM_BUILDER_HOT_PCT:-85}"
  UTIL=$(curl -4 -s -m 12 -H "Authorization: Bearer $CLAUDE_CODE_OAUTH_TOKEN"   -H "anthropic-beta: oauth-2025-04-20" https://api.anthropic.com/api/oauth/usage 2>/dev/null   | python3 -c "import json,sys
try: print(int(json.load(sys.stdin).get(\"five_hour\",{}).get(\"utilization\",0)))
except Exception: print(0)" 2>/dev/null || echo 0); UTIL=$(printf "%s" "$UTIL" | tail -1)
  if [ "${UTIL:-0}" -ge "$HOT" ]; then
    echo "$(date -u +%FT%TZ) quota guard: Max 5h window at ${UTIL}% (>= ${HOT}%) — deferring build to after reset"
    exit 0
  fi
else
  echo "quota guard: Claude Max window does not apply to ${YM_BUILDER:-claude} builder"
fi

GOALS=/var/lib/yantrik-mind/selfbuild-goals.txt
GOAL=""

# 1) human-queued goal (pop the first real line)
FROM_QUEUE=0
if [ -s "$GOALS" ]; then
  GOAL="$(grep -vE '^[[:space:]]*(#|$)' "$GOALS" | head -1 || true)"
  if [ -n "$GOAL" ]; then
    grep -vxF "$GOAL" "$GOALS" > "$GOALS.tmp" 2>/dev/null && mv "$GOALS.tmp" "$GOALS" || true
    FROM_QUEUE=1
    echo "goal source: human queue"
  fi
fi

# 2) self-review: Claude proposes ONE new goal by reading the code (read-only), avoiding recent work
if [ -z "$GOAL" ]; then
  echo "goal source: self-review (deriving a goal via claude)"
  W="$(mktemp -d /root/codes/ymreview.XXXXXX)"; CH="$(mktemp -d /opt/yantrik-mind/ymrh.XXXXXX)"
  trap 'rm -rf "$W" "$CH"' EXIT
  export HOME="$CH"
  if [ "${YM_BUILDER:-claude}" = "codex" ]; then
    export CODEX_HOME="$CODEX_AUTH_HOME"
  fi
  git clone -q https://github.com/yantrikos/yantrik-mind.git "$W" 2>/dev/null || { echo "self-review: clone failed — skip tick"; rm -rf "$W" "$CH"; exit 0; }
  cd "$W"
  # 40, not 20: self-improve subjects are enormous run-on sentences, so a 20-commit window covers
  # very little real history and already-built work falls off the end.
  RECENT="$(git log --oneline -40 --pretty='- %s' 2>/dev/null | cut -c1-160 || true)"
  # CAPABILITY INVENTORY — the fix for re-proposing what already exists. Measured 2026-08-03: the
  # loop proposed `UncertaintyReason {Decayed|Contradicted|Sparse|LowPrior}` which was already
  # implemented AND already threaded into the grounding prompt, and proposed belief-text
  # normalisation for the THIRD time (it had merged twice already, 349d2ca and 8569220 with
  # byte-identical subjects). A commit log of paraphrased subjects cannot prevent that; a list of
  # what the code actually CONTAINS can.
  INVENTORY="$( { echo "capability modules (crates/mind-conversation/src):";
      ls crates/mind-conversation/src/*.rs 2>/dev/null | xargs -n1 basename | sed 's/\.rs$//' | tr '
' ' ';
      echo; echo "typed vocabulary (public enums/structs in mind-types):";
      grep -rhoE '^pub (enum|struct) [A-Za-z_]+' crates/mind-types/src/*.rs 2>/dev/null | awk '{print $3}' | sort -u | tr '
' ' ';
      echo; echo "operator verbs already wired:";
      grep -ohE '^\s+"[a-z_]+"( \| "[a-z_]+")* =>' crates/mind-conversation/src/lib.rs 2>/dev/null | grep -oE '"[a-z_]+"' | tr -d '"' | sort -u | tr '
' ' '; echo; } 2>/dev/null | cut -c1-1400 )"
  # TWO-TIER FITNESS: hand the goal generator the mind's REAL outcome numbers. Without these it has
  # only the north star and a commit list, so it optimises for "made the tests pass" and its merged
  # work stays cosmetic. Tests are a GATE; these numbers are the TARGET. Fail-soft: no metrics, no block.
  FITNESS="$(curl -s -m 20 -H "Authorization: Bearer $(cat /var/lib/yantrik-mind/console.token 2>/dev/null)"       -X POST "http://127.0.0.1:${YM_CONTROL_PORT:-8077}/cli" -d "fitness_prompt" 2>/dev/null || true)"
  [ -n "$FITNESS" ] && echo "self-review: fitness block attached ($(printf %s "$FITNESS" | wc -l) lines)"
  # HANDOFF: what my previous ticks did, INCLUDING what never merged. `git log` shows only merged
  # commits, so without this the loop cannot see its own aborts/drafts and re-proposes doomed goals.
  HANDOFF="$(curl -s -m 20 -H "Authorization: Bearer $(cat /var/lib/yantrik-mind/console.token 2>/dev/null)"       -X POST "http://127.0.0.1:${YM_CONTROL_PORT:-8077}/cli" -d "handoff_prompt" 2>/dev/null || true)"
  [ -n "$HANDOFF" ] && echo "self-review: handoff attached ($(printf %s "$HANDOFF" | wc -l) lines)"
  # The GOAL GENERATOR rides the selected builder too. Otherwise Codex/Qwen can pass their auth
  # preflight and still die here on an unrelated Claude credential before their builder is reached.
  # That exact split-brain failure stalled six consecutive ticks on 2026-07-27/28.
  GOAL_PROMPT="You are yantrik-mind reviewing your own codebase to pick your next improvement.

$FITNESS
$HANDOFF
WHAT ALREADY EXISTS (do NOT propose building any of this again — extend or fix it instead):
$INVENTORY

NORTH STAR: make the typed-memory moat — typed beliefs, confidence scores, contradiction detection, Bayesian revision, consolidation, reflection — more CORRECT, more ROBUST, or more USEFUL in the live chat product. Those are the things a flat-text RAG assistant structurally cannot do; that is where your value compounds. Favor closing a real gap or hardening correctness over adding surface commands or cosmetic cleanup.

Recently done (do NOT repeat or trivially restate these):
$RECENT

Read the core moat crates (crates/mind-conversation, crates/mind-memory, crates/mind-core) and propose exactly ONE concrete, minimal, genuinely high-value improvement to implement next as a single focused PR. It MUST be self-contained, keep the build green WITH a test, be reversible, and MUST NOT touch crates/mind-governance. Reply with ONLY the goal as one imperative sentence — no preamble, no markdown, no quotes."
  if [ "${YM_BUILDER:-claude}" = "qwen" ]; then
    export ANTHROPIC_BASE_URL="https://token-plan.ap-southeast-1.maas.aliyuncs.com/apps/anthropic"
    export ANTHROPIC_AUTH_TOKEN="$QWEN_API_KEY"
    export ANTHROPIC_MODEL="${YM_QWEN_MODEL:-qwen3.8-max}"
    GOAL="$(timeout 480 claude -p "$GOAL_PROMPT" --allowedTools "Read" --output-format text 2>/dev/null | awk 'NF{l=$0} END{print l}' | tr -d '\r' || true)"
  elif [ "${YM_BUILDER:-claude}" = "codex" ]; then
    GOAL="$(timeout 480 codex exec --skip-git-repo-check --sandbox read-only "$GOAL_PROMPT" </dev/null 2>/dev/null | awk 'NF{l=$0} END{print l}' | tr -d '\r' || true)"
  else
    GOAL="$(timeout 480 claude -p "$GOAL_PROMPT" --allowedTools "Read" --output-format text 2>/dev/null | awk 'NF{l=$0} END{print l}' | tr -d '\r' || true)"
  fi
  cd /; rm -rf "$W" "$CH"; trap - EXIT
  [ -n "$GOAL" ] && echo "self-review proposed a goal" || echo "self-review produced no goal"
fi

if [ -z "$GOAL" ]; then echo "no goal derived — skip"; exit 0; fi

# GOAL SANITY GATE: the self-review capture is `GOAL="$(claude -p ...)"` — when the CLI fails, its
# ERROR goes to stdout and becomes "the goal". That is exactly how five auth-failure PRs (#41–#48)
# got titled "self-improve: Failed to authenticate. API Error: 401 …" and AUTO-MERGED: the (working)
# codex builder dutifully "implemented" the error message and the gates saw green tests. An error
# message is not a goal — refuse anything that looks like one, log it where vigilance_scan looks
# (the signatures now include these), and alert.
EVLOG=/var/lib/yantrik-mind/evolution.log
# Match ERROR-MESSAGE SHAPES, not topic words — "add a rate limiter to web_fetch" or "return 401 on
# bad token" are legitimate goals; "Rate limit exceeded" / "API Error: 401" are not.
if printf '%s' "$GOAL" | grep -qiE "api error|failed to authenticate|invalid api key|invalid authentication|access token has been revoked|token (has )?expired|error:? ?(40[139]|429|5[0-9][0-9])|http (40[139]|429)|rate limit (exceeded|reached)|credit balance|usage limit|quota exceeded|command not found|no such file or directory"; then
  echo "$(date -u +%FT%TZ) GOAL REJECTED (CLI/API error captured as goal, not a real goal): $GOAL"
  echo "$(date -u +%FT%TZ) | build | GOAL-REJECTED-ERRORTEXT | $GOAL" >> "$EVLOG"
  curl -s -m 15 -H "Authorization: Bearer $(cat /var/lib/yantrik-mind/console.token 2>/dev/null)"     -X POST "http://127.0.0.1:${YM_CONTROL_PORT:-8077}/cli"     -d "handoff_write GOAL-REJECTED|(goal generation failed)|The goal generator returned an API/CLI error instead of a goal - builder auth is probably broken. Nothing was attempted." >/dev/null 2>&1 || true
  tg_alert goalerr "self-review 'goal' was an error message ($(printf '%s' "$GOAL" | head -c 120)) — claude auth likely broken; tick skipped, nothing PR'd"
  exit 0
fi
echo "TICK GOAL: $GOAL"

# Run the build with auto-merge enabled (self_improve still gates every merge).
set +e
OUT="$(YM_AUTOMERGE=1 bash /root/codes/yantrik-mind/deploy/self_improve.sh "$GOAL" 2>&1)"
IMPROVE_RC=$?
set -e
echo "$OUT"
# Builder unavailable (credit/quota/auth) — the goal never got a fair attempt, so DON'T let the pop
# consume it. Re-queue it (if it came from the human queue) and log a distinct outcome; otherwise a
# dry builder silently drains the whole queue over successive ticks (4/day) with nothing to show.
AUTH_FAILURE=0
if echo "$OUT" | grep -qiE "credit balance is too low|usage limit|quota exceeded|invalid api key|authentication_error|oauth token.*expired|401 unauthorized|failed to authenticate|access token has been revoked|invalid authentication credentials"; then
  AUTH_FAILURE=1
  echo "$(date -u +%FT%TZ) | build | BUILDER-NO-CREDIT | $GOAL" >> "$EVLOG"
  if [ "$FROM_QUEUE" = "1" ] && ! grep -qxF "$GOAL" "$GOALS" 2>/dev/null; then
    printf '%s\n' "$GOAL" >> "$GOALS"
    echo "==> builder unavailable — goal re-queued (not consumed)"
  fi
  tg_alert builder "builder unavailable mid-run (credit/quota/auth) — goal re-queued; check token + Max window"
fi
# A non-auth builder crash or timeout is equally not a fair attempt. `self_improve` emits this exact
# marker only before staging; do not requeue compile/harm-gate failures that earned a terminal handoff.
if [ "$AUTH_FAILURE" = "0" ] && [ "$IMPROVE_RC" -ne 0 ] && echo "$OUT" | grep -q "ABORT-BUILDER:"; then
  echo "$(date -u +%FT%TZ) | build | BUILDER-FAILED | $GOAL" >> "$EVLOG"
  if [ "$FROM_QUEUE" = "1" ] && ! grep -qxF "$GOAL" "$GOALS" 2>/dev/null; then
    printf '%s\n' "$GOAL" >> "$GOALS"
    echo "==> builder failed before staging — goal re-queued (not consumed)"
  fi
  tg_alert builderfail "builder crashed or timed out before staging — queued goal preserved; check selfbuild-cron.log"
fi
# CRASH SAFETY: an empty OUT means self_improve died before producing anything (workdir deleted,
# OOM, kill) — the goal never got a fair attempt, so put it back (dup-guarded).
if [ -z "$OUT" ] && [ "$FROM_QUEUE" = "1" ] && ! grep -qxF "$GOAL" "$GOALS" 2>/dev/null; then
  printf '%s
' "$GOAL" >> "$GOALS"
  echo "==> empty build output — goal re-queued (not consumed)"
  tg_alert emptyout "build produced no output - goal re-queued; check selfbuild-cron.log"
fi
echo "$(date -u +%FT%TZ) self-build tick done"
