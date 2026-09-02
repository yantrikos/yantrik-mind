#!/bin/bash
# E.CB2 Hermes leg v3, contained: the pinned image on the INTERNAL network, the model reachable
# only through this run's counting proxy (429 from request 9), fresh per-task HERMES_HOME with
# agent.max_turns 8, toolsets file,terminal,code_execution, /work + home the only writable
# mounts, 1800 s wall. Cleanup guaranteed by a trap. Receipt: counts only; raw stdout kept in a
# separate file. Fails closed on a missing session id or log.
set -u
T=$1; OUT=${2:-/root/cb2/out}; WALL=1800; CAP=8
FIX="$(cd "$(dirname "$0")/.." && pwd)"
W="$OUT/artifacts/hermes_$T"; H="$OUT/homes/hermes_$T"; R="$OUT/receipts"; CD="$OUT/proxy/hermes_$T"; RAW="$OUT/raw"
[ -e "$W" ] && { echo "refusing: $W exists (one invocation per task)"; exit 2; }
mkdir -p "$W" "$H" "$R" "$RAW"; chown -R 10001:10001 "$W" "$H"
NAME="cb2-hermes-$T"; PROXY="cb2proxy-hermes-$T"
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1; bash "$FIX/run/proxy.sh" down "$PROXY" >/dev/null 2>&1; chmod -R a-w "$W" 2>/dev/null; }
trap cleanup EXIT
printf 'model:\n  default: qwen3.8:27b-q4_K_M\n  provider: custom\n  base_url: http://%s:8080/v1\nagent:\n  max_turns: %s\n' "$PROXY" "$CAP" > "$H/config.yaml"
chown 10001:10001 "$H/config.yaml"
bash "$FIX/run/proxy.sh" up "$PROXY" "$CD" >/dev/null
BRIEF="$(cat "$FIX/briefs/$T.txt")"
START=$(date -u +%Y-%m-%dT%H:%M:%SZ); T0=$(date +%s)
timeout -k 10 $WALL docker run --name "$NAME" --network cb2net --dns 127.0.0.1 \
  --memory 4g --cpus 4 --pids-limit 512 --read-only --tmpfs /tmp:size=512m \
  -v "$W:/work" -v "$H:/hermes_home" -e HERMES_HOME=/hermes_home -e OPENAI_API_KEY=none -e OPENAI_BASE_URL="http://$PROXY:8080/v1" \
  cb2-hermes chat -Q -t file,terminal,code_execution -q "Work only inside the current directory (/work). $BRIEF" > "$RAW/hermes_${T}_stdout.txt" 2>&1
RC=$?
END=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))
docker rm -f "$NAME" >/dev/null 2>&1
chmod -R a-w "$W" 2>/dev/null
SID=$(grep -o "session_id: [0-9a-z_]*" "$RAW/hermes_${T}_stdout.txt" | tail -1 | cut -d' ' -f2)
LOG="$H/logs/agent.log"
if [ -z "$SID" ] || [ ! -f "$LOG" ]; then VALID=false; CALLS=-1; TOKS="absent"; else
  VALID=true; CALLS=$(grep -c "\[$SID\] agent.conversation_loop: API call #" "$LOG")
  TOKS=$(grep "\[$SID\] agent.conversation_loop: API call #" "$LOG" | sed -E 's/.*in=([0-9]+) out=([0-9]+).*/\1 \2/' | awk '{i+=$1;o+=$2} END {print i" "o}')
fi
PROXYN=$(python3 -c "import json;d=json.load(open('$CD/requests.json'));print(d['model_requests'], d['refused_over_cap'])")
DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$RAW/hermes_${T}_stdout.txt")
HASH=$(python3 "$FIX/tools/tree_hash.py" "$W")
IMG=$(docker image inspect cb2-hermes --format '{{.Id}}')
DQ=false; [ "$VALID" = false ] && DQ=true; [ "$CALLS" -gt $CAP ] && DQ=true; [ "$DL" -gt 0 ] && DQ=true; [ $RC -eq 124 ] && DQ=true
printf '{"system":"hermes","task":"%s","image":"%s","commit":"3ce1cf2bb768f39026e059f5236522dea2a4afe3","started":"%s","finished":"%s","wall_s":%s,"rc":%s,"timed_out":%s,"valid_log":%s,"session":"%s","api_calls_from_log":%s,"proxy_model_requests_and_refusals":"%s","tokens_in_out":"%s","download_or_install_lines":%s,"disqualified":%s,"tree":"%s"}\n' \
  "$T" "$IMG" "$START" "$END" "$WALLS" "$RC" "$([ $RC -eq 124 ] && echo true || echo false)" "$VALID" "$SID" "$CALLS" "$PROXYN" "$TOKS" "$DL" "$DQ" "$HASH" | tee "$R/hermes_$T.json"
