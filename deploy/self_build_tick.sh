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
if [ "${YM_BUILDER:-claude}" = "codex" ]; then
  [ -f "$HOME/.codex/auth.json" ] || [ -f /root/.codex/auth.json ] || { echo "$(date -u +%FT%TZ) codex builder selected but no ~/.codex/auth.json — tick skipped"; exit 0; }
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
: "${CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN}"
: "${YANTRIKDB_ACC_GIT_TOKEN:?need YANTRIKDB_ACC_GIT_TOKEN}"
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL ANTHROPIC_API_KEY
export CARGO_HOME=/root/.cargo RUSTUP_HOME=/root/.rustup
# cron runs with a minimal PATH; claude lives in /usr/local/bin, cargo in /root/.cargo/bin.
export PATH="/usr/local/bin:/root/.cargo/bin:$PATH"

echo "=========================================================="
echo "$(date -u +%FT%TZ) self-build tick start"

# QUOTA GUARD: the builder shares Pranab's Max subscription. If the 5-hour window is hot
# (>= YM_BUILDER_HOT_PCT, default 85%), defer — his interactive hours outrank autonomous builds.
# Runs BEFORE the goal pop, so nothing is consumed; cron retries in 6h.
HOT="${YM_BUILDER_HOT_PCT:-85}"
UTIL=$(curl -4 -s -m 12 -H "Authorization: Bearer $CLAUDE_CODE_OAUTH_TOKEN"   -H "anthropic-beta: oauth-2025-04-20" https://api.anthropic.com/api/oauth/usage 2>/dev/null   | python3 -c "import json,sys
try: print(int(json.load(sys.stdin).get(\"five_hour\",{}).get(\"utilization\",0)))
except Exception: print(0)" 2>/dev/null || echo 0); UTIL=$(printf "%s" "$UTIL" | tail -1)
if [ "${UTIL:-0}" -ge "$HOT" ]; then
  echo "$(date -u +%FT%TZ) quota guard: Max 5h window at ${UTIL}% (>= ${HOT}%) — deferring build to after reset"
  exit 0
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
  git clone -q https://github.com/yantrikos/yantrik-mind.git "$W" 2>/dev/null || { echo "self-review: clone failed — skip tick"; rm -rf "$W" "$CH"; exit 0; }
  cd "$W"
  RECENT="$(git log --oneline -20 --pretty='- %s' 2>/dev/null || true)"
  GOAL="$(timeout 480 claude -p "You are yantrik-mind reviewing your own codebase to pick your next improvement.

NORTH STAR: make the typed-memory moat — typed beliefs, confidence scores, contradiction detection, Bayesian revision, consolidation, reflection — more CORRECT, more ROBUST, or more USEFUL in the live chat product. Those are the things a flat-text RAG assistant structurally cannot do; that is where your value compounds. Favor closing a real gap or hardening correctness over adding surface commands or cosmetic cleanup.

Recently done (do NOT repeat or trivially restate these):
$RECENT

Read the core moat crates (crates/mind-conversation, crates/mind-memory, crates/mind-core) and propose exactly ONE concrete, minimal, genuinely high-value improvement to implement next as a single focused PR. It MUST be self-contained, keep the build green WITH a test, be reversible, and MUST NOT touch crates/mind-governance. Reply with ONLY the goal as one imperative sentence — no preamble, no markdown, no quotes." \
    --allowedTools "Read" --output-format text 2>/dev/null | awk 'NF{l=$0} END{print l}' | tr -d '\r' || true)"
  cd /; rm -rf "$W" "$CH"; trap - EXIT
  [ -n "$GOAL" ] && echo "self-review proposed a goal" || echo "self-review produced no goal"
fi

if [ -z "$GOAL" ]; then echo "no goal derived — skip"; exit 0; fi
echo "TICK GOAL: $GOAL"

# Run the build with auto-merge enabled (self_improve still gates every merge).
EVLOG=/var/lib/yantrik-mind/evolution.log
set +e
OUT="$(YM_AUTOMERGE=1 bash /root/codes/yantrik-mind/deploy/self_improve.sh "$GOAL" 2>&1)"
set -e
echo "$OUT"
# Builder unavailable (credit/quota/auth) — the goal never got a fair attempt, so DON'T let the pop
# consume it. Re-queue it (if it came from the human queue) and log a distinct outcome; otherwise a
# dry builder silently drains the whole queue over successive ticks (4/day) with nothing to show.
if echo "$OUT" | grep -qiE "credit balance is too low|usage limit|quota exceeded|invalid api key|invalid authentication credentials|authentication_error|oauth token.*expired|401 unauthorized"; then
  echo "$(date -u +%FT%TZ) | build | BUILDER-NO-CREDIT | $GOAL" >> "$EVLOG"
  if [ "$FROM_QUEUE" = "1" ] && ! grep -qxF "$GOAL" "$GOALS" 2>/dev/null; then
    printf '%s\n' "$GOAL" >> "$GOALS"
    echo "==> builder unavailable — goal re-queued (not consumed)"
  fi
  tg_alert builder "builder unavailable mid-run (credit/quota/auth) — goal re-queued; check token + Max window"
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
