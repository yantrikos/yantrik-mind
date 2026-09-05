"""Drives run/verdict.py through every classification. Prints one line per case and exits 1 on any
disagreement. Runs inside the checker image with no network.

Each case names the flags it expects, so a change that makes the decision more lenient in one place
fails here instead of quietly letting a leg through. The clean case is first and must be clean: a
suite whose only cases are failures cannot notice a rule that rejects everything."""
import json, sys, os
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "run"))
from verdict import (classify, host_independent, receipt_shape_ok, reported_project,
                     call_latencies, model_liveness, explain_gone)

# EVERY CASE RUNS AT BOTH CAPS. The rule used to import a module-level `CAP = 8`, so it was only
# ever exercised at eight while the `nim-cap24` reading enforces twenty-four — a self-test that
# could not see the budget the graded run uses.
CAPS = (8, 24)

BASE = dict(present=True, ledger_requests=3, ledger_attempts=3, ledger_malformed=0,
            accepted=3, stop=None)

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
    # is graded, so the Mind being 429'd at the same threshold must be graded too. The refusal
    # COUNT is not an argument to `classify` at all — it used to be, unused, which made this case
    # inert while the real leg disqualified the run through the receipt shape. That rule now has
    # its own cases below.
    ("cap_stop",                  dict(stop="cap"),                       False, False, True),
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
    # A mention of a fetch is RECORDED and no longer a violation: the scan fired on an agent
    # fetching its own local server, and the network is what prevents a real download.
    ("host_download_line",    dict(downloads=1),        []),
    # Two at once must name both, so a receipt says WHICH rule fired.
    ("host_two_at_once",      dict(symlinks=1, downloads=3), ["symlinks"]),
]
for name, over, want in HOST_CASES:
    args = dict(HOST_CLEAN); args.update(over)
    violated, reasons = host_independent(**args)
    ok = (violated, reasons) == (bool(want), want)
    if not ok:
        bad = 1
    print(f"{name}: {'agree' if ok else 'DISAGREE'} got=({violated},{reasons}) want=({bool(want)},{want})")

# ── per-call latency, so a leg carries its own timing evidence ────────────────────────────────
# E.LAT1 could not say where a leg's 420 seconds went, and reconstructing it from probes hours
# later produced a confounded comparison and a withdrawn claim. The mind already records
# latency_ms and the callsite on every spend row; this is the summary that reaches the receipt.
def _row(site, ms, verdict_s="served"):
    return json.dumps({"event": {"kind": "inference_call",
                                 "trigger": "callsite:" + site,
                                 "latency_ms": ms,
                                 "verdict": verdict_s}})


LAT_ROWS = [
    _row("delegate:build", 120000),
    _row("delegate:build", 90000),
    _row("delegate:route", 20000, "failed"),
    json.dumps({"event": {"kind": "loop_tick"}}),
    "not json at all",
]
_got = call_latencies(LAT_ROWS)
LAT_CASES = [
    ("two calls from one callsite are summed", _got.get("delegate:build", {}).get("calls"), 2),
    ("their total is carried", _got.get("delegate:build", {}).get("total_ms"), 210000),
    ("and the worst single call", _got.get("delegate:build", {}).get("max_ms"), 120000),
    ("a failed call counts but is not served", _got.get("delegate:route", {}).get("served"), 0),
    ("a loop row is not a spend row", "loop_tick" in _got, False),
    ("a malformed line is skipped, not fatal", len(_got), 2),
    # THE DISTINCTION THIS HARNESS HAS ALREADY GOT WRONG ONCE: "no log" is not "no calls".
    ("no log is None, not an empty summary", call_latencies(None), None),
    ("no calls is an empty summary", call_latencies([]), {}),
]
# The except is NARROW on purpose, and this is what makes that observable. A malformed LINE is
# expected and skipped; anything else is a defect and must surface. A bare `except Exception` here
# swallowed a NameError (the module had no `import json`) and the function returned an empty
# summary for every input, with nothing in the output saying it had failed.
try:
    call_latencies([12345])          # not a string: json.loads raises TypeError, not a decode error
    _surfaced = False
except json.JSONDecodeError:
    _surfaced = False                 # would mean the narrow except caught the wrong thing
except Exception:
    _surfaced = True
LAT_CASES_EXTRA = [
    ("a non-line defect surfaces instead of emptying the summary", _surfaced, True),
]

for name, got, want in LAT_CASES + LAT_CASES_EXTRA:
    ok = got == want
    if not ok:
        bad = 1
    print(f"latency/{name}: {'agree' if ok else 'DISAGREE'} got={got!r} want={want!r}")

# ── which root gets graded ────────────────────────────────────────────────────────────────────
# The rule that decides what the checker reads. It lived inside the driver, which no test can
# import; a rule that decides a score does not get to live somewhere unreachable.
PROJECT_CASES = [
    # The result a build actually emits. The URL ends in a directory, and the newline after it
    # is what the pattern needs; a space is the same signal and keeps this table free of escapes.
    ("a build reports its project",
     "built - http://127.0.0.1:8099/cb2-t1-a1b2c3/ then more text", "cb2-t1-a1b2c3"),
    # The PAGE lane's URL ends in a file, so it is not a project and the web root stays graded.
    ("a published page is not a project",
     "it is live - http://127.0.0.1:8099/index.html and more", None),
    ("a bare project url at the end of the text", "http://127.0.0.1:8099/proj-9/", "proj-9"),
    ("a failure message names nothing", "I could not finish the build: no files", None),
    ("no result at all", "", None),
    # A NAME OF DOTS IS NOT A NAME. This case was written believing the charset stopped traversal;
    # it does not — it stops a separator inside a name, and `..` is a legal name under it. The URL
    # below returned `..`, which the driver would have joined onto the web directory and captured
    # its PARENT: the whole state directory, database included. Found here, before any run.
    ("a dot-only name is refused", "http://127.0.0.1:8099/../ ", None),
    ("so is a single dot", "http://127.0.0.1:8099/./ ", None),
    # A real segment after a traversal is a legal NAME and cannot escape: it has no separator, so
    # joining it onto the web directory stays inside it. It will simply not exist, and a reported
    # project whose directory is missing is an error rather than a silent fallback.
    ("a real segment after a traversal is just a name", "http://127.0.0.1:8099/../etc/ ", "etc"),
]
for name, result, want in PROJECT_CASES:
    got = reported_project(result)
    ok = got == want
    if not ok:
        bad = 1
    print(f"project/{name}: {'agree' if ok else 'DISAGREE'} got={got!r} want={want!r}")

# ── the proxy receipt's SHAPE, at both caps ────────────────────────────────────────────────────
# This rule lived inside each leg's heredoc where no case could reach it, and review 7 found it
# still enforcing `refused == 0` after the independent refusal rule had been removed - so every
# capped leg was disqualified anyway, through the dependent class, by a path the table below could
# not see. A rule that decides a leg lives here or it is untested.
SHAPE_CASES = [
    ("shape_clean",              dict(accepted=1),                    True),
    ("shape_at_the_cap",         dict(accepted="CAP"),                True),
    # THE CASE THAT WAS INVISIBLE: reaching the cap and being refused once is a VALID shape.
    ("shape_capped_and_refused", dict(accepted="CAP", refused=1),     True),
    ("shape_over_cap",           dict(accepted="CAP+1"),              False),
    ("shape_zero_requests",      dict(accepted=0),                    False),
    ("shape_untrusted_tls",      dict(tls_verified=False),            False),
    ("shape_tls_missing",        dict(tls_verified=None),             False),
    # E.CB2-HTTP-b: the http receipt is valid when reachable, invalid when not — and never by TLS.
    ("shape_http_reachable",     dict(tls_verified=False, upstream_scheme="http", upstream_reachable=True),  True),
    ("shape_http_unreachable",   dict(tls_verified=False, upstream_scheme="http", upstream_reachable=False), False),
    ("shape_https_unreachable_claim", dict(tls_verified=False, upstream_scheme="https", upstream_reachable=True), False),
    ("shape_untyped_accepted",   dict(accepted="3"),                  False),
    ("shape_untyped_refused",    dict(refused=True),                  False),
    ("shape_negative_refused",   dict(refused=-1),                    False),
]
for cap in CAPS:
    for name, over, want in SHAPE_CASES:
        args = dict(accepted=3, refused=0, upstream_errors=0, tls_verified=True, cap=cap)
        args.update({k: (cap if v == "CAP" else cap + 1 if v == "CAP+1" else v)
                     for k, v in over.items()})
        got = receipt_shape_ok(**args)
        ok = got is want
        if not ok:
            bad = 1
        print(f"cap{cap}/{name}: {'agree' if ok else 'DISAGREE'} got={got} want={want}")

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
# E.MODEL1 — THE LIVENESS CLASSIFIER. Its two failure directions are opposite and both costly:
# calling a bad minute "death" cancels readings that would have been fine, and calling death "a bad
# minute" is what let a retired model corrupt half a day of measurements. The transport-failure
# case alone does not watch the first direction — a mutation classifying EVERY non-2xx as death
# survived a suite that had only a code-000 inconclusive case. So the HTTP errors are enumerated.
LIVENESS_CASES = [
    # name,                status, body,                                              want
    ("answers",            200,    '{"choices":[]}',                                  "alive"),
    ("answers_202",        202,    "",                                                "alive"),
    ("retired",            410,    '{"detail":"The model gpt-oss-120b has reached its end of life"}', "gone"),
    ("unknown_model",      404,    '{"error":"model not found"}',                     "gone"),
    ("named_400",          400,    '{"error":"The model foo is no longer available"}', "gone"),
    # Everything below is a PROVIDER having a bad minute, an operator misconfiguration, or the
    # network. None of them is evidence the model has been retired.
    ("rate_limited",       429,    "slow down",                                       "inconclusive"),
    ("server_error",       500,    "internal",                                        "inconclusive"),
    ("bad_gateway",        502,    "",                                                "inconclusive"),
    ("gateway_timeout",    504,    "gateway timeout",                                 "inconclusive"),
    ("overloaded",         503,    "overloaded",                                      "inconclusive"),
    ("bad_key",            401,    "unauthorized",                                    "inconclusive"),
    ("forbidden",          403,    "forbidden",                                       "inconclusive"),
    ("request_defect_400", 400,    '{"error":"max_tokens must be positive"}',          "inconclusive"),
    ("transport_failure",  0,      "",                                                "inconclusive"),
    ("unreadable_status",  "xyz",  "",                                                "inconclusive"),
]
for name, status, body, want in LIVENESS_CASES:
    got, reason = model_liveness(status, body)
    ok = got == want
    if not ok:
        bad = 1
    print(f"liveness/{name}: {'agree' if ok else 'DISAGREE'} got={got} want={want}")

# The reason must carry the PROVIDER'S OWN WORDS. An operator who is told only "the model failed"
# goes looking for a bug in the harness; one who is told "end of life on 2026-09-03" does not.
_v, _r = model_liveness(410, '{"detail":"reached its end of life on 2026-09-03T08:00:00Z"}')
if "end of life" not in _r or "410" not in _r:
    bad = 1
    print(f"liveness/reason_quotes_provider: DISAGREE got={_r!r}")
else:
    print("liveness/reason_quotes_provider: agree")

# E.MODEL1b -- WHICH KIND OF GONE. A 404 answers for a retired model and for a mistyped id alike,
# and the refusal used to report both as "no longer exists". That happened for real: probing
# `deepseek-ai/deepseek-v4-flash-0813` returned a flat 404 and was read as a retirement; the id
# carried pro's date suffix, and the provider had listed `-0731` throughout. These cases pin the
# four answers, including the one that must NOT be guessed.
_LISTED = ["deepseek-ai/deepseek-v4-flash-0731", "deepseek-ai/deepseek-v4-pro-0813",
           "meta/llama-3.1-8b-instruct"]
GONE_CASES = [
    ("mistyped_id_names_the_near_miss", "deepseek-ai/deepseek-v4-flash-0813", _LISTED,
     ["not among", "deepseek-v4-flash-0731", "before concluding a retirement"]),
    ("unlisted_404_says_nothing_is_close", "openai/gpt-oss-120b", _LISTED,
     ["not among", "nothing is close"]),
    ("still_listed_is_not_a_retirement", "meta/llama-3.1-8b-instruct", _LISTED,
     ["still LISTS", "not a retirement"]),
    # An unreadable list is its own answer. Reporting it as either a retirement or a typo would be
    # inventing evidence, which is the failure this whole classifier exists to avoid.
    ("unreadable_list_is_unresolved", "anything/at-all", None,
     ["could not be read", "unresolved"]),
]
for name, model, listed, wants in GONE_CASES:
    got = explain_gone(model, listed)
    ok = all(w in got for w in wants)
    if not ok:
        bad = 1
    print(f"gone/{name}: {'agree' if ok else 'DISAGREE'}" + ("" if ok else f" got={got!r}"))

# The near-miss must be a REAL near miss, not the first thing in the list.
# A 410 is the provider SAYING it retired the model -- NVIDIA sends the end-of-life date in the
# body. Telling that operator to "check the id" is the same defect pointing the other way, and it
# only showed up when this ran against the real endpoint instead of a list. A retirement needs a
# SUCCESSOR, not a spell check.
STATED = [
    ("stated_retirement_is_not_a_spell_check", "openai/gpt-oss-120b", _LISTED + ["openai/gpt-oss-20b"], 410,
     ["states", "retired"], ["check the id"]),
    ("stated_retirement_offers_the_nearest_current", "openai/gpt-oss-120b", _LISTED + ["openai/gpt-oss-20b"], 410,
     ["gpt-oss-20b"], []),
    ("an_ambiguous_404_still_suggests_the_id", "deepseek-ai/deepseek-v4-flash-0813", _LISTED, 404,
     ["check the id"], ["states"]),
    ("stated_retirement_with_no_list_says_so", "openai/gpt-oss-120b", None, 410,
     ["states a retirement", "could not be read"], ["mistyped"]),
]
for name, model, listed, status, wants, forbids in STATED:
    got = explain_gone(model, listed, status)
    ok = all(w in got for w in wants) and not any(f in got for f in forbids)
    if not ok:
        bad = 1
    print(f"gone/{name}: {'agree' if ok else 'DISAGREE'}" + ("" if ok else f" got={got!r}"))

if "flash-0731" not in explain_gone("deepseek-ai/deepseek-v4-flash-0813", _LISTED):
    bad = 1
    print("gone/near_miss_is_actually_near: DISAGREE")
else:
    print("gone/near_miss_is_actually_near: agree")

sys.exit(bad)
