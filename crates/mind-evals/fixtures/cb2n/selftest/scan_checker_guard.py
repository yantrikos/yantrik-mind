"""Is every browser interaction in check_web.mjs inside the crash guard?

The pilot's artifact -- a truncated `server.py` that repeated one block 49 times -- made the page
navigate under an unguarded `page.$("form#cb2-lead-form")`. Playwright threw "Execution context was
destroyed", node died with an uncaught exception, and the run produced a ZERO-BYTE verdict.json
while exiting 1: the same exit code an honestly failing artifact gives, with none of the evidence.

The fix wraps the body so any such throw becomes a failed `checker_completed` check and a verdict
still gets written. That fix is one `await page.` away from being silently undone -- a new call
added above the `try` restores the old behaviour and nothing fails. So this asserts the SHAPE:
every `await page.` / `await form.` sits between the guard's open and its catch.

It is a structural check and says so. It cannot prove the guard catches a real navigation race --
only the artifact that caused it could, and a fixture that depends on a navigation winning a race
would be exactly the flaky guard this suite keeps learning not to write.

Usage: scan_checker_guard.py <check_web.mjs>   -> offending lines on stdout, exit 1 if any.
"""
import re
import sys

OPEN = "let checkerCrash = null;"
CATCH = "} catch (e) {\n  checkerCrash = e;"
INTERACTION = re.compile(r"await (page|form)\.")


def scan(path):
    with open(path, encoding="utf-8") as fh:
        text = fh.read().replace("\r\n", "\n")
    problems = []
    if OPEN not in text or CATCH not in text:
        return ["the crash guard is missing entirely (no `checkerCrash` open/catch pair)"]
    if "checker_completed" not in text:
        return ["the crash path no longer records `checker_completed`"]
    lo, hi = text.index(OPEN), text.index(CATCH)
    for n, line in enumerate(text.split("\n"), 1):
        if line.lstrip().startswith("//") or not INTERACTION.search(line):
            continue
        at = sum(len(l) + 1 for l in text.split("\n")[: n - 1])
        if not (lo < at < hi):
            problems.append(f"{path}:{n}: browser interaction outside the crash guard: {line.strip()[:80]}")
    return problems


if __name__ == "__main__":
    found = scan(sys.argv[1])
    for p in found:
        print(p)
    sys.exit(1 if found else 0)
