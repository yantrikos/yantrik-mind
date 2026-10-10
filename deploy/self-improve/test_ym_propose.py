"""Tests for ym-propose.py (E.SELF1). Run: python3 -m unittest deploy/self-improve/test_ym_propose.py"""
import http.server
import importlib.util
import json
import os
import shutil
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("ym_propose", os.path.join(HERE, "ym-propose.py"))
yp = importlib.util.module_from_spec(spec)
spec.loader.exec_module(yp)

GOOD = {"title": "Calendar reminders fire twice after resume", "kind": "bug", "area": "calendar",
        "evidence": "scout: two notifications at 09:00", "reasoning": "the resume handler re-arms timers",
        "proposed_change": "clear armed timers before re-arming on resume", "confidence": 0.7}


def at(hour):
    """A local timestamp today at `hour`:30."""
    t = time.localtime()
    return time.mktime((t.tm_year, t.tm_mon, t.tm_mday, hour, 30, 0, 0, 0, -1))


class FakeGate:
    model = "fake-model"

    def __init__(self, answers):
        self.answers, self.calls = list(answers), []

    def ask(self, system, user, max_tokens=1800):
        self.calls.append((system, user))
        return self.answers.pop(0), 1000


class FakeMemory:
    def __init__(self, rows=None):
        self.queries = []
        self.rows = rows if rows is not None else [{"text": "Pranab wants the composer to pick models from his providers.",
                                                    "domain": "yantrik-os"}]

    def recall(self, query, k=6):
        self.queries.append(query)
        return list(self.rows)


class Base(unittest.TestCase):
    def setUp(self):
        self.d = tempfile.mkdtemp()
        yp.FINDINGS = os.path.join(self.d, "findings.jsonl")
        yp.OUT_DIR = os.path.join(self.d, "proposals")
        yp.STATE = os.path.join(self.d, "state.json")
        yp.DAILY_PROPOSALS, yp.DAILY_TOKENS = 10, 400000

    def tearDown(self):
        shutil.rmtree(self.d, ignore_errors=True)

    def out(self):
        p = os.path.join(yp.OUT_DIR, f"proposals-{time.strftime('%Y-%m-%d')}.jsonl")
        return [json.loads(l) for l in open(p, encoding="utf-8")] if os.path.exists(p) else []

    def finding(self, title):
        with open(yp.FINDINGS, "a", encoding="utf-8") as f:
            f.write(json.dumps({"title": title, "area": "shell", "severity": "degraded"}) + "\n")


class Pure(Base):
    def test_quiet_hours_bounds(self):
        self.assertTrue(yp.quiet(22) and yp.quiet(23) and yp.quiet(0) and yp.quiet(6))
        self.assertFalse(yp.quiet(7) or yp.quiet(12) or yp.quiet(21))

    def test_validate_each_field(self):
        self.assertEqual(yp.validate(GOOD)["kind"], "bug")
        for k in yp.FIELDS:
            bad = dict(GOOD)
            del bad[k]
            self.assertIsNone(yp.validate(bad), f"missing {k} accepted")
        self.assertIsNone(yp.validate(dict(GOOD, kind="chore")))
        self.assertIsNone(yp.validate(dict(GOOD, confidence=1.5)))
        self.assertIsNone(yp.validate(dict(GOOD, confidence="high")))
        self.assertIsNone(yp.validate(dict(GOOD, area="  ")))
        self.assertEqual(len(yp.validate(dict(GOOD, title="x" * 500))["title"]), 140)
        self.assertEqual(yp.validate(dict(GOOD, evidence=["a", "b"]))["evidence"], "a; b")

    def test_secrets_never_pass(self):
        for leak in ["key sk-proj-abcdefghijklmnopqrstuv", "ghp_abcdefghijklmnopqrstuvwxyz12", "AKIAIOSFODNN7EXAMPLE",
                     "password = hunter2hunter", "-----BEGIN RSA PRIVATE KEY-----", "Bearer abcdefghijklmnopqrstu"]:
            self.assertIsNone(yp.validate(dict(GOOD, reasoning=leak)), leak)

    def test_parse_fenced_and_chatty(self):
        self.assertEqual(len(yp.parse_array("```json\n[" + json.dumps(GOOD) + "]\n```")), 1)
        self.assertEqual(len(yp.parse_array("Here you go: [" + json.dumps(GOOD) + "] hope it helps")), 1)
        self.assertEqual(yp.parse_array("no array here"), [])
        self.assertEqual(yp.parse_array("[not json"), [])


class Runs(Base):
    def test_quiet_hours_do_nothing(self):
        self.finding("A")
        gate = FakeGate(["[" + json.dumps(GOOD) + "]"])
        yp.run(now=at(23), gate=gate, memory=FakeMemory())
        self.assertEqual(gate.calls, [])
        self.assertEqual(self.out(), [])

    def test_a_finding_becomes_a_proposal_once(self):
        self.finding("Calendar double reminders")
        mem = FakeMemory()
        yp.run(now=at(10), gate=FakeGate(["[" + json.dumps(GOOD) + "]"]), memory=mem)
        out = self.out()
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0]["title"], GOOD["title"])
        self.assertEqual(out[0]["source"], json.load(open(yp.STATE))["seen_findings"][0])
        self.assertEqual(mem.queries, ["Calendar double reminders"])
        # The same finding is never asked about again; the next run only does the memory review.
        gate2 = FakeGate(["[" + json.dumps(dict(GOOD, title="Something else entirely")) + "]"])
        yp.run(now=at(12), gate=gate2, memory=FakeMemory())
        self.assertEqual(len(gate2.calls), 1)
        self.assertIn("No new findings", gate2.calls[0][1])
        # And after the review, nothing: no model call at all.
        gate3 = FakeGate([])
        yp.run(now=at(14), gate=gate3, memory=FakeMemory())
        self.assertEqual(gate3.calls, [])
        self.assertEqual(len(self.out()), 2)

    def test_a_repeated_title_is_dropped(self):
        self.finding("A")
        self.finding("B")
        yp.run(now=at(10), gate=FakeGate(["[" + json.dumps(GOOD) + "]", "[" + json.dumps(dict(GOOD, title=GOOD["title"].upper())) + "]"]),
               memory=FakeMemory())
        self.assertEqual(len(self.out()), 1)

    def test_the_daily_cap_stops(self):
        yp.DAILY_PROPOSALS = 2
        for t in "ABC":
            self.finding(t)
        answers = ["[" + json.dumps(dict(GOOD, title=f"T{i}{j}")) + "," + json.dumps(dict(GOOD, title=f"U{i}{j}")) + "]"
                   for i in range(3) for j in range(1)]
        gate = FakeGate(answers)
        yp.run(now=at(10), gate=gate, memory=FakeMemory())
        self.assertEqual(len(self.out()), 2)
        self.assertEqual(len(gate.calls), 1, "a call was made after the cap was reached")

    def test_a_model_that_returns_too_many_is_cut_at_the_cap(self):
        yp.DAILY_PROPOSALS = 2
        self.finding("A")
        three = json.dumps([dict(GOOD, title=f"P{i}") for i in range(3)])
        yp.run(now=at(10), gate=FakeGate([three]), memory=FakeMemory())
        self.assertEqual([p["title"] for p in self.out()], ["P0", "P1"])

    def test_the_token_budget_stops(self):
        yp.DAILY_TOKENS = 1500
        for t in "ABC":
            self.finding(t)
        gate = FakeGate(["[]", "[]", "[]"])
        yp.run(now=at(10), gate=gate, memory=FakeMemory())
        self.assertEqual(len(gate.calls), 2, "1000 tokens a call: the third call would pass 1500")
        yp.run(now=at(11), gate=FakeGate([]), memory=FakeMemory())

    def test_invalid_and_secret_proposals_are_dropped(self):
        self.finding("A")
        answer = json.dumps([dict(GOOD, kind="chore"), dict(GOOD, title="leak", reasoning="token: abcdef123456"), dict(GOOD, title="ok one")])
        yp.run(now=at(10), gate=FakeGate([answer]), memory=FakeMemory())
        self.assertEqual([p["title"] for p in self.out()], ["ok one"])


class EndToEnd(Base):
    """A real HTTP gate and a real HTTP memory server, the way the driver talks to them."""

    def test_a_full_run_over_http(self):
        self.finding("Wi-Fi drops after sleep")
        seen = {"gate": [], "mem": []}

        class H(http.server.BaseHTTPRequestHandler):
            def log_message(self, *a):
                pass

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                if self.path == "/v1/chat/completions":
                    seen["gate"].append((self.headers["Authorization"], body["model"]))
                    ans = {"choices": [{"message": {"content": "[" + json.dumps(GOOD) + "]"}}], "usage": {"total_tokens": 777}}
                    data = json.dumps(ans).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                else:
                    seen["mem"].append((self.headers["Authorization"], body.get("method")))
                    if body.get("method", "").startswith("notifications/"):
                        self.send_response(202)
                        self.end_headers()
                        return
                    seen["recall_args"] = body.get("params", {}).get("arguments")
                    rows = [{"kind": "memory", "text": "the Wi-Fi chip is a BCM4331", "domain": "infrastructure"}]
                    result = {"content": [{"type": "text", "text": json.dumps({"results": rows})}]}
                    data = json.dumps({"jsonrpc": "2.0", "id": body.get("id"), "result": result}).encode()
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Mcp-Session-Id", "s1")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

        srv = http.server.HTTPServer(("127.0.0.1", 0), H)
        threading.Thread(target=srv.serve_forever, daemon=True).start()
        port = srv.server_address[1]
        env = os.path.join(self.d, "mind.env")
        open(env, "w").write(f"YM_PROVIDER_BASE_URL_NANOGPT=http://127.0.0.1:{port}/v1\nNANOGPT_KEY=gk-test\nYM_MODEL=strata-x\n")
        tok = os.path.join(self.d, "mem.token")
        open(tok, "w").write("mem-token-123\n")
        try:
            os.environ["NO_PROXY"] = "127.0.0.1"
            yp.run(now=at(10), gate=yp.Gate(env), memory=yp.Memory(f"http://127.0.0.1:{port}/mcp", tok))
        finally:
            srv.shutdown()
        self.assertEqual(seen["gate"], [("Bearer gk-test", "strata-x")])
        self.assertEqual([m for _, m in seen["mem"]], ["initialize", "notifications/initialized", "tools/call"])
        self.assertEqual(seen["recall_args"]["include"], "memories", "beliefs carry no domain; they are not recalled")
        self.assertTrue(all(a == "Bearer mem-token-123" for a, _ in seen["mem"]))
        out = self.out()
        self.assertEqual(len(out), 1)
        self.assertEqual(out[0]["model"], "strata-x")
        st = json.load(open(yp.STATE))
        self.assertEqual((st["proposals"], st["tokens"]), (1, 777))


if __name__ == "__main__":
    unittest.main()


class Private(Base):
    """E.SELF1b: the person's private memories never reach a prompt or a proposal."""

    ROWS = [
        {"text": "Pranab keeps a trading journal of his positions", "domain": "finance"},
        {"text": "RELATIONSHIP WITH PRANAB: he pushes hard on my conclusions", "domain": "people"},
        {"text": "his daughter starts school in August", "domain": "general"},
        {"text": "the scanner card feeds the trading dashboard", "domain": "work"},
        {"text": "the Notes app loses its scroll position when the window is resized on the Mac mini", "domain": "yantrik-os"},
    ]

    def test_private_domains_and_topics_never_reach_the_gate(self):
        gate = FakeGate(["[]"])
        yp.run(now=at(10), gate=gate, memory=FakeMemory(self.ROWS))
        prompt = gate.calls[0][1]
        for leak in ["trading journal", "RELATIONSHIP", "daughter", "scanner card"]:
            self.assertNotIn(leak, prompt, leak)
        self.assertIn("Notes app loses its scroll position", prompt)

    def test_the_prompt_says_scope_and_no_quotes(self):
        gate = FakeGate(["[]"])
        yp.run(now=at(10), gate=gate, memory=FakeMemory(self.ROWS))
        system = gate.calls[0][0]
        self.assertIn("only for Yantrik OS or the Yantrik Mind", system)
        self.assertIn("never quote a memory", system)

    def test_a_proposal_copying_eight_words_is_dropped_seven_kept(self):
        mem = [{"text": "the Notes app loses its scroll position when the window is resized on the Mac mini",
                "domain": "yantrik-os"}]
        # Eight in a row shared: "loses its scroll position when the window is".
        eight = dict(GOOD, title="Copy", evidence="it loses its scroll position when the window is resized")
        # Exactly seven words in a row shared ("loses its scroll position when the window"), then its own.
        seven = dict(GOOD, title="Own words", evidence="it loses its scroll position when the window shrinks")
        yp.run(now=at(10), gate=FakeGate([json.dumps([eight, seven])]), memory=FakeMemory(mem))
        self.assertEqual([p["title"] for p in self.out()], ["Own words"])

    def test_a_forced_review_runs_again_the_same_day(self):
        yp.run(now=at(10), gate=FakeGate(["[]"]), memory=FakeMemory())
        gate = FakeGate(["[]"])
        yp.FORCE_REVIEW = True
        try:
            yp.run(now=at(11), gate=gate, memory=FakeMemory())
        finally:
            yp.FORCE_REVIEW = False
        self.assertEqual(len(gate.calls), 1)


class Repeats(Base):
    """E.SELF1c: the field's 9 real titles, in the order the Mac wrote them (Oct 7-10)."""

    REAL = [
        "Implement Structured Memory Schema to Replace Flat CLAUDE.md",
        "Add Deterministic Mode and Flake Detection to E2E Test Runner",
        "Implement Relationship-Aware Context Weighting",
        "Implement Deterministic E2E Test Harness for Yantrik OS",
        "Automated Legacy Memory Cleanup Tool",
        "Enhance E2E Test Determinism with Playwright Best Practices",
        "Automated Cleanup of Legacy/Invalid Durable Beliefs",
        "Implement deterministic Playwright test runner with auto-retry and trace capture",
        "Add automated cleanup for legacy/durable memory entries",
    ]

    def test_the_real_titles_keep_exactly_the_distinct_ideas(self):
        kept = []
        for t in self.REAL:
            if not yp.near_duplicate(t, kept):
                kept.append(t)
        self.assertEqual(kept, [self.REAL[0], self.REAL[1], self.REAL[2], self.REAL[4]])

    def test_the_prompt_shows_earlier_titles_as_text(self):
        self.finding("A")
        yp.run(now=at(10), gate=FakeGate(["[" + json.dumps(GOOD) + "]"]), memory=FakeMemory())
        self.finding("B")
        gate = FakeGate(["[]"])
        yp.run(now=at(11), gate=gate, memory=FakeMemory())
        user = gate.calls[0][1]
        self.assertIn("- " + GOOD["title"], user)
        self.assertNotIn(yp.title_id(GOOD["title"]), user, "a hash is not a title the model can avoid")

    def test_a_near_duplicate_is_dropped(self):
        self.finding("A")
        self.finding("B")
        first = dict(GOOD, title="Automated Legacy Memory Cleanup Tool")
        again = dict(GOOD, title="Add automated cleanup for legacy/durable memory entries")
        yp.run(now=at(10), gate=FakeGate(["[" + json.dumps(first) + "]", "[" + json.dumps(again) + "]"]), memory=FakeMemory())
        self.assertEqual([p["title"] for p in self.out()], [first["title"]])

    def test_the_review_theme_changes_from_day_to_day(self):
        mem = FakeMemory()
        days = []
        for d in (0, 1):
            t = time.localtime(at(10) + d * 86400)
            yp.STATE = os.path.join(self.d, f"state{d}.json")
            yp.run(now=time.mktime(t), gate=FakeGate(["[]"]), memory=mem)
        self.assertEqual(len(mem.queries), 2)
        self.assertNotEqual(mem.queries[0], mem.queries[1])
        self.assertTrue(all(q in yp.REVIEW_THEMES for q in mem.queries))
