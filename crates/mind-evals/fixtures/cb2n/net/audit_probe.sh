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
# The seeded rule is a /32 HOST rule. The purge loop in cb2net.sh matches only the /24 forms and
# the CB2-EGRESS jump, so this one SURVIVES the rebuild and reaches the audit, which is the point:
# a /24 rule would simply be remediated and the probe would prove nothing about the audit.
STRAY=(-s 172.30.1.9/32 -d 8.8.8.8 -p tcp --dport 443 -j ACCEPT)
cleanup() { while iptables -C DOCKER-USER "${STRAY[@]}" 2>/dev/null; do iptables -D DOCKER-USER "${STRAY[@]}" || break; done; }
trap cleanup EXIT
iptables -I DOCKER-USER 1 "${STRAY[@]}" || { echo "could not seed the stray rule"; exit 2; }
OUT=$(bash "$HERE/net/cb2net.sh" 2>&1); RC=$?
cleanup
if [ $RC -eq 0 ]; then echo "AUDIT DID NOT FIRE: cb2net.sh accepted a stray DOCKER-USER rule"; exit 1; fi
STRAY_MSG="CONTAINMENT NOT PROVEN: a stray DOCKER-USER rule still names our subnets"
# the EXACT message, not any refusal: a generic match once accepted a run that failed for an unrelated
# reason (a malformed expected-policy string), which is how a broken audit passed for a broken reason.
echo "$OUT" | grep -qF "$STRAY_MSG" || { echo "AUDIT FIRED FOR THE WRONG REASON (rc=$RC):"; echo "$OUT" | tail -8; exit 1; }
echo "with a stray rule: refused (rc=$RC), with the exact stray-rule message"
OUT2=$(bash "$HERE/net/cb2net.sh" 2>&1); RC2=$?
[ $RC2 -eq 0 ] || { echo "AUDIT IS NOT REPEATABLE: a clean run failed (rc=$RC2):"; echo "$OUT2" | tail -5; exit 1; }
echo "$OUT2" | grep -q "containment proven" || { echo "clean run did not print containment proven"; exit 1; }
echo "without it: proven (rc=0). audit probe PASS"
