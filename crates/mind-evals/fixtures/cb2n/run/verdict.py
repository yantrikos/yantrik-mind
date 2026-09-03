"""The Mind leg's disqualification decision, as a PURE function.

It lived inline in the driver's receipt literal, where it could not be tested, and an adversarial
review found three separate ways it was wrong: the Mind was not held to the agreement check the
manifest promises for both systems, it alone could pass with zero model requests, and a missing
spend log counted as zero calls. All three were invisible because nothing could exercise the
decision without running a whole graded leg. Here it takes numbers and returns booleans, and
`selftest/verdict_cases.py` drives it through every classification.

The taxonomy (MANIFEST.json, unchanged): INDEPENDENT violations disqualify unconditionally — the
run broke a rule of its own whatever the upstream was doing. DEPENDENT violations disqualify only
when the leg is not void, because an infrastructure outage must not be charged to the agent. A
void never rescues an independent violation, and this function never decides voidness: that is
the proxy receipt's business, upstream, in `receipt_checks.py`.
"""

import json
# NO MODULE-LEVEL CAP. There was a `CAP = 8` here and `verdict_cases.py` imported it, so the
# disqualification rule was only ever self-tested at eight — never at the twenty-four the
# `nim-cap24` reading actually enforces. The cap is a required argument now, and the cases run at
# both. It was also the fifth surviving literal in the change that claimed to remove all five.


def classify(*, present, ledger_requests, ledger_attempts, ledger_malformed,
             accepted, stop, cap):
    """present: the proxy receipt was readable. ledger_*: the mind's own spend log (-1 = the log
    was absent, which is not a count). accepted: the proxy's count. stop: None, "cap" or "timeout".
    Returns (dq_independent, dq_dependent, accounting_agrees).

    There is deliberately NO `refused` parameter. It used to be one and was unused, which made the
    self-test's `cap_refusal` case inert: it passed at both caps while the real leg disqualified the
    run through `receipt_shape_ok` below. A parameter nothing reads is a case nothing tests."""
    log_missing = ledger_requests < 0 or ledger_attempts < 0

    # One attempt is one HTTP request to the provider; one `inference_call` row may carry several,
    # so ATTEMPTS is the number comparable to what the proxy counted — the exact analogue of
    # `accepted == the calls in the agent's own log` that the Hermes leg has always applied.
    accounting_agrees = present and not log_missing and ledger_attempts == accepted

    # THE CAP IS A BUDGET, NOT A TRAP. `refused > 0` used to disqualify, and that was an asymmetry
    # rather than a rule: the Hermes leg writes `agent.max_turns: $CAP` into its config, so Hermes
    # self-limits at exactly the budget and can never BE refused, while the Mind has no client-side
    # turn limit and runs until the proxy 429s it. Both agents wanting one more request than the
    # budget allows: Hermes stopped cleanly and was graded, the Mind was disqualified. Running out
    # of budget is the budget working. EXCEEDING it would be misconduct, and `accepted > cap` still
    # catches that — though only the proxy could cause it, since the proxy is what enforces the cap.
    dq_independent = (
        (not present)                # no proxy receipt: never a void, always a violation
        or log_missing               # the mind cannot account for itself at all
        or ledger_malformed > 0      # a row that is not a v1 inference_call
        or accepted > cap            # more requests left the box than the budget allowed
        # Hermes has always been disqualified for more calls in its OWN log than the cap. The Mind
        # was held to no such rule. Symmetric now. (The download/install scan is the other rule
        # Hermes alone carried; it is a HOST observation, made after the driver has finished, so it
        # lives in `host_independent` below rather than here.)
        or ledger_attempts > cap
    )
    # `accounting_agrees` is REPORTED, not enforced (reading 5). The proxy is the authoritative
    # meter and refuses over-cap requests before the model, so an agent cannot hide spend from it;
    # whether its own log agrees characterises its self-accounting and is not a reason to discard an
    # artifact. Relaxed for both systems together, in the same commit, after it disqualified a
    # Hermes leg that had finished cleanly -- and it would have had to go had it disqualified ours.
    dq_dependent = (
        stop == "timeout"            # the wall
        or accepted < 1              # never reached the model; Hermes's receipt requires 1 <= a
    )
    return dq_independent, dq_dependent, accounting_agrees


def host_independent(*, capture_ok, symlinks, specials, key_leak_hits, receipt_valid, downloads):
    """The INDEPENDENT violations only the host can see, after the container is gone.

    These lived as one long boolean expression inside the leg script's heredoc, where no test could
    reach them — the same shape as the disqualification rule before it moved here, and the shape in
    which three defects hid. `downloads` is the scan the Hermes leg has always run over its
    transcript and the Mind was never held to.

    Returns (violated, reasons) so a receipt can say WHICH rule fired rather than only that one did.
    """
    reasons = []
    if not capture_ok:
        reasons.append("capture")          # the binary sha, provenance or tree hash is malformed
    if symlinks > 0:
        reasons.append("symlinks")
    if specials > 0:
        reasons.append("specials")         # a fifo or device node in the artifact
    if key_leak_hits != 0:
        reasons.append("key_leak")
    if not receipt_valid:
        reasons.append("untyped_receipt")  # never a void: a missing receipt is not evidence of one
    # `downloads` is RECORDED, not enforced — see the note in hermes_leg.sh. The scan counts
    # MENTIONS of a fetch, it fired on an agent verifying its own local server over loopback, and
    # the act it names is prevented by the network rather than by this rule. The count still rides
    # in the receipt; it just no longer decides anything.
    _ = downloads
    return bool(reasons), reasons


def receipt_shape_ok(*, accepted, refused, upstream_errors, tls_verified, cap):
    """Is the proxy receipt the shape a valid leg produces?

    This lived as one expression inside each leg's heredoc — the third rule in this harness to be
    written where no test could reach it, and the third to be wrong there. It is what review 7
    found still enforcing `refused == 0` after the independent rule had been removed: a cap 429
    never touches the upstream, so the leg was not void and the DEPENDENT class disqualified it
    anyway, by a path the case table could not see.

    `refused` is TYPED and RECORDED and does not decide: reaching the cap is the budget working.
    Exceeding it (`accepted > cap`) is the only misconduct here, and only the proxy could cause it.
    """
    typed = (
        type(accepted) is int
        and type(refused) is int
        and type(upstream_errors) is int
        and refused >= 0
    )
    return typed and 1 <= accepted <= cap and tls_verified is True


def reported_project(result_text):
    """The project directory a system SAYS it built, from its own result, or None.

    Hermes works in /work and its artifact IS /work — project root and graded root are the same
    directory by construction. A Mind build writes into its own project directory, so this is what
    gives both systems the same relationship between where the agent built and what is graded.

    The name comes from the system's own report, never from the brief and never guessed. A URL
    ending in a FILE (the page lane's `.../index.html`) is not a project: that lane's deliverable is
    the web root, exactly as before.
    """
    import re

    m = re.search(r"https?://[^\s]+?/([A-Za-z0-9._-]+)/(?:\s|$)", result_text or "")
    if not m:
        return None
    name = m.group(1)
    # A NAME OF DOTS IS NOT A NAME. The charset allows `.` so that `v1.2` is a legal project, and a
    # URL of `.../../` therefore extracted `..` — which the caller would have joined onto the web
    # directory, capturing its PARENT as the artifact: the whole state directory, database included.
    # The self-test case written to prove the charset stopped traversal is what found this; the
    # charset stops a separator INSIDE a name and never stopped the name being a traversal itself.
    if name.strip(".") == "":
        return None
    return name


def call_latencies(lines):
    """Per-callsite latency from the mind's own spend rows. Counts and integers only.

    E.LAT2. The mind already records `latency_ms` and the callsite on every `inference_call` row;
    the harness deleted them with the state at teardown, so a leg's timing had to be reconstructed
    from separate probes hours later — which produced a confounded comparison and a withdrawn
    claim. A leg should carry its own timing evidence beside its own wall clock.

    `lines` is the decision log's lines, or None when the log was not readable. None is NOT an
    empty summary: "no calls" and "no log" are different facts, and conflating them is a mistake
    this harness has already made once.
    """
    if lines is None:
        return None
    per = {}
    for line in lines:
        try:
            ev = json.loads(line).get("event", {})
        except json.JSONDecodeError:
            # A malformed LINE is expected and skipped. Anything else — a missing import, a bad
            # attribute — is a defect and must surface: a bare `except Exception` here swallowed a
            # NameError (this module had no `import json`) and the function silently returned an
            # empty summary for every input. It was caught by exercising it; nothing about the
            # output said it had failed.
            continue
        if ev.get("kind") != "inference_call":
            continue
        trigger = str(ev.get("trigger") or "")
        site = trigger.split("callsite:", 1)[1] if "callsite:" in trigger else "unattributed"
        # The callsite is a static string the code authored; take the leading segment so the
        # summary stays small and no free text can ride in on it.
        site = site.split()[0][:60] if site.split() else "unattributed"
        ms = ev.get("latency_ms")
        ms = int(ms) if isinstance(ms, int) else 0
        row = per.setdefault(site, {"calls": 0, "total_ms": 0, "max_ms": 0, "served": 0})
        row["calls"] += 1
        row["total_ms"] += ms
        row["max_ms"] = max(row["max_ms"], ms)
        if ev.get("verdict") == "served":
            row["served"] += 1
    return per
