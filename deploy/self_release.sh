#!/usr/bin/env bash
# yantrik-mind SELF-RELEASE — the companion versions + markets itself (owned surface only).
#
# Clone → Claude Code (subscription) writes a marketing-grade README.md + RELEASE_NOTES.md → commit
# to main (as yantrikdb) → tag the version → cut a GitHub Release. OWNED-SURFACE marketing only;
# external posting (X/HN/etc.) is a separate, human-approved step — never done here.
#
# BOUNDS: harm-gate carve-out (never touches crates/mind-governance); kill-switch
# /var/lib/yantrik-mind/SELF_IMPROVE_OFF; one release per run.
#
# Usage:  self_release.sh [vX.Y.Z]   (defaults to v0.1.0 if no tags yet, else patch-bumps latest)
set -euo pipefail

KILL=/var/lib/yantrik-mind/SELF_IMPROVE_OFF
[ -f "$KILL" ] && { echo "kill-switch present — self-release disabled"; exit 0; }

set -a; . /etc/yantrik-mind.env 2>/dev/null || true; set +a
: "${CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN}"
: "${YANTRIKDB_ACC_GIT_TOKEN:?need YANTRIKDB_ACC_GIT_TOKEN}"
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL

WORK="$(mktemp -d /opt/yantrik-mind/release.XXXXXX)"; trap 'rm -rf "$WORK"' EXIT
cd "$WORK"; export HOME="$WORK"
git clone -q https://github.com/yantrikos/yantrik-mind.git repo; cd repo
git config user.name "yantrikdb"; git config user.email "yantrikdb@gmail.com"

# Decide the version.
if [ "${1:-}" ]; then VER="$1"
else
  LAST="$(git tag | sort -V | tail -1)"
  if [ -z "$LAST" ]; then VER="v0.1.0"
  else b="${LAST#v}"; IFS=. read -r MA MI PA <<<"$b"; VER="v${MA}.${MI}.$((PA+1))"; fi
fi
echo "==> releasing $VER"

echo "==> Claude (subscription) writes marketing README + release notes"
timeout 600 claude -p "You are preparing the $VER release of yantrik-mind and you are writing its public marketing.

1. Rewrite README.md as a compelling, ACCURATE marketing-grade overview: what yantrik-mind is (a ground-up Rust AI companion built on the YantrikDB typed-memory moat), why it's different from flat-RAG assistants (typed beliefs with Bayesian revision, contradiction detection, consolidation that COMPOUNDS from conversation; commitments; research that revises its own beliefs from cited evidence; multi-LLM routing; an agentic coder on Claude; persistent delegation + inbox/GitHub/web monitors; an NL planner; a parallel worker pool; ALL behind one deterministic, property-tested harm-gate; and it improves its own code via bounded self-build PRs). Include a short feature list, a 'why it's different' section, and a quickstart pointing at BUILD.md/CONTRIBUTING.md. Confident but truthful — no invented benchmarks, no fake testimonials.

2. Write RELEASE_NOTES.md for $VER: a crisp highlights summary of the above capabilities.

Do not touch crates/mind-governance. Do not include any secrets." \
  --permission-mode acceptEdits --allowedTools "Write Edit Read" --output-format text 2>&1 | tail -15

git add -A
if git diff --cached --name-only | grep -q '^crates/mind-governance/'; then echo "ABORT: touched harm-gate"; exit 1; fi
git diff --cached --quiet && { echo "no content produced"; exit 1; }

echo "==> commit + push main + tag"
git commit -q -m "release: $VER — marketing README + notes (self-authored)"
git remote set-url origin "https://yantrikdb:${YANTRIKDB_ACC_GIT_TOKEN}@github.com/yantrikos/yantrik-mind.git"
git push -q origin main
git tag -a "$VER" -m "yantrik-mind $VER"
git push -q origin "$VER"
git remote set-url origin "https://github.com/yantrikos/yantrik-mind.git"

echo "==> cut GitHub Release $VER"
NOTES="$(cat RELEASE_NOTES.md 2>/dev/null || echo "yantrik-mind $VER")"
python3 - "$VER" "$NOTES" <<'PY'
import json, os, sys, urllib.request, urllib.error
ver, notes = sys.argv[1], sys.argv[2]
tok = os.environ["YANTRIKDB_ACC_GIT_TOKEN"]
data = json.dumps({"tag_name": ver, "name": f"yantrik-mind {ver}", "body": notes, "make_latest": "true"}).encode()
req = urllib.request.Request("https://api.github.com/repos/yantrikos/yantrik-mind/releases", data=data,
    headers={"Authorization": f"token {tok}", "Accept": "application/vnd.github+json", "User-Agent": "ym-release"})
try:
    print("RELEASE:", json.load(urllib.request.urlopen(req))["html_url"])
except urllib.error.HTTPError as e:
    print("RELEASE-FAIL", e.code, e.read().decode()[:300]); sys.exit(1)
PY
echo "==> done: $VER released + README marketed"
