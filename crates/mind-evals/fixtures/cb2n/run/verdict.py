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
CAP = 8


def classify(*, present, ledger_requests, ledger_attempts, ledger_malformed,
             accepted, refused, stop, cap=CAP):
    """present: the proxy receipt was readable. ledger_*: the mind's own spend log (-1 = the log
    was absent, which is not a count). accepted/refused: the proxy's counts. stop: None, "cap" or
    "timeout". Returns (dq_independent, dq_dependent, accounting_agrees)."""
    log_missing = ledger_requests < 0 or ledger_attempts < 0

    # One attempt is one HTTP request to the provider; one `inference_call` row may carry several,
    # so ATTEMPTS is the number comparable to what the proxy counted — the exact analogue of
    # `accepted == the calls in the agent's own log` that the Hermes leg has always applied.
    accounting_agrees = present and not log_missing and ledger_attempts == accepted

    dq_independent = (
        (not present)                # no proxy receipt: never a void, always a violation
        or log_missing               # the mind cannot account for itself at all
        or ledger_malformed > 0      # a row that is not a v1 inference_call
        or refused > 0               # a ninth request was attempted: the cap was hit
        or accepted > cap            # more requests left the box than the budget allowed
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
