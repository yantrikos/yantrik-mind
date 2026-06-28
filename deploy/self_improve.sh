#!/usr/bin/env bash
# yantrik-mind SELF-BUILD — the companion improves its own code.
#
# Clone the repo → let Claude Code (on the Max-plan subscription) implement a goal → enforce the
# bounds → open a DRAFT PR as `yantrikdb` for review/merge. Run on CT173 as the `yantrikmind` user
# (non-root, so Claude can use its full tools incl. cargo to self-verify).
#
# BOUNDS (the safety contract):
#   1. HARM-GATE CARVE-OUT — never modifies crates/mind-governance/** (the one inviolable wall stays
#      human-only). If Claude touches it, the run ABORTS with no PR.
#   2. COMPILE-GATE — if any .rs changed, `cargo build` must pass; a red build never opens a PR.
#   3. DRAFT PR only (fork-less: branch on origin, as collaborator yantrikdb). Pranab/maintainer
#      merges. (Graduating to auto-merge-on-green is a later, deliberate step.)
#   4. KILL-SWITCH — `touch /var/lib/yantrik-mind/SELF_IMPROVE_OFF` halts it.
#   5. One branch + one PR per run.
#
# Usage:  self_improve.sh "<concrete improvement goal>"
set -euo pipefail

GOAL="${1:?usage: self_improve.sh '<improvement goal>'}"
KILL=/var/lib/yantrik-mind/SELF_IMPROVE_OFF
[ -f "$KILL" ] && { echo "kill-switch present ($KILL) — self-build disabled"; exit 0; }

# Auth: subscription token for Claude, yantrikdb token for the push. (root:600 env.)
set -a; . /etc/yantrik-mind.env 2>/dev/null || true; set +a
: "${CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN}"
: "${YANTRIKDB_ACC_GIT_TOKEN:?need YANTRIKDB_ACC_GIT_TOKEN}"
# Force real Claude (drop any MiniMax override that may be in the env).
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL

# Clone as a SIBLING of the path-dep repos (../yantrikdb, ../yantrik-companion live under /root/codes)
# so the relative path deps resolve and the compile-gate can actually build. Claude's config goes in a
# SEPARATE HOME so its dotfiles never pollute the git tree. Reuse the warm release target + registry.
WORK="$(mktemp -d /root/codes/ymbuild.XXXXXX)"          # the repo clone (sibling of the path deps)
CFGHOME="$(mktemp -d /opt/yantrik-mind/ymhome.XXXXXX)"  # Claude config, outside the git tree
trap 'rm -rf "$WORK" "$CFGHOME"' EXIT
export HOME="$CFGHOME"
export CARGO_HOME=/root/.cargo                          # warm crates registry (avoid re-download)
export PATH="/root/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/root/codes/yantrik-mind/target # warm release target -> fast compile-gate

echo "==> clone (sibling of path-dep repos)"
git clone -q https://github.com/yantrikos/yantrik-mind.git "$WORK"
cd "$WORK"
git config user.name "yantrikdb"
git config user.email "yantrikdb@gmail.com"
BR="self/$(date +%s)"
git checkout -q -b "$BR"

echo "==> Claude Code (subscription) implementing: $GOAL"
timeout 900 claude -p "You are improving the yantrik-mind codebase (you are the companion improving your own code). GOAL: $GOAL

Rules: make a focused, minimal, idiomatic change. Do NOT modify anything under crates/mind-governance (the harm-gate is off-limits). If you change Rust, keep it compiling. Add or update a test when it makes sense. Do not touch secrets or CI auth." \
  --permission-mode acceptEdits --allowedTools "Write Edit Read" --output-format text 2>&1 | tail -25

echo "==> enforce bounds"
git add -A   # stage everything incl. NEW files (git diff alone ignores untracked)
if git diff --cached --quiet; then
  echo "no changes produced — nothing to PR"
  exit 0
fi
if git diff --cached --name-only | grep -q '^crates/mind-governance/'; then
  echo "ABORT: change touched the harm-gate (crates/mind-governance) — human-only. No PR."
  exit 1
fi
if git diff --cached --name-only | grep -q '\.rs$'; then
  echo "==> compile-gate (cargo build --release — matches the warm target)"
  if ! cargo build --release -p mind-core 2>&1 | tail -8; then
    echo "ABORT: changes do not compile — no PR"
    exit 1
  fi
fi

echo "==> changed files:"; git diff --cached --name-only | sed 's/^/   /'
echo "==> commit + push (as yantrikdb) + draft PR"
git commit -q -m "self-improve: $GOAL"
git remote set-url origin "https://yantrikdb:${YANTRIKDB_ACC_GIT_TOKEN}@github.com/yantrikos/yantrik-mind.git"
git push -q -u origin "$BR"
git remote set-url origin "https://github.com/yantrikos/yantrik-mind.git"   # scrub token from config
# Open the draft PR via the API (no gh dependency on the box).
python3 - "$GOAL" "$BR" <<'PY'
import json, os, sys, urllib.request, urllib.error
goal, br = sys.argv[1], sys.argv[2]
tok = os.environ["YANTRIKDB_ACC_GIT_TOKEN"]
body = ("Autonomous self-improvement by yantrik-mind, built with Claude Code on the subscription. "
        "Compile-verified; harm-gate untouched (enforced). Draft — review before merge.")
data = json.dumps({"title": f"self-improve: {goal}", "head": br, "base": "main", "draft": True, "body": body}).encode()
req = urllib.request.Request("https://api.github.com/repos/yantrikos/yantrik-mind/pulls", data=data,
                             headers={"Authorization": f"token {tok}", "Accept": "application/vnd.github+json", "User-Agent": "ym-selfbuild"})
try:
    print("PR:", json.load(urllib.request.urlopen(req))["html_url"])
except urllib.error.HTTPError as e:
    print("PR-FAIL", e.code, e.read().decode()[:300])
    sys.exit(1)
PY
echo "==> done: opened a draft PR from $BR"
