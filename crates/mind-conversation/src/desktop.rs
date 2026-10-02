//! E.ARENA1-F1: on a Yantrik OS machine, the calendar is the desktop's.
//!
//! The harness arena (2026-09-22, VM 520) put the same seven desktop tasks to every mind attached to
//! the OS. Three of this mind's losses had one cause: asked about "my calendar", it reached for its
//! OWN calendar tools instead of the desktop's. Asked what was on 25 September it read its private
//! store, found nothing, and told the owner "there is nothing on the calendar for the next 14 days"
//! while the desktop calendar held three events that day. Asked to add an event it wrote to the
//! private store — on the wrong date — and replied "Added", which the desktop did not bear out.
//! Hermes, holding only the desktop's tools, passed all three.
//!
//! Two calendars on one machine is the defect. When the desktop's own tools are connected, its
//! calendar is the person's calendar, whichever channel the question arrives on: the private event
//! tools come off the agent's menu, and a call made to one by name is answered with where the
//! calendar actually is rather than being run against a store the person cannot see.
//!
//! `forget_date` is deliberately NOT superseded: it removes a dated entry from a PERSON's profile (a
//! birthday, an open house), which the desktop calendar does not model.

use mind_tools::McpTool;

/// The MCP server name a Yantrik OS desktop is attached under (`/opt/yantrik/bin/yos-mcp`).
pub(crate) const DESKTOP_SERVER: &str = "yantrik-os";

/// The private calendar-EVENT tools, and every alias the dispatcher accepts for them.
pub(crate) const SUPERSEDED_BY_DESKTOP: [&str; 6] = [
    "calendar",
    "calendar_view",
    "calendar_add",
    "add_event",
    "calendar_remove",
    "remove_event",
];

/// The plugin whose menu entry changes when the desktop owns the calendar.
pub(crate) const CALENDAR_PLUGIN: &str = "calendar_tools";

/// What the menu says for that plugin while the desktop is attached. The desktop's calendar is
/// reached through its own tools; only the person-profile tool stays here.
pub(crate) const CALENDAR_ON_DESKTOP: &str = "- CALENDAR: this computer's calendar is the DESKTOP calendar app. Read it with mcp.yantrik-os.os_describe {app: calendar} (select a day with os_act calendar select_day first); change it with mcp.yantrik-os.os_act calendar add_event / update_event / delete_event. There is no other calendar on this machine.\n\
- forget_date {name, label}: remove one dated entry (e.g. open house) from a person's profile — not a calendar event";

/// Is a Yantrik OS desktop connected, with its tools on offer?
pub(crate) fn desktop_attached(tools: &[McpTool]) -> bool {
    tools.iter().any(|t| t.server == DESKTOP_SERVER)
}

/// If `tool` is a private calendar tool and the desktop owns the calendar, what to answer instead of
/// running it. `None` means run the tool as usual.
pub(crate) fn superseded(tool: &str, desktop: bool) -> Option<String> {
    if !desktop || !SUPERSEDED_BY_DESKTOP.contains(&tool) {
        return None;
    }
    Some(format!(
        "(`{tool}` is this mind's private calendar, and it is not used on this computer: the \
         person's calendar is the desktop calendar app. Nothing was read or changed. Use \
         mcp.yantrik-os.os_describe {{app: calendar}} to read it — select_day first for a given \
         date — and mcp.yantrik-os.os_act calendar add_event / update_event / delete_event to \
         change it.)"
    ))
}

/// How much of an app's STATE a desktop description keeps in the work log.
pub(crate) const DESKTOP_STATE_HEAD: usize = 900;
/// How much room the ACTION list gets, descriptions included; signatures are never dropped.
pub(crate) const DESKTOP_ACTIONS_BUDGET: usize = 6000;

/// E.ARENA1-F3: a desktop description, condensed so its ACTIONS survive the work log.
///
/// Every successful tool result entered the agent's work log clipped to 900 characters. A desktop
/// app's description is its state (JSON) followed by the list of actions it offers, so the clip always
/// landed in the state: the calendar's `update_event` sat past character 900, and in the shell's
/// 22.6 KB description the first action line starts at character 11,847. Captured through a proxy on
/// the live nightly, on the same model Hermes passes with: the model described the calendar, saw no
/// way to edit it, described it again, and again, then answered "I don't have a tool to edit calendar
/// events". Its repeats were not a habit; it was asking for the part the harness kept cutting off.
///
/// So: the head of the state, then EVERY action signature, with each action's description and
/// argument lines while `DESKTOP_ACTIONS_BUDGET` lasts. `None` when `obs` is not a description with
/// actions, so every other tool keeps the old clip.
tokio::task_local! {
    /// E.ARENA1-F31: the names this turn's request quotes, so condensing a description keeps them.
    pub(crate) static FOCUS: Vec<String>;
}

/// E.ARENA1-F31: names quoted in a request -- 'x', "x", and the curly forms -- 2 to 80 characters.
pub(crate) fn quoted_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (open, close) in [('\'', '\''), ('"', '"'), ('\u{2018}', '\u{2019}'), ('\u{201c}', '\u{201d}')] {
        let mut rest = text;
        while let Some(i) = rest.find(open) {
            let after = &rest[i + open.len_utf8()..];
            let Some(j) = after.find(close) else { break };
            let name = after[..j].trim();
            let n = name.chars().count();
            // An apostrophe inside a word ("don't") is not an opening quote.
            let starts_word = i == 0 || !rest[..i].chars().last().is_some_and(char::is_alphanumeric);
            if starts_word && (2..=80).contains(&n) && !out.iter().any(|o| o == name) {
                out.push(name.to_string());
            }
            rest = &after[j + close.len_utf8()..];
        }
    }
    out
}

/// E.ARENA1-F31: the smallest `{…}` object in `state` that contains `at`, if it is at most `max`.
fn enclosing_object(state: &str, at: usize, max: usize) -> Option<&str> {
    let bytes = state.as_bytes();
    let mut depth = 0i32;
    let mut start = None;
    for j in (0..at).rev() {
        match bytes[j] {
            b'}' => depth += 1,
            b'{' if depth == 0 => {
                start = Some(j);
                break;
            }
            b'{' => depth -= 1,
            _ => {}
        }
    }
    let start = start?;
    let (mut depth, mut in_str, mut esc) = (0i32, false, false);
    for (k, c) in state[start..].char_indices() {
        if in_str {
            match (esc, c) {
                (true, _) => esc = false,
                (false, '\\') => esc = true,
                (false, '"') => in_str = false,
                _ => {}
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    let obj = &state[start..start + k + 1];
                    return (obj.chars().count() <= max).then_some(obj);
                }
            }
            _ => {}
        }
    }
    None
}

/// E.ARENA1-F31: the state objects naming each focused name that the head did not show.
fn focused_objects(state: &str, head: &str, names: &[String]) -> Vec<String> {
    const PER_NAME: usize = 3;
    const MAX_OBJECT: usize = 600;
    let mut kept: Vec<String> = Vec::new();
    for name in names {
        let mut found = 0;
        let mut from = 0;
        while let Some(i) = state[from..].find(name.as_str()) {
            let at = from + i;
            from = at + name.len();
            if let Some(obj) = enclosing_object(state, at, MAX_OBJECT) {
                if !head.contains(obj) && !kept.iter().any(|k| k == obj) {
                    kept.push(obj.to_string());
                    found += 1;
                }
            }
            if found >= PER_NAME {
                break;
            }
        }
    }
    kept
}

pub(crate) fn condense_description(obs: &str) -> Option<String> {
    // Idempotent: the MCP boundary condenses first and the work log condenses again, and a second
    // pass must not trim a state that is already trimmed.
    if obs.contains(CONDENSED_MARK) {
        return Some(obs.to_string());
    }
    let lines: Vec<&str> = obs.lines().collect();
    let first_act = lines.iter().position(|l| l.starts_with("  act: "))?;
    let state = lines[..first_act].join("\n");
    let mut out: String = state.chars().take(DESKTOP_STATE_HEAD).collect();
    if state.chars().count() > DESKTOP_STATE_HEAD {
        out.push_str("\n… (state trimmed)");
        // Small fields worth more than their place in the alphabet: kept whole even when the
        // head cut them off.
        for key in ALWAYS_KEPT {
            let marker = format!("\"{key}\": ");
            if !out.contains(&marker) {
                if let Some(field) = kept_field(&state, key) {
                    out.push('\n');
                    out.push_str(&field);
                }
            }
        }
        // E.ARENA1-F31: what the request names, where the head cut it off. 557e4ef's T4: 39 events
        // on the day, and 'Arena minr1f' with its id was past the cut, so it could not be moved.
        let names = FOCUS.try_with(|f| f.clone()).unwrap_or_default();
        for obj in focused_objects(&state, &out, &names) {
            out.push_str("\n… kept because the request names it: ");
            out.push_str(&obj);
        }
    }
    out.push_str(CONDENSED_MARK);

    // Signatures first, all of them: they are what the model must be able to call.
    let signatures: Vec<&str> = lines[first_act..]
        .iter()
        .copied()
        .filter(|l| l.starts_with("  act: "))
        .collect();
    let sig_len: usize = signatures.iter().map(|l| l.len() + 1).sum();
    let mut room = DESKTOP_ACTIONS_BUDGET.saturating_sub(sig_len);

    // Then each action WITH its detail lines while room lasts; after that, signature only.
    let mut trimmed = false;
    let mut i = first_act;
    while i < lines.len() {
        let line = lines[i];
        if !line.starts_with("  act: ") {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < lines.len() && !lines[j].starts_with("  act: ") && lines[j].starts_with("   ") {
            j += 1;
        }
        let detail: String = lines[i + 1..j].iter().map(|d| format!("\n{d}")).collect();
        out.push('\n');
        out.push_str(line);
        if detail.len() <= room {
            out.push_str(&detail);
            room -= detail.len();
        } else if !detail.is_empty() {
            trimmed = true;
        }
        i = j;
    }
    if trimmed {
        out.push_str("\n(some action descriptions trimmed; every action above can still be called)");
    }
    Some(out)
}

/// E.ARENA1-F5: what `discover_tools` adds while the desktop is attached.
///
/// Reading B'' (same model as Hermes, the first four fixes in) failed the folder, file and
/// calendar-to-file tasks the same way: the mind searched `discover_tools` for "create folder" /
/// "write file", was told "no tool or saved skill matches — use build_capability to create one",
/// and concluded it could not. That search covers only the mind's OWN tools. The desktop's
/// `files_new_folder` and `editor_save_as` are listed by `os_describe`, and nothing said so.
pub(crate) const DISCOVER_DESKTOP_NOTE: &str = "\nON THIS COMPUTER the desktop's own apps do most things, and this search does not cover them: files and folders are the `shell` app (files_*), writing a file's text is the `editor` app, and the calendar and notes are apps too. mcp.yantrik-os.os_describe {app} lists each app's actions; call them with mcp.yantrik-os.os_act.";

/// E.ARENA1-F5: the nudge for a turn that has not looked at the desktop at all and wants to answer.
///
/// The old nudge ended "Do that now with one tool call, or say plainly that you cannot." With the
/// mind's own earlier replies in the conversation reading "I have no tool that writes files" — true
/// only because a since-fixed clip hid the actions — the model took the exit it was offered, citing
/// "same wall as arena-minnop.txt". Its earlier words are not evidence of what this computer can do.
/// Before it may say it cannot, it looks.
pub(crate) fn look_first_nudge(step: usize, clause: &str) -> String {
    format!(
        "\n[{step}] (you have not yet {clause}, and you have not looked at what this computer can do \
         this turn. What you said in EARLIER turns is not evidence of that — the desktop's apps are. \
         Call mcp.yantrik-os.os_describe on the app that would do it — `shell` for files and folders, \
         `editor` for a file's text — and then do it with mcp.yantrik-os.os_act. Only if that app \
         lists no action for it may you say you cannot.)"
    )
}

/// The one call that shows what an app can DO: its actions.
pub(crate) const DESCRIBE: &str = "mcp.yantrik-os.os_describe";

/// Should the unfinished-request nudge send this turn to look at the desktop first?
///
/// "Looked" means described an app. Reading B‴ refused the file task after only `os_apps`, which
/// lists what is open and not what anything can do — and any desktop call used to count as looking.
pub(crate) fn must_look_first(desktop: bool, tools_called: &[&str]) -> bool {
    desktop && !tools_called.iter().any(|t| *t == DESCRIBE)
}

/// E.ARENA1-F6: does this call change the desktop, so that what was read before it is no longer
/// true?
pub(crate) fn changes_the_desktop(tool: &str) -> bool {
    // E.ARENA1-F41: every browser tool that is not a pure read changes what a read would see.
    tool == "mcp.yantrik-os.os_act" || (tool.starts_with(WEB_PREFIX) && !PURE_WEB_READS.contains(&tool))
}

/// E.ARENA1-F41 (yantrik-os #480): the browser's tools on the desktop server.
const WEB_PREFIX: &str = "mcp.yantrik-os.web_";

/// E.ARENA1-F41: browser tools that only read -- stale after any change, like F6's reads.
const PURE_WEB_READS: [&str; 5] = [
    "mcp.yantrik-os.web_read",
    "mcp.yantrik-os.web_text",
    "mcp.yantrik-os.web_find",
    "mcp.yantrik-os.web_tabs",
    "mcp.yantrik-os.web_listen",
];

/// E.ARENA1-F41: browser tools meant to be repeated -- "scroll, read, scroll" pages a site. The
/// repeat guard does not remember them; only an immediate identical repeat is still nudged.
const REPEATABLE_WEB: [&str; 4] = [
    "mcp.yantrik-os.web_scroll",
    "mcp.yantrik-os.web_wait",
    "mcp.yantrik-os.web_press",
    "mcp.yantrik-os.web_back",
];

/// E.ARENA1-F42: after a web change, forget every earlier web call -- a URL or an element ref means
/// something new on the next page (520, 4128222: "go back to the front page" was refused as a repeat
/// after the browser had moved to a story). The immediate identical repeat is still the loop's to nudge.
pub(crate) fn forget_web_calls(done: &mut std::collections::HashSet<String>) {
    done.retain(|sig| !sig.starts_with(WEB_PREFIX));
}

/// E.ARENA1-F41: may this call run again later in the turn, however often it already ran?
pub(crate) fn repeatable(tool: &str) -> bool {
    REPEATABLE_WEB.contains(&tool)
}

/// E.ARENA1-F6b: may a desktop action that just FAILED be tried again later this turn?
///
/// F6 kept actions deduplicated so a repeated `new` could not open a second tab. Right for an action
/// that worked; wrong for one that did not. Reading B5: `new` was refused ("Eight tabs are already
/// open"), the mind closed a tab — exactly the right move — tried `new` again, and the loop answered
/// "already called with these args", handing back the refusal. A failed action is not remembered as
/// done; an immediate identical retry, with nothing changed in between, still meets the loop's
/// ordinary repeat nudge.
pub(crate) fn retry_after_failure(tool: &str, succeeded: bool) -> bool {
    changes_the_desktop(tool) && !succeeded
}

/// E.ARENA1-F22: the desktop says the call did not happen. yos-mcp's refusals all begin
/// `REFUSED — nothing was run.`; the mind's MCP wrapper puts "Done — " in front, and the outcome
/// classifier scored that Ok -- so F6b never un-recorded it. R1 B0, T6: `editor.new` came back
/// "not run" because the editor was not open, the mind opened it, and its own done-guard refused
/// the retry twice. The file was never made.
pub(crate) fn nothing_was_run(obs: &str) -> bool {
    obs.contains("REFUSED \u{2014} nothing was run")
}

/// Desktop READS — the calls whose answer depends on the desktop's current state.
const DESKTOP_READS: [&str; 4] = [
    "mcp.yantrik-os.os_describe",
    "mcp.yantrik-os.os_apps",
    "mcp.yantrik-os.os_perception",
    // E.ARENA1-F38 (yantrik-os #478): the whole screen as text -- stale after an act like the rest.
    "mcp.yantrik-os.os_screen",
];

/// E.ARENA1-F6: forget earlier desktop reads, because an action just changed what they describe.
///
/// The loop refuses a call identical to an earlier one and hands back the logged result instead —
/// right for a read of something that does not change, wrong on a desktop. Reading B‴: the mind
/// described `files`, was told "files is not running", opened it, described it again, and was handed
/// "not running" from the log; the same with the editor after it had just opened a new tab. Actions
/// stay remembered — repeating the same `new` must not open a second tab — only reads are forgotten.
/// `done` holds the loop's `tool|args` call signatures.
pub(crate) fn forget_desktop_reads(done: &mut std::collections::HashSet<String>) {
    done.retain(|sig| {
        !DESKTOP_READS.iter().chain(PURE_WEB_READS.iter()).any(|r| sig.starts_with(&format!("{r}|")))
            && !read_act_sig(sig)
    });
}

/// E.ARENA1-F40: an app name as yos-mcp's `surface_name` folds it (yantrik-os #479) -- trimmed,
/// lower-cased, runs of spaces or underscores to `-`, a leading `app-` dropped. `yos` resolves every
/// spelling to the same surface, so every rule here must compare this one.
pub(crate) fn fold_app(name: &str) -> String {
    let lower = name.trim().to_lowercase();
    let mut out = String::with_capacity(lower.len());
    let mut in_run = false;
    for c in lower.chars() {
        if c.is_whitespace() || c == '_' {
            if !in_run {
                out.push('-');
            }
            in_run = true;
        } else {
            out.push(c);
            in_run = false;
        }
    }
    let out = out.strip_prefix("app-").map(str::to_string).unwrap_or(out);
    // E.ARENA1-F41: yos-mcp's alias for the browser app (yantrik-os #480).
    if out == "chromium" {
        return "browser".to_string();
    }
    out
}

/// E.ARENA1-F40: a desktop call with its `app` folded, so the Mind's rules and the desktop agree
/// on the name. Other tools' arguments are returned as they were.
pub(crate) fn fold_desktop_args(tool: &str, args: serde_json::Value) -> serde_json::Value {
    if tool != ACT && tool != DESCRIBE {
        return args;
    }
    let mut args = args;
    if let Some(app) = args.get("app").and_then(|a| a.as_str()).map(fold_app) {
        args["app"] = serde_json::Value::String(app);
    }
    args
}

/// E.ARENA1-F39: shell actions that READ the screen (OCR of windows with no element tree) but
/// arrive as an act.
const READ_ACTS: [&str; 2] = ["read_screen", "read_mind_view"];

/// E.ARENA1-F39: an act that only reads -- not a change, and stale after a real one.
pub(crate) fn is_read_act(tool: &str, args: &serde_json::Value) -> bool {
    act_target(tool, args).is_some_and(|(app, action)| {
        (app == TWIN_HOST && READ_ACTS.contains(&action.as_str()))
            // E.ARENA1-F41 (yantrik-os #480): the browser app's pure reads.
            || (app == "browser" && BROWSER_READ_ACTS.contains(&action.as_str()))
    })
}

/// E.ARENA1-F41: `os_act browser <action>` that only reads.
const BROWSER_READ_ACTS: [&str; 5] = ["read", "find", "text", "tabs", "media"];

fn read_act_sig(sig: &str) -> bool {
    sig.split_once('|')
        .filter(|(tool, _)| *tool == ACT)
        .and_then(|(tool, args)| serde_json::from_str::<serde_json::Value>(args).ok().map(|a| is_read_act(tool, &a)))
        .unwrap_or(false)
}

/// The call that changes the desktop.
pub(crate) const ACT: &str = "mcp.yantrik-os.os_act";

/// E.ARENA1-F25: the desktop's app catalogue, and how much of it the work log keeps. It is ~2.5 KB
/// on 185b4c0 and the work log cut it at 900: the model saw five openable apps and told a person
/// the image viewer did not exist.
pub(crate) const APPS: &str = "mcp.yantrik-os.os_apps";
const APPS_BUDGET: usize = 4000;

/// One action an app lists: its name, its permission grade, and its signature as the app wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Listed {
    pub(crate) name: String,
    pub(crate) grade: String,
    pub(crate) sig: String,
}

/// What one app lists.
pub(crate) type ActionList = Vec<Listed>;

/// Every `act: name(args)  [grade, …]` line in a description.
pub(crate) fn listed_actions(obs: &str) -> ActionList {
    obs.lines()
        .filter_map(|l| l.strip_prefix("  act: "))
        .filter_map(|rest| {
            let name = rest.split('(').next()?.trim();
            let grade = rest.split('[').nth(1)?.split([',', ']']).next()?.trim();
            let sig = rest.split('[').next()?.trim();
            (!name.is_empty()).then(|| Listed {
                name: name.to_string(),
                grade: grade.to_string(),
                sig: sig.to_string(),
            })
        })
        .collect()
}

/// The grade `app` listed for `action` this turn, if `app` was described.
fn grade_of<'a>(
    described: &'a std::collections::HashMap<String, ActionList>,
    app: &str,
    action: &str,
) -> Option<&'a str> {
    described.get(app)?.iter().find(|l| l.name == action).map(|l| l.grade.as_str())
}

/// Does this grade put a card in front of the person before the action runs?
fn asks_first(grade: &str) -> bool {
    grade != "standard" && grade != "safe"
}

/// The `app` and `action` of a desktop action call.
pub(crate) fn act_target(tool: &str, args: &serde_json::Value) -> Option<(String, String)> {
    if tool != ACT {
        return None;
    }
    let app = args.get("app")?.as_str()?;
    let action = args.get("action")?.as_str()?;
    Some((app.to_string(), action.to_string()))
}

/// Remember what an `os_describe` call showed, keyed by the app it described.
pub(crate) fn record_described(
    args: &serde_json::Value,
    obs: &str,
    described: &mut std::collections::HashMap<String, ActionList>,
) {
    let Some(app) = args.get("app").and_then(|a| a.as_str()) else {
        return;
    };
    let acts = listed_actions(obs);
    if !acts.is_empty() {
        described.insert(app.to_string(), acts);
    }
}

/// The app that offers other apps' actions as its own `<app>_<action>` family (`files_*`,
/// `editor_*`).
pub(crate) const TWIN_HOST: &str = "shell";

/// E.ARENA1-F15: the read an action needs when its app's grades are unknown this turn.
///
/// F7, F8 and F14 all start from the grade the app LISTED for the action -- so a model that acts
/// on an app without describing it first (`editor.set_content` straight away) meets none of them,
/// and a card goes up. The loop now reads the app's own listing itself, once per app per turn,
/// before the first action on it. The read is for the loop, not the model: it only lets the
/// checks that already exist see the grade. The shell (the twin host) is read by F8 on demand.
pub(crate) fn grade_lookup(
    tool: &str,
    args: &serde_json::Value,
    described: &std::collections::HashMap<String, ActionList>,
) -> Option<serde_json::Value> {
    let (app, _) = act_target(tool, args)?;
    (app != TWIN_HOST && !described.contains_key(&app)).then(|| serde_json::json!({ "app": app }))
}

/// E.ARENA1-F8: the one read a sensitive action needs before it may put a card in front of the
/// person.
///
/// F7 pointed a sensitive action at its standard twin, but only among apps described THIS turn.
/// Reading C, T7: the mind described the calendar and the editor, never the shell, went straight
/// to `editor.set_content`, and sat through two unanswered cards (231.8 s) while
/// `shell.editor_set_content` was graded standard. The loop now looks for itself: one read of the
/// shell's `<app>_` family, before the card rather than after it. Returns the describe arguments,
/// or `None` when there is nothing to look for.
pub(crate) fn twin_lookup(
    tool: &str,
    args: &serde_json::Value,
    described: &std::collections::HashMap<String, ActionList>,
) -> Option<serde_json::Value> {
    let (app, action) = act_target(tool, args)?;
    if app == TWIN_HOST || described.contains_key(TWIN_HOST) {
        return None;
    }
    let grade = grade_of(described, &app, &action)?;
    if !asks_first(grade) || same_app_route(&app, &action, grade, described).is_some() {
        return None;
    }
    Some(serde_json::json!({ "app": TWIN_HOST, "actions": format!("{app}_") }))
}

/// E.ARENA1-F7: a sensitive action whose SAME change is offered at a lower grade elsewhere.
///
/// Reading B4 (six fixes in) lost the file tasks at the last step: the mind wrote the text with the
/// editor app's `set_content(text)` — graded **sensitive**, so the person was asked, nobody answered
/// in an unattended run, and after 110 s the mind said honestly it had not done it. The desktop also
/// offers `shell.editor_set_content(text)` — "replace the whole document with this text" — graded
/// **standard**. Hermes took that door and passed in 27 s. The OS's own naming joins them: the shell
/// exposes the editor's actions as `editor_<action>`.
///
/// This does not route around a grade. It tells the model the twin exists, once; a model that means
/// to ask the person re-issues the same call and it goes through. When the OS grades both sensitive,
/// there is no twin and nothing changes. F8: the hint names the twin's whole family with each
/// signature — Reading C's Hermes wrote its files with the shell's `editor_new` →
/// `editor_set_content` → `editor_save_as`, and a model that only learns one name has to guess the
/// rest.
pub(crate) fn lower_grade_twin(
    tool: &str,
    args: &serde_json::Value,
    described: &std::collections::HashMap<String, ActionList>,
) -> Option<String> {
    let (app, action) = act_target(tool, args)?;
    let grade = grade_of(described, &app, &action)?;
    if !asks_first(grade) {
        return None;
    }
    if let Some(route) = same_app_route(&app, &action, grade, described) {
        return Some(route);
    }
    let twin = format!("{app}_{action}");
    let family = format!("{app}_");
    described.iter().find_map(|(other, acts)| {
        let t = acts.iter().find(|l| l.name == twin)?;
        (!asks_first(&t.grade)).then(|| {
            let kin: Vec<String> = acts
                .iter()
                .filter(|l| l.name.starts_with(&family))
                .map(|l| format!("{} [{}]", l.sig, l.grade))
                .collect();
            format!(
                "({app}.{action} is graded {grade}, so it would ask the person first. `{other}` offers \
                 the same change as `{}`, graded {}, which does not ask. Its {family}* family: {}. Use \
                 it unless you mean to ask; if you do, send this call again and it will go to the \
                 person.)",
                t.name,
                t.grade,
                kin.join(", ")
            )
        })
    })
}

/// E.ARENA1-F36: the app-state revision a desktop reply reports (`revision: <hex>`, on its own line
/// in act and describe replies). The same act after this has moved is a new act.
pub(crate) fn revision_of(obs: &str) -> Option<String> {
    let rest = obs.split("revision: ").nth(1)?;
    let rev: String = rest.chars().take_while(|c| c.is_ascii_alphanumeric()).collect();
    (!rev.is_empty()).then_some(rev)
}

/// E.ARENA1-F36: the app an act or describe is about.
pub(crate) fn app_of(tool: &str, args: &serde_json::Value) -> Option<String> {
    if tool == ACT || tool == DESCRIBE {
        return args.get("app").and_then(|a| a.as_str()).map(str::to_string);
    }
    None
}

/// E.ARENA1-F36: is an act still the latest thing its app has seen? True while the app's revision is
/// the one that act's own reply reported -- and when either is unknown, as before F36.
pub(crate) fn still_current(
    app: Option<&str>,
    act_rev: Option<&String>,
    app_rev: &std::collections::HashMap<String, String>,
) -> bool {
    match (act_rev, app.and_then(|a| app_rev.get(a))) {
        (Some(then), Some(now)) => then == now,
        _ => true,
    }
}

/// E.ARENA1-F35: an action the desktop refused because it is graded above this machine's ceiling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CeilingRefusal {
    pub(crate) app: String,
    pub(crate) action: String,
    pub(crate) grade: String,
    pub(crate) ceiling: String,
}

/// E.ARENA1-F35: read a ceiling refusal, in either of the desktop's two forms (real bytes, 520):
/// yos-mcp's guard -- "refused: blender.run_python is graded dangerous, and this machine does not
/// allow callers like this past sensitive (…" -- or the app's own -- "CEILING: blender.run_python is
/// graded `dangerous`, above this machine's `sensitive` ceiling".
pub(crate) fn ceiling_refusal(obs: &str) -> Option<CeilingRefusal> {
    let word = |s: &str| s.trim_matches(|c: char| c == '`' || c.is_whitespace()).to_string();
    let target = |s: &str| {
        let t = word(s);
        let (app, action) = t.split_once('.')?;
        (!app.is_empty() && !action.is_empty()).then(|| (app.to_string(), action.to_string()))
    };
    if let Some(rest) = obs.split("refused: ").nth(1) {
        if let Some((what, tail)) = rest.split_once(" is graded ") {
            if let Some((grade, after)) = tail.split_once(", and this machine does not allow callers like this past ") {
                let ceiling = after.split([' ', '(', '.', ',']).next().unwrap_or("");
                let (app, action) = target(what)?;
                return Some(CeilingRefusal { app, action, grade: word(grade), ceiling: word(ceiling) });
            }
        }
    }
    let rest = obs.split("CEILING: ").nth(1)?;
    let (what, tail) = rest.split_once(" is graded ")?;
    let (grade, after) = tail.split_once(", above this machine's ")?;
    let ceiling = after.split(" ceiling").next()?;
    let (app, action) = target(what)?;
    Some(CeilingRefusal { app, action, grade: word(grade), ceiling: word(ceiling) })
}

/// The desktop's grade ladder, lowest first.
fn grade_rank(grade: &str) -> Option<usize> {
    ["safe", "standard", "sensitive", "dangerous"].iter().position(|g| *g == grade)
}

/// E.ARENA1-F35: the refused app's own actions within the machine's ceiling, as a note for the
/// model -- 520 turn 433: run_python was refused, and the Mind told the person the scene could not
/// be built while Blender listed add_primitive, set_material, set_light, set_camera and set_render
/// at standard. None when the app lists nothing within the ceiling (or was not read this turn).
pub(crate) fn within_ceiling(
    r: &CeilingRefusal,
    described: &std::collections::HashMap<String, ActionList>,
) -> Option<String> {
    let limit = grade_rank(&r.ceiling)?;
    let within: Vec<String> = described
        .get(&r.app)?
        .iter()
        .filter(|l| l.name != r.action)
        .filter(|l| grade_rank(&l.grade).is_some_and(|g| g <= limit))
        .map(|l| {
            if asks_first(&l.grade) {
                format!("{} [{}, asks the person first]", l.sig, l.grade)
            } else {
                format!("{} [{}]", l.sig, l.grade)
            }
        })
        .collect();
    if within.is_empty() {
        return None;
    }
    Some(format!(
        "({app}.{action} is graded {grade}, above this machine's {ceiling} limit, so it cannot run and \
         no one here can approve it. {app} itself offers these actions within the limit: {list}. If \
         they can do what was asked, do it with them, step by step. Say it cannot be done only if none \
         of them can.)",
        app = r.app,
        action = r.action,
        grade = r.grade,
        ceiling = r.ceiling,
        list = within.join("; ")
    ))
}

/// E.ARENA1-F11: the app the desktop said does not exist, read off its own refusal: "…how the OS
/// grades it could not be read: yos: no socket for 'files'".
pub(crate) fn missing_app(obs: &str) -> Option<String> {
    let rest = obs.split("no socket for '").nth(1)?;
    let app = rest.split('\'').next()?;
    (!app.is_empty() && !app.contains(char::is_whitespace)).then(|| app.to_string())
}

/// E.ARENA1-F11: a family action sent to an app that does not exist, re-addressed to the shell.
///
/// Reading D, T5: the model read `files_go` in the shell's `files_` family and sent it to app
/// `files`. The desktop answered "no socket for 'files'", and the mind, correctly finding nothing
/// done, said so — about an action that exists, on the shell. Reactive only: this runs after the
/// desktop has said the app does not exist, and only when the shell lists the action (as named, or
/// as `<app>_<action>`), so a call the desktop would have accepted is never touched. The corrected
/// call is an ordinary desktop action, graded by the OS at the shell.
pub(crate) fn host_redirect(
    tool: &str,
    args: &serde_json::Value,
    obs: &str,
    described: &std::collections::HashMap<String, ActionList>,
) -> Option<serde_json::Value> {
    let (app, action) = act_target(tool, args)?;
    if app == TWIN_HOST || missing_app(obs)? != app {
        return None;
    }
    let host = described.get(TWIN_HOST)?;
    let name = [action.clone(), format!("{app}_{action}")]
        .into_iter()
        .find(|n| host.iter().any(|l| &l.name == n))?;
    let mut fixed = args.clone();
    fixed["app"] = serde_json::json!(TWIN_HOST);
    fixed["action"] = serde_json::json!(name);
    Some(fixed)
}

/// The read F11 needs when the shell was not described this turn: its `<app>_` family.
pub(crate) fn host_family_lookup(
    tool: &str,
    args: &serde_json::Value,
    obs: &str,
    described: &std::collections::HashMap<String, ActionList>,
) -> Option<serde_json::Value> {
    let (app, _) = act_target(tool, args)?;
    (app != TWIN_HOST && !described.contains_key(TWIN_HOST) && missing_app(obs)? == app)
        .then(|| serde_json::json!({ "app": TWIN_HOST, "actions": format!("{app}_") }))
}

/// What the model is told when its call was re-addressed, followed by what the shell answered.
pub(crate) fn redirected(from_app: &str, fixed: &serde_json::Value, out: &str) -> String {
    let action = fixed.get("action").and_then(|a| a.as_str()).unwrap_or("");
    format!("(there is no `{from_app}` app; `{action}` is the {TWIN_HOST}'s, so it was sent there:) {out}")
}

/// E.ARENA1-F12: keep the one bit "a document written this turn is still unsaved", from the
/// desktop's own words. An editor action's result line carries `, unsaved` until a save takes:
/// `editing "untitled", 7 words, unsaved` becomes `editing "arena-minp0x.txt", 3 words`. Set by any
/// action whose result says so; cleared only by a SAVE whose result shows the document without it,
/// so `editor_new` after an unsaved write does not pretend the text was kept. Reads never touch it.
pub(crate) fn update_unsaved(tool: &str, args: &serde_json::Value, obs: &str, unsaved: &mut bool) {
    if tool != ACT {
        return;
    }
    let head = obs.lines().next().unwrap_or("");
    let head = head.split(" accepted:").next().unwrap_or(head);
    if !(head.contains("editing \"") || head.contains("Text Editor \u{2014}")) {
        return;
    }
    if head.contains(", unsaved") {
        *unsaved = true;
    } else if args
        .get("action")
        .and_then(|a| a.as_str())
        .is_some_and(|a| a.contains("save"))
    {
        *unsaved = false;
    }
}

/// E.ARENA1-F12: said once, before a turn ends with the document unsaved.
pub(crate) fn unsaved_nudge(step: usize) -> String {
    format!(
        "\n[{step}] (what you wrote is still only in the editor \u{2014} the desktop says it is unsaved. \
         Save it now with the path the request named (the shell's editor_save_as, or the editor's \
         save_as), or say plainly that it is not saved.)"
    )
}

/// E.ARENA1-F12: appended, by code, when a turn ends with the document still unsaved.
pub(crate) const UNSAVED_NOTE: &str =
    "(What I wrote is in the editor but has not been saved to a file.)";

/// E.ARENA1-F12: the repeat nudge for a desktop ACTION. The loop's own nudge was written for fetch
/// tasks and ends "otherwise answer" -- Reading E's T7 took that exit with the save still undone.
/// E.ARENA1-F43 (VM 561, Mind 371bedb): the same `os_act` sent again after the desktop REFUSED it.
/// Nothing has changed, so it will be refused again -- yet the note it met was F12's "that action
/// already ran", false for a refusal, and the model resent an identical bare-value call four more
/// times. Say it was refused, that the same call cannot succeed, and quote what the desktop said to
/// change (yos-mcp's refusals name the fix: "takes named parameters: title, …").
pub(crate) fn repeated_refusal_note(refusal: &str) -> String {
    let why = refusal_reason(refusal, 500);
    // E.ARENA1-F44 (VM 561, f2ba31e): with the call it should send buried after the reason, the
    // model resent the refused one three times. A ready-to-copy call leads.
    match copyable_call(refusal) {
        Some(call) => format!(
            "(Send exactly this next, with each <placeholder> filled in: {call} -- that exact call you just \
             repeated was REFUSED and nothing ran; sent again unchanged it will be refused again. What the \
             desktop said: {why})"
        ),
        None => format!(
            "(that exact call was REFUSED and nothing ran -- sending it again unchanged will be refused again. \
             What the desktop said: {why} Change the call the way it says, or say plainly what you could not do.)"
        ),
    }
}

/// The desktop's reason for a refusal: what follows "refused:", clipped to `max` chars.
pub(crate) fn refusal_reason(refusal: &str, max: usize) -> String {
    let why = refusal.split_once("refused:").map_or(refusal, |(_, w)| w).trim();
    why.chars().take(max).collect()
}

/// The ready-to-copy call a refusal offers (yos-mcp's `bare_value_refusal`: "Send this, with each
/// <placeholder> filled in: {…}"), when it offers one.
pub(crate) fn copyable_call(refusal: &str) -> Option<String> {
    let lc = refusal.to_ascii_lowercase();
    let from = lc.find("send this")?;
    let start = from + refusal[from..].find('{')?;
    let end = refusal.rfind('}')?;
    (end > start).then(|| refusal[start..=end].to_string())
}

/// E.ARENA1-F44: what the person is told when the model resent a refused call after being shown
/// the fix. The turn ends here: more nudges were wasted steps, and a composed answer could claim
/// the thing was made.
pub(crate) fn refusal_stop_reply(args: &serde_json::Value, refusal: &str) -> String {
    let what = act_target(ACT, args).map_or_else(|| "that".to_string(), |(app, action)| format!("`{app}.{action}`"));
    format!(
        "I couldn't do that: the desktop refused {what}, and nothing was created. What it said: {} I was shown how to fix the call and sent it unchanged again, so I stopped rather than keep repeating it.",
        refusal_reason(refusal, 300)
    )
}

pub(crate) fn repeated_action_note(
    tool: &str,
    args: &serde_json::Value,
    unsaved: bool,
    path: Option<&str>,
) -> Option<String> {
    if tool != ACT {
        return None;
    }
    if !unsaved {
        return Some(
            "(that action already ran; its result is above. Do not repeat it. Take the NEXT step the \
             request still needs, or say plainly what is left undone.)"
                .to_string(),
        );
    }
    // E.ARENA1-F19: the exact next call, when the request named where the file goes. On yantrik-os
    // 0f3e733 the model repeated `editor.new{text}` three to five times through a nudge that only
    // SAID "save it" -- and named the shell's `editor_save_as`, which #253 removed. The save that
    // fits the app the model is using, with the request's own path, is something to copy.
    let app = act_target(tool, args).map(|(a, _)| a).unwrap_or_default();
    let save = if app == TWIN_HOST { "editor_save_as" } else { "save_as" };
    Some(match path {
        Some(p) => format!(
            "(that action already ran; its result is above. Do not repeat it. What you wrote is still \
             only in the editor, unsaved. The next call is: os_act {{\"app\": \"{app}\", \"action\": \
             \"{save}\", \"args\": {{\"path\": \"{p}\"}}}} -- or say plainly that it is not saved.)"
        ),
        None => format!(
            "(that action already ran; its result is above. Do not repeat it. What you wrote is still \
             only in the editor, unsaved: the next step is `{app}.{save}` with the path the request \
             named, or say plainly that it is not saved.)"
        ),
    })
}

/// E.ARENA1-F20: another `new` while the document written this turn is still unsaved -- whatever
/// its text. V9's T7 (yantrik-os 2c687eb): the model called `editor.new` with the three titles
/// again and again, alternating with and without a trailing newline, so every other call was not a
/// repeat and met no nudge; it opened tab after tab and never saved. A second new tab is not the
/// next step for a document that still needs saving, so it is answered with the exact save call.
pub(crate) fn another_new_while_unsaved(tool: &str, args: &serde_json::Value, unsaved: bool) -> bool {
    unsaved
        && act_target(tool, args).is_some_and(|(_, action)| action == "new" || action == "editor_new")
}

/// E.ARENA1-F19: the file path a request names -- `~/…` or an absolute path -- as the person wrote
/// it, trailing punctuation dropped.
pub(crate) fn requested_path(user_text: &str) -> Option<String> {
    user_text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| matches!(c, ',' | ';' | ':' | ')' | '(' | '"' | '\'' | '`')))
        .map(|w| w.trim_end_matches('.'))
        .find(|w| (w.starts_with("~/") || w.starts_with('/')) && w.len() > 2)
        .map(str::to_string)
}

/// E.ARENA1-F23: how long to wait before looking again at an unsettled action. yantrik-os-f4:
/// a `.defers()` action has scheduled its work on the app's UI loop, "lands shortly"; 1-2 s.
pub(crate) const SETTLE_WAIT_MS: u64 = if cfg!(test) { 0 } else { 1500 };

/// E.ARENA1-F23: the desktop accepted the action but had not finished it when it answered, so the
/// result's first line is the state from BEFORE. The desktop's own words, on their own line:
/// `accepted: True, settled: False`.
pub(crate) fn unsettled(obs: &str) -> bool {
    obs.lines().any(|l| l.trim().to_ascii_lowercase().contains("settled: false"))
}

/// E.ARENA1-F23: the look to take again -- the app the action addressed. F23b: for the shell's
/// `open_app`, the app it opened. On 79ae685 the second look at the shell said "files screen, 1
/// windows open" -- nothing about the image viewer just opened -- and the model opened it again.
pub(crate) fn settle_look(tool: &str, args: &serde_json::Value) -> Option<serde_json::Value> {
    let (app, action) = act_target(tool, args)?;
    let opened = (app == TWIN_HOST && action == "open_app")
        .then(|| args.get("args")?.get("name")?.as_str())
        .flatten()
        .filter(|n| !n.trim().is_empty());
    Some(serde_json::json!({"app": opened.unwrap_or(&app)}))
}

/// E.ARENA1-F30: the app's own summary says it has not finished loading. 520, turn 390: Weather's
/// first line was "Weather — loading …"; a minute later it said "15°C in London".
pub(crate) fn still_loading(obs: &str) -> bool {
    let head = obs.lines().next().unwrap_or("");
    let head = head.split(" revision:").next().unwrap_or(head);
    head.to_ascii_lowercase().contains("loading")
}

/// E.ARENA1-F34 (yantrik-os #464): the app's window is up but it has not answered yet. The desktop
/// has already waited 15 s and says to describe again -- not a failure, and nothing to open again.
/// The sentence is on the reply's second line (the first is yos-mcp's generic "failed (exit 1)").
pub(crate) fn still_starting(obs: &str) -> bool {
    obs.contains("is open but still starting")
}

/// E.ARENA1-F34: how long to wait before describing a starting app again.
pub(crate) const STARTING_WAIT_MS: u64 = if cfg!(test) { 0 } else { 3000 };

/// E.ARENA1-F34: the starting reply, with what a second look found.
pub(crate) fn started_since(obs: &str, seen: &str) -> String {
    let head = seen.lines().next().unwrap_or("");
    let head = head.split(" revision:").next().unwrap_or(head).trim();
    format!(
        "{obs}\n(it was still starting. Looked again {:.1} s later: {head}. Use this, and do not \
         open it again.)",
        STARTING_WAIT_MS.max(3000) as f64 / 1000.0
    )
}

/// E.ARENA1-F30: how long to wait before describing a loading app again.
pub(crate) const LOADING_WAIT_MS: u64 = if cfg!(test) { 0 } else { 2000 };

/// E.ARENA1-F30: the loading description, with what a second look found.
pub(crate) fn loaded_since(obs: &str, seen: &str) -> String {
    let head = seen.lines().next().unwrap_or("");
    let head = head.split(" revision:").next().unwrap_or(head).trim();
    format!(
        "{obs}\n(it said it was still loading. Looked again {:.1} s later: {head}. Use this, not the \
         first line above.)",
        LOADING_WAIT_MS.max(2000) as f64 / 1000.0
    )
}

/// E.ARENA1-F23: the unsettled result, with what a second look found. Seen on 185b4c0: the Mind
/// told the person "the weather app was not opened" from a first line that still said "0 windows
/// open" -- it was open; and on T5 read an unsettled `files_go` as a failure and never reached
/// `files_new_folder`, saying it could not make folders.
pub(crate) fn settled_since(obs: &str, seen: &str) -> String {
    let head = seen.lines().next().unwrap_or("");
    let head = head.split(" revision:").next().unwrap_or(head).trim();
    if head.is_empty() {
        return format!(
            "{obs}\n(settled: False -- the desktop had not finished when it answered, so the first line \
             above is from BEFORE the action. Look again with os_describe before saying whether it worked.)"
        );
    }
    format!(
        "{obs}\n(settled: False -- the desktop had not finished when it answered, so the first line above \
         is from BEFORE the action. Looked again {:.1} s later: {head}. Judge the action from this, not \
         from the first line.)",
        SETTLE_WAIT_MS.max(1500) as f64 / 1000.0
    )
}

/// E.ARENA1-F26: an action's own arguments as a map. A bare string (`"args": "hello"`, as models
/// send `editor.new`) is one unnamed field.
fn inner_args(args: &serde_json::Value) -> serde_json::Map<String, serde_json::Value> {
    match args.get("args") {
        Some(serde_json::Value::Object(m)) => m.clone(),
        Some(serde_json::Value::String(t)) => {
            let mut m = serde_json::Map::new();
            m.insert(String::new(), serde_json::Value::String(t.clone()));
            m
        }
        _ => serde_json::Map::new(),
    }
}

/// E.ARENA1-F26: the same change as one that already RAN this turn, re-sent with a field added or
/// dropped -- which the exact repeat guard cannot see. R2 and R3, T3: `add_event {date, duration_min,
/// time, title}` succeeded ("added": ..., "on": "2026-09-30 15:00"), and the model sent it again
/// with `reminder_minutes: 10`, copied from that result: two events, and T4's false claim after.
/// `made` holds each action that ran, with its result's first line. Different VALUES are a
/// different change and pass; an earlier call with no arguments matches nothing.
pub(crate) fn same_change_again(
    tool: &str,
    args: &serde_json::Value,
    made: &[(serde_json::Value, String)],
) -> Option<String> {
    let (app, action) = act_target(tool, args)?;
    let now = inner_args(args);
    if now.is_empty() {
        return None;
    }
    let within = |a: &serde_json::Map<String, serde_json::Value>, b: &serde_json::Map<String, serde_json::Value>| {
        a.iter().all(|(k, v)| b.get(k) == Some(v))
    };
    made.iter().find_map(|(prev, head)| {
        let (p_app, p_action) = act_target(ACT, prev)?;
        let before = inner_args(prev);
        (p_app == app && p_action == action && !before.is_empty()
            && (within(&before, &now) || within(&now, &before)))
        .then(|| {
            format!(
                "({app}.{action} already ran this turn with the same details, and it worked: {}. That \
                 change is made -- do not make it again. Take the next step the request still needs, or \
                 answer.)",
                head.trim()
            )
        })
    })
}

/// E.ARENA1-F32: whether the model is offered this MCP tool. From the desktop's server only the
/// `os_*` and `web_*` tools: yos-mcp publishes nine more (`run_command`, agents, hand-off) when it
/// runs with an agent token, and E.TOKEN1 gives it one. The token says who acts; it must not change
/// what the model reaches for -- on 557e4ef the model wrote files with `run_command`, the Mind's
/// own gate asked "confirm with yes", and T6/T7 ended there. Other servers are unaffected.
/// E.WEBGATE1 (Pranab, 2026-09-29): the desktop's own browser tools, whose gate is the desktop's --
/// browsing runs, a commitment raises its card every time, and its taint still blocks typing out
/// private data. The Mind does not ask for these on top; every other MCP tool keeps its own ask.
pub(crate) fn the_desktop_gates(t: &mind_tools::McpTool) -> bool {
    t.server == DESKTOP_SERVER && t.name.starts_with("web_")
}

pub(crate) fn offered_to_the_model(t: &mind_tools::McpTool) -> bool {
    t.server != DESKTOP_SERVER || t.name.starts_with("os_") || t.name.starts_with("web_")
}

/// E.ARENA1-F21: the path a request asks to have MADE -- created, written, saved -- or `None`.
/// Every "is the work done" signal before this one was scraped from the first line of a desktop
/// result; this one asks the world. A request to delete, move or rename a path is not asking for
/// it to exist, so it has no goal here.
pub(crate) fn goal_path(user_text: &str) -> Option<String> {
    const MAKES: [&str; 12] = [
        "create", "creates", "write", "writes", "written", "save", "saved", "saves", "make", "put",
        "store", "export",
    ];
    const UNMAKES: [&str; 7] = ["delete", "remove", "move", "rename", "erase", "trash", "rm"];
    let lower = user_text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !c.is_alphanumeric()).collect();
    if words.iter().any(|w| UNMAKES.contains(w)) || !words.iter().any(|w| MAKES.contains(w)) {
        return None;
    }
    requested_path(user_text)
}

/// E.ARENA1-F21: where a requested path is on this machine -- `~/` against `home`. `None` when
/// that cannot be said (no home), so nothing is claimed about a file the Mind cannot look for.
pub(crate) fn on_this_machine(path: &str, home: Option<&str>) -> Option<std::path::PathBuf> {
    match path.strip_prefix("~/") {
        Some(rest) => {
            let home = home.map(str::trim).filter(|h| !h.is_empty())?;
            Some(std::path::Path::new(home).join(rest))
        }
        None if path.starts_with('/') => Some(std::path::PathBuf::from(path)),
        None => None,
    }
}

/// E.HOME1: is something at `at`? `Some(false)` only when the filesystem says it is not there;
/// `None` when it cannot say -- a permission error is not absence. Under yantrik-os #411 the Mind
/// runs as its own account and may not read the person's files, and `Path::exists` would have
/// answered "missing" for every one of them.
pub(crate) fn is_there(at: &std::path::Path) -> Option<bool> {
    classify_lookup(std::fs::metadata(at).map(|_| ()))
}

fn classify_lookup(r: std::io::Result<()>) -> Option<bool> {
    match r {
        Ok(()) => Some(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(false),
        Err(_) => None,
    }
}

/// E.HOME3: what the desktop says about a path (yantrik-os #442 `files_stat`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stat {
    There,
    Missing,
    /// Not allowed, outside the person's home, protected, a broken link, not a path -- the desktop
    /// cannot say, and neither may the Mind.
    Unknown,
}

/// E.HOME3: `files_stat`'s answer, read from the result object after the header lines.
pub(crate) fn parse_files_stat(obs: &str) -> Option<Stat> {
    let at = obs.find("\n{")? + 1;
    let v: serde_json::Value = serde_json::Deserializer::from_str(&obs[at..])
        .into_iter::<serde_json::Value>()
        .next()?
        .ok()?;
    match v.get("exists")? {
        serde_json::Value::Bool(true) => Some(Stat::There),
        serde_json::Value::Bool(false) => Some(Stat::Missing),
        serde_json::Value::String(s) if s == "unknown" => Some(Stat::Unknown),
        _ => None,
    }
}

/// E.ARENA1-F21: said once, before a turn ends with the requested path still missing.
pub(crate) fn goal_nudge(step: usize, path: &str, missing_folder: Option<&str>) -> String {
    // E.ARENA1-F33: the concrete blocker, when the world shows one. The hard smoke's T9: the
    // folder did not exist, the Editor refuses to save into a missing folder, and the model went
    // back to `files_go` twice.
    let folder = missing_folder
        .map(|f| format!(" The folder {f} does not exist yet: make it first (Files: go to its parent, then `files_new_folder`)."))
        .unwrap_or_default();
    format!(
        "\n[{step}] (the request asked for {path}, and nothing is at {path} yet -- it was not \
         created.{folder} Make it now (for text: open the editor if it is closed, then its `new` with \
         the text, then `save_as` with that path), or say plainly that it was not made.)"
    )
}

/// E.ARENA1-F33: the requested path's folder, as the person wrote it, when it is not there.
pub(crate) fn missing_folder_of(path: &str, home: Option<&str>) -> Option<String> {
    let (parent, _) = path.rsplit_once('/')?;
    if parent.is_empty() || parent == "~" {
        return None;
    }
    let at = on_this_machine(parent, home)?;
    (is_there(&at) == Some(false)).then(|| parent.to_string())
}

/// E.ARENA1-F21: appended, by code, when a turn ends with the requested path still missing.
pub(crate) fn goal_missing_note(path: &str) -> String {
    format!("(Nothing is at {path} yet \u{2014} it was not created.)")
}

/// E.ARENA1-F14: a sensitive `set_content` whose own app opens a new document holding the text at a
/// lower grade -- yantrik-os #253's `editor.new(text?)`, standard: "Open a new tab ... empty or
/// holding the text given. Nothing is written to disk until `save_as`". A new tab replaces nothing,
/// so writing a file needs no card: `new{text}` then `save_as{path}`. Preferred over the shell's
/// `editor_*` twin (two calls, not three) and still there once #253 removes that twin. Read off the
/// app's own listing: the rule needs a standard `new` whose signature takes `text`, and `save_as`.
fn same_app_route(
    app: &str,
    action: &str,
    grade: &str,
    described: &std::collections::HashMap<String, ActionList>,
) -> Option<String> {
    if action != "set_content" {
        return None;
    }
    let acts = described.get(app)?;
    let new = acts
        .iter()
        .find(|l| l.name == "new" && l.sig.contains("text") && !asks_first(&l.grade))?;
    let save = acts
        .iter()
        .find(|l| l.name == "save_as" && !asks_first(&l.grade))?;
    Some(format!(
        "({app}.{action} is graded {grade}, so it would ask the person first. `{app}` also lists \
         `{}` graded {}, which opens a new tab already holding the text and replaces nothing, then \
         `{}` graded {} writes it to the path. Use them unless you mean to ask; if you do, send this \
         call again and it will go to the person.)",
        new.sig, new.grade, save.sig, save.grade
    ))
}

/// What the person did with a card this turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Answer {
    SaidNo,
    NoAnswer,
}

/// E.ARENA1-F10: what the person did with the card, read off the desktop's own refusal. The
/// sentences are the OS's (`yos-mcp`, `guard_act`): "…was asked to allow editor.set_content and
/// said no…" and "…and did not answer within 110s…". A card the desktop lost track of ("…and then
/// the desktop stopped answering") is neither — nobody knows what the person did — so it is not
/// read as an answer.
pub(crate) fn approval_answer(obs: &str) -> Option<(String, Answer)> {
    let rest = obs.split("was asked to allow ").nth(1)?;
    let (key, after) = rest.split_once(" and ")?;
    if !key.contains('.') || key.contains(char::is_whitespace) {
        return None;
    }
    let answer = if after.starts_with("said no") {
        Answer::SaidNo
    } else if after.starts_with("did not answer") {
        Answer::NoAnswer
    } else {
        return None;
    };
    Some((key.to_string(), answer))
}

/// The doors that make the same change as `app.action`: itself, and its twin across the host.
fn same_change(app: &str, action: &str) -> Vec<String> {
    let mut keys = vec![format!("{app}.{action}")];
    if app == TWIN_HOST {
        if let Some((a, b)) = action.split_once('_') {
            keys.push(format!("{a}.{b}"));
        }
    } else {
        keys.push(format!("{TWIN_HOST}.{app}_{action}"));
    }
    keys
}

/// E.ARENA1-F10: a card the person already answered this turn, for this action or its twin.
///
/// Reading C, T7: `editor.set_content` raised a card, nobody answered for 110 s, and the mind
/// asked again with the trailing newline dropped — different arguments, so no repeat guard saw it
/// — and waited another 110 s for the same silence. The desktop's own words say what comes next:
/// after no answer, "tell them what you were trying to do; they can ask you to try it again";
/// after a no, "do not ask again and do not look for another route to the same effect". The loop
/// now keeps both. A no also closes the twin, so F7 can never route around one.
pub(crate) fn already_answered(
    tool: &str,
    args: &serde_json::Value,
    answered: &std::collections::HashMap<String, Answer>,
) -> Option<String> {
    let (app, action) = act_target(tool, args)?;
    let this = format!("{app}.{action}");
    for key in same_change(&app, &action) {
        match (answered.get(&key), key == this) {
            (Some(Answer::SaidNo), true) => {
                return Some(format!(
                    "(Not sent. The person was asked to allow {this} this turn and said no. That is their \
                     answer: do not ask again, and do not look for another way to make the same change. \
                     Say what you were going to do and leave it with them.)"
                ))
            }
            (Some(Answer::SaidNo), false) => {
                return Some(format!(
                    "(Not sent. The person said no to {key} this turn, and {this} makes the same change, so \
                     their no covers it. Say what you were going to do and leave it with them.)"
                ))
            }
            (Some(Answer::NoAnswer), true) => {
                return Some(format!(
                    "(Not sent. The person was asked to allow {this} this turn and did not answer; they are \
                     probably away from the keyboard, and asking again would wait for the same silence. \
                     Tell them what you were trying to do; they can ask you to try it again.)"
                ))
            }
            _ => {}
        }
    }
    None
}

/// E.ARENA1-F16: the desktop's map, in every desktop turn's place line.
///
/// The live check of yantrik-os #253 (VM 520, 2026-09-23): with the shell's `editor_*` gone, the
/// Mind was asked to create a text file, opened Files, described a `files` app that was not
/// running, listed the apps twice and gave up -- 0/2. The note that says where a file's text is
/// written (`DISCOVER_DESKTOP_NOTE`) was only shown when the model searched for tools, and it did
/// not. This is that note, on every desktop turn. It names where, not how -- `os_describe editor`
/// shows how, on whichever editor this OS has, and F7/F14 route a card-raising write.
pub(crate) fn desktop_sentence(desktop: bool) -> String {
    if !desktop {
        return String::new();
    }
    " On this desktop a file's text is written with the `editor` app (open it with the shell's      open_app if it is not running; os_describe editor shows its actions); folders are the shell's      files_* actions; the calendar and notes are apps of their own."
        .to_string()
}

/// E.ARENA1-F18: the desktop's CLI advice, said the way an MCP caller can act on it.
///
/// yos-mcp's action results end "(state omitted; `yos describe editor`, or re-run with --full)" --
/// advice for the `yos` command line. A model has no `--full`, and "re-run" reads as "call it
/// again": on yantrik-os 0f3e733 the Mind called `editor.new{text}` three to five times per task and
/// never `save_as`, through every nudge not to repeat it. The line becomes one a model can use.
pub(crate) fn mcp_voice(obs: &str) -> String {
    if !obs.contains("re-run with --full") {
        return obs.to_string();
    }
    obs.lines()
        .map(|l| {
            let t = l.trim();
            if t.starts_with("(state omitted;") && t.contains("re-run with --full") {
                let app = t.split("`yos describe ").nth(1).and_then(|r| r.split('`').next());
                match app {
                    Some(a) => format!("(state trimmed; os_describe {a} shows all of it)"),
                    None => "(state trimmed)".to_string(),
                }
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// E.ARENA1-F7: where home is on this machine. `shell.editor_save_as` gives `/home/user/notes.txt`
/// as its example path, this machine's home is not `/home/user`, and the mind copied the example.
pub(crate) fn home_sentence(desktop: bool, home: Option<&str>) -> String {
    match (desktop, home.map(str::trim).filter(|h| !h.is_empty())) {
        (true, Some(h)) => format!(" The person's home folder on this computer is {h} (so ~ means {h})."),
        _ => String::new(),
    }
}

/// Where a condensed description's action list begins. Also how a second pass recognises one.
const CONDENSED_MARK: &str = "\nACTIONS:";

/// Top-level state fields a condensed description keeps even when the head cut them off.
///
/// `clock`: the desktop's own date, weekday, time and zone (yantrik-os #207 makes it an object,
/// so minds stop running `date` through an approval card). `describe shell` sorts its keys, and
/// `apps` -- a list of ~3,000 characters -- sorts before `clock`, so the 900-character head cut it
/// off every time: the mind was never shown the desktop's clock.
const ALWAYS_KEPT: [&str; 1] = ["clock"];

/// One TOP-LEVEL field of a description's state, whole and on one line: `  "key": value`. The
/// value is taken to its matching bracket when it is an object or a list (however it was
/// pretty-printed), else to the end of its line. `None` when absent, or longer than a kept field
/// should be.
fn kept_field(state: &str, key: &str) -> Option<String> {
    const MAX: usize = 300;
    let pat = format!("\n  \"{key}\": ");
    let rest = &state[state.find(&pat)? + pat.len()..];
    let value = if rest.starts_with('{') || rest.starts_with('[') {
        let (mut depth, mut in_str, mut esc, mut end) = (0i32, false, false, None);
        for (i, c) in rest.char_indices() {
            if in_str {
                match (esc, c) {
                    (true, _) => esc = false,
                    (false, '\\') => esc = true,
                    (false, '"') => in_str = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' | '[' => depth += 1,
                '}' | ']' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(i + c.len_utf8());
                        break;
                    }
                }
                _ => {}
            }
        }
        &rest[..end?]
    } else {
        rest.lines().next()?.trim_end().trim_end_matches(',')
    };
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (compact.chars().count() <= MAX).then(|| format!("  \"{key}\": {compact}"))
}

/// The cap every read-only MCP result gets, for data from somewhere else.
pub(crate) const MCP_OUTPUT_CAP: usize = 6000;

/// E.ARENA1-F4: bound an MCP tool's output without cutting a desktop description off its actions.
///
/// Every read-only MCP result was clipped to 6,000 characters as untrusted third-party data. Right
/// for somebody else's API; wrong for this machine's own control surface, whose shell description
/// is ~18 KB with its first action near character 11,800 — so the shell reached the agent with no
/// actions at all, and the arena's folder and file tasks failed on the same model Hermes passes
/// with. Found by asking the desktop's MCP server directly: it returns `files_new_folder`; the mind
/// never saw it. A tool ON THIS COMPUTER whose output is a description is condensed instead, which
/// is itself bounded (state head + action budget); everything else keeps the cap.
pub(crate) fn bound_mcp_output(out: &str, on_this_computer: bool) -> String {
    if on_this_computer {
        if let Some(condensed) = condense_description(out) {
            return condensed;
        }
    }
    out.chars().take(MCP_OUTPUT_CAP).collect()
}

/// E.ARENA1-F24: a desktop action's `state:` line moved after everything else.
///
/// yantrik-os #384 puts one line of the app's state between `revision:` and the result object. The
/// shell's is over a thousand characters, so on 185b4c0 `files_go`'s result -- where it went, that
/// it is loading -- began at character 1,330 of a work-log entry cut at 900: the Mind never saw what
/// a shell action did. The result is what the action did; the state is context. Nothing is dropped
/// here, only reordered, and a result without a state line comes back unchanged.
pub(crate) fn result_before_state(obs: &str) -> String {
    let (state, rest): (Vec<&str>, Vec<&str>) = obs.lines().partition(|l| l.starts_with("state: {"));
    if state.is_empty() {
        return obs.to_string();
    }
    let mut out = rest.join("\n");
    for l in state {
        out.push('\n');
        out.push_str(l);
    }
    out
}

/// One work-log line for a tool result: a desktop description condensed so its actions survive,
/// anything else clipped to `head` as before. The agent loop's only writer of successful results, so
/// the condensing is tested here rather than trusted to a line inside a 1,500-line loop.
pub(crate) fn work_log_entry(
    step: usize,
    tool: &str,
    obs: &str,
    ok: bool,
    head: usize,
    note: &str,
) -> String {
    let body = if ok && tool.starts_with("mcp.yantrik-os.") {
        condense_description(obs)
    } else {
        None
    }
    .unwrap_or_else(|| {
        let obs = if ok && tool == ACT { result_before_state(obs) } else { obs.to_string() };
        let head = if ok && tool == APPS { head.max(APPS_BUDGET) } else { head };
        obs.chars().take(head).collect::<String>()
    });
    format!("\n[{step}] {tool} -> {body}{note}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CALENDAR: &str = include_str!("../fixtures/desktop/describe_calendar.txt");
    const SHELL: &str = include_str!("../fixtures/desktop/describe_shell.txt");

    const EDITOR: &str = include_str!("../fixtures/desktop/describe_editor.txt");

    #[test]
    fn only_a_failed_desktop_action_may_be_retried() {
        assert!(retry_after_failure("mcp.yantrik-os.os_act", false));
        assert!(!retry_after_failure("mcp.yantrik-os.os_act", true), "a success stays deduplicated");
        assert!(!retry_after_failure("web_fetch", false), "off the desktop nothing changes");
        assert!(!retry_after_failure("mcp.yantrik-os.os_describe", false), "reads are handled by F6");
        assert!(nothing_was_run("Done \u{2014} REFUSED \u{2014} nothing was run. refused: files.files_go was not run, because how the OS grades it could not be read: yos: no socket for 'files'"));
        assert!(!nothing_was_run("Done \u{2014} Text Editor \u{2014} x.txt, 1 line, saved"));
    }

    #[test]
    fn grades_are_read_off_the_real_descriptions() {
        let editor = listed_actions(EDITOR);
        let has = |list: &ActionList, name: &str, grade: &str| {
            list.iter().any(|l| l.name == name && l.grade == grade)
        };
        assert!(has(&editor, "set_content", "sensitive"), "{editor:?}");
        assert!(has(&editor, "save_as", "standard"));
        let shell = listed_actions(SHELL);
        assert!(has(&shell, "editor_set_content", "standard"));
        let sig = shell.iter().find(|l| l.name == "editor_save_as").map(|l| l.sig.as_str());
        assert_eq!(sig, Some("editor_save_as(path)"), "the signature as the app wrote it");
    }

    /// Reading C's T7, from the real descriptions: the editor was described, the shell was not, and
    /// the sensitive write is the moment to look.
    #[test]
    fn a_sensitive_action_looks_for_its_twin_only_when_the_host_is_unread() {
        let mut d = std::collections::HashMap::new();
        record_described(&serde_json::json!({"app": "editor"}), EDITOR, &mut d);
        let act = |app: &str, action: &str| serde_json::json!({"app": app, "action": action, "args": {"text": "x"}});
        assert_eq!(
            twin_lookup(ACT, &act("editor", "set_content"), &d),
            Some(serde_json::json!({"app": "shell", "actions": "editor_"}))
        );
        assert_eq!(twin_lookup(ACT, &act("editor", "save_as"), &d), None, "standard: no card, nothing to find");
        assert_eq!(twin_lookup(ACT, &act("calendar", "delete_event"), &d), None, "grade unknown: not described");
        assert_eq!(twin_lookup(DESCRIBE, &act("editor", "set_content"), &d), None, "reads raise no card");
        record_described(&serde_json::json!({"app": "shell"}), SHELL, &mut d);
        assert_eq!(twin_lookup(ACT, &act("editor", "set_content"), &d), None, "the shell is read already");
    }

    /// The hint carries the family with its signatures — what Hermes used — not one bare name.
    #[test]
    fn the_twin_hint_names_the_whole_family_with_signatures() {
        let d = described_from_fixtures();
        let hint = lower_grade_twin(
            ACT,
            &serde_json::json!({"app": "editor", "action": "set_content", "args": {"text": "x"}}),
            &d,
        )
        .expect("the twin exists on the shell");
        for sig in ["editor_new()", "editor_set_content(text)", "editor_save_as(path)"] {
            assert!(hint.contains(sig), "{sig} missing: {hint}");
        }
        assert!(!hint.contains("files_new_folder"), "only the editor's family: {hint}");
    }

    /// The desktop's refusal from Reading D's T5, as the mind received it.
    const NO_FILES_APP: &str = "Done \u{2014} REFUSED \u{2014} nothing was run. refused: files.files_go was not run, because how the OS grades it could not be read: yos: no socket for 'files'";

    #[test]
    fn a_family_action_sent_to_a_missing_app_is_readdressed_to_the_shell() {
        assert_eq!(missing_app(NO_FILES_APP), Some("files".to_string()));
        assert_eq!(missing_app("Done \u{2014} files screen"), None);
        let d = described_from_fixtures();
        let go = serde_json::json!({"app": "files", "action": "files_go", "args": {"path": "/home/yantrik"}});
        let fixed = host_redirect(ACT, &go, NO_FILES_APP, &d).expect("the shell lists files_go");
        assert_eq!(fixed["app"], "shell");
        assert_eq!(fixed["action"], "files_go");
        assert_eq!(fixed["args"], go["args"], "the arguments are the model's own");
        let short = serde_json::json!({"app": "files", "action": "new_folder", "args": {"name": "x"}});
        assert_eq!(host_redirect(ACT, &short, NO_FILES_APP, &d).unwrap()["action"], "files_new_folder");
    }

    #[test]
    fn nothing_is_readdressed_without_the_desktops_word_and_the_shells_listing() {
        let d = described_from_fixtures();
        let go = serde_json::json!({"app": "files", "action": "files_go"});
        assert_eq!(host_redirect(ACT, &go, "Done \u{2014} files screen", &d), None, "accepted: untouched");
        let nope = serde_json::json!({"app": "files", "action": "levitate"});
        assert_eq!(host_redirect(ACT, &nope, NO_FILES_APP, &d), None, "the shell does not list it");
        let other = serde_json::json!({"app": "photos", "action": "files_go"});
        assert_eq!(host_redirect(ACT, &other, NO_FILES_APP, &d), None, "a different app was missing");
        let shell = serde_json::json!({"app": "shell", "action": "files_go"});
        assert_eq!(host_redirect(ACT, &shell, NO_FILES_APP, &d), None);
        let unread = std::collections::HashMap::new();
        assert_eq!(host_redirect(ACT, &go, NO_FILES_APP, &unread), None, "shell unread: look first");
        assert_eq!(
            host_family_lookup(ACT, &go, NO_FILES_APP, &unread),
            Some(serde_json::json!({"app": "shell", "actions": "files_"}))
        );
        assert_eq!(host_family_lookup(ACT, &go, NO_FILES_APP, &d), None, "already read");
    }

    /// Reading E's T7, from the desktop's own result lines.
    #[test]
    fn the_unsaved_bit_follows_the_desktops_own_words() {
        let act = |action: &str| serde_json::json!({"app": "shell", "action": action});
        let mut u = false;
        update_unsaved(ACT, &act("editor_new"), "Done \u{2014} Yantrik \u{2014} editing \"untitled\", 0 words\naccepted: True", &mut u);
        assert!(!u, "an empty new document is not unsaved work");
        update_unsaved(ACT, &act("editor_set_content"), "Done \u{2014} Yantrik \u{2014} editing \"untitled\", 7 words, unsaved\naccepted: True, settled: True", &mut u);
        assert!(u, "the desktop said unsaved");
        update_unsaved(ACT, &act("editor_new"), "Done \u{2014} Yantrik \u{2014} editing \"untitled\", 0 words\naccepted: True", &mut u);
        assert!(u, "a new document does not keep the text that was written");
        update_unsaved(DESCRIBE, &act("x"), "Yantrik \u{2014} editing \"a.txt\", 3 words", &mut u);
        assert!(u, "reads never touch it");
        update_unsaved(ACT, &serde_json::json!({"app": "calendar", "action": "add_event"}), "Done \u{2014} Calendar \u{2014} September 2026", &mut u);
        assert!(u, "an action on no document says nothing about one");
        update_unsaved(ACT, &act("editor_save_as"), "Done \u{2014} Yantrik \u{2014} editing \"arena-minp0x.txt\", 3 words\naccepted: True, settled: True", &mut u);
        assert!(!u, "a save that took clears it");
        let mut e = false;
        update_unsaved(ACT, &serde_json::json!({"app": "editor", "action": "set_content"}), "Done \u{2014} Text Editor \u{2014} Untitled (no file yet), 2 lines, unsaved", &mut e);
        assert!(e, "the editor app's own line counts too");
        update_unsaved(ACT, &serde_json::json!({"app": "editor", "action": "save_as"}), "Done \u{2014} Text Editor \u{2014} notes.txt, 2 lines, saved \u{b7} Recovered unsaved drafts", &mut e);
        assert!(!e, "\"unsaved drafts\" elsewhere on the line is not this document");
    }

    #[test]
    fn a_repeated_desktop_action_is_sent_onward_not_to_answer() {
        let editor = serde_json::json!({"app": "editor", "action": "new"});
        let saved = repeated_action_note(ACT, &editor, false, None).unwrap();
        assert!(saved.contains("NEXT step") && !saved.contains("otherwise answer"), "{saved}");
        let unsaved = repeated_action_note(ACT, &editor, true, None).unwrap();
        assert!(unsaved.contains("`editor.save_as`") && !unsaved.contains("editor_save_as"), "{unsaved}");
        assert_eq!(repeated_action_note(DESCRIBE, &editor, true, None), None, "reads keep the loop's own nudge");
    }

    #[test]
    fn a_new_tab_while_unsaved_is_caught_whatever_its_text() {
        let a = serde_json::json!({"app": "editor", "action": "new", "args": "a\nb"});
        assert!(another_new_while_unsaved(ACT, &a, true));
        assert!(!another_new_while_unsaved(ACT, &a, false), "nothing unsaved: a new tab is fine");
        let shell = serde_json::json!({"app": "shell", "action": "editor_new"});
        assert!(another_new_while_unsaved(ACT, &shell, true));
        let save = serde_json::json!({"app": "editor", "action": "save_as", "args": {"path": "~/x"}});
        assert!(!another_new_while_unsaved(ACT, &save, true));
        assert!(!another_new_while_unsaved(DESCRIBE, &a, true));
    }

    /// E.ARENA1-F19: with the request's path, the exact call -- for the app the model is using.
    #[test]
    fn a_repeat_with_an_unsaved_document_gets_the_exact_save_call() {
        let editor = serde_json::json!({"app": "editor", "action": "new"});
        let n = repeated_action_note(ACT, &editor, true, Some("~/arena-x.txt")).unwrap();
        assert!(n.contains(r#"os_act {"app": "editor", "action": "save_as", "args": {"path": "~/arena-x.txt"}}"#), "{n}");
        let shell = serde_json::json!({"app": "shell", "action": "editor_set_content"});
        let n = repeated_action_note(ACT, &shell, true, Some("/home/y/a.txt")).unwrap();
        assert!(n.contains(r#""action": "editor_save_as""#), "{n}");
        assert_eq!(
            requested_path("Create a text file at ~/arena-minjk8.txt containing exactly this line: hello").as_deref(),
            Some("~/arena-minjk8.txt")
        );
        assert_eq!(
            requested_path("Write the titles ... into a new file ~/arena-x-friday.txt, one title per line.").as_deref(),
            Some("~/arena-x-friday.txt")
        );
        assert_eq!(requested_path("What is on my calendar on 25 September?"), None);
    }

    /// The real shell description: `apps` (~3,000 characters) sorts before `clock`, and the head
    /// cut the clock off every time. It is kept now -- today's string, and #207's object whether
    /// it arrives inline or pretty-printed.
    #[test]
    fn the_desktop_clock_survives_condensing() {
        let today = condense_description(SHELL).unwrap();
        assert!(today.contains("\"clock\": \"14:23\""), "the clock was cut: {}", &today[..today.find(CONDENSED_MARK).unwrap()]);
        let inline = SHELL.replace(
            "  \"clock\": \"14:23\",",
            "  \"clock\": {\"date\": \"2026-09-23\", \"weekday\": \"Wednesday\", \"time\": \"18:31\", \"utc_offset\": \"-05:00\", \"zone\": \"America/Chicago\"},",
        );
        assert_ne!(inline, SHELL, "the fixture's clock line moved; update this test");
        let c = condense_description(&inline).unwrap();
        assert!(c.contains("\"weekday\": \"Wednesday\"") && c.contains("\"zone\": \"America/Chicago\""), "{c}");
        let pretty = SHELL.replace(
            "  \"clock\": \"14:23\",",
            "  \"clock\": {\n    \"date\": \"2026-09-23\",\n    \"weekday\": \"Wednesday\",\n    \"time\": \"18:31\",\n    \"utc_offset\": \"-05:00\",\n    \"zone\": \"\"\n  },",
        );
        let c = condense_description(&pretty).unwrap();
        let kept = c.lines().find(|l| l.starts_with("  \"clock\": ")).expect("kept on one line");
        assert!(kept.ends_with("\"zone\": \"\" }"), "whole, to its matching brace: {kept}");
        assert_eq!(c.matches("\"clock\": ").count(), 1, "kept once");
    }

    /// The real describe shell after yantrik-os #251 (VM 520, 57dbd6f), its values redacted but
    /// its key order and sizes kept: `agents` (~11.5k) and `catalog` (~4.5k) now sort before
    /// `clock`, so the head cannot reach it -- the clock object is kept all the same.
    #[test]
    fn the_251_clock_object_survives_the_real_key_order() {
        const SHELL_251: &str = include_str!("../fixtures/desktop/describe_shell_251.txt");
        let head: String = SHELL_251.chars().take(DESKTOP_STATE_HEAD).collect();
        assert!(!head.contains("\"clock\": "), "the fixture no longer puts the clock past the head");
        let c = condense_description(SHELL_251).unwrap();
        let kept = c.lines().find(|l| l.starts_with("  \"clock\": ")).expect("the clock was cut");
        assert!(kept.contains("\"weekday\": \"Thursday\"") && kept.contains("\"utc_offset\": \"-05:00\""), "{kept}");
    }

    #[test]
    fn only_a_top_level_field_is_kept_and_only_when_small() {
        let state = "Head\n{\n  \"a\": 1,\n  \"inner\": {\n    \"clock\": \"nested\"\n  },\n  \"clock\": \"12:00\"\n}";
        assert_eq!(kept_field(state, "clock").as_deref(), Some("  \"clock\": \"12:00\""));
        let big = format!("Head\n{{\n  \"clock\": \"{}\"\n}}", "x".repeat(400));
        assert_eq!(kept_field(&big, "clock"), None, "a kept field stays small");
        assert_eq!(kept_field(state, "absent"), None);
    }

    const EDITOR_253: &str = include_str!("../fixtures/desktop/describe_editor_253.txt");

    /// yantrik-os #253's editor surface, captured on VM 520 through yos-mcp: `set_content` is
    /// pointed at `new(text?)` then `save_as(path)`, with no shell read and no shell twin needed.
    #[test]
    fn set_content_is_pointed_at_new_with_text_on_the_253_editor() {
        let mut d = std::collections::HashMap::new();
        record_described(&serde_json::json!({"app": "editor"}), EDITOR_253, &mut d);
        let write = serde_json::json!({"app": "editor", "action": "set_content", "args": {"text": "x"}});
        let hint = lower_grade_twin(ACT, &write, &d).expect("the editor offers the route itself");
        assert!(hint.contains("`new(text?)` graded standard") && hint.contains("`save_as(path)`"), "{hint}");
        assert!(hint.contains("send this call again"), "the person's door stays open: {hint}");
        assert_eq!(twin_lookup(ACT, &write, &d), None, "no shell read when the app has the route");
        // Both the app route and the shell twin present (before #253 removes the twin): the
        // shorter route wins.
        record_described(&serde_json::json!({"app": "shell"}), SHELL, &mut d);
        assert!(lower_grade_twin(ACT, &write, &d).unwrap().contains("`new(text?)`"));
    }

    /// The route is read off the listing: the pre-#253 editor, whose `new()` takes no text, still
    /// gets the shell twin; and only `set_content` is rerouted.
    #[test]
    fn the_same_app_route_needs_new_with_text_and_only_serves_set_content() {
        let d = described_from_fixtures(); // the pre-#253 editor + shell
        let write = serde_json::json!({"app": "editor", "action": "set_content"});
        let hint = lower_grade_twin(ACT, &write, &d).unwrap();
        assert!(hint.contains("`editor_set_content`") && !hint.contains("`new(text?)`"), "{hint}");
        let mut d253 = std::collections::HashMap::new();
        record_described(&serde_json::json!({"app": "editor"}), EDITOR_253, &mut d253);
        assert_eq!(same_app_route("editor", "discard", "sensitive", &d253), None);
        // No card-free way to write the new tab to disk: no route is offered.
        for l in d253.get_mut("editor").unwrap().iter_mut().filter(|l| l.name == "save_as") {
            l.grade = "sensitive".into();
        }
        assert_eq!(same_app_route("editor", "set_content", "sensitive", &d253), None);
    }

    #[test]
    fn an_undescribed_apps_grades_are_read_before_acting_on_it() {
        let write = serde_json::json!({"app": "editor", "action": "set_content", "args": {"text": "x"}});
        let empty = std::collections::HashMap::new();
        assert_eq!(grade_lookup(ACT, &write, &empty), Some(serde_json::json!({"app": "editor"})));
        let d = described_from_fixtures();
        assert_eq!(grade_lookup(ACT, &write, &d), None, "already described this turn");
        let shell = serde_json::json!({"app": "shell", "action": "files_go"});
        assert_eq!(grade_lookup(ACT, &shell, &empty), None, "the shell is F8's to read");
        assert_eq!(grade_lookup(DESCRIBE, &serde_json::json!({"app": "editor"}), &empty), None);
    }

    /// The OS's own sentences (yos-mcp `guard_act`), as the mind receives them.
    const NO_ANSWER: &str = "Done \u{2014} REFUSED \u{2014} nothing was run. refused: the person at the machine was asked to allow editor.set_content and did not answer within 110s, so nothing was run. They were probably away from the keyboard. Tell them what you were trying to do; they can ask you to try it again.";
    const SAID_NO: &str = "REFUSED \u{2014} nothing was run. refused: the person at the machine was asked to allow editor.set_content and said no. Nothing was run and nothing was changed. This is an answer, not an error: do not ask again and do not look for another route to the same effect. Say what you were going to do and leave it with them.";
    const LOST: &str = "refused: the person was shown a card asking them to allow editor.set_content, and then the desktop stopped answering: timed out. It was asked again every 2s for 110s and never replied. Nothing was run here.";

    #[test]
    fn the_persons_answer_is_read_off_the_desktops_own_refusal() {
        assert_eq!(approval_answer(NO_ANSWER), Some(("editor.set_content".into(), Answer::NoAnswer)));
        assert_eq!(approval_answer(SAID_NO), Some(("editor.set_content".into(), Answer::SaidNo)));
        assert_eq!(approval_answer(LOST), None, "nobody knows what they did");
        assert_eq!(approval_answer("Done \u{2014} editing arena.txt"), None);
    }

    /// Reading C's T7 second card, and the rule that a no covers the twin.
    #[test]
    fn an_answered_card_is_not_raised_again_and_a_no_covers_the_twin() {
        let act = |app: &str, action: &str, text: &str| {
            serde_json::json!({"app": app, "action": action, "args": {"text": text}})
        };
        let mut answered = std::collections::HashMap::new();
        answered.insert("editor.set_content".to_string(), Answer::NoAnswer);
        let again = already_answered(ACT, &act("editor", "set_content", "a\nb"), &answered)
            .expect("different arguments, same card");
        assert!(again.contains("did not answer"), "{again}");
        assert_eq!(
            already_answered(ACT, &act("shell", "editor_set_content", "a"), &answered),
            None,
            "no answer is not a no: the door that does not ask stays open"
        );
        answered.insert("editor.set_content".to_string(), Answer::SaidNo);
        let twin = already_answered(ACT, &act("shell", "editor_set_content", "a"), &answered)
            .expect("a no covers the same change through the other door");
        assert!(twin.contains("their no covers it"), "{twin}");
        let mut by_shell = std::collections::HashMap::new();
        by_shell.insert("shell.editor_set_content".to_string(), Answer::SaidNo);
        assert!(already_answered(ACT, &act("editor", "set_content", "a"), &by_shell).is_some());
        assert_eq!(already_answered(ACT, &act("editor", "save_as", "a"), &answered), None);
        assert_eq!(already_answered(DESCRIBE, &serde_json::json!({"app": "editor"}), &answered), None);
    }

    fn described_from_fixtures() -> std::collections::HashMap<String, ActionList> {
        let mut d = std::collections::HashMap::new();
        record_described(&serde_json::json!({"app": "editor"}), EDITOR, &mut d);
        record_described(&serde_json::json!({"app": "shell"}), SHELL, &mut d);
        d
    }

    /// Reading B4's T6/T7, from the real descriptions: the sensitive editor door is pointed at its
    /// standard twin on the shell.
    #[test]
    fn a_sensitive_action_is_pointed_at_its_standard_twin() {
        let d = described_from_fixtures();
        let hint = lower_grade_twin(
            "mcp.yantrik-os.os_act",
            &serde_json::json!({"app": "editor", "action": "set_content", "args": {"text": "x"}}),
            &d,
        )
        .expect("the twin exists on the shell");
        assert!(hint.contains("`editor_set_content`") && hint.contains("graded standard"), "{hint}");
        assert!(hint.contains("send this call again"), "the person's door stays open: {hint}");
    }

    /// No hint where there is no lower door, where the action is already standard, or where the
    /// app was never described this turn.
    /// E.ARENA1-F35 on the real bytes (520, ac4473c9): the refusal names the action, its grade and
    /// the limit; the note lists Blender's own actions within the limit, never the refused one.
    #[test]
    fn a_refusal_above_the_limit_shows_what_the_app_offers_within_it() {
        let f = |n: &str| std::fs::read_to_string(format!("{}/fixtures/desktop/{n}", env!("CARGO_MANIFEST_DIR"))).unwrap();
        let refused = f("act_blender_run_python_refused_ac4473c9.txt");
        let r = ceiling_refusal(&refused).expect("the guard's refusal was not read");
        assert_eq!((r.app.as_str(), r.action.as_str(), r.grade.as_str(), r.ceiling.as_str()), ("blender", "run_python", "dangerous", "sensitive"));
        let own = ceiling_refusal("CEILING: blender.run_python is graded `dangerous`, above this machine's `sensitive` ceiling (`tool_permission` in ~/.config/yantrik/settings.yaml), so it was not run.").unwrap();
        assert_eq!(own, r, "the app's own CEILING form reads the same");
        let mut d = std::collections::HashMap::new();
        record_described(&serde_json::json!({"app": "blender"}), &f("describe_blender_ac4473c9.txt"), &mut d);
        let note = within_ceiling(&r, &d).expect("Blender lists actions within the limit");
        for a in ["add_primitive(", "set_material(", "set_light(", "set_camera(", "set_render("] {
            assert!(note.contains(a), "{a} missing: {note}");
        }
        assert!(note.contains("render(") && note.contains("[sensitive, asks the person first]"), "{note}");
        assert!(!note.contains("run_python("), "the refused action was offered back: {note}");
        // A machine held at `standard`: the sensitive actions are above the limit too, not only the
        // refused one (the real listing, the app's own CEILING form).
        let low = ceiling_refusal("CEILING: blender.save is graded `sensitive`, above this machine's `standard` ceiling (`tool_permission` in ~/.config/yantrik/settings.yaml), so it was not run.").unwrap();
        let note = within_ceiling(&low, &d).expect("Blender lists standard actions");
        assert!(note.contains("add_primitive("), "{note}");
        for above in ["  render(", "; render(", "open(", "run_python("] {
            assert!(!note.contains(above), "{above} is above a standard limit: {note}");
        }
        assert_eq!(ceiling_refusal("REFUSED \u{2014} nothing was run. refused: shell.files_create_folder was not run, because how the OS grades it could not be read"), None, "another refusal");
    }

    /// E.ARENA1-F38 (yantrik-os #478): a screen read is stale after an act, like any desktop read --
    /// "look, act, look again" must look again, not be handed the screen from before the act.
    #[test]
    fn a_screen_read_is_forgotten_after_an_act() {
        let mut done: std::collections::HashSet<String> =
            ["mcp.yantrik-os.os_screen|{}".to_string(), "web_search|{\"q\":\"x\"}".to_string()].into_iter().collect();
        forget_desktop_reads(&mut done);
        assert!(!done.contains("mcp.yantrik-os.os_screen|{}"), "the pre-act screen would be served again");
        assert!(done.contains("web_search|{\"q\":\"x\"}"), "a non-desktop read was forgotten");
    }

    /// E.ARENA1-F39: an OCR read reached through `os_act` is a read -- stale after a real act,
    /// never a change of its own.
    #[test]
    fn a_read_that_arrives_as_an_act_is_still_a_read() {
        let read = serde_json::json!({"app": "shell", "action": "read_screen"});
        let change = serde_json::json!({"app": "editor", "action": "new", "args": {"text": "x"}});
        let odd = serde_json::json!({"app": "shell", "action": "open_app", "args": {"name": "editor"}});
        assert!(is_read_act(ACT, &read) && is_read_act(ACT, &serde_json::json!({"app": "shell", "action": "read_mind_view"})));
        assert!(!is_read_act(ACT, &change) && !is_read_act(ACT, &odd), "a change is not a read");
        assert!(!is_read_act(DESCRIBE, &read), "only an act can be a read act");
        let mut done: std::collections::HashSet<String> =
            [format!("{ACT}|{read}"), format!("{ACT}|{change}")].into_iter().collect();
        forget_desktop_reads(&mut done);
        assert!(!done.contains(&format!("{ACT}|{read}")), "the pre-act screen reading would be served again");
        assert!(done.contains(&format!("{ACT}|{change}")), "a change was forgotten -- it could be made twice");
    }

    /// E.ARENA1-F40: the fold is yos-mcp's `surface_name`, on its own documented spellings.
    #[test]
    fn the_app_is_folded_as_the_desktop_folds_it() {
        for spelled in ["shell", "Shell", " app_shell", "App Shell", "app-shell", "APP__SHELL"] {
            assert_eq!(fold_app(spelled), "shell", "{spelled:?}");
        }
        assert_eq!(fold_app("Container Manager"), "container-manager");
        assert_eq!(fold_app("notes"), "notes");
        let read = fold_desktop_args(ACT, serde_json::json!({"app": "App Shell", "action": "read_screen"}));
        assert!(is_read_act(ACT, &read), "{read}");
        assert_eq!(
            format!("{DESCRIBE}|{}", fold_desktop_args(DESCRIBE, serde_json::json!({"app": "Shell"}))),
            format!("{DESCRIBE}|{}", fold_desktop_args(DESCRIBE, serde_json::json!({"app": "shell"}))),
            "two spellings of one read are one repeat identity"
        );
        let web = serde_json::json!({"app": "App Shell"});
        assert_eq!(fold_desktop_args("web_search", web.clone()), web, "another tool's args were touched");
    }

    /// E.ARENA1-F41 (yantrik-os #480): browser reads go stale after a browser change; a read
    /// forgets nothing; the browser's own reads through os_act are read acts; `chromium` is the browser.
    #[test]
    fn the_browsers_reads_and_changes_are_told_apart() {
        let read = "mcp.yantrik-os.web_read|{}".to_string();
        let find = "mcp.yantrik-os.web_find|{\"text\":\"Buy\"}".to_string();
        let click = "mcp.yantrik-os.web_click|{\"ref\":\"e12\"}".to_string();
        assert!(changes_the_desktop("mcp.yantrik-os.web_click") && changes_the_desktop("mcp.yantrik-os.web_type"));
        assert!(changes_the_desktop("mcp.yantrik-os.web_scroll"), "a scroll changes what a read sees");
        assert!(!changes_the_desktop("mcp.yantrik-os.web_read") && !changes_the_desktop("mcp.yantrik-os.web_tabs"));
        let mut done: std::collections::HashSet<String> = [read.clone(), find.clone(), click.clone()].into_iter().collect();
        forget_desktop_reads(&mut done);
        assert!(!done.contains(&read) && !done.contains(&find), "a page read would be served stale after a click");
        assert!(done.contains(&click), "a click was forgotten -- it could be sent twice");
        assert!(is_read_act(ACT, &serde_json::json!({"app": "browser", "action": "text"})));
        assert!(!is_read_act(ACT, &serde_json::json!({"app": "browser", "action": "click", "args": {"ref": "e3"}})));
        assert!(repeatable("mcp.yantrik-os.web_scroll") && !repeatable("mcp.yantrik-os.web_click"));
        assert_eq!(fold_app("Chromium"), "browser");
        assert_eq!(fold_app("app-browser"), "browser");
    }

    /// E.WEBGATE1: exactly the desktop server's web_* tools -- no other server, no other prefix.
    #[test]
    fn only_the_desktops_browser_tools_are_gated_by_the_desktop() {
        let t = |server: &str, name: &str| mind_tools::McpTool {
            server: server.into(),
            name: name.into(),
            description: String::new(),
            read_only: false,
            open_world: true,
            destructive: false,
            input_schema: serde_json::json!({}),
        };
        assert!(the_desktop_gates(&t("yantrik-os", "web_go")) && the_desktop_gates(&t("yantrik-os", "web_commit")));
        assert!(!the_desktop_gates(&t("somewhere-else", "web_go")), "another server's web_ tool");
        assert!(!the_desktop_gates(&t("yantrik-os", "os_act")), "a desktop tool that is not the browser");
        assert!(!the_desktop_gates(&t("yantrik-os", "webhook_send")), "a lookalike prefix");
    }

    #[test]
    fn no_hint_without_a_real_lower_grade_twin() {
        let d = described_from_fixtures();
        let act = |app: &str, action: &str| {
            lower_grade_twin("mcp.yantrik-os.os_act", &serde_json::json!({"app": app, "action": action}), &d)
        };
        assert_eq!(act("editor", "discard"), None, "sensitive, but the shell offers no twin");
        assert_eq!(act("editor", "save_as"), None, "already standard");
        assert_eq!(act("calendar", "delete_event"), None, "not described this turn");
        assert_eq!(
            lower_grade_twin("mcp.yantrik-os.os_describe", &serde_json::json!({"app": "editor"}), &d),
            None
        );
    }

    /// The line yos-mcp returned for `editor.new{text}` on 0f3e733, verbatim.
    #[test]
    fn cli_advice_is_said_the_way_a_model_can_act_on_it() {
        let got = "Text Editor \u{2014} Untitled (no file yet), 1 line, unsaved \u{b7} tab 2 of 2\naccepted: True, settled: True\n{\n  \"bytes\": 10\n}\n(state omitted; `yos describe editor`, or re-run with --full)";
        let v = mcp_voice(got);
        assert!(!v.contains("re-run") && !v.contains("--full"), "{v}");
        assert!(v.ends_with("(state trimmed; os_describe editor shows all of it)"), "{v}");
        assert!(v.starts_with("Text Editor \u{2014} Untitled (no file yet), 1 line, unsaved"), "the first line is untouched");
        assert_eq!(mcp_voice("Done \u{2014} Calendar"), "Done \u{2014} Calendar");
    }

    /// E.ARENA1-F21: which requests have a path to check, and where it is.
    /// E.HOME1: only NotFound is absence; a permission error says nothing.
    #[test]
    fn only_not_found_is_missing() {
        use std::io::{Error, ErrorKind};
        assert_eq!(classify_lookup(Ok(())), Some(true));
        assert_eq!(classify_lookup(Err(Error::from(ErrorKind::NotFound))), Some(false));
        assert_eq!(classify_lookup(Err(Error::from(ErrorKind::PermissionDenied))), None, "not allowed to look is not missing");
        assert_eq!(classify_lookup(Err(Error::other("io"))), None);
        assert_eq!(is_there(std::path::Path::new("/ym-home1-nowhere/x.txt")), Some(false));
    }

    #[test]
    fn a_goal_is_a_path_the_request_asks_to_have_made() {
        let t7 = "Write the titles of my calendar events on Friday into a new file ~/arena-x-friday.txt, one title per line.";
        assert_eq!(goal_path(t7).as_deref(), Some("~/arena-x-friday.txt"));
        assert_eq!(
            goal_path("Create a text file at ~/arena-minjk8.txt containing exactly this line: hello").as_deref(),
            Some("~/arena-minjk8.txt")
        );
        assert_eq!(goal_path("Save it as /tmp/notes.txt").as_deref(), Some("/tmp/notes.txt"));
        assert_eq!(goal_path("Delete ~/old.txt"), None, "a delete is not asking for the file");
        assert_eq!(goal_path("Move ~/a.txt somewhere tidy"), None);
        assert_eq!(goal_path("Rename ~/a.txt, then write a note"), None);
        assert_eq!(goal_path("What is in ~/notes.txt?"), None, "a question makes nothing");
        assert_eq!(goal_path("Write me a poem about rain"), None, "no path, no goal");
        assert_eq!(
            on_this_machine("~/a.txt", Some("/home/yantrik")),
            Some(std::path::PathBuf::from("/home/yantrik").join("a.txt"))
        );
        assert_eq!(on_this_machine("~/a.txt", None), None, "no home: nothing to look at");
        assert_eq!(on_this_machine("~/a.txt", Some("  ")), None);
        assert_eq!(on_this_machine("/tmp/a.txt", None), Some(std::path::PathBuf::from("/tmp/a.txt")));
        assert!(goal_missing_note("~/a.txt").contains("~/a.txt"));
        assert!(goal_nudge(3, "~/a.txt", None).contains("save_as"));
        assert!(goal_nudge(3, "~/d/a.txt", Some("~/d")).contains("The folder ~/d does not exist yet"));
        assert!(!goal_nudge(3, "~/a.txt", None).contains("does not exist yet"));
        assert_eq!(missing_folder_of("~/a.txt", Some("/home/y")), None, "home itself is never the missing folder");
        assert_eq!(missing_folder_of("/ym-f33-nowhere/sub/a.txt", None).as_deref(), Some("/ym-f33-nowhere/sub"));
    }

    /// yantrik-os #384 adds a `state: {…}` line to every action result, and keeps the prose first
    /// line the unsaved scrape reads. Real captures from VM 520 (the lines up to and including
    /// `state:`; the result object that follows is not needed here): the scrape still says unsaved
    /// after `new`, saved after `save_as`, and mcp_voice leaves both untouched.
    #[test]
    fn the_state_line_leaves_the_unsaved_scrape_working() {
        const NEW: &str = include_str!("../fixtures/desktop/act_editor_new_384.txt");
        const SAVED: &str = include_str!("../fixtures/desktop/act_editor_save_as_384.txt");
        let mut unsaved = false;
        let new = serde_json::json!({"app": "editor", "action": "new", "args": {"text": "state line capture"}});
        update_unsaved(ACT, &new, NEW, &mut unsaved);
        assert!(unsaved, "after new");
        let save = serde_json::json!({"app": "editor", "action": "save_as", "args": {"path": "/tmp/state-capture.txt"}});
        update_unsaved(ACT, &save, SAVED, &mut unsaved);
        assert!(!unsaved, "after save_as");
        assert_eq!(mcp_voice(NEW), NEW);
        assert_eq!(mcp_voice(SAVED), SAVED);
    }

    /// E.ARENA1-F23 on the desktop's real line shape (VM 520, 185b4c0, `open_app weather`).
    #[test]
    fn an_unsettled_result_carries_the_later_look() {
        let got = "Done \u{2014} Yantrik \u{2014} desktop screen, 0 windows open, calendar, email and notes not running\naccepted: True, settled: False\nrevision: 99e35624c6c";
        assert!(unsettled(got));
        assert!(!unsettled("Done \u{2014} Notes\naccepted: True, settled: True"));
        let open = serde_json::json!({"app": "shell", "action": "open_app", "args": {"name": "weather"}});
        assert_eq!(settle_look(ACT, &open), Some(serde_json::json!({"app": "weather"})), "F23b: the app opened");
        let go = serde_json::json!({"app": "shell", "action": "files_go", "args": {"path": "/home/yantrik"}});
        assert_eq!(settle_look(ACT, &go), Some(serde_json::json!({"app": "shell"})));
        let bare = serde_json::json!({"app": "shell", "action": "open_app"});
        assert_eq!(settle_look(ACT, &bare), Some(serde_json::json!({"app": "shell"})), "no name: the shell");
        let folder = serde_json::json!({"app": "shell", "action": "files_new_folder", "args": {"name": "arena-x"}});
        assert_eq!(settle_look(ACT, &folder), Some(serde_json::json!({"app": "shell"})), "a folder's name is not an app");
        assert_eq!(settle_look(DESCRIBE, &open), None);
        let later = settled_since(got, "Yantrik \u{2014} desktop screen, 1 windows open, calendar, email and notes not running\nrevision: 1\n{}");
        assert!(later.starts_with(got), "the desktop's own answer is kept");
        assert!(later.contains("Looked again 1.5 s later: Yantrik \u{2014} desktop screen, 1 windows open"), "{later}");
        let blind = settled_since(got, "");
        assert!(blind.contains("Look again with os_describe"), "{blind}");
    }

    /// E.ARENA1-F24 on real yos-mcp output from 185b4c0: the shell's result survives the cut; the
    /// calendar's, which already fitted, still does; nothing without a state line changes.
    #[test]
    fn a_desktop_action_result_is_read_before_its_state() {
        const FILES_GO: &str = include_str!("../fixtures/desktop/act_files_go_185b4c0.txt");
        const ADD_EVENT: &str = include_str!("../fixtures/desktop/act_add_event_185b4c0.txt");
        const SELECT_25: &str = include_str!("../fixtures/desktop/act_select_day_25_185b4c0.txt");
        let go = work_log_entry(3, ACT, FILES_GO, true, 900, "");
        assert!(go.contains("\"requested_path\": \"/home/yantrik\""), "{go}");
        assert!(go.contains("\"loading\": true"), "{go}");
        assert!(go.contains("accepted: True, settled: False"), "{go}");
        // yantrik-os #388 (e855ad7's predecessor c0b853d) prints the result before the state: F24 is a
        // no-op there, and the result is still in view.
        const FILES_GO_388: &str = include_str!("../fixtures/desktop/act_files_go_c0b853d.txt");
        let go388 = work_log_entry(3, ACT, FILES_GO_388, true, 900, "");
        assert!(go388.contains("\"requested_path\""), "{go388}");
        let add = work_log_entry(1, ACT, ADD_EVENT, true, 900, "");
        assert!(add.contains("\"added\": \"Arena f24probe\"") && add.contains("\"on\": \"2026-09-30 15:00\""), "{add}");
        let sel = work_log_entry(1, ACT, SELECT_25, true, 900, "");
        assert!(sel.contains("\"Perception API review\"") && sel.contains("\"Dentist\""), "{sel}");
        let plain = "Done \u{2014} Notes\naccepted: True, settled: True\n{\n  \"ok\": true\n}";
        assert_eq!(result_before_state(plain), plain);
        let moved = result_before_state(FILES_GO);
        assert_eq!(moved.lines().count(), FILES_GO.lines().count(), "nothing dropped");
        assert!(moved.lines().last().unwrap().starts_with("state: {"));
        assert_eq!(work_log_entry(1, DESCRIBE, FILES_GO, true, 900, ""), format!("\n[1] {DESCRIBE} -> {}", FILES_GO.chars().take(900).collect::<String>()), "only actions are reordered");
    }

    /// E.ARENA1-F25 on real yos-mcp output (185b4c0): the whole catalogue reaches the work log.
    #[test]
    fn the_app_catalogue_reaches_the_model_whole() {
        const APPS_185: &str = include_str!("../fixtures/desktop/os_apps_185b4c0.txt");
        let e = work_log_entry(0, APPS, APPS_185, true, 900, "");
        for name in ["image-viewer", "terminal", "presentation", "Screens of the desktop itself"] {
            assert!(e.contains(name), "{name} missing from the work log");
        }
        let other = work_log_entry(0, "web_fetch", APPS_185, true, 900, "");
        assert_eq!(other.chars().count(), format!("\n[0] web_fetch -> ").chars().count() + 900, "others keep the cut");
    }

    /// E.ARENA1-F26: R3's T3 sequence, verbatim arguments; and what must still pass.
    #[test]
    fn the_same_change_with_one_more_field_is_a_repeat() {
        let first = serde_json::json!({"action": "add_event", "app": "calendar", "args": {"date": "2026-09-30", "duration_min": 30, "time": "15:00", "title": "Arena minyk1"}});
        let again = serde_json::json!({"action": "add_event", "app": "calendar", "args": {"date": "2026-09-30", "duration_min": 30, "reminder_minutes": 10, "time": "15:00", "title": "Arena minyk1"}});
        let made = vec![(first.clone(), "Done \u{2014} Calendar \u{2014} September 2026, 3 things on day 25".to_string())];
        let n = same_change_again(ACT, &again, &made).expect("a superset of what ran is the same change");
        assert!(n.contains("calendar.add_event already ran") && n.contains("3 things on day 25"), "{n}");
        let later = serde_json::json!({"action": "add_event", "app": "calendar", "args": {"date": "2026-09-30", "duration_min": 30, "time": "16:00", "title": "Arena minyk1"}});
        assert_eq!(same_change_again(ACT, &later, &made), None, "another time is another event");
        let moved = serde_json::json!({"action": "update_own_event", "app": "calendar", "args": {"id": "01a0e225", "time": "16:30"}});
        let moved_again = serde_json::json!({"action": "update_own_event", "app": "calendar", "args": {"date": "2026-09-30", "id": "01a0e225", "time": "16:30"}});
        assert!(same_change_again(ACT, &moved_again, &[(moved.clone(), "Done".into())]).is_some());
        assert!(same_change_again(ACT, &moved, &[(moved_again, "Done".into())]).is_some(), "a subset too");
        let empty = serde_json::json!({"action": "new", "app": "editor"});
        let text = serde_json::json!({"action": "new", "app": "editor", "args": "hello"});
        assert_eq!(same_change_again(ACT, &text, &[(empty, "Done".into())]), None, "no arguments matches nothing");
        assert_eq!(same_change_again(ACT, &again, &[]), None, "nothing ran yet");
        assert_eq!(same_change_again(DESCRIBE, &again, &made), None);
    }

    /// E.ARENA1-F30 on 520's real first line (turn 390).
    #[test]
    fn a_loading_app_is_recognised_by_its_own_summary() {
        assert!(still_loading("Weather \u{2014} loading ... revision: 1cf0963ee3e2b9f6 {"));
        assert!(!still_loading("Weather \u{2014} 15\u{b0}C in London, Overcast, feels like 14\u{b0}C\nrevision: 57a1"));
        assert!(!still_loading("Files \u{2014} 21 entries\n{\"loading\": true}"), "only the first line, the app's own summary");
        let later = loaded_since("Weather \u{2014} loading ...", "Weather \u{2014} 15\u{b0}C in London\nrevision: 1");
        assert!(later.contains("Looked again 2.0 s later: Weather \u{2014} 15\u{b0}C in London"), "{later}");
    }

    /// E.ARENA1-F32 on yos-mcp's real token-mode list (VM 520, 557e4ef).
    #[test]
    fn the_token_does_not_widen_what_the_model_is_offered() {
        let t = |server: &str, name: &str| mind_tools::McpTool {
            server: server.into(),
            name: name.into(),
            description: String::new(),
            read_only: false,
            open_world: false,
            destructive: false,
            input_schema: serde_json::json!({}),
        };
        for n in ["os_apps", "os_describe", "os_act", "os_perception", "web_read", "web_go", "web_type"] {
            assert!(offered_to_the_model(&t("yantrik-os", n)), "{n} was offered before the token too");
        }
        for n in ["run_command", "command_status", "command_input", "command_kill", "new_agent", "send_to_agent", "stop_agent", "read_agent", "hand_off"] {
            assert!(!offered_to_the_model(&t("yantrik-os", n)), "{n} arrives only with the token");
        }
        assert!(offered_to_the_model(&t("github", "create_issue")), "other servers are unaffected");
    }

    /// E.ARENA1-F31 on the real crowded calendar (VM 520, 185b4c0).
    #[tokio::test]
    async fn what_the_request_names_survives_condensation() {
        const CROWDED: &str = include_str!("../fixtures/desktop/describe_calendar_crowded_185b4c0.txt");
        let id = "01a0e225-2ca5-705f-87ca-326ae6576b11";
        let plain = condense_description(CROWDED).unwrap();
        assert!(!plain.contains(id), "control: without focus the id is past the cut");
        let names = quoted_names("Move 'Arena mine9f' on 30 September to 16:30.");
        assert_eq!(names, vec!["Arena mine9f".to_string()]);
        let focused = FOCUS.scope(names, async { condense_description(CROWDED).unwrap() }).await;
        assert!(focused.contains(id), "the named event's id was not kept");
        assert!(focused.contains("kept because the request names it"));
        assert!(focused.len() < plain.len() + 3 * 700, "kept whole but bounded");
        assert_eq!(quoted_names("Open the notes app and don't close it"), Vec::<String>::new(), "no quotes, no focus");
        assert_eq!(quoted_names("Move \u{201c}Lunch with Sam\u{201d} to 2pm"), vec!["Lunch with Sam".to_string()]);
    }

    /// E.HOME3 on the real captures (520, df42338).
    #[test]
    fn the_desktops_file_answer_is_read_as_it_is_given() {
        let f = |name: &str| std::fs::read_to_string(format!("{}/fixtures/desktop/files_stat_{name}_df42338.txt", env!("CARGO_MANIFEST_DIR"))).unwrap();
        assert_eq!(parse_files_stat(&f("true")), Some(Stat::There));
        assert_eq!(parse_files_stat(&f("false")), Some(Stat::Missing));
        assert_eq!(parse_files_stat(&f("protected")), Some(Stat::Unknown));
        assert_eq!(parse_files_stat(&f("outside")), Some(Stat::Unknown));
        assert_eq!(parse_files_stat(&format!("Done \u{2014} {}", f("false"))), Some(Stat::Missing), "with the Mind's own prefix");
        assert_eq!(parse_files_stat("That didn't go through: execution failed"), None, "a refusal says nothing");
    }

    #[test]
    fn the_desktop_map_is_said_only_with_a_desktop() {
        let s = desktop_sentence(true);
        assert!(s.contains("written with the `editor` app") && s.contains("os_describe editor"), "{s}");
        assert!(s.contains("files_*"), "{s}");
        assert_eq!(desktop_sentence(false), "");
    }

    #[test]
    fn the_home_sentence_names_the_real_home_only_with_a_desktop() {
        assert!(home_sentence(true, Some("/home/yantrik")).contains("/home/yantrik"));
        assert_eq!(home_sentence(false, Some("/home/yantrik")), "");
        assert_eq!(home_sentence(true, Some("  ")), "");
    }

    /// Reading B‴: after opening Files, a second describe of `files` was served the pre-action
    /// "not running" from the log. An action forgets the reads it invalidated, and only those.
    #[test]
    fn an_action_forgets_the_reads_it_invalidated_and_nothing_else() {
        let mut done: std::collections::HashSet<String> = [
            r#"mcp.yantrik-os.os_describe|{"app":"files"}"#,
            "mcp.yantrik-os.os_apps|{}",
            r#"mcp.yantrik-os.os_act|{"app":"editor","action":"new"}"#,
            r#"web_fetch|{"url":"x"}"#,
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert!(changes_the_desktop("mcp.yantrik-os.os_act"));
        assert!(!changes_the_desktop("mcp.yantrik-os.os_describe"));
        forget_desktop_reads(&mut done);
        assert!(!done.iter().any(|s| s.starts_with("mcp.yantrik-os.os_describe|")));
        assert!(!done.iter().any(|s| s.starts_with("mcp.yantrik-os.os_apps|")));
        assert!(done.iter().any(|s| s.starts_with("mcp.yantrik-os.os_act|")), "a repeated action must stay deduplicated");
        assert!(done.contains(r#"web_fetch|{"url":"x"}"#), "nothing off the desktop is touched");
    }

    /// Reading B'': the mind searched its own tools, found nothing, and said it could not. The
    /// search must say where the desktop's actions are, and keep the phrase the outcome classifier
    /// reads as an empty result.
    #[test]
    fn the_search_points_at_the_desktops_own_actions() {
        for app in ["`shell`", "`editor`", "files_", "os_describe"] {
            assert!(DISCOVER_DESKTOP_NOTE.contains(app), "{app}");
        }
        assert!(DISCOVER_DESKTOP_NOTE.contains("this search does not cover them"));
    }

    /// A turn that never looked at the desktop is sent to look before it may refuse; one that
    /// looked keeps the old, honest exit.
    #[test]
    fn a_turn_that_never_looked_must_look_before_refusing() {
        assert!(must_look_first(true, &[]));
        assert!(must_look_first(true, &["discover_tools"]), "searching its own tools is not looking");
        assert!(
            must_look_first(true, &["mcp.yantrik-os.os_apps"]),
            "listing what is open is not looking at what anything can do"
        );
        assert!(!must_look_first(true, &["discover_tools", "mcp.yantrik-os.os_describe"]));
        assert!(!must_look_first(false, &[]), "no desktop, no desktop to look at");
        let n = look_first_nudge(2, "create a folder");
        assert!(n.contains("EARLIER turns is not evidence"), "{n}");
        assert!(!n.contains("or say plainly that you cannot"), "the old exit is the defect: {n}");
    }

    /// The arena's T5/T6 on the same model Hermes passes with: the 6,000-character MCP cap cut the
    /// shell before its first action. Pinned from the real description.
    #[test]
    fn the_mcp_cap_no_longer_cuts_the_desktop_off_its_actions() {
        let capped: String = SHELL.chars().take(MCP_OUTPUT_CAP).collect();
        assert!(!capped.contains("files_new_folder"), "the defect this test exists for");
        let bounded = bound_mcp_output(SHELL, true);
        assert!(bounded.contains("files_new_folder(name)") && bounded.contains("editor_save_as(path)"));
        assert!(bounded.len() < SHELL.len() / 2, "still bounded: {} bytes", bounded.len());
    }

    /// Data from somewhere else keeps its cap, description-shaped or not.
    #[test]
    fn an_off_machine_tool_keeps_the_cap() {
        let out = bound_mcp_output(SHELL, false);
        assert_eq!(out.chars().count(), MCP_OUTPUT_CAP);
        assert!(!out.contains("files_new_folder"));
    }

    /// The boundary condenses and the work log condenses again: the second pass is a no-op.
    #[test]
    fn condensing_twice_changes_nothing() {
        for doc in [CALENDAR, SHELL] {
            let once = condense_description(doc).unwrap();
            assert_eq!(condense_description(&once).unwrap(), once);
        }
    }

    /// The loop's log line for the real calendar description carries `update_event` — the action
    /// the arena's T4 needed and the 900-character clip withheld.
    #[test]
    fn the_work_log_line_for_a_desktop_description_carries_its_actions() {
        let line = work_log_entry(0, "mcp.yantrik-os.os_describe", CALENDAR, true, 900, "");
        assert!(line.contains("update_event("), "{line}");
        let shell = work_log_entry(1, "mcp.yantrik-os.os_describe", SHELL, true, 900, "");
        assert!(shell.contains("files_new_folder(name)"));
    }

    /// Only desktop tools, and only successful results, are condensed; everything else keeps the
    /// clip it always had.
    #[test]
    fn other_tools_and_failures_keep_the_old_clip() {
        let other = work_log_entry(0, "web_fetch", CALENDAR, true, 900, "");
        assert!(!other.contains("update_event"));
        let failed = work_log_entry(0, "mcp.yantrik-os.os_describe", CALENDAR, false, 300, " (failed)");
        assert!(!failed.contains("update_event") && failed.ends_with(" (failed)"));
    }

    /// The defect, pinned from the real description: the old 900-character clip never reaches
    /// `update_event` or `files_new_folder`. Kept as a test so the reason for this module is
    /// checkable, not remembered.
    #[test]
    fn the_old_clip_never_reached_the_actions_the_tasks_needed() {
        let clip = |s: &str| s.chars().take(900).collect::<String>();
        assert!(!clip(CALENDAR).contains("update_event"));
        assert!(!clip(SHELL).contains("files_new_folder"));
        assert!(!clip(SHELL).contains("editor_save_as"));
    }

    #[test]
    fn every_action_signature_survives_condensing() {
        for (name, doc) in [("calendar", CALENDAR), ("shell", SHELL)] {
            let c = condense_description(doc).unwrap_or_else(|| panic!("{name} not recognised"));
            for sig in doc.lines().filter(|l| l.starts_with("  act: ")) {
                assert!(c.contains(sig), "{name}: lost {sig}");
            }
        }
        let cal = condense_description(CALENDAR).unwrap();
        assert!(cal.contains("update_event(id, date?, duration_min?, notes?, time?, title?)"));
        let shell = condense_description(SHELL).unwrap();
        assert!(shell.contains("files_new_folder(name)") && shell.contains("editor_save_as(path)"));
    }

    /// The point of condensing rather than raising the clip: the shell's 22.6 KB description must
    /// come out small enough to re-send on every step.
    #[test]
    fn a_huge_description_comes_out_bounded() {
        let shell = condense_description(SHELL).unwrap();
        assert!(SHELL.len() > 20_000);
        assert!(
            shell.len() <= DESKTOP_STATE_HEAD * 4 + DESKTOP_ACTIONS_BUDGET + 200,
            "condensed shell is {} bytes",
            shell.len()
        );
    }

    #[test]
    fn a_small_description_keeps_its_action_details() {
        let cal = condense_description(CALENDAR).unwrap();
        assert!(cal.contains("Show what is on one day"), "{cal}");
        assert!(!cal.contains("trimmed; every action"), "a 2 KB description should need no trimming");
    }

    #[test]
    fn anything_that_is_not_a_description_is_left_to_the_old_clip() {
        assert_eq!(condense_description("Done — notes screen, 2 windows open"), None);
        assert_eq!(condense_description(""), None);
    }

    fn tool(server: &str, name: &str) -> McpTool {
        McpTool {
            server: server.into(),
            name: name.into(),
            description: String::new(),
            read_only: false,
            open_world: false,
            destructive: false,
            input_schema: serde_json::json!({}),
        }
    }

    #[test]
    fn the_desktop_is_attached_only_when_its_own_tools_are_connected() {
        assert!(desktop_attached(&[tool("yantrik-os", "os_describe")]));
        assert!(!desktop_attached(&[tool("github", "search")]));
        assert!(!desktop_attached(&[]));
    }

    /// The arena's T2/T3/T4: every private calendar-event tool yields to the desktop, and says
    /// that nothing was read or changed — so a reply built on it cannot become "Added".
    #[test]
    fn every_private_calendar_event_tool_yields_to_the_desktop() {
        for t in SUPERSEDED_BY_DESKTOP {
            let msg = superseded(t, true).unwrap_or_else(|| panic!("{t} ran against the private store"));
            assert!(msg.contains("Nothing was read or changed"), "{msg}");
            assert!(msg.contains("os_act calendar add_event"), "{msg}");
        }
    }

    #[test]
    fn without_a_desktop_the_private_calendar_is_the_calendar() {
        for t in SUPERSEDED_BY_DESKTOP {
            assert_eq!(superseded(t, false), None, "{t}");
        }
    }

    /// A person's dated entry is not a calendar event, and the desktop does not model it.
    #[test]
    fn forget_date_is_not_superseded() {
        assert_eq!(superseded("forget_date", true), None);
        assert!(CALENDAR_ON_DESKTOP.contains("forget_date"));
    }

    /// The menu entry that replaces the private one must not name a private event tool, or the
    /// model is still offered two calendars.
    #[test]
    fn the_desktop_menu_entry_offers_one_calendar() {
        for t in ["calendar_add", "calendar_remove", "calendar {}"] {
            assert!(!CALENDAR_ON_DESKTOP.contains(t), "still offers {t}");
        }
        assert!(CALENDAR_ON_DESKTOP.contains("There is no other calendar on this machine"));
    }
}
