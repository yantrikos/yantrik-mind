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
EVLOG=/var/lib/yantrik-mind/evolution.log   # outcome ledger — read by `ym evolution`
[ -f "$KILL" ] && { echo "kill-switch present ($KILL) — self-build disabled"; exit 0; }

# Auth: subscription token for Claude, yantrikdb token for the push. (root:600 env.)
set -a; . /etc/yantrik-mind.env 2>/dev/null || true; set +a
: "${CLAUDE_CODE_OAUTH_TOKEN:?need CLAUDE_CODE_OAUTH_TOKEN}"
: "${YANTRIKDB_ACC_GIT_TOKEN:?need YANTRIKDB_ACC_GIT_TOKEN}"
# Force real Claude (drop any MiniMax override that may be in the env).
unset ANTHROPIC_BASE_URL ANTHROPIC_AUTH_TOKEN ANTHROPIC_MODEL ANTHROPIC_API_KEY

# Clone as a SIBLING of the path-dep repos (../yantrikdb, ../yantrik-companion live under /root/codes)
# so the relative path deps resolve and the compile-gate can actually build. Claude's config goes in a
# SEPARATE HOME so its dotfiles never pollute the git tree. Reuse the warm release target + registry.
WORK="$(mktemp -d /root/codes/ymbuild.XXXXXX)"          # the repo clone (sibling of the path deps)
CFGHOME="$(mktemp -d /opt/yantrik-mind/ymhome.XXXXXX)"  # Claude config, outside the git tree
trap 'rm -rf "$WORK" "$CFGHOME"' EXIT
export HOME="$CFGHOME"
export CARGO_HOME=/root/.cargo                          # warm crates registry (avoid re-download)
export RUSTUP_HOME=/root/.rustup                        # keep rustup's default toolchain (HOME moved)
# cron runs with a minimal PATH; claude lives in /usr/local/bin, cargo in /root/.cargo/bin.
export PATH="/usr/local/bin:/root/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/root/codes/yantrik-mind/target # warm release target -> fast compile-gate

echo "==> clone (sibling of path-dep repos)"
git clone -q https://github.com/yantrikos/yantrik-mind.git "$WORK"
cd "$WORK"
git config user.name "yantrikdb"
git config user.email "yantrikdb@gmail.com"
BR="self/$(date +%s)"
git checkout -q -b "$BR"

echo "==> Claude Code (subscription) implementing: $GOAL"
# Cargo-scoped Bash so the builder can SELF-VERIFY (write a test, run it, prove green) — this is what
# lets good changes clear the auto-merge test-gate instead of piling up as drafts. Only cargo
# build/test/check are allowed; any other shell command is denied by the tool allowlist.
timeout 1500 claude -p "You are improving the yantrik-mind codebase (you are the companion improving your own code). GOAL: $GOAL

Rules: make a focused, minimal, idiomatic change. Do NOT modify anything under crates/mind-governance (the harm-gate is off-limits). If you change Rust, keep it compiling. ADD a #[test] covering your change and RUN it (you can execute cargo build / cargo test / cargo check) — verify green before you finish. Do not touch secrets or CI auth." \
  --permission-mode acceptEdits --allowedTools "Write Edit Read Bash(cargo build:*) Bash(cargo test:*) Bash(cargo check:*)" --output-format text 2>&1 | tail -25

echo "==> enforce bounds"
git add -A   # stage everything incl. NEW files (git diff alone ignores untracked)
if git diff --cached --quiet; then
  echo "no changes produced — nothing to PR"
  echo "$(date -u +%FT%TZ) | build | NO-CHANGE | $GOAL" >> "$EVLOG"
  exit 0
fi
if git diff --cached --name-only | grep -q '^crates/mind-governance/'; then
  echo "ABORT: change touched the harm-gate (crates/mind-governance) — human-only. No PR."
  echo "$(date -u +%FT%TZ) | build | ABORT-HARMGATE | $GOAL" >> "$EVLOG"
  exit 1
fi
if git diff --cached --name-only | grep -q '\.rs$'; then
  echo "==> compile-gate (cargo build --release — matches the warm target)"
  if ! cargo build --release -p mind-core 2>&1 | tail -8; then
    echo "ABORT: changes do not compile — no PR"
    echo "$(date -u +%FT%TZ) | build | ABORT-COMPILE | $GOAL" >> "$EVLOG"
    exit 1
  fi
fi

echo "==> changed files:"; git diff --cached --name-only | sed 's/^/   /'

# ---- auto-merge gate ----------------------------------------------------------------------------
# Default: always a DRAFT PR for human review. Auto-merge ONLY when YM_AUTOMERGE=1 AND every gate
# passes: tests green + small diff + no sensitive paths. Anything failing a gate => draft for human.
# (The harm-gate carve-out above already ABORTs the whole run before here if mind-governance changed.)
AUTOMERGE=0
if [ "${YM_AUTOMERGE:-0}" = "1" ]; then
  AUTOMERGE=1
  files_changed=$(git diff --cached --name-only | wc -l | tr -d ' ')
  lines_changed=$(git diff --cached --numstat | awk '{a+=$1; d+=$2} END{print a+d+0}')
  if git diff --cached --name-only | grep -qE '^(\.github/|deploy/|Cargo\.lock|.*\.env$)'; then
    echo "auto-merge BLOCKED: touches CI/deploy/lock/env — leaving as draft for human"; AUTOMERGE=0
  fi
  if [ "$files_changed" -gt 10 ] || [ "$lines_changed" -gt 400 ]; then
    echo "auto-merge BLOCKED: diff too large ($files_changed files / $lines_changed lines) — draft for human"; AUTOMERGE=0
  fi
  # test-PRESENCE gate: a Rust change must ADD a test in the diff. Without this, `cargo test` passes
  # vacuously (ok with 0 new tests) and inert/untested code can auto-merge. No added test => draft.
  if [ "$AUTOMERGE" = "1" ] && git diff --cached --name-only | grep -q '\.rs$'; then
    # LC_ALL=C + count (not -q) so the log shows WHAT the gate saw — a false negative here once
    # sent a fully-tested PR (#29) to draft with no evidence to diagnose why.
    TADD=$(git diff --cached -U0 | LC_ALL=C grep -acE '^\+.*(#\[(tokio::)?test|fn test_)' || true)
    echo "test-presence gate: $TADD test marker line(s) in staged diff"
    if [ "${TADD:-0}" -eq 0 ]; then
      echo "auto-merge BLOCKED: Rust changed but no test added in the diff — draft for human"; AUTOMERGE=0
    fi
  fi
  if [ "$AUTOMERGE" = "1" ] && git diff --cached --name-only | grep -q '\.rs$'; then
    pkgs=$(git diff --cached --name-only | sed -n 's#^crates/\([^/]*\)/.*#\1#p' | sort -u)
    # mind-evals ALWAYS runs: the behavioral suite (standard_suite_is_green) gates every merge, not
    # just changes touching the evals crate — scoped-only testing let 3 regressions land invisibly.
    PFLAGS="-p mind-core -p mind-evals"
    for p in $pkgs; do { [ "$p" = "mind-core" ] || [ "$p" = "mind-evals" ]; } || PFLAGS="$PFLAGS -p $p"; done
    echo "==> test-gate (cargo test --release $PFLAGS)"
    if ! cargo test --release $PFLAGS 2>&1 | tail -15; then
      echo "auto-merge BLOCKED: tests failed — draft for human (no merge)"; AUTOMERGE=0
    fi
  fi
fi

echo "==> commit + push (as yantrikdb)"
git commit -q -m "self-improve: $GOAL"
git remote set-url origin "https://yantrikdb:${YANTRIKDB_ACC_GIT_TOKEN}@github.com/yantrikos/yantrik-mind.git"
git push -q -u origin "$BR"
git remote set-url origin "https://github.com/yantrikos/yantrik-mind.git"   # scrub token from config

# Open the PR (draft unless every auto-merge gate passed) and, if cleared, squash-merge it.
PYOUT=$(python3 - "$GOAL" "$BR" "$AUTOMERGE" <<'PY'
import json, os, sys, time, urllib.request, urllib.error
goal, br, automerge = sys.argv[1], sys.argv[2], (sys.argv[3] == "1")
tok = os.environ["YANTRIKDB_ACC_GIT_TOKEN"]
api = "https://api.github.com/repos/yantrikos/yantrik-mind"
def call(method, url, body=None):
    data = json.dumps(body).encode() if body is not None else None
    r = urllib.request.Request(url, data=data, method=method, headers={
        "Authorization": f"token {tok}", "Accept": "application/vnd.github+json", "User-Agent": "ym-selfbuild"})
    return json.load(urllib.request.urlopen(r))
body = ("Autonomous self-improvement by yantrik-mind (Claude Code on the subscription). "
        "Compile-verified; harm-gate untouched (enforced). "
        + ("Tests green + auto-merge gates passed." if automerge else "Draft — review before merge."))
try:
    pr = call("POST", api + "/pulls", {"title": f"self-improve: {goal}", "head": br, "base": "main",
                                       "draft": (not automerge), "body": body})
    print("PR:", pr["html_url"])
except urllib.error.HTTPError as e:
    print("PR-FAIL", e.code, e.read().decode()[:300]); sys.exit(1)
if automerge:
    head_sha = pr.get("head", {}).get("sha", "")
    # Wait for the required GitHub check (harm-gate-guard) to go GREEN before merging — the
    # independent, GitHub-side wall. The box-side gates already passed; this is defense in depth.
    ok = False
    for _ in range(18):  # ~3 min
        time.sleep(10)
        try:
            cr = call("GET", f"{api}/commits/{head_sha}/check-runs")
        except Exception:
            continue
        guard = [r for r in cr.get("check_runs", []) if r.get("name") == "harm-gate-guard"]
        if guard and all(r.get("status") == "completed" for r in guard):
            ok = all(r.get("conclusion") == "success" for r in guard)
            break
    if not ok:
        print("auto-merge HELD: harm-gate CI check not green/done — PR left open for human")
    else:
        try:
            m = call("PUT", f"{api}/pulls/{pr['number']}/merge", {"merge_method": "squash"})
            print("MERGED:", (m.get("sha") or "")[:7], "— auto-merged on green (CI gate passed)")
        except urllib.error.HTTPError as e:
            print("MERGE-FAIL", e.code, e.read().decode()[:300])  # PR stays open for a human
PY
)
echo "$PYOUT"
# The last mile: a merge that only lands on GitHub is not self-improvement of the RUNNING system.
# On a green auto-merge, hand off to self_deploy.sh (health-checked, auto-rollback) so the live
# service updates itself. Uses the script from THIS clone — the deploy logic is itself self-updating.
if echo "$PYOUT" | grep -q "^MERGED:"; then
  echo "$(date -u +%FT%TZ) | build | MERGED | $GOAL" >> "$EVLOG"
  echo "==> merged on green — self-deploying"
  bash "$WORK/deploy/self_deploy.sh" || echo "==> self-deploy failed or rolled back (see evolution.log)"
elif echo "$PYOUT" | grep -q "^PR:"; then
  echo "$(date -u +%FT%TZ) | build | DRAFT-FOR-HUMAN | $GOAL" >> "$EVLOG"
fi
echo "==> done"
