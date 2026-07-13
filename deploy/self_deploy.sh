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

if [ ! -d "$CLONE/.git" ]; then
  git clone -q https://github.com/yantrikos/yantrik-mind.git "$CLONE"
fi
cd "$CLONE"
git fetch -q origin main
git checkout -q main
git reset -q --hard origin/main
COMMIT=$(git rev-parse --short HEAD)

echo "==> self-deploy: building main @ $COMMIT"
if ! cargo build --release -p mind-core 2>&1 | tail -3; then
  echo "$(date -u +%FT%TZ) | deploy | ABORT-BUILD | $COMMIT" >> "$EVLOG"
  exit 1
fi

# stop -> cp -> start (NEVER cp over a running binary — Text file busy).
systemctl stop yantrik-mind
cp "$BIN" "$BIN.prev" 2>/dev/null || true
cp "$CARGO_TARGET_DIR/release/mind-core" "$BIN"
chown yantrikmind:yantrikmind "$BIN"
systemctl start yantrik-mind
sleep 6

# Health probe: the control endpoint must answer a trivial command with a date-shaped reply.
# ARCH-2: /cli is now authenticated — present the local console operator token minted at first boot
# (owner-only file in the state dir). The daemon creates it before the endpoint starts listening.
CONSOLE_TOKEN_FILE="${YM_STATE_DIR:-/var/lib/yantrik-mind}/console.token"
CONSOLE_TOKEN="$(cat "$CONSOLE_TOKEN_FILE" 2>/dev/null || true)"
if printf "now" | curl -s -m 20 -H "Authorization: Bearer ${CONSOLE_TOKEN}" --data-binary @- http://127.0.0.1:8077/cli | grep -qE '[0-9]{4}-[0-9]{2}-[0-9]{2}'; then
  echo "$(date -u +%FT%TZ) | deploy | DEPLOYED | $COMMIT health-ok" >> "$EVLOG"
  echo "==> self-deploy OK @ $COMMIT"
else
  echo "==> HEALTH PROBE FAILED — rolling back to previous binary"
  systemctl stop yantrik-mind || true
  if [ -f "$BIN.prev" ]; then
    cp "$BIN.prev" "$BIN"
    chown yantrikmind:yantrikmind "$BIN"
  fi
  systemctl start yantrik-mind || true
  echo "$(date -u +%FT%TZ) | deploy | ROLLED-BACK | $COMMIT health probe failed" >> "$EVLOG"
  exit 1
fi

# ── companion components (idempotent; added 2026-07-10 with the immune system) ──
# The main binary is deployed above with health-gate + rollback; these are
# sidecars that ride the same tick. Failures here must NOT roll back the mind.
set +e
echo "==> self-deploy: companion components (immune + observatory)"
if cargo build --release -p mind-evals 2>&1 | tail -2; then
  cp "$CARGO_TARGET_DIR/release/mind-evals" /opt/yantrik-mind/mind-evals
  chmod 755 /opt/yantrik-mind/mind-evals
fi
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
