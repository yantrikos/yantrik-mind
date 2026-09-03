#!/bin/bash
# One request-counting proxy per leg run, FAIL-CLOSED: returns non-zero unless the container
# started, took its fixed cb2net address, wrote its request receipt, answers on its listener,
# and reports tls_hostname_verified=true. Usage: proxy.sh up <name> <count-dir> <cb2net-ip> | down <name>
set -u
CMD=$1; NAME=$2
# teardown is unconditional and needs no profile, key or run state (a vanished key file must never strand a proxy)
if [ "$CMD" = down ]; then docker rm -f "$NAME" >/dev/null 2>&1; echo "proxy $NAME down"; exit 0; fi
FIX="$(cd "$(dirname "$0")/.." && pwd)"; . "$FIX/run/profile.sh"; cb2_profile_load "$FIX" || exit 1
KEYMOUNT=(); [ -n "$CB2_KEY_FILE" ] && KEYMOUNT=(-v "$CB2_KEY_FILE:/run/secrets/upstream.key:ro" -e CB2_KEY_PATH=/run/secrets/upstream.key)
fail() { echo "proxy $NAME: $1"; docker rm -f "$NAME" >/dev/null 2>&1; exit 1; }
case "$CMD" in
  up)
    CD=$3; IP=$4; mkdir -p "$CD" || fail "count dir"; chown 10002:10002 "$CD" || fail "count dir owner"
    docker rm -f "$NAME" >/dev/null 2>&1
    docker run -d --name "$NAME" --network cb2egress --dns 127.0.0.1 --add-host "$CB2_UPSTREAM:$CB2_UPSTREAM_IP" --read-only --tmpfs /tmp:size=64m \
      --memory 512m --cpus 1 --pids-limit 64 -v "$CD:/count" "${KEYMOUNT[@]}" -e CB2_CAP="${CB2_CAP:-8}" -e CB2_WALL="${CB2_WALL:-1800}" -e CB2_COUNT_FILE=/count/requests.json \
      -e CB2_UPSTREAM="$CB2_UPSTREAM" -e CB2_UPSTREAM_IP="$CB2_UPSTREAM_IP" -e CB2_UPSTREAM_IPS="$CB2_UPSTREAM_IPS" -e CB2_RESOLVED_AT="$CB2_RESOLVED_AT" -e CB2_PROFILE="$CB2_PROFILE" -e CB2_MODEL="$CB2_MODEL" cb2n-proxy >/dev/null || fail "container did not start"
    docker network connect --ip "$IP" cb2net "$NAME" || fail "could not take $IP on cb2net"
    GOT=$(docker inspect "$NAME" --format '{{(index .NetworkSettings.Networks "cb2net").IPAddress}}'); [ "$GOT" = "$IP" ] || fail "address mismatch ($GOT)"
    for i in $(seq 1 40); do [ -f "$CD/requests.json" ] && break; sleep 0.5; done
    [ -f "$CD/requests.json" ] || fail "no request receipt"
    WANTKEY=$([ -n "$CB2_KEY_FILE" ] && echo True || echo False)
    python3 -c "import json,sys; d=json.load(open('$CD/requests.json')); sys.exit(0 if d.get('tls_hostname_verified') is True and d.get('model_requests')==0 and d.get('upstream_errors')==0 and d.get('upstream')=='$CB2_UPSTREAM' and d.get('upstream_ip')=='$CB2_UPSTREAM_IP' and d.get('key_injected') is $WANTKEY else 1)" || fail "receipt not clean, TLS not verified, or profile mismatch"
    # listener health from inside the container (loopback; no request leaves the proxy)
    docker exec "$NAME" python3 -c "import socket; s=socket.create_connection(('127.0.0.1',8080),timeout=5); s.close()" || fail "listener not answering"
    echo "proxy $NAME up at $IP: $(cat "$CD/requests.json")";;
  down)
    docker rm -f "$NAME" >/dev/null 2>&1; echo "proxy $NAME down";;
  *) fail "unknown command";;
esac
