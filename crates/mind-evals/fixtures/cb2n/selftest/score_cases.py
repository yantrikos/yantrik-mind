"""Drives run/score.py through every way a score can lie. Prints one line per case, exits 1 on any
disagreement. Runs inside the checker image with no network.

The clean case is first and must be clean: a suite whose only cases are failures cannot notice a
rule that rejects everything — the same reason `verdict_cases.py` opens the way it does.

The case that earns this file is `crashed_checker_keeps_the_denominator`. Every reading so far was
tallied off the checks PRESENT in the verdict, so a checker that died after one check reported
"1/2" for a fourteen-check task and the worst possible run produced the best-looking fraction.
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "run"))
from score import META_CHECKS, expected_checks, render, score  # noqa: E402

EXPECTED = ["a", "b", "c", "checker_completed"]  # 3 graded + 1 meta


def v(**checks):
    return {"task": "TX", "checks": {k: {"pass": p} for k, p in checks.items()}}


BAD = 0


def say(name, got, want):
    global BAD
    if got == want:
        print(f"{name}: agree [{got}]")
    else:
        BAD = 1
        print(f"{name}: DISAGREE got=[{got}] want=[{want}]")


# 1. The clean case: everything the task defines, all passing.
r = score(v(a=True, b=True, c=True), EXPECTED)
say("clean_is_clean", (r["passed"], r["total"], r["trustworthy"]), (3, 3, True))

# 2. `checker_completed` is NOT one of the graded checks. A run that reports it must have the same
#    denominator as one that does not, or a crash could move the score by that route instead.
r = score(v(a=True, b=True, c=True, checker_completed=True), EXPECTED)
say("meta_check_is_not_graded", (r["passed"], r["total"]), (3, 3))
say("meta_check_is_reported_beside", r["checker_completed"], True)

# 3. An honest partial failure: everything ran, some failed.
r = score(v(a=True, b=False, c=False), EXPECTED)
say("honest_failure_scores_honestly", (r["passed"], r["total"]), (1, 3))
say("failures_are_named", r["failed"], ["b", "c"])
say("nothing_is_missing_when_all_ran", r["missing"], [])

# 4. THE CASE THIS FILE EXISTS FOR. The checker died after one check. The naive tally — passed over
#    checks PRESENT — would read 1/2, a better fraction than the honest failure above scored while
#    actually running everything. The denominator must stay the task's.
crashed = v(a=True, checker_completed=False)
r = score(crashed, EXPECTED)
naive_total = len([k for k in crashed["checks"] if k not in META_CHECKS])
say("the_naive_tally_really_would_have_lied", (r["passed"], naive_total), (1, 1))
say("crashed_checker_keeps_the_denominator", (r["passed"], r["total"]), (1, 3))
say("checks_that_never_ran_are_named", r["missing"], ["b", "c"])
say("a_crash_is_reported_as_a_crash", r["checker_completed"], False)
say("the_rendered_line_says_the_checker_crashed",
    "CHECKER CRASHED" in render("TX", r), True)
say("the_rendered_line_names_what_never_ran", "never reported" in render("TX", r), True)

# 5. The closed schema wall: a verdict naming a check the task does not define means the two
#    disagree about what was measured. No fraction is meaningful across that.
r = score(v(a=True, b=True, c=True, invented=True), EXPECTED)
say("an_undefined_check_name_is_caught", r["unexpected"], ["invented"])
say("a_disagreeing_verdict_is_untrustworthy", r["trustworthy"], False)
say("no_fraction_is_rendered_for_it", "UNTRUSTWORTHY" in render("TX", r), True)

# 6. An empty parse must raise, not return an empty set: 0/0 is not a score.
try:
    expected_checks("this source defines no checks at all")
    say("an_empty_parse_raises", "returned", "raised")
except ValueError:
    say("an_empty_parse_raises", "raised", "raised")

# 7. The parse reads real checker sources, not just the synthetic set above.
here = os.path.dirname(os.path.abspath(__file__))
for rel, want_min in (("../checks/check_web.mjs", 14), ("../checks/check_t3.py", 4)):
    p = os.path.join(here, rel)
    if os.path.exists(p):
        with open(p, encoding="utf-8") as f:
            names = expected_checks(f.read())
        say(f"parses_{os.path.basename(rel)}", len(names) >= want_min, True)

sys.exit(BAD)
