#!/bin/bash
# Teardown with a receipt: what the scratch dir held (names, sizes, tree hash), then removal.
set -u
D=/var/lib/ym-cb2
[ -f "$D/pid" ] && kill "$(cat $D/pid)" 2>/dev/null && sleep 1
echo "teardown $(date -u +%Y-%m-%dT%H:%M:%SZ)"
[ -d "$D" ] || { echo "nothing to tear down"; exit 0; }
find "$D" -type f -printf '%s %p\n' | sort -k2 | sed "s#$D/##" | grep -v "mind.db" | head -40
echo "tree sha256: $(cd $D && find . -type f ! -name 'mind.db*' -print0 | sort -z | xargs -0 sha256sum | sha256sum | cut -c1-32)"
echo "spend rows written by the scratch instance: $(grep -c '"kind":"inference_call"' $D/mind.db.decisions.jsonl 2>/dev/null || echo 0)"
rm -rf "$D"
echo "removed $D; live /var/lib/yantrik-mind untouched: $(ls /var/lib/yantrik-mind/mind.db >/dev/null 2>&1 && echo yes)"
