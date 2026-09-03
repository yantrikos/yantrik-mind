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
# NO MODULE-LEVEL CAP. There was a `CAP = 8` here and `verdict_cases.py` imported it, so the
# disqualification rule was only ever self-tested at eight — never at the twenty-four the
# `nim-cap24` reading actually enforces. The cap is a required argument now, and the cases run at
# both. It was also the fifth surviving literal in the change that claimed to remove all five.


def classify(*, present, ledger_requests, ledger_attempts, ledger_malformed,
             accepted, refused, stop, cap):
    """present: the proxy receipt was readable. ledger_*: the mind's own spend log (-1 = the log
    was absent, which is not a count). accepted/refused: the proxy's counts. stop: None, "cap" or
    "timeout". Returns (dq_independent, dq_dependent, accounting_agrees)."""
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
    if downloads > 0:
        reasons.append("download_or_install")
    return bool(reasons), reasons
