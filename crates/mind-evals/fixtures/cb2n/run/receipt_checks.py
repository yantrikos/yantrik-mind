"""Profile-side receipt checks, counts only (strictly typed). Usage:
  receipt_checks.py <proxy requests.json> <expected model>
Prints one line: http_errors=<n> transport_errors=<n> client_errors=<n> model_ok=<true|false>
models=<distinct> usage_p=<n> usage_c=<n> usage_n=<n>
Every count must be an exact non-negative int (bool/str/negative are rejected) and every
response_models value a positive int, else the fallback line (all -1 / false / 0) is printed.
model_ok is true iff at least one successful model response was tallied and every tallied model
equals the expected model."""
import json, sys


def nn(x):
    return type(x) is int and x >= 0


def main():
    fallback = "http_errors=-1 transport_errors=-1 client_errors=-1 model_ok=false models=0 usage_p=0 usage_c=0 usage_n=0"
    try:
        d = json.load(open(sys.argv[1]))
        want = sys.argv[2]
        http_err, trans, client = d["upstream_http_errors"], d["upstream_errors"], d["upstream_client_errors"]
        models, usage = d["response_models"], d["usage"]
        typed = (nn(http_err) and nn(trans) and nn(client) and isinstance(models, dict)
                 and all(isinstance(k, str) and type(v) is int and v > 0 for k, v in models.items())
                 and isinstance(usage, dict) and all(nn(usage.get(k)) for k in ("responses_with_usage", "prompt_tokens", "completion_tokens")))
        if not typed:
            print(fallback)
            return
        ok = bool(models) and all(k == want for k in models)
        print(f"http_errors={http_err} transport_errors={trans} client_errors={client} model_ok={str(ok).lower()} models={len(models)} "
              f"usage_p={usage['prompt_tokens']} usage_c={usage['completion_tokens']} usage_n={usage['responses_with_usage']}")
    except Exception:
        print(fallback)


main()
