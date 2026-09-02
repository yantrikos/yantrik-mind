#!/bin/bash
# Hermes smoke through a run proxy: the pinned image, a throwaway home with max_turns 2, one
# single-word prompt (NOT a task brief). Proves the contained path end to end. Counts only.
set -u
FIX="$(cd "$(dirname "$0")/.." && pwd)"; OUT=$(mktemp -d /tmp/cb2-smoke-hermes-XXXX)
trap 'docker rm -f cb2-hermes-smoke >/dev/null 2>&1; bash "$FIX/run/proxy.sh" down cb2proxy-hermes-smoke >/dev/null 2>&1; rm -rf "$OUT"' EXIT
mkdir -p "$OUT/home" "$OUT/work" "$OUT/count"; chown -R 10001:10001 "$OUT/home" "$OUT/work"
bash "$FIX/run/proxy.sh" up cb2proxy-hermes-smoke "$OUT/count" 172.30.0.6 >/dev/null || { echo "proxy not ready"; exit 1; }
printf 'model:\n  default: qwen3.8:27b-q4_K_M\n  provider: custom\n  base_url: http://172.30.0.6:8080/v1\nagent:\n  max_turns: 2\n' > "$OUT/home/config.yaml"; chown 10001:10001 "$OUT/home/config.yaml"
timeout -k 10 300 docker run --name cb2-hermes-smoke --rm --network cb2net --dns 127.0.0.1 --read-only --tmpfs /tmp:size=256m \
  -v "$OUT/work:/work" -v "$OUT/home:/hermes_home" -e HERMES_HOME=/hermes_home -e OPENAI_API_KEY=none -e OPENAI_BASE_URL=http://172.30.0.6:8080/v1 \
  cb2-hermes chat -Q -t file,terminal,code_execution -q "Reply with the single word READY and nothing else." > "$OUT/stdout.txt" 2>&1
RC=$?
READY=$(grep -c "^READY" "$OUT/stdout.txt"); SID=$(grep -o "session_id: [0-9a-z_]*" "$OUT/stdout.txt" | tail -1 | cut -d' ' -f2)
CALLS=$(grep -c "\[$SID\] agent.conversation_loop: API call #" "$OUT/home/logs/agent.log" 2>/dev/null)
python3 - "$OUT/count/requests.json" "$RC" "$READY" "$CALLS" <<'EOF'
import json, sys
d = json.load(open(sys.argv[1])); rc, ready, calls = sys.argv[2:]
print(f"hermes smoke: rc {rc} / READY-line {ready} / log api_calls {calls} / proxy model_requests {d['model_requests']} / refused {d['refused_over_cap']} / upstream_errors {d['upstream_errors']} / tls {d.get('tls_hostname_verified')}")
sys.exit(0 if (rc, ready, d["refused_over_cap"], d["upstream_errors"], d.get("tls_hostname_verified")) == ("0", "1", 0, 0, True) and int(calls) == d["model_requests"] >= 1 else 1)
EOF
