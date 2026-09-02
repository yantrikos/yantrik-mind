#!/bin/bash
# E.CB2 Hermes leg, ON THE BOX, contained: the pinned image, a fresh per-task HERMES_HOME with
# agent.max_turns=8 (request 9 cannot happen), toolsets file,terminal,code_execution, the task
# directory as the only writable mount besides the home, egress = the owned endpoint only,
# 1800 s wall (the container is removed on timeout). Fail closed: no session id or log → invalid.
set -u
T=$1; OUT=${2:-/root/cb2/out}; WALL=1800; CAP=8
FIX="$(cd "$(dirname "$0")/.." && pwd)"
W="$OUT/artifacts/hermes_$T"; H="$OUT/hermes_home_$T"; R="$OUT/receipts"
[ -e "$W" ] && { echo "refusing: $W exists (one invocation per task)"; exit 2; }
mkdir -p "$W" "$H" "$R"; chown -R 10001:10001 "$W" "$H"
cat > "$H/config.yaml" <<EOF
model:
  default: qwen3.8:27b-q4_K_M
  provider: custom
  base_url: https://aig.mycluster.cyou/v1
agent:
  max_turns: $CAP
EOF
chown 10001:10001 "$H/config.yaml"
BRIEF="$(cat "$FIX/briefs/$T.txt")"
START=$(date -u +%Y-%m-%dT%H:%M:%SZ); T0=$(date +%s); NAME="cb2-hermes-$T-$T0"
timeout -k 10 $WALL docker run --name "$NAME" --rm --network cb2net --add-host aig.mycluster.cyou:192.168.4.203 --dns 127.0.0.1 \
  --memory 4g --cpus 4 --pids-limit 512 --read-only --tmpfs /tmp:size=512m \
  -v "$W:/work" -v "$H:/hermes_home" -e HERMES_HOME=/hermes_home -e OPENAI_API_KEY=none -e OPENAI_BASE_URL=https://aig.mycluster.cyou/v1 \
  cb2-hermes chat -Q -t file,terminal,code_execution -q "Work only inside the current directory (/work). $BRIEF" > "$R/hermes_${T}_stdout.txt" 2>&1
RC=$?
docker rm -f "$NAME" >/dev/null 2>&1
END=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))
chmod -R a-w "$W" 2>/dev/null
SID=$(grep -o "session_id: [0-9a-z_]*" "$R/hermes_${T}_stdout.txt" | tail -1 | cut -d' ' -f2)
LOG="$H/logs/agent.log"
if [ -z "$SID" ] || [ ! -f "$LOG" ]; then VALID=false; CALLS=-1; TOKS="absent"; else
  VALID=true
  CALLS=$(grep -c "\[$SID\] agent.conversation_loop: API call #" "$LOG")
  TOKS=$(grep "\[$SID\] agent.conversation_loop: API call #" "$LOG" | sed -E 's/.*in=([0-9]+) out=([0-9]+).*/\1 \2/' | awk '{i+=$1;o+=$2} END {print i" "o}')
fi
DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$R/hermes_${T}_stdout.txt")
HASH=$(python3 "$FIX/tools/tree_hash.py" "$W")
DQ=false; [ "$VALID" = false ] && DQ=true; [ "$CALLS" -gt $CAP ] && DQ=true; [ "$DL" -gt 0 ] && DQ=true; [ $RC -eq 124 ] && DQ=true
printf '{"system":"hermes","task":"%s","image":"cb2-hermes","commit":"3ce1cf2bb768f39026e059f5236522dea2a4afe3","started":"%s","finished":"%s","wall_s":%s,"rc":%s,"timed_out":%s,"valid_log":%s,"session":"%s","api_calls":%s,"tokens_in_out":"%s","download_or_install_lines":%s,"disqualified":%s,"tree":"%s"}\n' \
  "$T" "$START" "$END" "$WALLS" "$RC" "$([ $RC -eq 124 ] && echo true || echo false)" "$VALID" "$SID" "$CALLS" "$TOKS" "$DL" "$DQ" "$HASH" | tee "$R/hermes_$T.json"
