"""Is a budget or a timeout assigned a bare number anywhere in the tree?

`cap_cases.sh` case 6 and W6 each grep three files -- the three whoever wrote them thought of. The
MANIFEST was on neither list, and it was the manifest that nearly killed a reading: it declared
`"wall_clock_seconds": 1800` and disqualified "any run ... over 1800 s" while every script happily
ran a leg at 3600. A completeness check is only as complete as its list, so this one takes no list.

It is a separate FILE rather than a heredoc inside the suite because the first version was a
heredoc inside `$( )`, python never received it, and the case reported zero hits for a clean tree
and for two deliberately broken ones alike. A check that cannot fail is worse than no check: it
reports success. This file can be run on its own and made to fail on demand.

Usage: scan_literals.py <fixture root>   -> one line per hit on stdout, exit 1 if any.
"""
import os
import re
import sys

# `scratch/` holds the recorded patch, which contains every literal in the tree by construction;
# `selftest/` is these cases, which name the numbers on purpose.
SKIP_DIRS = {"scratch", "selftest", "__pycache__", ".git"}

# Deliberately NOT matched: `WALL=${CB2_WALL:-1800}` (a fallback, not a decision), `CB2_CAP=24` in
# a profile (that is exactly where the number belongs -- `_` before CAP fails the prefix class),
# and `WALL = int(sys.argv[4]) ...` (the right-hand side is not a digit).
PATTERN = re.compile(r"(^|[^A-Z_])(CAP|WALL) *= *[0-9]")


def scan(root):
    """Hits as (path, line number, text). Comment lines are prose: a guard that fires on prose is
    one that gets commented out rather than obeyed, so they are skipped."""
    hits = []
    for dirpath, dirnames, filenames in os.walk(root):
        dirnames[:] = [d for d in dirnames if d not in SKIP_DIRS]
        for name in sorted(filenames):
            path = os.path.join(dirpath, name)
            try:
                with open(path, encoding="utf-8", errors="replace") as fh:
                    text = fh.read()
            except OSError:
                continue
            for n, line in enumerate(text.splitlines(), 1):
                if line.lstrip().startswith("#"):
                    continue
                if PATTERN.search(line):
                    hits.append((os.path.relpath(path, root), n, line.strip()[:90]))
    return hits


if __name__ == "__main__":
    found = scan(sys.argv[1] if len(sys.argv) > 1 else ".")
    for path, n, line in found:
        print(f"{path}:{n}: {line}")
    sys.exit(1 if found else 0)
