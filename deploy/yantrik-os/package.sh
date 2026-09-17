#!/usr/bin/env bash
# ═══════════════════════════════════════════════════════════════════════════════════════
# package.sh — Yantrik Mind as a bundle a Yantrik OS image installs beside the OS
# ═══════════════════════════════════════════════════════════════════════════════════════
#
#   ./package.sh [--target-dir DIR] [--out DIR] [--allow-unstamped]
#
# Packages binaries that are already built (this script never compiles):
#
#   yantrik-mind-<commit>-linux-amd64/
#     bin/yantrik-mind          the mind (mind-core)
#     bin/yantrik-memory        the memory server, for when no mind owns the file
#     systemd/user/*.service    both user units; the image enables yantrik-mind.service
#     BUILD                     which commit this is, read from the binary itself
#
# The commit comes from `yantrik-mind --build-commit`, not from git in this directory: the
# binary is what ships, and a directory can say one thing while its target dir holds a build of
# another. An unstamped binary is refused, because an image that cannot say which mind it
# carries is the stale-binary incident waiting to happen again.
#
# Build first, stamped:
#   YM_BUILD_COMMIT=$(git rev-parse --short HEAD) cargo build --release --locked -p mind-core -p mind-memory-mcp
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
OUT="$PROJECT_ROOT/dist"
TARGET_DIR=""
ALLOW_UNSTAMPED=false

while [ $# -gt 0 ]; do
  case "$1" in
    --target-dir) TARGET_DIR="$2"; shift 2 ;;
    --out) OUT="$2"; shift 2 ;;
    --allow-unstamped) ALLOW_UNSTAMPED=true; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

if [ -z "$TARGET_DIR" ]; then
  TARGET_DIR="$(cd "$PROJECT_ROOT" && cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
fi
RELEASE="$TARGET_DIR/release"

for b in mind-core yantrik-memory; do
  [ -x "$RELEASE/$b" ] || { echo "missing $RELEASE/$b — build it first (see the header)" >&2; exit 1; }
  file "$RELEASE/$b" | grep -q "ELF 64-bit LSB.*x86-64" \
    || { echo "$RELEASE/$b is not a linux x86-64 binary" >&2; exit 1; }
done

COMMIT="$("$RELEASE/mind-core" --build-commit)"
if [ "$COMMIT" = "unstamped" ] && [ "$ALLOW_UNSTAMPED" != true ]; then
  echo "the mind binary is unstamped; rebuild with YM_BUILD_COMMIT set, or pass --allow-unstamped" >&2
  exit 1
fi

NAME="yantrik-mind-${COMMIT}-linux-amd64"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
ROOT="$STAGE/$NAME"
mkdir -p "$ROOT/bin" "$ROOT/systemd/user"
cp "$RELEASE/mind-core" "$ROOT/bin/yantrik-mind"
cp "$RELEASE/yantrik-memory" "$ROOT/bin/yantrik-memory"
cp "$SCRIPT_DIR/yantrik-mind.service" "$SCRIPT_DIR/yantrik-memory.service" "$ROOT/systemd/user/"
cat > "$ROOT/BUILD" <<EOF
name=$NAME
commit=$COMMIT
built=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF

mkdir -p "$OUT"
TARBALL="$OUT/$NAME.tar.zst"
tar --zstd -cf "$TARBALL" -C "$STAGE" "$NAME"
sha256sum "$TARBALL" | sed "s|$OUT/||" > "$TARBALL.sha256"
echo "$TARBALL ($(du -h "$TARBALL" | cut -f1))"
