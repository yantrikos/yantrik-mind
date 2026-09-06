# E.USAGE2: drives proxy.usage_from and proxy.parse_bodies. Exits 1 on any disagreement.
import importlib.util, json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("cb2proxy", os.path.join(HERE, "..", "proxy", "proxy.py"))
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)

OPENAI_BODY = json.dumps({"model": "x", "choices": [{"message": {"content": "hi"}}],
                         "usage": {"prompt_tokens": 11, "completion_tokens": 7}}).encode()
NATIVE_BODY = json.dumps({"model": "gpt-oss-backup:20b", "message": {"content": "hi"},
                         "done": True, "prompt_eval_count": 70, "eval_count": 16}).encode()
NATIVE_STREAM = ("\n".join([
    json.dumps({"model": "gpt-oss-backup:20b", "message": {"content": "h"}, "done": False}),
    json.dumps({"model": "gpt-oss-backup:20b", "message": {"content": "i"}, "done": False}),
    json.dumps({"model": "gpt-oss-backup:20b", "done": True, "prompt_eval_count": 5, "eval_count": 3}),
]) + "\n").encode()
SSE_STREAM = ("\n".join([
    "data: " + json.dumps({"model": "m", "choices": [{"delta": {"content": "h"}}]}),
    "data: " + json.dumps({"model": "m", "choices": [], "usage": {"prompt_tokens": 76, "completion_tokens": 40}}),
    "data: [DONE]",
]) + "\n").encode()
NO_COUNTS = json.dumps({"model": "x", "choices": [{"message": {"content": "hi"}}]}).encode()

CASES = [
    ("openai_body",   OPENAI_BODY,   {"prompt_tokens": 11, "completion_tokens": 7}, {"x"}),
    ("native_body",   NATIVE_BODY,   {"prompt_tokens": 70, "completion_tokens": 16}, {"gpt-oss-backup:20b"}),
    ("native_stream", NATIVE_STREAM, {"prompt_tokens": 5,  "completion_tokens": 3},  {"gpt-oss-backup:20b"}),
    ("sse_stream",    SSE_STREAM,    {"prompt_tokens": 76, "completion_tokens": 40}, {"m"}),
    ("no_counts",     NO_COUNTS,     None,                                           {"x"}),
]
bad = 0
for name, raw, want_usage, want_models in CASES:
    objs = m.parse_bodies(raw)
    got = m.usage_from(objs)
    got_models = m.tally_models(objs)
    ok = got == want_usage and got_models == want_models
    bad |= not ok
    print(name + ": " + ("agree" if ok else "DISAGREE") + " usage=" + str(got) + " want=" + str(want_usage) + " models=" + str(sorted(got_models)))
sys.exit(1 if bad else 0)
