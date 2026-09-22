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
    tool == "mcp.yantrik-os.os_act"
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

/// Desktop READS — the calls whose answer depends on the desktop's current state.
const DESKTOP_READS: [&str; 3] = [
    "mcp.yantrik-os.os_describe",
    "mcp.yantrik-os.os_apps",
    "mcp.yantrik-os.os_perception",
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
    done.retain(|sig| !DESKTOP_READS.iter().any(|r| sig.starts_with(&format!("{r}|"))));
}

/// The call that changes the desktop.
pub(crate) const ACT: &str = "mcp.yantrik-os.os_act";

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
fn act_target(tool: &str, args: &serde_json::Value) -> Option<(String, String)> {
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
    if !asks_first(grade_of(described, &app, &action)?) {
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
    .unwrap_or_else(|| obs.chars().take(head).collect::<String>());
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
