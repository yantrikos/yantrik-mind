"""Self-test fixture (NEGATIVE): the tracker with a wrong `done` message — only that line and the
tests must fail."""
import json, sys, datetime

STORE = "tasks.json"


def load():
    try:
        return json.load(open(STORE, encoding="utf-8"))
    except Exception:
        return []


def save(tasks):
    json.dump(tasks, open(STORE, "w", encoding="utf-8"), indent=1)


def line(t):
    return f"#{t['id']} [{'x' if t['done'] else ' '}] {t['text']}"


def main(argv):
    cmd = argv[1] if len(argv) > 1 else "list"
    tasks = load()
    if cmd == "add":
        t = {"id": (max(x["id"] for x in tasks) + 1) if tasks else 1, "text": " ".join(argv[2:]), "done": False,
             "added": datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")}
        tasks.append(t)
        save(tasks)
        print(f"added #{t['id']}")
    elif cmd == "list":
        for t in sorted(tasks, key=lambda x: x["id"]):
            print(line(t))
    elif cmd == "done":
        i = int(argv[2])
        for t in tasks:
            if t["id"] == i:
                t["done"] = True
        save(tasks)
        print(f"completed #{i}")
    elif cmd == "today":
        today = datetime.datetime.now(datetime.timezone.utc).strftime("%Y-%m-%d")
        for t in sorted(tasks, key=lambda x: x["id"]):
            if not t["done"] and t["added"] == today:
                print(line(t))


if __name__ == "__main__":
    main(sys.argv)
