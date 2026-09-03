"""Drives run/verdict.py through every classification. Prints one line per case and exits 1 on any
disagreement. Runs inside the checker image with no network.

Each case names the flags it expects, so a change that makes the decision more lenient in one place
fails here instead of quietly letting a leg through. The clean case is first and must be clean: a
suite whose only cases are failures cannot notice a rule that rejects everything."""
import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "run"))
from verdict import classify, host_independent

# EVERY CASE RUNS AT BOTH CAPS. The rule used to import a module-level `CAP = 8`, so it was only
# ever exercised at eight while the `nim-cap24` reading enforces twenty-four — a self-test that
# could not see the budget the graded run uses.
CAPS = (8, 24)

BASE = dict(present=True, ledger_requests=3, ledger_attempts=3, ledger_malformed=0,
            accepted=3, refused=0, stop=None)

CASES = [
    # name,                       overrides,                              ind,   dep,   agrees
    ("clean",                     {},                                     False, False, True),
    # dep is False since reading 5: with no receipt the dependent class has no complaint of its
    # own, and the INDEPENDENT violation is what disqualifies. It used to be True only because the
    # agreement check could not pass without a receipt.
    ("no_proxy_receipt",          dict(present=False),                    True,  False, False),
    ("spend_log_absent",          dict(ledger_requests=-1,
                                       ledger_attempts=-1,
                                       ledger_malformed=-1),              True,  False, False),
    ("malformed_ledger_row",      dict(ledger_malformed=1),               True,  False, True),
    # A cap stop is the budget working, not misconduct: Hermes self-limits at max_turns = cap and
    # is graded, so the Mind being 429'd at the same threshold must be graded too.
    ("cap_refusal",               dict(refused=1, stop="cap"),            False, False, True),
    ("over_cap_accepted",         dict(accepted="CAP+1",
                                       ledger_attempts="CAP+1",
                                       ledger_requests="CAP+1"),          True,  False, True),
    # Hermes's own-log-over-cap rule, now applied to the Mind as well. (Its download/install scan
    # is a HOST observation and is covered by the host_* cases above.)
    ("own_log_over_cap",          dict(ledger_attempts="CAP+1"),          True,  False, False),
    # The three the review found. Each must be a DEPENDENT violation and nothing more: an upstream
    # outage that produces them must be voidable, exactly as on the Hermes side.
    # Reading 5: a disagreement is RECORDED and no longer disqualifies. The case stays, with the
    # expectation inverted, so the relaxation is visible in the suite rather than only in a commit.
    ("accounting_disagrees",      dict(ledger_attempts=4),                False, False, False),
    ("zero_model_requests",       dict(accepted=0, ledger_attempts=0,
                                       ledger_requests=0),                False, True,  True),
    ("wall",                      dict(stop="timeout"),                   False, True,  True),
    # Retries: one inference_call row, two HTTP requests. ATTEMPTS is what the proxy counted, so
    # this agrees — comparing `ledger_requests` instead would fail an honest leg.
    ("retry_counts_as_attempts",  dict(ledger_requests=2, ledger_attempts=3),
                                                                          False, False, True),
]

bad = 0

# ── the host-side independent rules, each fired alone ────────────────────────────────────────
HOST_CLEAN = dict(capture_ok=True, symlinks=0, specials=0, key_leak_hits=0, receipt_valid=True,
                  downloads=0)
HOST_CASES = [
    ("host_clean",            {},                       []),
    ("host_capture",          dict(capture_ok=False),   ["capture"]),
    ("host_symlink",          dict(symlinks=1),         ["symlinks"]),
    ("host_special_node",     dict(specials=1),         ["specials"]),
    ("host_key_leak",         dict(key_leak_hits=2),    ["key_leak"]),
    ("host_untyped_receipt",  dict(receipt_valid=False), ["untyped_receipt"]),
    # The scan the Hermes leg has always run and the Mind was never held to.
    ("host_download_line",    dict(downloads=1),        ["download_or_install"]),
    # Two at once must name both, so a receipt says WHICH rule fired.
    ("host_two_at_once",      dict(symlinks=1, downloads=3), ["symlinks", "download_or_install"]),
]
for name, over, want in HOST_CASES:
    args = dict(HOST_CLEAN); args.update(over)
    violated, reasons = host_independent(**args)
    ok = (violated, reasons) == (bool(want), want)
    if not ok:
        bad = 1
    print(f"{name}: {'agree' if ok else 'DISAGREE'} got=({violated},{reasons}) want=({bool(want)},{want})")

for cap in CAPS:
    for name, over, want_ind, want_dep, want_agree in CASES:
        args = dict(BASE)
        # "CAP+1" is written symbolically so a case means the same thing at every cap.
        args.update({k: (cap + 1 if v == "CAP+1" else v) for k, v in over.items()})
        args["cap"] = cap
        ind, dep, agree = classify(**args)
        ok = (ind, dep, agree) == (want_ind, want_dep, want_agree)
        if not ok:
            bad = 1
        print(f"cap{cap}/{name}: {'agree' if ok else 'DISAGREE'} "
              f"got=(ind={ind},dep={dep},agrees={agree}) want=(ind={want_ind},dep={want_dep},agrees={want_agree})")
sys.exit(bad)
