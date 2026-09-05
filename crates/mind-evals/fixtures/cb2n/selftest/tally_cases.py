"""E.CB2-TALLY1: drives proxy.tally_models. Exits 1 on any disagreement."""
import importlib.util, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("cb2proxy", os.path.join(HERE, "..", "proxy", "proxy.py"))
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
CASES = [
    ("empty_id_is_not_a_claim", [{"model": "gpt-oss-backup:20b"}, {"model": ""}], {"gpt-oss-backup:20b"}),
    ("only_empty_is_none",       [{"model": ""}, {"model": "  "}],                 set()),
    ("junk_ignored",            [{"model": 3}, "x", None, {"id": "y"}],           set()),
    ("two_models_both_count",   [{"model": "a"}, {"model": "b "}],                 {"a", "b"}),
]
bad = 0
for name, objs, want in CASES:
    got = m.tally_models(objs); ok = got == want; bad |= not ok
    print(f"{name}: {'agree' if ok else 'DISAGREE'} got={sorted(got)} want={sorted(want)}")
sys.exit(1 if bad else 0)
