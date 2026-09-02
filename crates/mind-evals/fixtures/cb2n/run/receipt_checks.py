"""Profile-side receipt checks, counts only. Usage:
  receipt_checks.py <proxy requests.json> <expected model>
Prints: http_errors=<n> transport_errors=<n> model_ok=<true|false> models=<n distinct> usage_p=<n> usage_c=<n> usage_n=<n>
model_ok is true iff every tallied response model equals the expected model (a leg with no
tallied model at all is NOT ok — the identity check must have evidence)."""
import json, sys
try:
    d = json.load(open(sys.argv[1]))
    want = sys.argv[2]
    models = d.get("response_models") or {}
    http_err = int(d.get("upstream_http_errors", -1)); trans = int(d.get("upstream_errors", -1))
    ok = bool(models) and all(k == want for k in models) and type(http_err) is int
    u = d.get("usage") or {}
    print(f"http_errors={http_err} transport_errors={trans} model_ok={str(ok).lower()} models={len(models)} usage_p={u.get('prompt_tokens', 0)} usage_c={u.get('completion_tokens', 0)} usage_n={u.get('responses_with_usage', 0)}")
except Exception:
    print("http_errors=-1 transport_errors=-1 model_ok=false models=0 usage_p=0 usage_c=0 usage_n=0")
