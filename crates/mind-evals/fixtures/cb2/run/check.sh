#!/bin/bash
# Run the exact checkers INSIDE the checker image on a WRITABLE COPY of an artifact, with NO
# network at all, a read-only root and a tmpfs. Writes a counts-only verdict and a separate
# excerpts file. Usage: check.sh <t1|t2|t3> <artifact-dir> <verdict.json> [excerpts.txt]
set -u
T=$1; SRC=$2; OUTJ=$3; EXC=${4:-${OUTJ%.json}.excerpts.txt}
FIX="$(cd "$(dirname "$0")/.." && pwd)"
C=$(mktemp -d /tmp/cb2-check-XXXX); NAME="cb2-check-$$-$RANDOM"; trap 'docker rm -f "$NAME" >/dev/null 2>&1; rm -rf "$C"' EXIT
TREE=$(timeout -k 5 60 python3 "$FIX/tools/tree_hash.py" "$SRC")
echo "$TREE" | grep -Eq '^[0-9a-f]{64} files=[0-9]+ bytes=[0-9]+ symlinks=0 specials=0$' || { echo "refusing unsafe artifact tree" >&2; exit 2; }
cp -r --no-dereference --preserve=links "$SRC/." "$C/"; chmod -R u+w "$C"; chown -R 1000:1000 "$C" 2>/dev/null
if [ "$T" = "t3" ]; then CMD="python3 /checker/check_t3.py /work /work/.excerpts.txt"; else CMD="node /checker/check_web.mjs $T /work /work/.excerpts.txt"; fi
timeout -k 5 300 docker run --rm --name "$NAME" --network none --dns 127.0.0.1 --read-only --tmpfs /tmp:size=256m \
  --memory 2g --cpus 2 --pids-limit 256 -v "$C:/work" -w /work cb2-check bash -c "$CMD" > "$OUTJ" 2>"$C/.stderr.txt"
RC=$?
{ echo "== checker stderr"; cat "$C/.stderr.txt" 2>/dev/null; echo "== excerpts"; cat "$C/.excerpts.txt" 2>/dev/null; } > "$EXC"
exit $RC
