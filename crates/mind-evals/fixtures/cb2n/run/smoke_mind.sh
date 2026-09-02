#!/bin/bash
# No-model Mind smoke: boot the containerised staging binary through a run proxy, pair from
# inside, submit NOTHING, and prove the proxy saw zero model requests. Counts only. Exit non-zero
# unless boot lines, pairing 200 and a zero-request receipt are all present.
set -u
FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=$(mktemp -d /tmp/cb2-smoke-mind-XXXX); . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1
if [ "$CB2_MIND_LANE" = local ]; then
  LANE=(-e YM_LOCAL_OLLAMA_URL=http://172.30.0.7:8080 -e YM_LOCAL_OLLAMA_MODEL="$CB2_MODEL" -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local); BOOTLINE="LOCAL primary + private lane active (ollama-local:$CB2_MODEL)"
else
  PU=$(echo "$CB2_MIND_PROVIDER" | tr 'a-z-' 'A-Z_')
  LANE=(-e YM_PRIMARY_BRAIN="$CB2_MIND_PROVIDER:$CB2_MODEL" -e "YM_PROVIDER_BASE_URL_$PU=http://172.30.0.7:8080/v1" -e "$CB2_MIND_KEY_ENV=none" -e YM_PRIVATE_PROVIDERS= -e YM_HOUSEHOLD_PROVIDERS="$CB2_MIND_PROVIDER,chain"); BOOTLINE="$CB2_MIND_PROVIDER:$CB2_MODEL"
fi
trap 'docker rm -f cb2-mind-smoke >/dev/null 2>&1; bash "$FIX/run/proxy.sh" down cb2proxy-mind-smoke >/dev/null 2>&1; rm -rf "$OUT"' EXIT
mkdir -p "$OUT/state/public" "$OUT/count"; chown -R 10003:10003 "$OUT/state"
bash "$FIX/run/proxy.sh" up cb2proxy-mind-smoke "$OUT/count" 172.30.0.7 >/dev/null || { echo "proxy not ready"; exit 1; }
docker run -d --name cb2-mind-smoke --network cb2net --dns 127.0.0.1 --memory 4g --cpus 4 --pids-limit 512 --read-only --tmpfs /tmp:size=256m \
  -v /opt/yantrik-mind/mind-core:/mind-core:ro -v "$OUT/state:/state" -v "$OUT/count:/count:ro" \
  -e YM_DB=/state/mind.db -e YM_WEB_DIR=/state/public -e YM_WEB_PORT=8099 -e YM_WEB_URL=http://127.0.0.1:8099 -e YM_WEBUI_PORT=8091 -e YM_CTL_PORT=8078 \
  -e YM_OPERATOR=cb2 -e YM_TZ=Asia/Kolkata "${LANE[@]}" -e YM_INFER_PERMITS=2 \
  -e YM_DMN=off -e YM_PROACTIVE=off -e YM_PATTERNS=off -e YM_HOME_WATCH=off cb2-mind /mind-core >/dev/null || { echo "container did not start"; exit 1; }
sleep 12
LOCAL=$(docker logs cb2-mind-smoke 2>&1 | grep -cF -- "$BOOTLINE")
REG=$(docker logs cb2-mind-smoke 2>&1 | grep -c "first-time registration")
PAIR=$(docker exec cb2-mind-smoke python3 -c "
import urllib.request, json, pathlib
code = pathlib.Path('/state/web-pairing.code').read_text().strip()
req = urllib.request.Request('http://127.0.0.1:8091/api/pair', method='POST', data=json.dumps({'code': code, 'name': 'smoke'}).encode(), headers={'Content-Type': 'application/json', 'x-ym-web': 'cb2'})
print(urllib.request.urlopen(req, timeout=20).status)" 2>/dev/null)
python3 - "$OUT/count/requests.json" "$LOCAL" "$REG" "$PAIR" <<'EOF'
import json, sys
d = json.load(open(sys.argv[1])); local, reg, pair = sys.argv[2:]
print(f"mind smoke [{d.get('profile')} -> {d.get('upstream')}]: brain-line {local} / registration-line {reg} / pair-status {pair} / proxy model_requests {d['model_requests']} / upstream_errors {d['upstream_errors']} / tls {d.get('tls_hostname_verified')}")
sys.exit(0 if (local, reg, pair, d["model_requests"], d["upstream_errors"], d.get("tls_hostname_verified")) == ("1", "1", "200", 0, 0, True) else 1)
EOF
