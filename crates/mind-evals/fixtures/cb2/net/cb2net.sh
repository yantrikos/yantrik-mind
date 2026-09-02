#!/bin/bash
# E.CB2 network containment on the box: a dedicated Docker network whose ONLY egress is the pinned
# owned model endpoint (192.168.4.203:443). Idempotent. Ends with a probe that must print
# "gateway ok / internet blocked / dns blocked" or exit non-zero.
set -u
NET=cb2net; SUBNET=172.30.0.0/24; GW=192.168.4.203
docker network inspect $NET >/dev/null 2>&1 || docker network create --subnet $SUBNET $NET >/dev/null
# rules: specific ACCEPT first, then DROP everything else from the subnet (forwarded traffic).
iptables -C DOCKER-USER -s $SUBNET -j DROP 2>/dev/null || iptables -I DOCKER-USER 1 -s $SUBNET -j DROP
iptables -C DOCKER-USER -s $SUBNET -d $GW -p tcp --dport 443 -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -s $SUBNET -d $GW -p tcp --dport 443 -j ACCEPT
iptables -C DOCKER-USER -d $SUBNET -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null || iptables -I DOCKER-USER 1 -d $SUBNET -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -S DOCKER-USER | grep "$SUBNET" | sed 's/^/rule: /'
# probe — a mounted file (a heredoc inside a command substitution does not survive ssh quoting)
P="$(dirname "$0")/probe.py"
R=$(docker run --rm --network $NET --add-host aig.mycluster.cyou:$GW --dns 127.0.0.1 -v "$P:/probe.py:ro" python:3.13-slim python3 /probe.py 2>/dev/null)
echo "probe: $R"
case "$R" in "gateway ok / internet blocked / dns blocked") exit 0;; *) echo "CONTAINMENT NOT PROVEN"; exit 1;; esac
