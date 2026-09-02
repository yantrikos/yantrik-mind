#!/bin/bash
# Exact re-derivation check: copy the frozen cb2 tree, apply the recorded patch, diff against this
# tree (the shipped Hermes archive is excluded: it is never committed). Exit non-zero on any diff.
set -u
HERE="$(cd "$(dirname "$0")/.." && pwd)"; SRC="${1:-$HERE/../cb2}"; T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
cp -r "$SRC/." "$T/" && python3 "$HERE/scratch/cb2n_patch.py" "$T" >/dev/null || { echo "patch failed"; exit 1; }
if diff -r --exclude='*.tar.gz' --exclude=__pycache__ "$T" "$HERE"; then echo "cb2n re-derives exactly from cb2 + scratch/cb2n_patch.py"; else echo "cb2n DOES NOT re-derive"; exit 1; fi
