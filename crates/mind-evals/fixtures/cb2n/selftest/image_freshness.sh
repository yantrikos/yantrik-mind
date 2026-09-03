#!/bin/bash
# Does the code RUNNING in each image match the fixture it was built from?
#
# Four fixture files are baked into images rather than mounted -- proxy/proxy.py into cb2n-proxy,
# checks/check_web.mjs, checks/check_t3.py and seed/ into cb2-check. Editing one of them changes
# NOTHING until that image is rebuilt, and nothing said so. It bit twice in one evening: the
# checker's crash fix was inert until cb2-check was rebuilt, and the proxy's wall/timeout change was
# inert during a diagnostic that then reported `proxy_request_timeouts=None` -- the only reason the
# staleness was noticed at all.
#
# A reading run against a stale image measures code that is not in the tree, and re-derivation
# proves nothing about it: `rederive.sh` compares the tree to the patch, not the tree to what is
# executing. This is the check that closes that gap. FAIL-CLOSED: a missing image or a missing
# docker is a failure, not a skip, because this suite exists to run on the box where both are
# present, and a case that quietly skips is a case that cannot fail.
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"; FIX="$(cd "$HERE/.." && pwd)"; BAD=0
say() { if [ "$2" = "$3" ]; then echo "$1: agree [$2]"; else echo "$1: DISAGREE got=[$2] want=[$3]"; BAD=1; fi; }

baked() {  # image, path-in-image -> sha256 of that file, or a reason
  local img=$1 path=$2 cid out
  command -v docker >/dev/null 2>&1 || { echo "no-docker"; return; }
  docker image inspect "$img" >/dev/null 2>&1 || { echo "no-image:$img"; return; }
  cid=$(docker create "$img" true 2>/dev/null) || { echo "cannot-create:$img"; return; }
  out=$(docker cp "$cid:$path" - 2>/dev/null | tar -xO 2>/dev/null | sha256sum | cut -c1-64)
  docker rm -f "$cid" >/dev/null 2>&1
  [ -n "$out" ] && echo "$out" || echo "cannot-read:$path"
}

# image            path inside the image        fixture file
while read -r img path src; do
  [ -z "$img" ] && continue
  want=$(sha256sum "$FIX/$src" | cut -c1-64)
  got=$(baked "$img" "$path")
  say "image_matches_tree:$img:$(basename "$src")" "$got" "$want"
done <<'TRIPLES'
cb2n-proxy /proxy.py proxy/proxy.py
cb2-check /checker/check_web.mjs checks/check_web.mjs
cb2-check /checker/check_t3.py checks/check_t3.py
cb2-check /checker/seed/leads.json seed/leads.json
cb2-check /checker/seed/expected.json seed/expected.json
TRIPLES

exit $BAD
