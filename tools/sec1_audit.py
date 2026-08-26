#!/usr/bin/env python3
"""E.SEC1 read-only historical audit.

Reports COUNTS, KINDS and record identifiers ONLY. It never prints a matching value, not even
truncated, and it never writes to anything it reads. The decision logs are hash-chained, so this
opens them read-only and proposes nothing.

The detection mirrors crates/mind-types/src/safety.rs. It is a second implementation on purpose:
if the two disagree the audit is wrong in a visible way rather than silently agreeing with a bug.
"""
import json, os, re, sqlite3, sys, collections

TOKEN_SHAPES = [("ghp_", 8, False), ("gho_", 8, False), ("ghu_", 8, False), ("ghs_", 8, False),
                ("github_pat_", 8, False), ("glpat-", 8, False), ("xoxb-", 8, False),
                ("xoxp-", 8, False), ("sk-", 6, False), ("akia", 16, True), ("asia", 16, True)]
CARD_CTX = {"card", "cards", "pin", "pins", "cvv", "cvc", "iban", "pan"}
SSN_CTX = {"ssn", "social"}
PHRASES = ["password", "passcode", "passphrase", "api key", "apikey", "api-key", "secret key",
           "access token", "auth token", "refresh token", "client secret", "bearer", "private key"]
TOKEN_RE = re.compile(r"[A-Za-z0-9_-]+")

def luhn(d):
    s = 0
    for i, ch in enumerate(reversed(d)):
        n = int(ch)
        if i % 2 == 1:
            n *= 2
            if n > 9:
                n -= 9
        s += n
    return s % 10 == 0

def digit_runs(text, want_free=False):
    """Digit runs. With want_free, only runs NOT buried inside a longer alphanumeric token — a
    64-char hex hash contains card-shaped substrings by chance (28 of 11,866 lines on the box)."""
    out, i, n = [], 0, len(text)
    while i < n:
        if not text[i].isdigit():
            i += 1
            continue
        start = i
        digits = ""
        while i < n and (text[i].isdigit() or text[i] in " -"):
            if text[i].isdigit():
                digits += text[i]
            i += 1
        embedded = (start > 0 and text[start - 1].isalpha()) or (i < n and text[i].isalpha())
        if not (want_free and embedded):
            out.append(digits)
    return out

def value_follows(after):
    for tok in TOKEN_RE.findall(after[:48])[:3]:
        if len(tok) >= 6 and (any(c.isdigit() for c in tok) or len(tok) >= 12):
            return True
    return False

def kinds(text):
    """Every KIND present. Never returns any part of the text."""
    if not text:
        return set()
    low, found = text.lower(), set()
    if "-----begin" in low and "private key" in low:
        found.add("pem-private-key")
    toks = TOKEN_RE.findall(text)
    for tok in toks:
        for pre, min_tail, upper in TOKEN_SHAPES:
            if tok.lower().startswith(pre) and len(tok) - len(pre) >= min_tail:
                if not upper or all(c.isupper() or c.isdigit() for c in tok):
                    found.add("token")
    runs = digit_runs(text)
    for d in digit_runs(text, want_free=True):
        if 13 <= len(d) <= 19 and d[0] in "3456" and luhn(d):
            found.add("payment-card")
    low_toks = {t.lower() for t in toks}
    if low_toks & CARD_CTX and any(len(d) >= 4 for d in runs):
        found.add("card-context-number")
    if low_toks & SSN_CTX and any(len(d) == 9 for d in runs):
        found.add("national-id")
    for p in PHRASES:
        at = low.find(p)
        while at != -1:
            before_ok = at == 0 or not (low[at - 1].isalnum() or low[at - 1] in "_-")
            if before_ok and value_follows(text[at + len(p):]):
                found.add("credential-phrase")
                break
            at = low.find(p, at + len(p))
    return found

def kinds_of_values(node):
    """Every KIND across a parsed JSON document's STRING values, each scanned on its own.

    Numbers are rendered back to text individually, so two adjacent fields can never be read as one
    long digit run — the artifact that a raw-line scan produces and then reports as a payment card.
    """
    found = set()
    if isinstance(node, str):
        return kinds(node)
    if isinstance(node, bool) or node is None:
        return found
    if isinstance(node, (int, float)):
        return kinds(str(node))
    if isinstance(node, list):
        for item in node:
            found |= kinds_of_values(item)
        return found
    if isinstance(node, dict):
        for k, v in node.items():
            found |= kinds_of_values(v)
        return found
    return found

def audit_jsonl(path):
    per_kind, hits, total = collections.Counter(), [], 0
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        for lineno, line in enumerate(f, 1):
            if not line.strip():
                continue
            total += 1
            try:
                ev = json.loads(line).get("event", {})
            except Exception:
                continue
            found = set()
            for field in ("goal", "outcome", "predicted", "lesson", "trigger", "chosen", "subject"):
                found |= kinds(ev.get(field) or "")
            for arr in ("candidates", "rejected", "policy", "evidence_ids"):
                for item in ev.get(arr) or []:
                    found |= kinds(item or "")
            if found:
                for k in found:
                    per_kind[k] += 1
                hits.append((lineno, ev.get("event_id") or f"line:{lineno}", sorted(found)))
    return total, per_kind, hits

def audit_db(path):
    per_kind, hits, total = collections.Counter(), [], 0
    try:
        con = sqlite3.connect(f"file:{path}?mode=ro", uri=True)
        cur = con.execute("SELECT rid, text FROM memories")
    except Exception as e:
        return None, f"{type(e).__name__}", []
    for rid, text in cur:
        total += 1
        found = kinds(text or "")
        if found:
            for k in found:
                per_kind[k] += 1
            hits.append((rid, sorted(found)))
    con.close()
    return total, per_kind, hits

print("E.SEC1 READ-ONLY HISTORICAL AUDIT — counts, kinds and record ids only; no values are printed")
print()
for path in sys.argv[1:]:
    if not os.path.exists(path):
        print(f"{path}: absent")
        continue
    if path.endswith(".jsonl"):
        # A DecisionEvent-shaped log is scanned field by field. Any OTHER jsonl is scanned as RAW
        # LINES, because assuming a schema that does not apply would report a clean bill of health
        # for a file this audit never actually looked inside.
        with open(path, "r", encoding="utf-8", errors="replace") as probe:
            first = probe.readline()
        decision_shaped = '"event"' in first and '"chain"' in first
        if not decision_shaped:
            per_kind, hits, total = collections.Counter(), [], 0
            with open(path, "r", encoding="utf-8", errors="replace") as f:
                for lineno, line in enumerate(f, 1):
                    if not line.strip():
                        continue
                    total += 1
                    # FIELD-AWARE, not raw. Scanning the whole line lets a digit run continue
                    # across JSON punctuation and MANUFACTURE a card-shaped number out of two
                    # unrelated fields — which is what a first raw-line pass appeared to find. Walk
                    # the parsed values instead, and fall back to the raw line only when the line
                    # will not parse at all (which is itself worth seeing).
                    found = set()
                    try:
                        found = kinds_of_values(json.loads(line))
                    except Exception:
                        found = kinds(line)
                    if found:
                        for k in found:
                            per_kind[k] += 1
                        hits.append((lineno, f"line:{lineno}", sorted(found)))
            print(f"{path}  [FIELD-AWARE SCAN of a non-DecisionEvent log]")
            print(f"  lines scanned  : {total}")
            print(f"  lines flagged  : {len(hits)}")
            for k, n in sorted(per_kind.items()):
                print(f"    {k:22} {n}")
            for lineno, eid, ks in hits[:20]:
                print(f"    line {lineno:<6} kinds={','.join(ks)}")
            if len(hits) > 20:
                print(f"    … {len(hits)-20} more flagged lines not listed")
            print()
            continue
        total, per_kind, hits = audit_jsonl(path)
        print(f"{path}\n  events scanned : {total}")
        print(f"  events flagged : {len(hits)}")
        for k, n in sorted(per_kind.items()):
            print(f"    {k:22} {n}")
        for lineno, eid, ks in hits[:20]:
            print(f"    line {lineno:<6} id={eid:<44} kinds={','.join(ks)}")
        if len(hits) > 20:
            print(f"    … {len(hits)-20} more flagged records not listed")
    else:
        total, per_kind, hits = audit_db(path)
        if total is None:
            print(f"{path}\n  unreadable: {per_kind}")
            continue
        print(f"{path}\n  memories scanned : {total}")
        print(f"  memories flagged : {len(hits)}")
        for k, n in sorted(per_kind.items()):
            print(f"    {k:22} {n}")
        for rid, ks in hits[:20]:
            print(f"    rid={rid} kinds={','.join(ks)}")
        if len(hits) > 20:
            print(f"    … {len(hits)-20} more flagged records not listed")
    print()
