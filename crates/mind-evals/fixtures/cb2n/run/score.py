"""The check score, as a PURE function — and specifically its DENOMINATOR.

Every reading so far was tallied by reading `verdict.checks` and counting how many passed out of
how many were THERE. That is wrong in the one case that matters most. `check_web.mjs` writes a
check only when the line that writes it runs, so a checker that dies partway emits a SHORTER
checks object — and the naive tally divides by that shorter number. A crash after one check reads
"1/2" when the task has fourteen. The worse the failure, the better the fraction looks.

So the denominator here comes from the checker's SOURCE, not from its output: a check the task
defines but the run never reported is a check that did not pass, and is scored as a failure with
its name listed. That number cannot move when a run goes badly.

`checker_completed` is deliberately NOT one of them. It is a statement about the harness, not
about the artifact, and adding it to the graded set would let a crash change the denominator by a
different route — the exact defect this file exists to remove. It is reported beside the score.

The check names are also a CLOSED SET: a verdict carrying a name the task never defined means the
verdict and the checker disagree about what was being measured, and a score computed across that
disagreement is not a measurement. It is reported as untrustworthy rather than quietly averaged.
"""

import json
import re

# Checks about the harness rather than the artifact. Scored beside the task, never inside it.
META_CHECKS = frozenset({"checker_completed"})

# The TWO ways a checker in this harness declares a check name. Both are needed, and finding that
# out is the reason to validate a scorer against real verdicts rather than only synthetic ones:
#   1. `check("name", ...)` — the direct call, in either checker language.
#   2. `("name", [...], [...])` — a row, ON ITS OWN LINE, in a table of scenarios that a loop
#      later feeds to `check`. The line-start anchor is load-bearing: without it the pattern also
#      matched `spawn("bash", ["run.sh"], ...)` in check_web.mjs and invented a check called
#      "bash". A guard that recognises too much is as wrong as one that recognises too little,
#      and only the exact-count assertion caught it.
#      `check_t3.py` declares six of its ten checks this way, and reading only form 1 recovered
#      four of them. Scored against the real T3 verdicts, every one came back UNTRUSTWORTHY.
# That refusal was the mechanism working — it declined to divide by a denominator it could not
# justify, rather than quietly scoring 4/4 and calling six real checks foreign. But a scorer that
# refuses every real verdict is useless, so the second form is recognised too, and anything it
# still cannot account for stays loud.
_CHECK_CALL = re.compile(r'check\(\s*"([a-z0-9_]+)"')
_CHECK_ROW = re.compile(r'^\s*\(\s*"([a-z0-9_]+)"\s*,\s*\[', re.MULTILINE)


def expected_checks(source):
    """The complete set of check names a checker DEFINES, from its source text.

    Raises on an empty parse: a checker with no checks is a broken read of the source, not a task
    with nothing to verify, and returning an empty set would make every score 0/0."""
    names = sorted(set(_CHECK_CALL.findall(source)) | set(_CHECK_ROW.findall(source)))
    if not names:
        raise ValueError("no check names found in checker source — the parse is broken, and an "
                         "empty expected set would silently make every score 0/0")
    return names


def score(verdict, expected):
    """Score one verdict against the checks its task defines.

    verdict: the parsed verdict.json. expected: the names from `expected_checks`.
    Returns a dict. `trustworthy` is False when the verdict and the checker disagree about what
    was being measured — the caller must not report a fraction in that case."""
    checks = verdict.get("checks") or {}
    graded = [n for n in expected if n not in META_CHECKS]
    reported = set(checks)

    passed = sorted(n for n in graded if bool((checks.get(n) or {}).get("pass")))
    # A check the task defines but the run never reported did not pass. Naming them is the point:
    # "11/14, missing 3" is a different finding from "11/14, three failed".
    missing = sorted(n for n in graded if n not in reported)
    failed = sorted(n for n in graded if n in reported and not bool((checks.get(n) or {}).get("pass")))
    # The closed schema wall: a name in the verdict that the task never defined.
    unexpected = sorted(reported - set(expected))

    completed = None
    if "checker_completed" in reported:
        completed = bool((checks.get("checker_completed") or {}).get("pass"))

    return {
        "passed": len(passed),
        "total": len(graded),
        "failed": failed,
        "missing": missing,
        "unexpected": unexpected,
        # The harness's own statement, beside the score and never inside it. None = the checker
        # never said, which is how a clean run looks.
        "checker_completed": completed,
        "trustworthy": not unexpected,
    }


def render(task, result):
    """One line a reading can be read off. Says `missing` out loud, because a missing check and a
    failing check are the same number and different findings."""
    if not result["trustworthy"]:
        return (f"{task}: UNTRUSTWORTHY — the verdict reports checks the task does not define "
                f"({', '.join(result['unexpected'])}); no fraction is meaningful across that")
    line = f"{task}: {result['passed']}/{result['total']}"
    if result["missing"]:
        line += f" · {len(result['missing'])} never reported ({', '.join(result['missing'])})"
    if result["checker_completed"] is False:
        line += " · CHECKER CRASHED (the denominator above is the task's, not what ran)"
    return line


def main(argv):
    if len(argv) != 3:
        print("usage: score.py <checker source> <verdict.json>")
        return 2
    with open(argv[1], encoding="utf-8") as f:
        expected = expected_checks(f.read())
    with open(argv[2], encoding="utf-8") as f:
        verdict = json.load(f)
    result = score(verdict, expected)
    print(render(verdict.get("task", "?"), result))
    return 0 if result["trustworthy"] and not result["failed"] and not result["missing"] else 1


if __name__ == "__main__":
    import sys
    sys.exit(main(sys.argv))
