#!/usr/bin/env bash
# yantrik-mind SELF-DEPLOY — the last mile of the self-improvement loop: after a self-authored PR
# auto-merges on green, the RUNNING service updates itself from main. Without this, "self-improvement"
# only changes GitHub while the live binary stays old.
#
# Safety: health-checked + auto-rollback. stop -> backup binary -> swap new -> start -> probe the
# control endpoint -> on failure restore the backup and restart. Honors the same kill-switch as the
# rest of the loop. Every outcome is appended to evolution.log (the `ym evolution` scorecard).
set -euo pipefail

KILL=/var/lib/yantrik-mind/SELF_IMPROVE_OFF
[ -f "$KILL" ] && { echo "kill-switch present — self-deploy skipped"; exit 0; }

EVLOG=/var/lib/yantrik-mind/evolution.log
CLONE=/root/codes/ym-autodeploy
BIN=/opt/yantrik-mind/mind-core
export CARGO_HOME=/root/.cargo RUSTUP_HOME=/root/.rustup
export PATH="/usr/local/bin:/root/.cargo/bin:$PATH"
# Own target dir: sharing one with other source trees makes cargo thrash on path fingerprints.
export CARGO_TARGET_DIR="$CLONE/target"

# Replace an executable by renaming a fully written sibling over it. Truncating an executable in
# place fails with ETXTBSY when any stale scratch process still maps the old inode, even after the
# systemd unit has stopped. Atomic rename lets those processes finish on the old inode while the
# service starts from the new one; it also prevents a partial binary from ever occupying $BIN.
replace_binary() {
  local source=$1 target=$2 staged failed_step
  staged=$(mktemp "${target}.next.XXXXXX") || return 1
  if ! install -m 0755 "$source" "$staged"; then
    failed_step=install
  elif ! chown yantrikmind:yantrikmind "$staged"; then
    failed_step=chown
  elif ! mv -f "$staged" "$target"; then
    failed_step=rename
  else
    return 0
  fi
  rm -f "$staged"
  echo "==> binary swap failed during $failed_step: $source -> $target" >&2
  return 1
}

# `set -e` must never strand the service after it has been stopped. Any unexpected failure in a
# stop/swap/start window reaches this EXIT guard, which restores the previous image when the new
# binary was already installed, attempts to start the service, and records both outcomes. The guard
# is armed only around those narrow windows; ordinary failures elsewhere keep their existing paths.
SWAP_GUARD_ACTIVE=0
SWAP_GUARD_PHASE=idle
SWAP_GUARD_ROLLBACK=0
restart_after_failed_swap() {
  local rc=$? rollback_status=not-needed service_status=FAILED event=ABORT-SWAP
  trap - EXIT
  if [ "$SWAP_GUARD_ACTIVE" -eq 1 ]; then
    set +e
    if [ "$SWAP_GUARD_ROLLBACK" -eq 1 ] && [ -f "$BIN.prev" ]; then
      if replace_binary "$BIN.prev" "$BIN"; then
        rollback_status=restored
      else
        rollback_status=FAILED
      fi
    fi
    if systemctl start yantrik-mind; then
      service_status=started
    fi
    [ "$SWAP_GUARD_PHASE" != health-rollback ] || event=ROLLBACK-FAILED
    echo "$(date -u +%FT%TZ) | deploy | $event | $COMMIT phase=$SWAP_GUARD_PHASE rc=$rc rollback=$rollback_status service=$service_status" >> "$EVLOG" || true
    echo "==> deploy aborted during $SWAP_GUARD_PHASE (rc=$rc); rollback=$rollback_status service=$service_status" >&2
  fi
  exit "$rc"
}

if [ ! -d "$CLONE/.git" ]; then
  git clone -q https://github.com/yantrikos/yantrik-mind.git "$CLONE"
fi
cd "$CLONE"
git fetch -q origin main
git checkout -q main
git reset -q --hard origin/main
COMMIT=$(git rev-parse --short HEAD)

# Keep the path-dependency sibling in sync too. yantrik-mind's Cargo.toml points
# `yantrik-ml` at ../yantrik-companion; if that tree stays stale, companion-side fixes
# (e.g. inference request shaping) never reach the built binary. Self-healing: clone if
# missing, hard-reset to origin/main if present (build cache in target/ is untracked, kept).
COMPANION=/root/codes/yantrik-companion
if [ ! -d "$COMPANION/.git" ]; then
  if [ -d "$COMPANION" ]; then git -C "$COMPANION" init -q && git -C "$COMPANION" remote add origin https://github.com/yantrikos/yantrik-companion.git;
  else git clone -q https://github.com/yantrikos/yantrik-companion.git "$COMPANION"; fi
fi
git -C "$COMPANION" fetch -q origin main && git -C "$COMPANION" reset -q --hard origin/main
echo "==> self-deploy: companion @ $(git -C "$COMPANION" rev-parse --short HEAD)"

# Senses: the media pipeline's binaries, self-healed like the companion tree above.
# A running host must not quietly lose its ears because a capability's dependency was
# installed by hand once. Best-effort — the mind degrades honestly and names what is
# missing, so a failure here must never abort a deploy that is otherwise good.
if ! command -v ffmpeg >/dev/null 2>&1; then
  echo "==> self-deploy: installing ffmpeg"
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq ffmpeg >/dev/null 2>&1 || echo "    (ffmpeg install failed — audio/video will report itself unavailable)"
fi
if ! command -v yt-dlp >/dev/null 2>&1; then
  echo "==> self-deploy: installing yt-dlp"
  pip3 install --break-system-packages -q -U yt-dlp >/dev/null 2>&1 || pip3 install -q -U yt-dlp >/dev/null 2>&1 \
    || echo "    (yt-dlp install failed — media links will report itself unavailable)"
fi
echo "==> self-deploy: senses ffmpeg=$(command -v ffmpeg >/dev/null 2>&1 && echo yes || echo NO) yt-dlp=$(command -v yt-dlp >/dev/null 2>&1 && echo yes || echo NO)"

echo "==> self-deploy: building main @ $COMMIT"
# BUILD PROVENANCE (the stale-binary incident, 2026-08-31): `git reset --hard` drags source mtimes
# BACKWARD, cargo's fingerprint then judges the cached artifact current, prints "Finished", and the
# swap ships week-old code while every check reports success. Two independent defenses:
#   1. The full checked-out SHA rides into the build as YM_BUILD_COMMIT; mind-core compiles it in
#      via option_env!, which makes the env var part of the crate fingerprint — a new SHA forces a
#      recompile regardless of mtimes.
#   2. Before the swap, the BUILT BINARY is asked for its stamp (--build-commit answers before any
#      boot) and anything but an exact match with the checkout — including "unstamped" — refuses
#      the deploy. The binary proves its own provenance; the build log's word is not accepted.
# Defense in depth: touch mind-core sources so even a cargo that ignores env fingerprints rebuilds.
COMMIT_FULL=$(git rev-parse HEAD)
export YM_BUILD_COMMIT="$COMMIT_FULL"
find crates/mind-core/src -name '*.rs' -exec touch {} +
if ! cargo build --release -p mind-core 2>&1 | tail -3; then
  echo "$(date -u +%FT%TZ) | deploy | ABORT-BUILD | $COMMIT" >> "$EVLOG"
  exit 1
fi
BUILT_COMMIT=$("$CARGO_TARGET_DIR/release/mind-core" --build-commit 2>/dev/null || echo "unreadable")
if [ "$BUILT_COMMIT" != "$COMMIT_FULL" ]; then
  echo "==> STALE/UNSTAMPED BINARY — built binary reports '$BUILT_COMMIT', checkout is '$COMMIT_FULL'. REFUSING the swap."
  echo "$(date -u +%FT%TZ) | deploy | ABORT-STALE-BINARY | checkout=$COMMIT built=$BUILT_COMMIT" >> "$EVLOG"
  exit 1
fi
echo "==> self-deploy: binary provenance verified ($BUILT_COMMIT)"

# Stop the managed service, preserve a rollback image, then atomically rename the new executable
# into place. Other scratch processes may still map the previous inode; they must not strand the
# deployment or expose a partially copied binary.
SWAP_GUARD_ACTIVE=1
SWAP_GUARD_PHASE=stop
SWAP_GUARD_ROLLBACK=0
trap restart_after_failed_swap EXIT
systemctl stop yantrik-mind
SWAP_GUARD_PHASE=backup
[ ! -f "$BIN" ] || replace_binary "$BIN" "$BIN.prev"
SWAP_GUARD_PHASE=install
replace_binary "$CARGO_TARGET_DIR/release/mind-core" "$BIN"
SWAP_GUARD_ROLLBACK=1
SWAP_GUARD_PHASE=start
systemctl start yantrik-mind
SWAP_GUARD_ACTIVE=0
trap - EXIT
sleep 6

# Health probe: the control endpoint must answer a trivial command with a date-shaped reply.
# ARCH-2: /cli is now authenticated — present the local console operator token minted at first boot
# (owner-only file in the state dir). The daemon creates it before the endpoint starts listening.
CONSOLE_TOKEN_FILE="${YM_STATE_DIR:-/var/lib/yantrik-mind}/console.token"
CONSOLE_TOKEN="$(cat "$CONSOLE_TOKEN_FILE" 2>/dev/null || true)"
E2E_RC=0
if printf "now" | curl -s -m 20 -H "Authorization: Bearer ${CONSOLE_TOKEN}" --data-binary @- http://127.0.0.1:8077/cli | grep -qE '[0-9]{4}-[0-9]{2}-[0-9]{2}'; then
  echo "$(date -u +%FT%TZ) | deploy | DEPLOYED | $COMMIT health-ok" >> "$EVLOG"
  echo "==> self-deploy OK @ $COMMIT"
  # COMPLETION CHECK. The health probe only proves the process answers; it said "ok" through every
  # failure of 2026-08-20/21, including weeks in which the planner had no model behind it and every
  # multi-step task died at step one. Unit suites were green throughout. So the deploy also asks
  # whether the mind can still FINISH things, and records the answer next to the deploy line.
  if [ -x "$CLONE/deploy/e2e_check.sh" ] || [ -f "$CLONE/deploy/e2e_check.sh" ]; then
    if E2E=$(YM_E2E_HOST=localhost YM_E2E_KEY=/root/.ssh/id_ed25519 bash "$CLONE/deploy/e2e_check.sh" 2>&1 | tail -1); then
      E2E_RC=0
    else
      E2E_RC=$?
    fi
    echo "$(date -u +%FT%TZ) | deploy | E2E | $COMMIT rc=$E2E_RC $E2E" >> "$EVLOG"
    echo "==> e2e: rc=$E2E_RC $E2E"
  fi
else
  echo "==> HEALTH PROBE FAILED — rolling back to previous binary"
  SWAP_GUARD_ACTIVE=1
  SWAP_GUARD_PHASE=health-stop
  SWAP_GUARD_ROLLBACK=0
  trap restart_after_failed_swap EXIT
  systemctl stop yantrik-mind
  SWAP_GUARD_PHASE=health-rollback
  if [ -f "$BIN.prev" ]; then
    replace_binary "$BIN.prev" "$BIN"
  else
    echo "==> rollback image is missing: $BIN.prev" >&2
    exit 1
  fi
  SWAP_GUARD_PHASE=health-start
  systemctl start yantrik-mind
  SWAP_GUARD_ACTIVE=0
  trap - EXIT
  echo "$(date -u +%FT%TZ) | deploy | ROLLED-BACK | $COMMIT health probe failed" >> "$EVLOG"
  exit 1
fi

# ── companion components (idempotent; added 2026-07-10 with the immune system) ──
# The main binary is deployed above with health-gate + rollback; these are
# sidecars that ride the same tick. Failures here must NOT roll back the mind.
set +e
# Helper scripts the builder shells out to. They live as REAL FILES because inline python inside a
# shell inside ssh does not survive the quoting (a silent failure once lost the whole spend ledger).
for h in ym-record-spend ym-json-result ym-tape-tick ym-bar-supervise; do
  [ -f "$(dirname "$0")/bin/$h" ] && install -m 0755 "$(dirname "$0")/bin/$h" "/usr/local/bin/$h" && echo "==> installed /usr/local/bin/$h"
done
echo "==> self-deploy: companion components (immune + observatory)"
if cargo build --release -p mind-evals 2>&1 | tail -2; then
  cp "$CARGO_TARGET_DIR/release/mind-evals" /opt/yantrik-mind/mind-evals
  chmod 755 /opt/yantrik-mind/mind-evals
fi
# Browser/vision helper scripts. These are CODE the mind shells out to, so they must ride the
# deploy like the binary does — a stale driver beside a fresh binary is a capability that reports
# itself present and behaves like an older version, which is worse than one that is missing.
for js in browser_agent.js headless_fetch.js headful_fetch.js snap_page.js bar_watch.sh; do
  [ -f "$CLONE/deploy/$js" ] && cp "$CLONE/deploy/$js" "/opt/yantrik-mind/$js" \
    && chown yantrikmind:yantrikmind "/opt/yantrik-mind/$js" 2>/dev/null
done
echo "==> self-deploy: browser helpers synced"
cp "$CLONE/deploy/immune_trial.sh" /opt/yantrik-mind/immune_trial.sh && chmod 755 /opt/yantrik-mind/immune_trial.sh
cp "$CLONE/deploy/observatory.py" /opt/yantrik-mind/observatory.py
for unit in immune-trial.service immune-trial.timer observatory.service; do
  if ! cmp -s "$CLONE/deploy/$unit" "/etc/systemd/system/$unit" 2>/dev/null; then
    cp "$CLONE/deploy/$unit" "/etc/systemd/system/$unit"
    UNITS_CHANGED=1
  fi
done
[ "${UNITS_CHANGED:-0}" = "1" ] && systemctl daemon-reload
systemctl enable --now immune-trial.timer 2>/dev/null
systemctl enable --now observatory.service 2>/dev/null || systemctl restart observatory.service 2>/dev/null
# First-ever trial: if the ledger doesn't exist yet, run one now so the board
# and observatory show real numbers by morning.
if [ ! -f /var/lib/yantrik-mind/immune/immune_trials.jsonl ]; then
  systemctl start immune-trial.service 2>/dev/null
fi
echo "$(date -u +%FT%TZ) | deploy | COMPONENTS | immune+observatory synced @ $COMMIT" >> "$EVLOG"
set -e

# A completion-check failure keeps the deploy command red, but only after the fail-soft companion
# sync has run and the exact E2E summary has been persisted. The core binary remains installed:
# rollback is reserved for the deterministic local health gate above, not external-service noise.
if [ "$E2E_RC" -ne 0 ]; then
  echo "==> self-deploy completed with E2E failures (rc=$E2E_RC); health-valid binary remains active"
  exit "$E2E_RC"
fi
