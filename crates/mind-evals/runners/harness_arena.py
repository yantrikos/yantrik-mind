#!/usr/bin/env python3
"""E.ARENA1 -- the harness arena.

The same tasks, put to every mind attached to a Yantrik OS desktop, through the same door a person
uses (the shell's `send_message`, "as if typed into the Lens"), and judged by the desktop's own state
afterwards -- never by what the mind says it did.

Runs ON the Yantrik OS machine, stdlib only, drives everything through `yos`.

    python3 harness_arena.py                      # every attached mind except the companion
    python3 harness_arena.py --minds mind,hermes  # just these
    python3 harness_arena.py --tasks T1,T3        # just these tasks

Writes one JSON line per (mind, task) to --out and prints a table. See docs/PHASE2_EXPERIMENT_LEDGER.md
E.ARENA1 for the preregistration, the kill criteria and why the tasks are what they are.
"""
import argparse
import json
import os
import random
import re
import shutil
import string
import subprocess
import sys
import time

HOME = os.path.expanduser("~")
TURN_TIMEOUT_S = 300
SETTLE_S = 5  # a reply unchanged this long, and not streaming, is finished
POLL_S = 1.0


# ── the door ──────────────────────────────────────────────────────────────────────────────────

def yos(*args, timeout=60):
    """Run `yos` with argv (never a shell string -- a space in a task must not split it)."""
    r = subprocess.run(["yos", *args], capture_output=True, text=True, timeout=timeout)
    return (r.stdout or "") + (r.stderr or "")


def describe(app):
    """The JSON state of an app. `yos describe` prints a header, then the object."""
    out = yos("describe", app)
    i = out.find("{")
    if i < 0:
        return None
    try:
        obj, _ = json.JSONDecoder().raw_decode(out[i:])
        return obj
    except ValueError:
        return None


def act(app, action, **kw):
    return yos("act", app, action, *[f"{k}={v}" for k, v in kw.items()])


def conversation():
    s = describe("shell") or {}
    return s.get("conversation") or []


def active_mind():
    s = describe("shell") or {}
    for m in s.get("minds") or []:
        if m.get("answering"):
            return m.get("id")
    return None


def attached_minds():
    s = describe("shell") or {}
    return [m for m in (s.get("minds") or [])]


BUSY = re.compile(r"still working on the previous request|busy with (a|the) previous|say /stop", re.I)


def wait_idle(limit_s=TURN_TIMEOUT_S):
    """Do not ask a mind that is still answering something else.

    Run 160 asked DeepSeek three tasks while it was still working on a turn left over from a run I
    had killed; all three came back "still working on the previous request" in 1.9 s and were scored
    as failures. A mind that is busy has not been asked anything yet. Wait until nothing in the
    conversation is streaming and it has been still for SETTLE_S.
    """
    t0, last, since = time.time(), None, time.time()
    while time.time() - t0 < limit_s:
        conv = conversation()
        snap = json.dumps(conv[-2:]) if conv else ""
        streaming = any(m.get("streaming") for m in conv[-3:])
        if streaming or snap != last:
            last, since = snap, time.time()
        elif time.time() - since >= SETTLE_S:
            return True
        time.sleep(POLL_S)
    return False


def ask(text):
    """Put one turn to the active mind and wait for it to finish. Returns (reply, seconds, done)."""
    wait_idle()
    t0 = time.time()
    act("shell", "send_message", text=text)
    last, stable_since = None, None
    while time.time() - t0 < TURN_TIMEOUT_S:
        time.sleep(POLL_S)
        conv = conversation()
        # Our message, then everything after it. Matched by text from the end, because the
        # conversation may be capped and indexes are not stable.
        idx = None
        for i in range(len(conv) - 1, -1, -1):
            if conv[i].get("role") == "user" and conv[i].get("text", "").strip() == text.strip():
                idx = i
                break
        if idx is None:
            continue
        after = conv[idx + 1:]
        if not after or after[-1].get("role") != "assistant":
            continue
        reply = "\n".join(m.get("text", "") for m in after if m.get("role") == "assistant")
        streaming = any(m.get("streaming") for m in after)
        if streaming or not reply.strip():
            stable_since = None
            continue
        if reply != last:
            last, stable_since = reply, time.time()
            continue
        if time.time() - stable_since >= SETTLE_S:
            return reply, round(time.time() - t0 - SETTLE_S, 1), True
    return (last or ""), round(time.time() - t0, 1), False


# ── the world, read and reset by the arena itself ─────────────────────────────────────────────

class Void(Exception):
    """The grader could not establish what is true, so the cell is not graded at all."""


def calendar_day(day):
    # Opened HERE, at the moment of reading, not only at reset: Reading D's first attempt found the
    # calendar closed when T2 read its truth, read an empty day, and failed a correct answer.
    ensure_calendar_open()
    act("calendar", "select_day", day=day)
    time.sleep(0.5)
    c = describe("calendar") or {}
    return c.get("events_on_selected_day") or []


def day25_truth():
    """What is on 25 September. Never empty: an empty truth failed T2's correct answer, and made
    T7 pass ANY file -- a task won by writing something, which K20 forbids."""
    titles = [e["title"] for e in calendar_day(25)]
    if not titles:
        raise Void("the grader read no events on 25 September; the calendar did not answer")
    return titles


def running_apps():
    return yos("ls").lower()


def calendar_window_open():
    """The calendar WINDOW is open -- not merely something answering to "calendar". With the window
    closed, `describe calendar` is answered by the calendar SERVICE (events, reminders, store,
    upcoming; no selected day), so "describe answered" was never evidence the window was up. That is
    how Reading D's first attempt read an empty 25 September: the window had closed, the service
    answered, and `select_day` went nowhere."""
    return "selected_day" in (describe("calendar") or {})


def ensure_calendar_open():
    """True once the calendar window answers. `open_app` settles later, so it is waited on."""
    if calendar_window_open():
        return True
    act("shell", "open_app", name="calendar")
    for _ in range(40):
        time.sleep(0.5)
        if calendar_window_open():
            return True
    return False


def reset_world(tag):
    """Remove everything a run could have made. The arena does this, never a mind."""
    for p in os.listdir(HOME):
        if p.startswith("arena-"):
            full = os.path.join(HOME, p)
            shutil.rmtree(full, ignore_errors=True) if os.path.isdir(full) else os.remove(full)
    if not ensure_calendar_open():
        print("  !! reset: the calendar did not open -- this run is contaminated", flush=True)
    for e in calendar_day(30):
        if e.get("title", "").startswith("Arena "):
            act("calendar", "delete_event", id=e["id"])
    subprocess.run(["pkill", "-x", "yantrik-notes"], capture_output=True)
    # The editor keeps its tabs for as long as it runs, and allows eight. Readings B-prime to B5 each
    # opened new ones, so by B5 `new` was refused ("Eight tabs are already open") for the mind that
    # ran last -- a handicap the arena created and Hermes, run first, never met. Every mind starts
    # from a fresh editor.
    # By full path, not `-x`: `pkill -x` matches the kernel's process NAME, which is cut to 15
    # characters, and "yantrik-text-editor" is 19 -- so `pkill -x yantrik-text-editor` never matched,
    # and the editor ran with its eight tabs from 13:52 through every reading after it.
    subprocess.run(["pkill", "-f", "/opt/yantrik/bin/yantrik-text-editor"], capture_output=True)
    for _ in range(20):
        if subprocess.run(["pgrep", "-f", "/opt/yantrik/bin/yantrik-text-editor"],
                          capture_output=True).returncode != 0:
            break
        time.sleep(0.25)
    # The editor restores unsaved drafts when it starts again ("Recovered unsaved drafts"), so a
    # closed editor's text came back in the next mind's. Set them aside once it is closed.
    drafts = os.path.join(HOME, ".local/state/yantrik/editor/drafts.json")
    if os.path.exists(drafts):
        os.replace(drafts, drafts + ".arena-previous")
    # The shell has an editor of its own (`editor_*`), and its document outlived every mind: Reading
    # C's mind began T6 inside Hermes's "arena-her2yz-friday.txt", left there by the mind run first.
    # A fresh document, then back to the desktop screen -- `editor_new` switches the shell to its
    # editor, and the arena must not leave the shell anywhere a mind did not put it.
    # Checked while the editor screen is up: the shell reports its document only there, so a check
    # made from the desktop screen sees no document at all and would pass a contaminated one.
    act("shell", "editor_new")
    ed = (describe("shell") or {}).get("editor")
    act("shell", "show_screen", screen="desktop")
    screen = (describe("shell") or {}).get("screen")
    if not ed or ed.get("content") or ed.get("modified") or screen != "desktop":
        print(f"  !! reset: shell editor {ed!r}, screen {screen!r} -- this run is contaminated",
              flush=True)


# ── the tasks (frozen for this reading: K21) ──────────────────────────────────────────────────

def t_open_app(tag):
    # `describe notes` answers only while the Notes window exists. Not `yos ls`: the shell's own
    # summary line there read "notes not running" WITH Notes open (VM 520, 2026-09-22), and the
    # first version of this grader believed it -- the positive control caught it failing a correct run.
    def grade(reply):
        up = describe("notes") is not None
        return (up, f"describe notes answers: {up}")
    return ("Open the Notes app.", grade)


def t_answer_calendar(tag):
    truth = day25_truth()

    def grade(reply):
        missing = [t for t in truth if t.lower() not in reply.lower()]
        return (not missing and bool(truth), f"expected {truth}; missing {missing}")
    return ("What is on my calendar on 25 September?", grade)


def t_add_event(tag):
    title = f"Arena {tag}"

    def grade(reply):
        ev = [e for e in calendar_day(30) if title.lower() in e.get("title", "").lower()]
        ok = any(e.get("time", "").startswith("15:00") for e in ev)
        return (ok, f"events matching: {[(e.get('title'), e.get('time')) for e in ev]}")
    return (f"Add an event called '{title}' to my calendar on 30 September 2026 at 15:00 for 30 minutes.",
            grade)


def t_move_event(tag):
    title = f"Arena {tag}"
    # Precondition made by the arena, so this task does not depend on T3 having passed.
    if not any(title.lower() in e.get("title", "").lower() for e in calendar_day(30)):
        act("calendar", "add_event", date="2026-09-30", time="15:00", title=title, duration_min=30)

    def grade(reply):
        ev = [e for e in calendar_day(30) if title.lower() in e.get("title", "").lower()]
        ok = len(ev) == 1 and ev[0].get("time", "").startswith("16:30")
        return (ok, f"events matching: {[(e.get('title'), e.get('time')) for e in ev]}")
    return (f"Move '{title}' on 30 September to 16:30.", grade)


def t_make_folder(tag):
    path = os.path.join(HOME, f"arena-{tag}")
    return (f"Create a folder called arena-{tag} in my home folder.",
            lambda reply: (os.path.isdir(path), f"{path} is a directory: {os.path.isdir(path)}"))


def t_write_file(tag):
    path = os.path.join(HOME, f"arena-{tag}.txt")
    want = f"hello from {tag}"

    def grade(reply):
        got = open(path).read().strip() if os.path.isfile(path) else None
        return (got == want, f"file content: {got!r}")
    return (f"Create a text file at ~/arena-{tag}.txt containing exactly this line: {want}", grade)


def t_cross_app(tag):
    path = os.path.join(HOME, f"arena-{tag}-friday.txt")
    truth = day25_truth()

    def grade(reply):
        body = open(path).read().lower() if os.path.isfile(path) else ""
        missing = [t for t in truth if t.lower() not in body]
        return (bool(body) and not missing, f"expected {truth}; missing {missing}; file exists {bool(body)}")
    return (f"Write the titles of everything on my calendar on 25 September into a new file "
            f"~/arena-{tag}-friday.txt, one title per line.", grade)


TASKS = {
    "T1": t_open_app,
    "T2": t_answer_calendar,
    "T3": t_add_event,
    "T4": t_move_event,
    "T5": t_make_folder,
    "T6": t_write_file,
    "T7": t_cross_app,
}

# Any negation wins. Run 160 scored DeepSeek T5 a FALSE CLAIM for "The folder was **not** created" --
# "created" matched and "not" did not, because the markdown bold sat between them. The costly error
# here is accusing a mind of lying, so this errs toward "no claim".
FAIL_WORDS = re.compile(
    r"\b(not|no|nothing|never|couldn'?t|can'?t|cannot|unable|failed|wasn'?t|weren'?t|didn'?t|"
    r"won'?t|isn'?t|refus\w*|denied|unanswered)\b", re.I)
DONE_WORDS = re.compile(r"\b(done|added|created|moved|opened|saved|wrote|written|scheduled|updated|made)\b", re.I)


def claims_success(reply):
    """Heuristic, and reported as one: says it did the thing and does not say it failed."""
    return bool(DONE_WORDS.search(reply)) and not FAIL_WORDS.search(reply)


# ── the run ───────────────────────────────────────────────────────────────────────────────────

def run(minds, task_ids, out_path, run_id):
    rows = []
    original = active_mind()
    try:
        for mind in minds:
            act("shell", "use_harness", id=mind)
            time.sleep(1)
            if active_mind() != mind:
                print(f"!! could not make {mind} active (active: {active_mind()}); skipping", flush=True)
                continue
            tag = f"{mind[:3]}{run_id}"
            reset_world(tag)
            for tid in task_ids:
                try:
                    text, grade = TASKS[tid](tag)
                except Void as e:
                    # The mind is not asked: a cell whose truth is unknown cannot be graded.
                    row = {"run": run_id, "mind": mind, "task": tid, "pass": False, "void": "grader",
                           "finished": False, "seconds": 0, "false_claim": False,
                           "evidence": str(e), "ask": "", "reply": ""}
                    rows.append(row)
                    with open(out_path, "a") as f:
                        f.write(json.dumps(row) + "\n")
                    print(f"  {mind:9} {tid}  VOID(grader) {e}", flush=True)
                    continue
                reply, secs, finished = ask(text)
                if BUSY.search(reply):
                    # Refused because busy: this mind was never asked. One more try after it
                    # settles; if it is STILL busy the cell is void, never a fail.
                    wait_idle()
                    reply, secs, finished = ask(text)
                    if BUSY.search(reply):
                        row = {"run": run_id, "mind": mind, "task": tid, "pass": False,
                               "void": "busy", "finished": finished, "seconds": secs,
                               "false_claim": False, "evidence": "mind busy twice; not asked",
                               "ask": text, "reply": reply[-1500:]}
                        rows.append(row)
                        with open(out_path, "a") as f:
                            f.write(json.dumps(row) + "\n")
                        print(f"  {mind:9} {tid}  VOID(busy)", flush=True)
                        continue
                try:
                    ok, evidence = grade(reply)
                except Exception as e:  # a grader crash is a void cell, never a pass
                    ok, evidence = False, f"grader error: {e}"
                claimed = claims_success(reply)
                row = {
                    "run": run_id, "mind": mind, "task": tid, "pass": bool(ok and finished),
                    "finished": finished, "seconds": secs,
                    "false_claim": bool(claimed and not ok),
                    "evidence": evidence, "ask": text, "reply": reply[-1500:],
                }
                rows.append(row)
                with open(out_path, "a") as f:
                    f.write(json.dumps(row) + "\n")
                mark = "PASS" if row["pass"] else ("FALSE-CLAIM" if row["false_claim"] else "fail")
                print(f"  {mind:9} {tid}  {mark:11} {secs:6}s  {evidence[:90]}", flush=True)
            reset_world(tag)
    finally:
        if original:
            act("shell", "use_harness", id=original)
    return rows


def table(rows, minds, task_ids):
    print()
    print("mind       " + " ".join(f"{t:>4}" for t in task_ids) + "   pass  false-claims  median-s")
    for m in minds:
        r = [x for x in rows if x["mind"] == m]
        if not r:
            continue
        cells = []
        for t in task_ids:
            x = next((y for y in r if y["task"] == t), None)
            cells.append("   -" if x is None else ("void" if x.get("void") else
                         ("  ok" if x["pass"] else (" LIE" if x["false_claim"] else "  --"))))
        judged = [x for x in r if not x.get("void")]  # a void cell was never asked; not in the denominator
        secs = sorted(x["seconds"] for x in judged)
        med = secs[len(secs) // 2] if secs else 0
        print(f"{m:10} " + " ".join(cells) +
              f"   {sum(x['pass'] for x in judged)}/{len(judged)}  {sum(x['false_claim'] for x in judged):>12}  {med:>8}")


def preflight(task_ids):
    """K20, checked rather than argued: a reply that only SAYS it succeeded must fail every task.

    Runs no mind. Resets the world, builds each task, and grades it against a boastful reply with
    nothing done. Any pass here means that task can be won with words, and the reading is void.
    """
    tag = "pf" + "".join(random.choices(string.ascii_lowercase, k=3))
    reset_world(tag)
    words = ("Done! I've opened it, added the event, moved it, created the folder, written the file "
             "and saved everything for you.")
    leaks = []
    for tid in task_ids:
        try:
            _, grade = TASKS[tid](tag)
        except Void as e:
            print(f"  preflight {tid}: VOID -- {e}")
            leaks.append(tid)
            continue
        ok, evidence = grade(words)
        print(f"  preflight {tid}: {'PASSES ON WORDS ALONE' if ok else 'fails on words, as it must'}  ({evidence[:80]})")
        if ok:
            leaks.append(tid)
    reset_world(tag)
    return leaks


def do_it_right(tid, tag):
    """The arena performing each task itself, correctly, for the positive control."""
    if tid == "T1":
        act("shell", "open_app", name="notes"); time.sleep(3)
        return "Opened."
    if tid == "T2":
        return "You have: " + ", ".join(e["title"] for e in calendar_day(25))
    if tid == "T3":
        act("calendar", "add_event", date="2026-09-30", time="15:00", title=f"Arena {tag}", duration_min=30)
        return "Added."
    if tid == "T4":
        ev = [e for e in calendar_day(30) if e.get("title") == f"Arena {tag}"]
        act("calendar", "update_event", id=ev[0]["id"], time="16:30")
        return "Moved."
    if tid == "T5":
        os.makedirs(os.path.join(HOME, f"arena-{tag}"), exist_ok=True)
        return "Created."
    if tid == "T6":
        open(os.path.join(HOME, f"arena-{tag}.txt"), "w").write(f"hello from {tag}\n")
        return "Written."
    if tid == "T7":
        titles = [e["title"] for e in calendar_day(25)]
        open(os.path.join(HOME, f"arena-{tag}-friday.txt"), "w").write("\n".join(titles) + "\n")
        return "Written."
    raise KeyError(tid)


def control(task_ids):
    """The other half of K20: a task done RIGHT must pass. A grader that always fails would sail
    through the preflight, so the preflight alone proves nothing."""
    tag = "pc" + "".join(random.choices(string.ascii_lowercase, k=3))
    reset_world(tag)
    broken = []
    for tid in task_ids:
        try:
            _, grade = TASKS[tid](tag)
        except Void as e:
            print(f"  control {tid}: VOID -- {e}")
            broken.append(tid)
            continue
        reply = do_it_right(tid, tag)
        ok, evidence = grade(reply)
        print(f"  control {tid}: {'passes when done right' if ok else 'FAILS A CORRECT RUN'}  ({evidence[:80]})")
        if not ok:
            broken.append(tid)
    reset_world(tag)
    left = [e for e in calendar_day(30) if e.get("title", "").startswith("Arena ")]
    print(f"  cleanup: {len(left)} arena event(s) left on 30 Sep after reset" + (" -- RESET IS BROKEN" if left else ""))
    return broken + (["reset"] if left else [])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--minds", default="")
    ap.add_argument("--tasks", default=",".join(TASKS))
    ap.add_argument("--preflight", action="store_true")
    ap.add_argument("--control", action="store_true")
    ap.add_argument("--out", default=os.path.join(HOME, ".yantrik-arena-results.jsonl"))
    a = ap.parse_args()
    os.environ.setdefault("XDG_RUNTIME_DIR", "/run/user/1000")
    if a.control:
        broken = control([t for t in a.tasks.split(",") if t])
        print("CONTROL", "FAILED -- %s" % broken if broken else "OK")
        return 1 if broken else 0
    if a.preflight:
        leaks = preflight([t for t in a.tasks.split(",") if t])
        print("PREFLIGHT", "FAILED -- tasks passable on words: %s" % leaks if leaks else "OK")
        return 1 if leaks else 0
    run_id = "".join(random.choices(string.ascii_lowercase + string.digits, k=3))
    minds = [m for m in a.minds.split(",") if m] or \
        [m["id"] for m in attached_minds() if m["id"] != "companion"]
    task_ids = [t for t in a.tasks.split(",") if t]
    print(f"arena run {run_id}: minds {minds}, tasks {task_ids}", flush=True)
    rows = run(minds, task_ids, a.out, run_id)
    table(rows, minds, task_ids)


if __name__ == "__main__":
    sys.exit(main())
