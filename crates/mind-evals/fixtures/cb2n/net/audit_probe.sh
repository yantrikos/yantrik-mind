#!/bin/bash
# Does the containment audit actually FAIL when it should? Seeds a stray DOCKER-USER rule naming our
# subnet, runs net/cb2net.sh, and requires it to refuse with the stray-rule message; then removes the
# stray rule, runs it again, and requires it to pass. A fail-closed check that cannot be seen failing
# is not evidence (the first version of this audit shipped with a grep pattern that matched nothing).
# Root, on the box, between runs only: it edits iptables and refuses if anything is attached.
set -u
HERE="$(cd "$(dirname "$0")/.." && pwd)"
for NET in cb2net cb2egress; do
  A=$(docker network inspect $NET --format '{{len .Containers}}' 2>/dev/null || echo 0)
  [ "$A" = 0 ] || { echo "refusing: $NET has $A attached container(s)"; exit 2; }
done
STRAY=(-I DOCKER-USER 1 -s 172.30.1.0/24 -d 8.8.8.8 -p tcp --dport 443 -j ACCEPT)
cleanup() { iptables -D DOCKER-USER -s 172.30.1.0/24 -d 8.8.8.8 -p tcp --dport 443 -j ACCEPT 2>/dev/null; }
trap cleanup EXIT
iptables "${STRAY[@]}" || { echo "could not seed the stray rule"; exit 2; }
OUT=$(bash "$HERE/net/cb2net.sh" 2>&1); RC=$?
cleanup
if [ $RC -eq 0 ]; then echo "AUDIT DID NOT FIRE: cb2net.sh accepted a stray DOCKER-USER rule"; exit 1; fi
echo "$OUT" | grep -q "CONTAINMENT NOT PROVEN" || { echo "AUDIT FIRED FOR THE WRONG REASON (rc=$RC):"; echo "$OUT" | tail -5; exit 1; }
echo "with a stray rule: refused (rc=$RC) — $(echo "$OUT" | grep -m1 "CONTAINMENT NOT PROVEN")"
OUT2=$(bash "$HERE/net/cb2net.sh" 2>&1); RC2=$?
[ $RC2 -eq 0 ] || { echo "AUDIT IS NOT REPEATABLE: a clean run failed (rc=$RC2):"; echo "$OUT2" | tail -5; exit 1; }
echo "$OUT2" | grep -q "containment proven" || { echo "clean run did not print containment proven"; exit 1; }
echo "without it: proven (rc=0). audit probe PASS"
