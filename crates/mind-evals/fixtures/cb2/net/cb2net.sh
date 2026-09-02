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
# probe
R=$(docker run --rm --network $NET --add-host aig.mycluster.cyou:$GW --dns 127.0.0.1 python:3.13-slim python3 - <<'EOF'
import urllib.request, socket
def try_url(u):
    try:
        urllib.request.urlopen(u, timeout=8); return "ok"
    except Exception as e:
        return "blocked" if "timed out" in str(e) or "Errno" in str(e) or "Name or service" in str(e) or "unreachable" in str(e).lower() else f"ok?({str(e)[:40]})"
g = try_url("https://aig.mycluster.cyou/v1/models")
i = try_url("https://1.1.1.1/")
try:
    socket.gethostbyname("github.com"); d = "resolved"
except Exception:
    d = "blocked"
print(f"gateway {g} / internet {i} / dns {d}")
EOF
)
echo "probe: $R"
case "$R" in "gateway ok / internet blocked / dns blocked") exit 0;; *) echo "CONTAINMENT NOT PROVEN"; exit 1;; esac
