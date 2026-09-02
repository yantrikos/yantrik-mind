#!/bin/bash
# Run the exact checkers INSIDE the checker image on a WRITABLE COPY of an artifact, contained.
# Usage: check.sh <t1|t2|t3> <artifact-dir> [verdict.json]. Exit code = the checker's.
set -u
T=$1; SRC=$2; OUTJ=${3:-/dev/stdout}
C=$(mktemp -d /tmp/cb2-check-XXXX); cp -r "$SRC/." "$C/"; chmod -R u+w "$C"; chown -R 1000:1000 "$C" 2>/dev/null
if [ "$T" = "t3" ]; then CMD="python3 /checker/check_t3.py /work"; else CMD="node /checker/check_web.mjs $T /work"; fi
timeout -k 5 300 docker run --rm --network cb2net --dns 127.0.0.1 --memory 2g --cpus 2 --pids-limit 256 \
  -v "$C:/work" -w /work cb2-check bash -c "$CMD" > "$OUTJ" 2>/tmp/cb2-check-err.txt
RC=$?; rm -rf "$C"; exit $RC
