#!/usr/bin/env bash
# FORGE BUILD — the strong-builder stage. The chain models spec and referee well but two ventures
# proved they can't emit a working artifact; Claude Code can. Builds the MVP directly into the
# venture dir per spec.json (+ issues.json on iterate). Called by the forge's build stage.
set -euo pipefail
VDIR="$1"
[ -d "$VDIR" ] || { echo "no venture dir: $VDIR"; exit 1; }
set -a; . /etc/yantrik-mind.env 2>/dev/null || true; set +a
: "${CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN}"
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL ANTHROPIC_API_KEY CLAUDECODE CLAUDE_CODE_ENTRYPOINT
export PATH="/usr/local/bin:$PATH"
CH="$(mktemp -d /opt/yantrik-mind/ymfh.XXXXXX)"
trap 'rm -rf "$CH"' EXIT
export HOME="$CH"
cd "$VDIR"
SPEC="$(cat spec.json 2>/dev/null || echo '{}')"
ISSUES=""
[ -f issues.json ] && ISSUES="FIX THESE ISSUES from the last referee review (this is an iteration): $(cat issues.json)"
timeout 900 claude -p "Build the MVP product in the CURRENT DIRECTORY per this spec:
$SPEC
$ISSUES

Rules: write COMPLETE, working, self-contained files directly into this directory — a single index.html with inline CSS/JS is strongly preferred (stdlib-only Python acceptable if the spec says python). No placeholders, no TODOs, no external CDNs or APIs. At most 6 files. Verify your HTML is structurally complete (doctype through closing tags) before finishing." \
  --permission-mode acceptEdits --allowedTools "Write Edit Read" --output-format text 2>&1 | tail -3
echo "FORGE_BUILD_DONE"
