#!/usr/bin/env python3
"""Daily behavior/state RETROSPECTIVE for JARVIS (yantrik-mind).

A STRONG model (Opus 4.8 via OpenRouter by default) reflects on JARVIS's current typed-memory state
and proposes ONE concrete, buildable *code* improvement, appended to the self-build goal queue that
`self_build_tick.sh` already consumes (build -> compile+test gate -> auto-merge, harm-gate protected).

So the loop is: retrospective (this) -> goal queued -> self_build_tick -> self_improve.sh (self-deploy).
Cost ~a few cents/day. Cron: once daily. Kill-switch: the same SELF_IMPROVE_OFF flag the tick honors.

Run as root (reads /etc/yantrik-mind.env, root:600). Override the model with YM_RETRO_MODEL.
"""
import json
import os
import subprocess
import urllib.request
import datetime

GOALS = "/var/lib/yantrik-mind/selfbuild-goals.txt"
ENVF = "/etc/yantrik-mind.env"
KILL = "/var/lib/yantrik-mind/SELF_IMPROVE_OFF"
CTL = "http://127.0.0.1:8077/cli"


def load_env():
    env = {}
    try:
        for line in open(ENVF):
            line = line.rstrip("\n")
            if "=" in line and not line.lstrip().startswith("#"):
                k, v = line.split("=", 1)
                env[k.strip()] = v
    except Exception:
        pass
    return env


def main():
    if os.path.exists(KILL):
        print("kill-switch present — retrospective skipped")
        return
    env = load_env()
    key = env.get("OPEN_ROUTER_KEY", "").strip()
    if not key:
        print("no OPEN_ROUTER_KEY — skip")
        return
    model = os.environ.get("YM_RETRO_MODEL", env.get("YM_RETRO_MODEL", "anthropic/claude-opus-4.8")).strip()

    # 1) current self-state: beliefs (by confidence) + open contradictions + goals
    try:
        state = subprocess.run(
            ["curl", "-s", "-m", "60", "--data-binary", "reflect", CTL],
            capture_output=True, text=True, timeout=80,
        ).stdout[:4500]
    except Exception:
        state = ""

    prompt = (
        "You are JARVIS running a DAILY RETROSPECTIVE to improve your own Rust code (crates/mind-* in the "
        "yantrik-mind workspace). Below is your current typed-memory state. Based on it and your known "
        "weaknesses, propose exactly ONE concrete, buildable CODE improvement — a specific change a "
        "developer could implement in a few hours (e.g. 'Dedupe near-duplicate beliefs on write by merging "
        "entries above a similarity threshold and combining their evidence/confidence'). It must be a code "
        "change, not a one-off data fix. Output ONLY the goal as a single imperative sentence — no "
        "preamble, no markdown, no numbering.\n\n=== CURRENT MEMORY STATE ===\n" + (state or "(state unavailable)")
    )
    body = json.dumps({
        "model": model,
        "max_tokens": 250,
        "messages": [{"role": "user", "content": prompt}],
    }).encode()
    req = urllib.request.Request(
        "https://openrouter.ai/api/v1/chat/completions",
        data=body,
        headers={"Authorization": f"Bearer {key}", "content-type": "application/json"},
    )
    try:
        r = json.load(urllib.request.urlopen(req, timeout=120))
        goal = r["choices"][0]["message"]["content"].strip().splitlines()[0].strip()
    except Exception as e:
        print("retrospective LLM call failed:", e)
        return

    goal = goal.lstrip("-*0123456789. ").strip()
    if len(goal) < 12:
        print("no usable goal produced")
        return
    stamp = datetime.datetime.utcnow().strftime("%Y-%m-%dT%H:%M:%SZ")
    os.makedirs(os.path.dirname(GOALS), exist_ok=True)
    with open(GOALS, "a") as f:
        f.write(goal + "\n")
    print(f"{stamp} queued self-build goal: {goal}")


if __name__ == "__main__":
    main()
