#!/usr/bin/env python3
"""Static wiring check for the desktop cockpit.

The client is vanilla JS with no build step, which is a deliberate constraint — the whole thing is a
few files you can read. The cost is that nothing catches a typo'd element id or an icon that does not
exist: those fail silently at runtime, usually as a panel that renders blank. This is the substitute
for a compiler.

Exits non-zero on any problem, so `verify.sh` can read the exit code rather than grep for words.
"""
import re
import sys
from pathlib import Path

desk = Path(sys.argv[1] if len(sys.argv) > 1 else "../yantrik-mind-desktop")
html = (desk / "dist/index.html").read_text(encoding="utf-8")
js = (desk / "dist/app.js").read_text(encoding="utf-8")
# render.js emits its own classes and must be held to the same rule.
render = (desk / "dist/render.js").read_text(encoding="utf-8")
css = (desk / "dist/styles.css").read_text(encoding="utf-8")

problems = []

# Element ids the JS reaches for must exist in the HTML. These two are built at render time by the
# code that then reads them, so they are legitimately absent from the static markup.
CREATED_AT_RUNTIME = {"funnel-box", "diag-reports"}
ids = set(re.findall(r'\bid="([^"]+)"', html))
used = set(re.findall(r'\$\("([^"]+)"\)', js))
missing_ids = sorted(used - ids - CREATED_AT_RUNTIME)
if missing_ids:
    problems.append(f"JS reads element ids that do not exist: {missing_ids}")

# Every icon referenced must be in the sprite, or it renders as empty space.
symbols = set(re.findall(r'<symbol id="(ic-[^"]+)"', html))
refs = set(re.findall(r'href="#(ic-[^"]+)"', html))
refs |= {"ic-" + m for m in re.findall(r'icon\("([a-z-]+)"', js)}
missing_icons = sorted(refs - symbols)
if missing_icons:
    problems.append(f"icons referenced but not in the sprite: {missing_icons}")

# Nav and views must correspond, both directions: a nav button with no view does nothing when
# clicked, and a view with no nav entry is unreachable.
views = set(re.findall(r'id="view-([a-z-]+)"', html))
navs = set(re.findall(r'data-view="([a-z-]+)"', html))
if navs - views:
    problems.append(f"nav entries with no view: {sorted(navs - views)}")
if views - navs:
    problems.append(f"views with no nav entry (unreachable): {sorted(views - navs)}")

# Every view needs a loader, or it opens empty. Chat and Console populate on interaction instead.
POPULATED_BY_INTERACTION = {"chat", "console"}
loaders = set(re.findall(r"^  ([a-z]+): ", js, re.M))
no_loader = sorted(views - loaders - POPULATED_BY_INTERACTION)
if no_loader:
    problems.append(f"views with no loader (would open blank): {no_loader}")

# A class the JS applies but the stylesheet never defines is invisible styling — the row renders, just
# wrong, which is harder to notice than a crash.
applied = set()
for m in re.finditer(r'class="([^"{}]+)"', js + render):
    applied.update(c for c in m.group(1).split() if c and not c.startswith("$"))
declared = set(re.findall(r"\.([a-zA-Z][\w-]*)", css))
# SELECTOR HOOKS carry no styling on purpose — they exist so JS can find an element after a re-render.
# They must be listed here rather than given an empty CSS rule: a no-op rule in the stylesheet is a
# lie about what the class is for, and the next person deletes it as dead.
SELECTOR_HOOKS = {"cap-state"}
undeclared = sorted(c for c in applied - declared - SELECTOR_HOOKS if "-" in c or len(c) > 4)
if undeclared:
    problems.append(f"classes used in JS but absent from the stylesheet: {undeclared}")

# Every script the page loads must exist, and every script that exists must be loaded — an orphaned
# file is dead weight and a missing one is a blank app.
loaded = set(re.findall(r'<script src="([^"]+)"', html))
on_disk = {f.name for f in (desk / "dist").glob("*.js")}
if loaded - on_disk:
    problems.append(f"index.html loads scripts that do not exist: {sorted(loaded - on_disk)}")
if on_disk - loaded:
    problems.append(f"scripts on disk that index.html never loads: {sorted(on_disk - loaded)}")

# render.js must be loaded BEFORE app.js, which calls into it at module scope.
order = re.findall(r'<script src="([^"]+)"', html)
if "render.js" in order and "app.js" in order and order.index("render.js") > order.index("app.js"):
    problems.append("render.js is loaded after app.js — RENDER would be undefined when app.js runs")

# The typed-surface handshake: the client must not ask for a surface the server does not advertise.
server = Path(__file__).resolve().parent.parent / "crates/mind-conversation/src/surface.rs"
if server.exists():
    src = server.read_text(encoding="utf-8")
    m = re.search(r"pub const TYPED_VERBS: &\[&str\] =\s*&\[([^\]]*)\]", src, re.S)
    if m:
        advertised = set(re.findall(r'"([a-z_]+)"', m.group(1)))
        asked = set(re.findall(r'surface\("([a-z_]+)"\)', js))
        unknown = sorted(asked - advertised)
        if unknown:
            problems.append(f"client asks for surfaces the server does not serve: {unknown}")

if problems:
    for p in problems:
        print(f"  - {p}")
    sys.exit(1)
print("desktop wiring consistent")
