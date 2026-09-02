#!/bin/bash
# One request-counting proxy per leg run. Usage: proxy.sh up <name> <count-dir> <cb2net-ip> | down <name>
set -u
CMD=$1; NAME=$2
case "$CMD" in
  up)
    CD=$3; IP=$4; mkdir -p "$CD"; chown 10002:10002 "$CD"
    docker rm -f "$NAME" >/dev/null 2>&1
    docker run -d --name "$NAME" --network cb2egress --dns 127.0.0.1 --add-host aig.mycluster.cyou:192.168.4.203 --read-only --tmpfs /tmp:size=64m \
      --memory 512m --cpus 1 --pids-limit 64 -v "$CD:/count" -e CB2_CAP=8 -e CB2_COUNT_FILE=/count/requests.json cb2-proxy >/dev/null
    docker network connect --ip "$IP" cb2net "$NAME"
    for i in $(seq 1 20); do [ -f "$CD/requests.json" ] && break; sleep 0.5; done
    echo "proxy $NAME up: $(cat "$CD/requests.json" 2>/dev/null)";;
  down)
    docker rm -f "$NAME" >/dev/null 2>&1; echo "proxy $NAME down";;
esac
