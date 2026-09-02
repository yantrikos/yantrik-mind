"""Drives run/verdict.py through every classification. Prints one line per case and exits 1 on any
disagreement. Runs inside the checker image with no network.

Each case names the flags it expects, so a change that makes the decision more lenient in one place
fails here instead of quietly letting a leg through. The clean case is first and must be clean: a
suite whose only cases are failures cannot notice a rule that rejects everything."""
import sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "run"))
from verdict import classify, CAP

BASE = dict(present=True, ledger_requests=3, ledger_attempts=3, ledger_malformed=0,
            accepted=3, refused=0, stop=None)

CASES = [
    # name,                       overrides,                              ind,   dep,   agrees
    ("clean",                     {},                                     False, False, True),
    ("no_proxy_receipt",          dict(present=False),                    True,  True,  False),
    ("spend_log_absent",          dict(ledger_requests=-1,
                                       ledger_attempts=-1,
                                       ledger_malformed=-1),              True,  True,  False),
    ("malformed_ledger_row",      dict(ledger_malformed=1),               True,  False, True),
    ("cap_refusal",               dict(refused=1, stop="cap"),            True,  False, True),
    ("over_cap_accepted",         dict(accepted=CAP + 1,
                                       ledger_attempts=CAP + 1,
                                       ledger_requests=CAP + 1),          True,  False, True),
    # The three the review found. Each must be a DEPENDENT violation and nothing more: an upstream
    # outage that produces them must be voidable, exactly as on the Hermes side.
    ("accounting_disagrees",      dict(ledger_attempts=4),                False, True,  False),
    ("zero_model_requests",       dict(accepted=0, ledger_attempts=0,
                                       ledger_requests=0),                False, True,  True),
    ("wall",                      dict(stop="timeout"),                   False, True,  True),
    # Retries: one inference_call row, two HTTP requests. ATTEMPTS is what the proxy counted, so
    # this agrees — comparing `ledger_requests` instead would fail an honest leg.
    ("retry_counts_as_attempts",  dict(ledger_requests=2, ledger_attempts=3),
                                                                          False, False, True),
]

bad = 0
for name, over, want_ind, want_dep, want_agree in CASES:
    args = dict(BASE); args.update(over)
    ind, dep, agree = classify(**args)
    ok = (ind, dep, agree) == (want_ind, want_dep, want_agree)
    if not ok:
        bad = 1
    print(f"{name}: {'agree' if ok else 'DISAGREE'} "
          f"got=(ind={ind},dep={dep},agrees={agree}) want=(ind={want_ind},dep={want_dep},agrees={want_agree})")
sys.exit(bad)
