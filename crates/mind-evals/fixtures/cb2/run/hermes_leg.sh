#!/bin/bash
# E.CB2 Hermes leg (MANIFEST.json systems.hermes). One top-level invocation, isolated profile, owned
# Qwen endpoint, toolsets file,terminal,code_execution, fresh directory, 1800 s wall. Receipt = counts.
set -u
T=$1; WALL=${2:-1800}
FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=${CB2_OUT:?set CB2_OUT to the run root}
H="$OUT/hermes_home/profiles/cb2"; mkdir -p "$H" "$OUT/receipts"
[ -f "$H/config.yaml" ] || printf 'model:\n  default: qwen3.8:27b-q4_K_M\n  provider: custom\n  base_url: https://aig.mycluster.cyou/v1\n' > "$H/config.yaml"
W="$OUT/artifacts/hermes_$T"; [ -e "$W" ] && { echo "refusing: $W exists (one invocation per task)"; exit 2; }; mkdir -p "$W"
BRIEF="$(cat "$FIX/briefs/$T.txt")"
START=$(date -u +%Y-%m-%dT%H:%M:%SZ); T0=$(date +%s)
cd "$W" && HERMES_HOME="$H" OPENAI_API_KEY=none OPENAI_BASE_URL=https://aig.mycluster.cyou/v1 \
  timeout "$WALL" hermes chat -Q -t file,terminal,code_execution -q "Work only inside the current directory ($(pwd)). $BRIEF" > "$OUT/receipts/hermes_${T}_stdout.txt" 2>&1
RC=$?
END=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))
chmod -R a-w "$W" 2>/dev/null
SID=$(grep -o "session_id: [0-9a-z_]*" "$OUT/receipts/hermes_${T}_stdout.txt" | tail -1 | cut -d' ' -f2)
CALLS=$(grep -c "\[$SID\] agent.conversation_loop: API call #" "$H/logs/agent.log")
TOKS=$(grep "\[$SID\] agent.conversation_loop: API call #" "$H/logs/agent.log" | sed -E 's/.*in=([0-9]+) out=([0-9]+).*/\1 \2/' | awk '{i+=$1;o+=$2} END {print i" "o}')
DL=$(grep -ciE "pip install|npm install|npm i |apt-get|curl -|wget " "$OUT/receipts/hermes_${T}_stdout.txt")
HASH=$(python "$FIX/tools/tree_hash.py" "$W")
echo "{\"system\":\"hermes\",\"task\":\"$T\",\"started\":\"$START\",\"finished\":\"$END\",\"wall_s\":$WALLS,\"rc\":$RC,\"session\":\"$SID\",\"api_calls\":$CALLS,\"tokens_in_out\":\"$TOKS\",\"download_or_install_lines\":$DL,\"disqualified\":$([ "$CALLS" -gt 8 ] || [ "$DL" -gt 0 ] && echo true || echo false),\"tree\":\"$HASH\"}" | tee "$OUT/receipts/hermes_$T.json"
