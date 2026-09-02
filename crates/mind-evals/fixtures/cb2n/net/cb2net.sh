#!/bin/bash
# E.CB2 network containment v3 (deterministic, idempotent). Two networks:
#   cb2net    172.30.0.0/24  --internal : work containers (hermes, mind, check). NO egress, no host
#                                          NAT; the only reachable service is the run's proxy.
#   cb2egress 172.30.1.0/24              : the proxy's second leg; forwarded egress ONLY to the
#                                          pinned gateway 192.168.4.203:443.
# Migration: a network with attached containers is refused; a mismatched network (not internal,
# wrong subnet) is deleted and recreated; superseded rules for the work subnet are removed; the
# inspect fields are verified. Host services are blocked from BOTH bridges by interface-scoped
# INPUT rules. Ends with two TCP-level probes that must match exactly or the script exits non-zero.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"; . "$HERE/../run/profile.sh"; cb2_profile_load "$HERE/.." || exit 1
GW=$CB2_UPSTREAM_IP
ensure_net() {  # name subnet internal(true|false)
  local n=$1 sub=$2 internal=$3
  if docker network inspect "$n" >/dev/null 2>&1; then
    local att; att=$(docker network inspect "$n" --format '{{len .Containers}}')
    local cur_int cur_sub; cur_int=$(docker network inspect "$n" --format '{{.Internal}}'); cur_sub=$(docker network inspect "$n" --format '{{(index .IPAM.Config 0).Subnet}}')
    if [ "$cur_int" != "$internal" ] || [ "$cur_sub" != "$sub" ]; then
      [ "$att" != "0" ] && { echo "refusing: $n is mismatched and has $att attached container(s)"; exit 1; }
      docker network rm "$n" >/dev/null
    fi
  fi
  if ! docker network inspect "$n" >/dev/null 2>&1; then
    if [ "$internal" = true ]; then docker network create --internal --subnet "$sub" "$n" >/dev/null; else docker network create --subnet "$sub" "$n" >/dev/null; fi
  fi
  [ "$(docker network inspect "$n" --format '{{.Internal}}')" = "$internal" ] || { echo "verify failed: $n internal"; exit 1; }
  [ "$(docker network inspect "$n" --format '{{(index .IPAM.Config 0).Subnet}}')" = "$sub" ] || { echo "verify failed: $n subnet"; exit 1; }
}
ensure_net cb2net 172.30.0.0/24 true
ensure_net cb2egress 172.30.1.0/24 false
# superseded rules for the work subnet (harness v2 gave it forwarded egress; it has none now)
for _ in 1 2 3 4 5 6 7 8; do
  N=$(iptables -L DOCKER-USER --line-numbers -n | grep "172.30.0.0/24" | head -1 | awk '{print $1}')
  [ -z "$N" ] && break
  iptables -D DOCKER-USER "$N" || { echo "could not delete superseded rule $N"; exit 1; }
done
iptables -L DOCKER-USER -n | grep -q "172.30.0.0/24" && { echo "superseded rules remain"; exit 1; }
BR_WORK="br-$(docker network inspect cb2net --format '{{.Id}}' | cut -c1-12)"
BR_EGRESS="br-$(docker network inspect cb2egress --format '{{.Id}}' | cut -c1-12)"
iptables -C DOCKER-USER -s 172.30.1.0/24 -j DROP 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -j DROP
# upstream ACCEPTs: exactly the profile's resolved addresses; an ACCEPT for any other destination (a previous profile) is removed
for RULE in $(iptables -S DOCKER-USER | grep -E -- "^-A DOCKER-USER -s 172.30.1.0/24 -d [0-9./]+ -p tcp -m tcp --dport 443 -j ACCEPT$" | awk '{print $6}'); do
  KEEP=0; for IP in $CB2_UPSTREAM_IPS; do [ "$RULE" = "$IP/32" ] && KEEP=1; done
  [ $KEEP = 1 ] || iptables -D DOCKER-USER -s 172.30.1.0/24 -d "$RULE" -p tcp --dport 443 -j ACCEPT || { echo "could not delete stale upstream rule $RULE"; exit 1; }
done
for IP in $CB2_UPSTREAM_IPS; do
  iptables -C DOCKER-USER -s 172.30.1.0/24 -d $IP -p tcp --dport 443 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -s 172.30.1.0/24 -d $IP -p tcp --dport 443 -j ACCEPT
done
iptables -C DOCKER-USER -d 172.30.1.0/24 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -d 172.30.1.0/24 -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
for BR in $BR_WORK $BR_EGRESS; do
  iptables -C INPUT -i $BR -j DROP 2>/dev/null || iptables -I INPUT 1 -i $BR -j DROP
  iptables -C INPUT -i $BR -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || iptables -I INPUT 1 -i $BR -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
done
echo "profile: $CB2_PROFILE upstream=$CB2_UPSTREAM addresses=[$CB2_UPSTREAM_IPS] resolved_at=${CB2_RESOLVED_AT:-static}"
echo "networks: cb2net internal=$(docker network inspect cb2net --format '{{.Internal}}') subnet=172.30.0.0/24; cb2egress subnet=172.30.1.0/24; bridges work=$BR_WORK egress=$BR_EGRESS"
iptables -S DOCKER-USER | grep "172.30" | sed 's/^/rule: /'; iptables -S INPUT | grep -E "$BR_WORK|$BR_EGRESS" | sed 's/^/rule: /'
# probes through a throwaway proxy at a fixed address
docker rm -f cb2probe-proxy cb2probe-work >/dev/null 2>&1
trap 'docker rm -f cb2probe-proxy cb2probe-work >/dev/null 2>&1' EXIT
docker run -d --name cb2probe-proxy --network cb2egress --dns 127.0.0.1 --add-host "$CB2_UPSTREAM:$GW" -e CB2_UPSTREAM="$CB2_UPSTREAM" -e CB2_UPSTREAM_IP="$GW" -e CB2_CAP=1 -e CB2_COUNT_FILE=/tmp/c.json cb2n-proxy >/dev/null
docker network connect --ip 172.30.0.9 cb2net cb2probe-proxy
sleep 3
W=$(docker run --rm --name cb2probe-work --network cb2net --dns 127.0.0.1 -e CB2_UPSTREAM_IP="$GW" -v "$HERE/probe_work.py:/probe.py:ro" python:3.13-slim python3 /probe.py 2>/dev/null)
P=$(docker exec cb2probe-proxy python3 -c "$(cat "$HERE/probe_proxy.py")" 2>/dev/null)
TLS=$(docker exec cb2probe-proxy cat /tmp/c.json 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin).get('tls_hostname_verified'))")
echo "work probe:  $W"; echo "proxy probe: $P"; echo "proxy tls_hostname_verified: $TLS"
[ "$W" = "proxy-tcp ok / gateway-tcp blocked / internet-tcp blocked / host-ssh-tcp blocked / host-http-tcp blocked / dns blocked" ] || { echo "CONTAINMENT NOT PROVEN (work)"; exit 1; }
[ "$P" = "gateway-tls-verified ok / internet-tcp blocked / host-ssh-tcp blocked / host-http-tcp blocked / dns blocked" ] || { echo "CONTAINMENT NOT PROVEN (proxy)"; exit 1; }
[ "$TLS" = "True" ] || { echo "PROXY TLS VERIFICATION NOT PROVEN"; exit 1; }
echo "containment proven"
