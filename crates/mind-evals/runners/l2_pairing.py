import json, collections

F = "/var/lib/yantrik-mind/mind.db.decisions.jsonl"
shadow = {}      # cycle -> row
ticks = collections.defaultdict(list)   # cycle -> [tick rows]

with open(F, "r", encoding="utf-8", errors="replace") as fh:
    for line in fh:
        if '"kind"' not in line:
            continue
        try:
            e = json.loads(line).get("event", {})
        except Exception:
            continue
        k = e.get("kind")
        if k == "attention_shadow":
            cyc = str(e.get("object_id", ""))
            if cyc:
                shadow[cyc] = e
        elif k == "loop_tick":
            cyc = str(e.get("context_fingerprint", ""))
            if cyc.startswith("cycle:"):
                ticks[cyc].append(e)

print("shadow rows: %d   wakes with timer rows: %d" % (len(shadow), len(ticks)))
paired = [c for c in shadow if c in ticks]
print("PAIRED wakes (shadow + at least one timer row): %d (%.1f%% of shadow rows)"
      % (len(paired), 100.0 * len(paired) / max(len(shadow), 1)))

def loop_of(t):
    a = str(t.get("actor", ""))
    return a[5:] if a.startswith("loop:") else a

agree_exact = agree_member = disagree = 0
disagreements = collections.Counter()
informative_paired = 0
for c in paired:
    s = shadow[c]
    cands = s.get("candidates") or []
    chosen = s.get("chosen")
    acted = [loop_of(t) for t in ticks[c] if t.get("verdict") == "acted"]
    if not acted:
        continue
    if len(cands) >= 2:
        informative_paired += 1
    if acted == [chosen]:
        agree_exact += 1
    elif chosen in acted:
        agree_member += 1
    else:
        disagree += 1
        disagreements[(chosen, ",".join(sorted(set(acted))))] += 1

judged = agree_exact + agree_member + disagree
print()
print("Of %d paired wakes where a loop actually ACTED:" % judged)
print("  the shadow's pick is the only thing that ran : %d" % agree_exact)
print("  the shadow's pick is among what ran          : %d" % agree_member)
print("  the shadow picked something that did NOT run : %d" % disagree)
print("  of those, wakes where the shadow had a REAL choice (>=2 candidates): %d" % informative_paired)
if disagreements:
    print()
    print("disagreements (shadow chose -> what actually acted):")
    for (ch, act), n in disagreements.most_common(10):
        print("  %-14s -> %-40s %d" % (ch, act, n))

# What the timers ran that the shadow never even offered as a candidate.
offered = collections.Counter()
ran = collections.Counter()
for c in paired:
    for x in shadow[c].get("candidates") or []:
        offered[x] += 1
    for t in ticks[c]:
        if t.get("verdict") == "acted":
            ran[loop_of(t)] += 1
print()
print("loops the shadow ever offered :", offered.most_common(12))
print("loops that actually acted     :", ran.most_common(12))
never = sorted(set(ran) - set(offered))
print("acted but NEVER a candidate   :", never)
