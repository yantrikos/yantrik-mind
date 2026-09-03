"""Profile-side receipt checks, counts only (strictly typed). Usage:
  receipt_checks.py <proxy requests.json> <expected model>
Prints one line:
  valid=<true|false> http_errors=<n> transport_errors=<n> client_errors=<n> disconnects=<n>
  accepted=<n> refused=<n> model_ok=<true|false> models=<distinct> usage_p=<n> usage_c=<n> usage_n=<n>

`valid` is the load-bearing field: it is true only when the receipt exists and EVERY count is an
exact non-negative int (a bool, a string or a negative fails) and every response_models value is a
positive int. A caller must treat valid=false as an INDEPENDENT disqualification — a missing or
malformed receipt is not evidence of an infrastructure fault, so it can never be a void. When
valid is false every other field is a placeholder (-1 / false / 0) and means nothing.

model_ok is true iff at least one SUCCESSFUL model response was tallied and every tallied model
equals the expected model."""
import json, sys

FALLBACK = ("valid=false http_errors=-1 transport_errors=-1 client_errors=-1 disconnects=-1 "
            "accepted=-1 refused=-1 model_ok=false models=0 usage_p=0 usage_c=0 usage_n=0")


def nn(x):
    return type(x) is int and x >= 0


def main():
    try:
        d = json.load(open(sys.argv[1]))
        want = sys.argv[2]
        http_err, trans = d["upstream_http_errors"], d["upstream_errors"]
        client, disc = d["upstream_client_errors"], d["client_disconnects"]
        # `.get(..., 0)` and NOT `d[...]`: every receipt written before this counter existed --
        # readings 3 to 6 -- has no such key, and a re-check of a finished reading must not start
        # failing because the harness grew a field. Absent means zero, which is exactly true for
        # them. It is typed here so "strictly typed receipt integers" stays honest; it is
        # deliberately NOT added to the printed line, which hermes_leg.sh parses POSITIONALLY --
        # an extra token there would spill into the last variable and silently corrupt usage_n.
        ptimeouts = d.get("proxy_request_timeouts", 0)
        acc, ref = d["model_requests"], d["refused_over_cap"]
        models, usage = d["response_models"], d["usage"]
        valid = (all(nn(x) for x in (http_err, trans, client, disc, acc, ref, ptimeouts))
                 and isinstance(models, dict)
                 and all(isinstance(k, str) and type(v) is int and v > 0 for k, v in models.items())
                 and isinstance(usage, dict)
                 and all(nn(usage.get(k)) for k in ("responses_with_usage", "prompt_tokens", "completion_tokens")))
        if not valid:
            print(FALLBACK)
            return
        ok = bool(models) and all(k == want for k in models)
        print(f"valid=true http_errors={http_err} transport_errors={trans} client_errors={client} disconnects={disc} "
              f"accepted={acc} refused={ref} model_ok={str(ok).lower()} models={len(models)} "
              f"usage_p={usage['prompt_tokens']} usage_c={usage['completion_tokens']} usage_n={usage['responses_with_usage']}")
    except Exception:
        print(FALLBACK)


main()
