#!/usr/bin/env bash
# yantrik-mind IMMUNE TRIAL — seeded-false-belief immunology on a cold snapshot.
#
# Runs as ROOT on a schedule (immune-trial.timer, 4x/week per the Sol protocol)
# while the mind runs as `yantrikmind`. That asymmetry IS the custody: the
# ledger + summary live in a root-owned directory the mind can read but never
# write, so the mind cannot rot its own immunology record. The chain head is
# additionally appended to a separate root-only heads file after every trial.
#
# The trial NEVER touches the live db: it snapshots via VACUUM INTO (WAL-safe)
# from the live file in read-only mode — the same guarantee snapshot_to gives
# in-process — then seeds and judges a scratch copy.
set -euo pipefail

STATE=/var/lib/yantrik-mind
LIVE_DB="$STATE/mind.db"
IMMUNE_DIR="$STATE/immune"            # root:root 755 — mind reads, never writes
LEDGER="$IMMUNE_DIR/immune_trials.jsonl"
SUMMARY="$IMMUNE_DIR/immune_summary.json"
HEADS="$IMMUNE_DIR/chain_heads.log"   # root-only (600): the custody anchor
BIN=/opt/yantrik-mind/mind-evals
KILL="$STATE/IMMUNE_OFF"

[ -f "$KILL" ] && { echo "kill-switch present ($KILL) — immune trials disabled"; exit 0; }
[ -x "$BIN" ] || { echo "mind-evals binary missing at $BIN (deploy it via build_on_box.sh)"; exit 1; }
[ -f "$LIVE_DB" ] || { echo "live db missing at $LIVE_DB"; exit 1; }

mkdir -p "$IMMUNE_DIR"
chmod 755 "$IMMUNE_DIR"

# Cold-copy the live db WAL-safely. sqlite3 CLI does VACUUM INTO in read-only
# mode; the mind's own writes serialize against the read transaction.
SNAP="$(mktemp -u "$IMMUNE_DIR/snap.XXXXXX.db")"
trap 'rm -f "$SNAP"' EXIT
sqlite3 "file:$LIVE_DB?mode=ro" "VACUUM INTO '$SNAP'"

# Local-only critic when configured (YM_CRITIC_URL must be a home-lab
# endpoint — belief text never leaves home hardware); null baseline otherwise.
CRITIC=null
if [ -n "${YM_CRITIC_URL:-}" ] && [ -n "${YM_CRITIC_MODEL:-}" ]; then
  case "$YM_CRITIC_URL" in
    http://192.168.*|http://127.0.0.1*|http://localhost*|http://10.*) CRITIC=api ;;
    *) echo "REFUSING non-local YM_CRITIC_URL=$YM_CRITIC_URL — null critic instead" ;;
  esac
fi

# --anchors closes the valid-prefix-truncation hole: the run refuses if the
# last root-anchored head is no longer present in the (internally valid) chain.
"$BIN" immune --db "$SNAP" --pairs 15 --ledger "$LEDGER" --summary "$SUMMARY" --critic "$CRITIC" --anchors "$HEADS"

# Custody anchor: append the chain head where only root can write.
HEAD="$(python3 -c "import json;print(json.load(open('$SUMMARY'))['chain_head'])")"
echo "$(date -u +%FT%TZ) $HEAD" >> "$HEADS"
chmod 600 "$HEADS"
chmod 644 "$LEDGER" "$SUMMARY"
echo "immune trial done — chain head $HEAD"
