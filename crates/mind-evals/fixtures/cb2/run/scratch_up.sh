#!/bin/bash
# E.CB2 scratch instance of the staging binary: fresh state, own port, owned Qwen lane ONLY,
# no Telegram, no proactive loops, no actuators. Never touches /var/lib/yantrik-mind.
set -u
D=/var/lib/ym-cb2
PORT=8099
if [ -d "$D" ]; then echo "refusing: $D exists (tear down first)"; exit 2; fi
mkdir -p "$D/public"
cat > "$D/env" <<EOF
YM_DB=$D/mind.db
YM_WEB_DIR=$D/public
YM_WEB_PORT=$PORT
YM_WEBUI_PORT=8091
YM_CTL_PORT=8078
YM_WEB_URL=http://127.0.0.1:$PORT
YM_OPERATOR=cb2
YM_TZ=Asia/Kolkata
YM_LOCAL_OLLAMA_URL=https://aig.mycluster.cyou
YM_LOCAL_OLLAMA_MODEL=qwen3.8:27b-q4_K_M
YM_PRIVATE_PROVIDERS=ollama-local
YM_HOUSEHOLD_PROVIDERS=ollama-local
YM_INFER_PERMITS=2
YM_DMN=off
YM_PROACTIVE=off
YM_PATTERNS=off
YM_HOME_WATCH=off
EOF
echo "binary: $(sha256sum /opt/yantrik-mind/mind-core | cut -c1-16)  provenance: $(cd /root/codes/ym-autodeploy && git rev-parse --short HEAD)"
cd "$D" && env -i PATH=/usr/local/bin:/usr/bin:/bin HOME=/root $(cat "$D/env" | xargs) nohup /opt/yantrik-mind/mind-core > "$D/stdout.log" 2>&1 &
echo $! > "$D/pid"
for i in $(seq 1 60); do
  if curl -s -m 2 -o /dev/null "http://127.0.0.1:$PORT/"; then break; fi; sleep 1
done
echo "up: pid $(cat $D/pid), port $PORT, started $(date -u +%H:%M:%SZ)"
grep -m3 -E "brain:|REFUSED|registration" "$D/stdout.log" | sed -E 's/(KEY|TOKEN)=\S+/\1=<r>/'
echo "pairing code file: $D/web-pairing.code ($( [ -f $D/web-pairing.code ] && echo present || echo absent ))"
