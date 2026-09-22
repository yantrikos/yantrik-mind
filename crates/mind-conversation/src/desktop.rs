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
