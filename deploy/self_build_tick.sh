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

set -a; . /etc/yantrik-mind.env 2>/dev/null || true; set +a
: "${CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN}"
: "${YANTRIKDB_ACC_GIT_TOKEN:?need YANTRIKDB_ACC_GIT_TOKEN}"
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL
export CARGO_HOME=/root/.cargo RUSTUP_HOME=/root/.rustup PATH="/root/.cargo/bin:$PATH"

echo "=========================================================="
echo "$(date -u +%FT%TZ) self-build tick start"

GOALS=/var/lib/yantrik-mind/selfbuild-goals.txt
GOAL=""

# 1) human-queued goal (pop the first real line)
if [ -s "$GOALS" ]; then
  GOAL="$(grep -vE '^[[:space:]]*(#|$)' "$GOALS" | head -1 || true)"
  if [ -n "$GOAL" ]; then
    grep -vxF "$GOAL" "$GOALS" > "$GOALS.tmp" 2>/dev/null && mv "$GOALS.tmp" "$GOALS" || true
    echo "goal source: human queue"
  fi
fi

# 2) self-review: Claude proposes ONE new goal by reading the code (read-only), avoiding recent work
if [ -z "$GOAL" ]; then
  W="$(mktemp -d /root/codes/ymreview.XXXXXX)"; CH="$(mktemp -d /opt/yantrik-mind/ymrh.XXXXXX)"
  trap 'rm -rf "$W" "$CH"' EXIT
  export HOME="$CH"
  git clone -q https://github.com/yantrikos/yantrik-mind.git "$W"; cd "$W"
  RECENT="$(git log --oneline -20 --pretty='- %s' 2>/dev/null || true)"
  GOAL="$(timeout 300 claude -p "You are yantrik-mind reviewing your own codebase to pick your next improvement.

Recently done (do NOT repeat or trivially restate these):
$RECENT

Read crates/mind-* and propose exactly ONE concrete, minimal, genuinely high-value improvement to implement next as a single focused PR. It must be self-contained, keep the build green, and must NOT touch crates/mind-governance. Prefer a real capability or correctness gain over cosmetic cleanup. Reply with ONLY the goal as one imperative sentence — no preamble, no markdown, no quotes." \
    --allowedTools "Read" --output-format text 2>/dev/null | awk 'NF{l=$0} END{print l}' | tr -d '\r')"
  cd /; rm -rf "$W" "$CH"; trap - EXIT
  echo "goal source: self-review"
fi

if [ -z "$GOAL" ]; then echo "no goal derived — skip"; exit 0; fi
echo "TICK GOAL: $GOAL"

# Run the build with auto-merge enabled (self_improve still gates every merge).
YM_AUTOMERGE=1 bash /root/codes/yantrik-mind/deploy/self_improve.sh "$GOAL"
echo "$(date -u +%FT%TZ) self-build tick done"
