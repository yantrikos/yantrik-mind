#!/bin/bash
# E.CB2 Mind leg v3, contained: the staging binary bind-mounted read-only into the cb2-mind
# image on the INTERNAL network, fresh state volume per task, the model reachable only through
# this run's counting proxy (429 from request 9), no cloud keys, no Telegram, no coder, loops
# off. The driver runs INSIDE the container (console API is loopback there). Cleanup by trap;
# the state volume is removed after a counts-only teardown receipt.
set -u
T=$1; OUT=${2:-/root/cb2/out}; WALL=1800
FIX="$(cd "$(dirname "$0")/.." && pwd)"; . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1
cb2_rerun_prepare mind "$T" "$OUT" || exit 2
A="$OUT/artifacts/mind_$T"; R="$OUT/receipts"; CD="$OUT/proxy/mind_$T"; ST="$OUT/state/mind_$T"
[ -e "$A" ] && { echo "refusing: $A exists (one invocation per task)"; exit 2; }
# runs are SEQUENTIAL: refuse if anything is still attached to either network (a stale proxy or a
# cross-run container would stay reachable)
for NET in cb2net cb2egress; do
  ATT=$(docker network inspect $NET --format '{{len .Containers}}' 2>/dev/null || echo missing)
  [ "$ATT" = "0" ] || { echo "refusing: $NET has $ATT attached container(s) — runs are sequential"; exit 3; }
done
mkdir -p "$A" "$R" "$ST/public" "$OUT/raw"; chown -R 10003:10003 "$ST" "$A"
NAME="cb2-mind-$T"; PROXY="cb2proxy-mind-$T"; PIP=172.30.0.2
# model lane by profile: "local" = the owned endpoint as the local/private lane (v3 behaviour);
# "roles" = YM_PRIMARY_BRAIN=<provider>:<model> behind YM_PROVIDER_BASE_URL_<PROVIDER> (the proxy),
# a placeholder key in the container, no local lane (the scratch state holds nothing private).
if [ "$CB2_MIND_LANE" = local ]; then
  LANE=(-e YM_LOCAL_OLLAMA_URL="http://$PIP:8080" -e YM_LOCAL_OLLAMA_MODEL="$CB2_MODEL" -e YM_PRIVATE_PROVIDERS=ollama-local -e YM_HOUSEHOLD_PROVIDERS=ollama-local)
else
  PU=$(echo "$CB2_MIND_PROVIDER" | tr 'a-z-' 'A-Z_')
  LANE=(-e YM_PRIMARY_BRAIN="$CB2_MIND_PROVIDER:$CB2_MODEL" -e "YM_PROVIDER_BASE_URL_$PU=http://$PIP:8080/v1" -e "$CB2_MIND_KEY_ENV=none" -e YM_PRIVATE_PROVIDERS= -e YM_HOUSEHOLD_PROVIDERS="$CB2_MIND_PROVIDER,chain")
fi
cleanup() {
  docker rm -f "$NAME" >/dev/null 2>&1; bash "$FIX/run/proxy.sh" down "$PROXY" >/dev/null 2>&1
  { echo "teardown $(date -u +%Y-%m-%dT%H:%M:%SZ)"; find "$ST" -type f ! -name 'mind.db*' -printf '%s %P\n' | sort -k2 | head -40
    echo "spend rows: $(grep -c '"kind":"inference_call"' "$ST/mind.db.decisions.jsonl" 2>/dev/null || echo 0)"; } > "$R/mind_${T}_teardown.txt" 2>/dev/null
  rm -rf "$ST"; chmod -R a-w "$A" 2>/dev/null
}
trap cleanup EXIT
bash "$FIX/run/proxy.sh" up "$PROXY" "$CD" "$PIP" >/dev/null || { echo "proxy not ready — leg aborted, nothing graded"; printf '{"system":"mind","task":"%s","status":"proxy-not-ready","disqualified":true}\n' "$T" | tee "$R/mind_$T.json"; exit 4; }
BIN_SHA=$(sha256sum /opt/yantrik-mind/mind-core | cut -c1-64); PROV=$(cd /root/codes/ym-autodeploy && git rev-parse --short HEAD)
docker run -d --name "$NAME" --network cb2net --dns 127.0.0.1 --memory 4g --cpus 4 --pids-limit 512 --read-only --tmpfs /tmp:size=256m \
  -v /opt/yantrik-mind/mind-core:/mind-core:ro -v "$ST:/state" -v "$FIX:/fixtures:ro" -v "$CD:/count:ro" \
  -e YM_DB=/state/mind.db -e YM_WEB_DIR=/state/public -e YM_WEB_PORT=8099 -e YM_WEB_URL=http://127.0.0.1:8099 -e YM_WEBUI_PORT=8091 -e YM_CTL_PORT=8078 \
  -e YM_OPERATOR=cb2 -e YM_TZ=Asia/Kolkata "${LANE[@]}" -e YM_INFER_PERMITS=2 \
  -e YM_DMN=off -e YM_PROACTIVE=off -e YM_PATTERNS=off -e YM_HOME_WATCH=off \
  cb2-mind /mind-core > "$OUT/raw/mind_${T}_stdout.txt" 2>&1 || true
docker logs "$NAME" > "$OUT/raw/mind_${T}_boot.txt" 2>&1 &
timeout -k 5 $((WALL + 60)) docker exec "$NAME" python3 /fixtures/run/mind_driver.py "$T" /count > "$OUT/raw/mind_${T}_driver.txt" 2>&1
RC=$?
docker logs "$NAME" > "$OUT/raw/mind_${T}_stdout.txt" 2>&1
ENVLEAK=$(cb2_env_leak_hits "$NAME")
docker rm -f "$NAME" >/dev/null 2>&1   # the parent stops the instance AFTER the driver wrote its receipt
LEAK=$(( ${ENVLEAK:-0} + $(cb2_key_leak_hits "$ST" "$OUT/raw/mind_${T}_stdout.txt" "$OUT/raw/mind_${T}_driver.txt") ))
CHECKS=$(python3 "$FIX/run/receipt_checks.py" "$CD/requests.json" "$CB2_MODEL")
# declared output: the driver leaves RESULT.md and the added files under /state/artifact
cp -r "$ST/artifact/." "$A/" 2>/dev/null
HASH=$(timeout -k 5 60 python3 "$FIX/tools/tree_hash.py" "$A"); IMG=$(docker image inspect cb2-mind --format '{{.Id}}')
python3 - "$ST/receipt.json" "$R/mind_$T.json" "$BIN_SHA" "$PROV" "$IMG" "$HASH" "$RC" "$T" "$CD/requests.json" "$CB2_PROFILE" "$CB2_UPSTREAM" "$CB2_UPSTREAM_IPS" "${CB2_RESOLVED_AT:-static}" "$CB2_MODEL" "$LEAK" "$CHECKS" "$BRAIN_GATE" <<'EOF'
import json, sys
src, dst, bin_sha, prov, img, tree, rc, task, prx, profile, upstream, ips, resolved_at, model, leak, checks, brain_gate = sys.argv[1:]
ck = dict(kv.split("=", 1) for kv in checks.split())
try:
    d = json.load(open(src))
except Exception:
    d = {"system": "mind", "task": task, "status": "driver-failed", "disqualified": True}
try:
    p = json.load(open(prx)); tls = p.get("tls_hostname_verified") is True; upe = int(p["upstream_errors"])
    acc = p["model_requests"]; ref = p["refused_over_cap"]
    receipt_ok = type(acc) is int and type(ref) is int and type(p["upstream_errors"]) is int and acc >= 0 and ref >= 0 and acc <= 8 and ref == 0 and upe == 0 and tls
except Exception:
    tls, upe, acc, ref, receipt_ok = False, -1, -1, -1, False
syml = int(tree.split("symlinks=")[1].split()[0]) if "symlinks=" in tree else 0
special = int(tree.split("specials=")[1].split()[0]) if "specials=" in tree else 0
import re
capture_ok = bool(re.fullmatch(r"[0-9a-f]{64}", bin_sha)) and bool(prov) and bool(re.fullmatch(r"[0-9a-f]{64} files=\d+ bytes=\d+ symlinks=\d+ specials=\d+", tree))
d.update({"binary_sha256": bin_sha, "binary_provenance": prov, "image": img, "tree": tree, "driver_rc": int(rc), "proxy_tls_hostname_verified": tls, "proxy_upstream_errors": upe, "proxy_accepted_parent": acc, "proxy_refused_parent": ref, "proxy_receipt_ok": receipt_ok, "capture_ok": capture_ok, "symlinks": syml, "specials": special})
void = ck.get("http_errors") != "0" or ck.get("transport_errors") != "0"
d.update({"profile": profile, "upstream": upstream, "upstream_ips": ips, "resolved_at": resolved_at, "model": model, "model_ok": ck.get("model_ok") == "true", "key_leak_hits": int(leak), "brain_gate": json.loads(brain_gate),
          "upstream_http_errors": int(ck.get("http_errors", -1)), "upstream_transport_errors": int(ck.get("transport_errors", -1)),
          "usage_prompt_tokens": int(ck.get("usage_p", 0)), "usage_completion_tokens": int(ck.get("usage_c", 0)), "usage_responses": int(ck.get("usage_n", 0)), "void": void})
d["disqualified"] = bool(d.get("disqualified")) or (not receipt_ok) or (not capture_ok) or syml > 0 or special > 0 or int(rc) != 0 or int(leak) != 0 or ck.get("model_ok") != "true"
json.dump(d, open(dst, "w"), indent=1); print(json.dumps(d))
EOF
