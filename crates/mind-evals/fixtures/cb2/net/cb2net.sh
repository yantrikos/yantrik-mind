#!/bin/bash
# E.CB2 network containment v3 (idempotent). Two networks:
#   cb2net    172.30.0.0/24  --internal : work containers (hermes, mind, check). NO egress, no host
#                                          NAT; the only reachable service is the proxy container.
#   cb2egress 172.30.1.0/24              : the proxy's second leg; forwarded egress ONLY to the
#                                          pinned gateway 192.168.4.203:443.
# Host services are blocked from BOTH bridges by interface-scoped INPUT rules. Ends with two
# probes (work side, proxy side) that must print the expected lines or exit non-zero.
set -u
GW=192.168.4.203; HERE="$(cd "$(dirname "$0")" && pwd)"
docker network inspect cb2net >/dev/null 2>&1 || docker network create --internal --subnet 172.30.0.0/24 cb2net >/dev/null
docker network inspect cb2egress >/dev/null 2>&1 || docker network create --subnet 172.30.1.0/24 cb2egress >/dev/null
BR_WORK="br-$(docker network inspect cb2net --format '{{.Id}}' | cut -c1-12)"
BR_EGRESS="br-$(docker network inspect cb2egress --format '{{.Id}}' | cut -c1-12)"
# forwarded egress from the proxy leg: only the gateway
iptables -C DOCKER-USER -s 172.30.1.0/24 -j DROP 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -j DROP
iptables -C DOCKER-USER -s 172.30.1.0/24 -d $GW -p tcp --dport 443 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -d $GW -p tcp --dport 443 -j ACCEPT
iptables -C DOCKER-USER -d 172.30.1.0/24 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -d 172.30.1.0/24 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
# host services: nothing from either bridge reaches the host (INPUT), except established replies
for BR in $BR_WORK $BR_EGRESS; do
  iptables -C INPUT -i $BR -j DROP 2>/dev/null || iptables -I INPUT 1 -i $BR -j DROP
  iptables -C INPUT -i $BR -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || iptables -I INPUT 1 -i $BR -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
done
echo "bridges: work=$BR_WORK egress=$BR_EGRESS"
iptables -S DOCKER-USER | grep "172.30.1" | sed 's/^/rule: /'; iptables -S INPUT | grep -E "$BR_WORK|$BR_EGRESS" | sed 's/^/rule: /'
# a throwaway proxy for the probes
docker rm -f cb2probe-proxy >/dev/null 2>&1
docker run -d --name cb2probe-proxy --network cb2egress -e CB2_CAP=1 -e CB2_COUNT_FILE=/tmp/c.json cb2-proxy >/dev/null
docker network connect cb2net cb2probe-proxy
sleep 2
W=$(docker run --rm --network cb2net --add-host aig.mycluster.cyou:$GW --dns 127.0.0.1 -v "$HERE/probe_work.py:/probe.py:ro" python:3.13-slim python3 /probe.py 2>/dev/null)
P=$(docker exec cb2probe-proxy python3 -c "$(cat "$HERE/probe_proxy.py")" 2>/dev/null)
docker rm -f cb2probe-proxy >/dev/null 2>&1
echo "work probe:  $W"; echo "proxy probe: $P"
[ "$W" = "proxy ok / gateway-direct blocked / internet blocked / host-service blocked / dns blocked" ] || { echo "CONTAINMENT NOT PROVEN (work)"; exit 1; }
[ "$P" = "gateway ok / internet blocked / host-service blocked / dns blocked" ] || { echo "CONTAINMENT NOT PROVEN (proxy)"; exit 1; }
echo "containment proven"
