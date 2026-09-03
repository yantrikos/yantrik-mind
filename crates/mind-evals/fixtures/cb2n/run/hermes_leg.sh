#!/bin/bash
# E.CB2 Hermes leg v3, contained: the pinned image on the INTERNAL network, the model reachable
# only through this run's counting proxy (429 from request 9), fresh per-task HERMES_HOME with
# agent.max_turns 8, toolsets file,terminal,code_execution, /work + home the only writable
# mounts, 1800 s wall. Cleanup guaranteed by a trap. Receipt: counts only; raw stdout kept in a
# separate file. Fails closed on a missing session id or log.
set -u
T=$1; OUT=${2:-/root/cb2n/out}; WALL=${CB2_WALL:-1800}
FIX="$(cd "$(dirname "$0")/.." && pwd)"; export CB2_OUT="$OUT"; . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1
# AFTER the loader, which is what exports CB2_CAP. It was read one line BEFORE, so the proxy
# enforced 24 while this leg checked against 8 and failed a run that had finished inside its budget.
CAP=${CB2_CAP:-8}
cb2_rerun_prepare hermes "$T" "$OUT" || exit 2
W="$OUT/artifacts/hermes_$T"; H="$OUT/homes/hermes_$T"; R="$OUT/receipts"; CD="$OUT/proxy/hermes_$T"; RAW="$OUT/raw"
[ -e "$W" ] && { echo "refusing: $W exists (one invocation per task)"; exit 2; }
# runs are SEQUENTIAL: refuse if anything is still attached to either network (a stale proxy or a
# cross-run container would stay reachable)
for NET in cb2net cb2egress; do
  ATT=$(docker network inspect $NET --format '{{len .Containers}}' 2>/dev/null || echo missing)
  [ "$ATT" = "0" ] || { echo "refusing: $NET has $ATT attached container(s) — runs are sequential"; exit 3; }
done
mkdir -p "$W" "$H" "$R" "$RAW"; chown -R 10001:10001 "$W" "$H"
NAME="cb2-hermes-$T"; PROXY="cb2proxy-hermes-$T"; PIP=172.30.0.3
cleanup() { docker rm -f "$NAME" >/dev/null 2>&1; bash "$FIX/run/proxy.sh" down "$PROXY" >/dev/null 2>&1; chmod -R a-w "$W" 2>/dev/null; }
trap cleanup EXIT
printf 'model:\n  default: %s\n  provider: custom\n  base_url: http://%s:8080/v1\nagent:\n  max_turns: %s\n' "$CB2_MODEL" "$PIP" "$CAP" > "$H/config.yaml"
chown 10001:10001 "$H/config.yaml"
bash "$FIX/run/proxy.sh" up "$PROXY" "$CD" "$PIP" >/dev/null || { echo "proxy not ready — leg aborted, nothing graded"; printf '{"system":"hermes","task":"%s","status":"proxy-not-ready","void":true,"dq_independent":false,"dq_dependent":false,"disqualified":false}\n' "$T" | tee "$R/hermes_$T.json"; exit 4; }
ARCHIVE_SHA=$(sha256sum "$FIX/docker/hermes-3ce1cf2.tar.gz" 2>/dev/null | cut -c1-64)
[ "$ARCHIVE_SHA" = "30698554ea31ae928ab757c6ab67ae0c38f83bee6974de31193d62533590aaac" ] || { echo "archive is not the pinned commit — leg aborted"; printf '{"system":"hermes","task":"%s","status":"archive-mismatch","disqualified":true}\n' "$T" | tee "$R/hermes_$T.json"; exit 5; }
BRIEF="$(cat "$FIX/briefs/$T.txt")"
START=$(date -u +%Y-%m-%dT%H:%M:%SZ); T0=$(date +%s)
timeout -k 10 $WALL docker run --name "$NAME" --network cb2net --dns 127.0.0.1 \
  --memory 4g --cpus 4 --pids-limit 512 --read-only --tmpfs /tmp:size=512m \
  -v "$W:/work" -v "$H:/hermes_home" -e HERMES_HOME=/hermes_home -e OPENAI_API_KEY=none -e OPENAI_BASE_URL="http://$PIP:8080/v1" \
  cb2-hermes chat -Q -t file,terminal,code_execution -q "Work only inside the current directory (/work). $BRIEF" > "$RAW/hermes_${T}_stdout.txt" 2>&1
RC=$?
END=$(date -u +%Y-%m-%dT%H:%M:%SZ); WALLS=$(( $(date +%s) - T0 ))
ENVLEAK=$(cb2_env_leak_hits "$NAME")
docker rm -f "$NAME" >/dev/null 2>&1
chmod -R a-w "$W" 2>/dev/null
SID=$(grep -o "session_id: [0-9a-z_]*" "$RAW/hermes_${T}_stdout.txt" | tail -1 | cut -d' ' -f2)
LOG="$H/logs/agent.log"
if [ -z "$SID" ] || [ ! -f "$LOG" ]; then VALID=false; CALLS=-1; TOKS="absent"; else
  VALID=true; CALLS=$(grep -c "\[$SID\] agent.conversation_loop: API call #" "$LOG")
  TOKS=$(grep "\[$SID\] agent.conversation_loop: API call #" "$LOG" | sed -E 's/.*in=([0-9]+) out=([0-9]+).*/\1 \2/' | awk '{i+=$1;o+=$2} END {print i" "o}')
fi
if [ -f "$CD/requests.json" ] && read -r PACC PREF PTLS PUPE <<< "$(python3 -c "import json;d=json.load(open('$CD/requests.json'));print(int(d['model_requests']), int(d['refused_over_cap']), d.get('tls_hostname_verified'), int(d['upstream_errors']))" 2>/dev/null)"; then PPRESENT=true; else PACC=-1; PREF=-1; PTLS=absent; PUPE=-1; PPRESENT=false; fi
# RECORDED, NOT ENFORCED. This is a regex over a whole transcript, so it counts MENTIONS: a curl
# example in a generated README, an HTTP error page echoing a prompt, a model reasoning aloud
# about whether wget exists. It fired on `curl -s http://0.0.0.0:8123/` — an agent fetching its
# own local server to verify a page it had just built, on a task that ASKS for a runnable server.
# The act it names is impossible here (internal network, no DNS, the proxy forwards only to the
# run state's upstream) and a real attempt would appear as a blocked connection, which the
# network layer and the proxy counters already record. Keep the number; stop deciding with it.
DL=$(grep -ciE "pip install|pip3 install|npm install|npm i |apt-get|apt install|curl |wget " "$RAW/hermes_${T}_stdout.txt")
LEAK=$(( ${ENVLEAK:-0} + $(cb2_key_leak_hits "$W" "$H" "$RAW/hermes_${T}_stdout.txt") ))
read -r RC_VALID PHTTP PTRANS PCLIENT PDISC RC_ACC RC_REF MODEL_OK NMODELS UP UC UN <<< "$(python3 "$FIX/run/receipt_checks.py" "$CD/requests.json" "$CB2_MODEL" | sed -E 's/[a-z_]+=//g')"
# VOID (infrastructure, never the agent's fault): a TYPED receipt showing an upstream 429/5xx on a
# model request or a transport/TLS failure. An untyped or missing receipt is NOT evidence of an
# infrastructure fault — it is an independent disqualification below.
VOID=false
if [ "$RC_VALID" = true ] && { [ "$PHTTP" != 0 ] || [ "$PTRANS" != 0 ]; }; then VOID=true; fi
HASH=$(timeout -k 5 60 python3 "$FIX/tools/tree_hash.py" "$W")
IMG=$(docker image inspect cb2-hermes --format '{{.Id}}')
# disqualification: INDEPENDENT violations always — including the CAP EVIDENCE, which must stand
# on its own: a refusal or an over-cap count is the agent exceeding its budget whatever the
# upstream was doing at the time. DEPENDENT ones (exit code/wall, receipt shape, model identity)
# only when the leg is not void.
# A CAP REFUSAL IS NOT A VIOLATION, on this side either. It was removed from the Mind's rule and
# left here, which made the asymmetry point the other way — at the opponent — one commit after
# the commit that claimed to remove it. `DL` is likewise RECORDED and no longer disqualifies:
# see the note above ACC_AGREE for why a transcript scan is not evidence of a download.
DQ_IND=false; [ "$VALID" = false ] && DQ_IND=true; [ "$CALLS" -gt $CAP ] && DQ_IND=true; [ "$LEAK" != 0 ] && DQ_IND=true
[ "$RC_VALID" = true ] || DQ_IND=true          # missing/malformed proxy receipt: independent, never a void
if [ "$RC_VALID" = true ]; then [ "$RC_ACC" -gt $CAP ] && DQ_IND=true; fi   # exceeding the cap, not reaching it
DQ_DEP=false; [ $RC -ne 0 ] && DQ_DEP=true; [ "$MODEL_OK" = true ] || DQ_DEP=true
# strict proxy receipt: typed non-negative integers, 1 <= accepted <= CAP, refused 0, TLS true.
# The own-log agreement is NO LONGER part of it: the proxy is the authoritative meter and enforces
# the cap before the model, so an agent cannot hide spend from it. Whether its own log AGREES with
# that meter characterises its self-accounting and is recorded as `own_log_agrees` -- it is not a
# reason to throw away an artifact. Relaxed for BOTH systems in the same commit.
ACC_AGREE=$(python3 -c "
try:
    import json; d=json.load(open('$CD/requests.json')); print('true' if d['model_requests']==int('$CALLS') else 'false')
except Exception:
    print('false')")
RECEIPT_OK=$(python3 -c "import json,sys
sys.path.insert(0, '$FIX/run')
from verdict import receipt_shape_ok
try:
    d=json.load(open('$CD/requests.json'))
    ok=receipt_shape_ok(accepted=d['model_requests'], refused=d['refused_over_cap'],
                        upstream_errors=d['upstream_errors'],
                        tls_verified=d.get('tls_hostname_verified'), cap=$CAP)
except Exception:
    ok=False
print('true' if ok else 'false')")
[ "$RECEIPT_OK" = true ] || DQ_DEP=true
# exact tree hash: 64 hex + files/bytes/symlinks/specials fields, zero unsafe nodes
echo "$HASH" | grep -Eq '^[0-9a-f]{64} files=[0-9]+ bytes=[0-9]+ symlinks=0 specials=0$' || DQ_IND=true
DQ=false; [ "$DQ_IND" = true ] && DQ=true; [ "$VOID" = false ] && [ "$DQ_DEP" = true ] && DQ=true
printf '{"system":"hermes","task":"%s","image":"%s","commit":"3ce1cf2bb768f39026e059f5236522dea2a4afe3","archive_sha256":"%s","started":"%s","finished":"%s","wall_s":%s,"rc":%s,"timed_out":%s,"valid_log":%s,"session":"%s","api_calls_from_log":%s,"proxy_receipt_present":%s,"proxy_accepted":%s,"proxy_refused":%s,"proxy_attempted":%s,"proxy_tls_hostname_verified":"%s","proxy_upstream_errors":%s,"tokens_in_out":"%s","download_or_install_lines":%s,"proxy_receipt_ok":%s,"own_log_agrees":%s,"profile":"%s","upstream":"%s","upstream_ips":"%s","resolved_at":"%s","run_state":"%s","run_state_sha256":"%s","model":"%s","model_ok":%s,"receipt_valid":%s,"key_leak_hits":%s,"upstream_http_errors":%s,"upstream_transport_errors":%s,"upstream_client_errors":%s,"client_disconnects":%s,"usage_prompt_tokens":%s,"usage_completion_tokens":%s,"usage_responses":%s,"void":%s,"dq_independent":%s,"dq_dependent":%s,"disqualified":%s,"tree":"%s"}\n' \
  "$T" "$IMG" "$ARCHIVE_SHA" "$START" "$END" "$WALLS" "$RC" "$([ $RC -eq 124 ] && echo true || echo false)" "$VALID" "$SID" "$CALLS" "$PPRESENT" "$PACC" "$PREF" "$(( PACC + PREF ))" "$PTLS" "$PUPE" "$TOKS" "$DL" "$RECEIPT_OK" "$ACC_AGREE" "$CB2_PROFILE" "$CB2_UPSTREAM" "$CB2_UPSTREAM_IPS" "$CB2_RESOLVED_AT" "$CB2_RUN_STATE" "$CB2_RUN_STATE_SHA" "$CB2_MODEL" "$MODEL_OK" "$RC_VALID" "$LEAK" "$PHTTP" "$PTRANS" "$PCLIENT" "$PDISC" "$UP" "$UC" "$UN" "$VOID" "$DQ_IND" "$DQ_DEP" "$DQ" "$HASH" | tee "$R/hermes_$T.json"
