#!/usr/bin/env bash
# yantrik-mind PROJECT-IMPROVE — the research wing ships PRs on Pranab's portfolio projects.
#
# The generalization of self_improve.sh from "the mind improves its own code" to "the mind
# improves the code it RESEARCHES": WorkOps studies a project, a proposal names one concrete
# buildable change grounded in that research, this script clones the project, lets the builder
# fleet implement it with per-language verification, and opens a DRAFT PR.
#
# HARD RULES (differences from self_improve.sh — portfolio repos are Pranab's public products):
#   1. DRAFT PR ALWAYS. There is NO auto-merge path here, green or not. Only Pranab merges.
#   2. Allowlist only: the repo must appear in /var/lib/yantrik-mind/project-allowlist.txt
#      (one "name git_url" per line, human-curated — being in the WorkOps registry is not enough).
#   3. Never touch .github/, CI, packaging auth, or secrets paths.
#   4. Every PR body must carry the research citation it came from and the disclosure line.
#   5. Rate: at most 1 portfolio PR/day (global stamp) + 7-day per-repo cooldown.
#   6. Same kill-switch as self-build.
#
# Usage: project_improve.sh <repo-name> "<concrete goal>" "<research-citation>" [p_merge]
set -euo pipefail

NAME="${1:?usage: project_improve.sh <repo-name> '<goal>' '<research-citation>' [p_merge]}"
GOAL="${2:?need a concrete goal}"
CITATION="${3:?need the research citation that motivated this}"
P_MERGE="${4:-0.5}"

STATE=/var/lib/yantrik-mind
KILL="$STATE/SELF_IMPROVE_OFF"
EVLOG="$STATE/evolution.log"
ALLOW="$STATE/project-allowlist.txt"
DAYSTAMP="$STATE/.project_pr_day"
REPOSTAMP="$STATE/.project_pr_repo_$NAME"

[ -f "$KILL" ] && { echo "kill-switch present — project-improve disabled"; exit 0; }
[ -f "$ALLOW" ] || { echo "no allowlist at $ALLOW — Pranab must create it (one 'name git_url' per line)"; exit 1; }
URL="$(awk -v n="$NAME" '$1==n{print $2}' "$ALLOW" | head -1)"
[ -n "$URL" ] || { echo "repo '$NAME' not in allowlist — refusing"; exit 1; }
# Owner-declared verifier (sol rule 3): everything after the URL on the allowlist line.
# No declared verifier => this repo is SHADOW-ONLY (research + proposal, no build).
VERIFY_CMD="$(awk -v n="$NAME" '$1==n{$1="";$2="";print substr($0,3)}' "$ALLOW" | head -1 | sed 's/^ *//')"
[ -n "$VERIFY_CMD" ] || { echo "repo '$NAME' has no owner-declared verifier — shadow-only, no build"; exit 0; }

# Rate limits: 1/day global, 7d per repo.
today="$(date -u +%F)"
[ "$(cat "$DAYSTAMP" 2>/dev/null || true)" = "$today" ] && { echo "daily portfolio-PR budget spent"; exit 0; }
if [ -f "$REPOSTAMP" ] && [ "$(( $(date +%s) - $(stat -c %Y "$REPOSTAMP") ))" -lt 604800 ]; then
  echo "repo '$NAME' in 7-day cooldown"; exit 0
fi

set -a; . /etc/yantrik-mind.env 2>/dev/null || true; set +a
: "${YM_GITHUB_TOKEN:?need YM_GITHUB_TOKEN (yantrikdb account, collaborator on portfolio repos)}"

WORK="$(mktemp -d /root/codes/projbuild.XXXXXX)"
CFGHOME="$(mktemp -d /opt/yantrik-mind/projhome.XXXXXX)"
trap 'rm -rf "$WORK" "$CFGHOME"' EXIT
export HOME="$CFGHOME"                       # isolate the builder's config from the git tree…
export CODEX_HOME="${CODEX_HOME:-/root/.codex}"  # …but the Codex builder still needs its real auth
export CARGO_HOME=/root/.cargo RUSTUP_HOME=/root/.rustup
export PATH="/usr/local/bin:/root/.cargo/bin:$PATH"

AUTH_URL="$(printf '%s' "$URL" | sed "s#https://#https://yantrikdb:${YM_GITHUB_TOKEN}@#")"
echo "==> clone $NAME"
git clone -q --depth 50 "$AUTH_URL" "$WORK/$NAME"
cd "$WORK/$NAME"
git remote set-url origin "$URL"   # scrub token immediately
git config user.name "yantrikdb"
git config user.email "yantrikdb@gmail.com"
DEFAULT_BRANCH="$(git rev-parse --abbrev-ref HEAD)"
BR="mind/$(date +%s)"
git checkout -q -b "$BR"

BUILDER_PROMPT="You are yantrik-mind's research wing improving the '$NAME' project (not your own codebase — someone else's product, so be conservative and idiomatic to ITS existing style).

GOAL: $GOAL
WHY (from research): $CITATION

Rules: focused minimal change; match the repo's existing conventions exactly; do NOT touch .github/, CI, packaging/auth, or secrets. Add or extend a test if the repo has a test setup and RUN the repo's own verification (cargo test / npm test / pytest — whatever this repo uses); if it has none, verify your change manually and be ready to describe the evidence. Keep the diff under 10 files / 400 lines."

if [ "${YM_BUILDER:-claude}" = "codex" ]; then
  echo "==> builder: OpenAI Codex CLI"
  timeout 1500 codex exec --skip-git-repo-check --sandbox danger-full-access "$BUILDER_PROMPT" 2>&1 | tail -20
else
  echo "==> builder: Claude Code"
  timeout 1500 claude -p "$BUILDER_PROMPT" \
    --permission-mode acceptEdits \
    --allowedTools "Write Edit Read Bash(cargo build:*) Bash(cargo test:*) Bash(cargo check:*) Bash(npm test:*) Bash(npm run:*) Bash(python -m pytest:*) Bash(pytest:*)" \
    --output-format text 2>&1 | tail -20
fi

echo "==> enforce bounds"
git add -A
if git diff --cached --quiet; then
  echo "no changes produced — nothing to PR"
  echo "$(date -u +%FT%TZ) | project | NO-CHANGE | $NAME | $GOAL" >> "$EVLOG"
  exit 0
fi
if git diff --cached --name-only | grep -qE '^\.github/|(^|/)\.env|secrets|\.pem$|\.key$'; then
  echo "ABORT: change touched CI/secrets paths — no PR"
  echo "$(date -u +%FT%TZ) | project | ABORT-SENSITIVE | $NAME" >> "$EVLOG"
  exit 1
fi
files_changed=$(git diff --cached --name-only | wc -l | tr -d ' ')
lines_changed=$(git diff --cached --numstat | awk '{a+=$1; d+=$2} END{print a+d+0}')
if [ "$files_changed" -gt 10 ] || [ "$lines_changed" -gt 400 ]; then
  echo "ABORT: diff too large ($files_changed files / $lines_changed lines)"
  echo "$(date -u +%FT%TZ) | project | ABORT-SIZE | $NAME" >> "$EVLOG"
  exit 1
fi

# OWNER-DECLARED verify gate (sol rule 3): the allowlist names the exact command;
# run it credential-free with bounded time. (Pinned rootless containers are the
# next rung once podman lands on the workers — inferred commands were rejected
# in review as brittle, so no fallback inference here.)
echo "==> verify (owner-declared): $VERIFY_CMD"
if ! env -i HOME="$CFGHOME" PATH="/usr/local/bin:/usr/bin:/bin:/root/.cargo/bin"      CARGO_HOME=/root/.cargo RUSTUP_HOME=/root/.rustup      timeout 900 bash -c "$VERIFY_CMD" 2>&1 | tail -8; then
  echo "ABORT: owner-declared verifier failed"
  echo "$(date -u +%FT%TZ) | project | ABORT-VERIFY | $NAME" >> "$EVLOG"
  exit 1
fi

git commit -q -m "mind: $GOAL"
OWNER_REPO="$(printf '%s' "$URL" | sed -E 's#https://github.com/##; s#\.git$##')"
UPSTREAM_OWNER="${OWNER_REPO%%/*}"
REPO_SLUG="${OWNER_REPO##*/}"
BASE_SHA="$(git rev-parse "origin/$DEFAULT_BRANCH")"

# FORK FLOW (RW-5): the bot is not a collaborator on portfolio repos, so it
# forks upstream to its own account, pushes the branch to the FORK, and opens
# a CROSS-FORK PR. Works on any public repo with zero upstream permissions.
echo "==> fork $OWNER_REPO -> yantrikdb"
curl -s -m 30 -X POST "https://api.github.com/repos/$OWNER_REPO/forks" \
  -H "Authorization: token $YM_GITHUB_TOKEN" -H "Accept: application/vnd.github+json" >/dev/null
# Fork creation is async; poll until the fork's git endpoint answers.
FORK_URL="https://github.com/yantrikdb/$REPO_SLUG.git"
FORK_AUTH="$(printf '%s' "$FORK_URL" | sed "s#https://#https://yantrikdb:${YM_GITHUB_TOKEN}@#")"
ready=0
for _ in $(seq 1 30); do
  if git ls-remote "$FORK_AUTH" >/dev/null 2>&1; then ready=1; break; fi
  sleep 4
done
[ "$ready" = "1" ] || { echo "ABORT: fork not ready after 120s"; echo "$(date -u +%FT%TZ) | project | ABORT-FORK | $NAME" >> "$EVLOG"; exit 1; }
git push -q "$FORK_AUTH" "$BR"

BUILDER_NAME="$( [ "${YM_BUILDER:-claude}" = codex ] && echo 'OpenAI Codex CLI' || echo 'Claude Code' )"
BODY="**Generated by yantrik-mind** — research wing proposal; implementation by $BUILDER_NAME; pushed via the yantrikdb account. Human review and merge required; nothing auto-merges.

**Goal:** $GOAL

**Research trail:** $CITATION

**Base:** \`$BASE_SHA\` on \`$DEFAULT_BRANCH\`
**Verification:** owner-declared \`$VERIFY_CMD\` passed locally (credential-free env)

*Prediction on record: p(merge within 14d) = $P_MERGE — graded either way in the mind's Judgment Ledger.*"
# Cross-fork PR: head is "yantrikdb:<branch>", base is upstream's default.
PR_JSON="$(curl -s -m 30 -X POST "https://api.github.com/repos/$OWNER_REPO/pulls" \
  -H "Authorization: token $YM_GITHUB_TOKEN" -H "Accept: application/vnd.github+json" \
  -d "$(python3 -c "import json,sys;print(json.dumps({'title':'mind: '+sys.argv[1],'head':'yantrikdb:'+sys.argv[2],'base':sys.argv[3],'draft':True,'body':sys.argv[4]}))" "$GOAL" "$BR" "$DEFAULT_BRANCH" "$BODY")")"
PR_URL="$(printf '%s' "$PR_JSON" | python3 -c "import json,sys;print(json.load(sys.stdin).get('html_url','FAILED'))")"
[ "$PR_URL" = "FAILED" ] && { echo "PR creation failed: $(printf '%s' "$PR_JSON" | head -c 300)"; exit 1; }

# Prediction chain (root-custodied like the immune ledger): hash-chained JSONL.
CHAIN_FILE="$STATE/project_pr_chain.jsonl"
PREV="$(tail -1 "$CHAIN_FILE" 2>/dev/null | python3 -c "import json,sys
try: print(json.loads(sys.stdin.read())['chain'])
except Exception: print('genesis')" )"
REC="$(python3 -c "import json,sys;print(json.dumps({'ts':sys.argv[1],'repo':sys.argv[2],'pr':sys.argv[3],'goal':sys.argv[4],'p_merge':float(sys.argv[5]),'base':sys.argv[6],'claim':'merged within 14d'},sort_keys=True))" "$(date -u +%FT%TZ)" "$NAME" "$PR_URL" "$GOAL" "$P_MERGE" "$BASE_SHA")"
CH="$(printf '%s%s' "$PREV" "$REC" | sha256sum | cut -d' ' -f1)"
printf '{"chain":"%s","record":%s}
' "$CH" "$REC" >> "$CHAIN_FILE"
chmod 600 "$CHAIN_FILE" 2>/dev/null || true

echo "$today" > "$DAYSTAMP"; touch "$REPOSTAMP"
echo "$(date -u +%FT%TZ) | project | DRAFT-PR | $NAME | $PR_URL | p_merge=$P_MERGE | $GOAL" >> "$EVLOG"
echo "==> DRAFT PR: $PR_URL"
