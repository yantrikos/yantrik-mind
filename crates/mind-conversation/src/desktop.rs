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
    let lines: Vec<&str> = obs.lines().collect();
    let first_act = lines.iter().position(|l| l.starts_with("  act: "))?;
    let state = lines[..first_act].join("\n");
    let mut out: String = state.chars().take(DESKTOP_STATE_HEAD).collect();
    if state.chars().count() > DESKTOP_STATE_HEAD {
        out.push_str("\n… (state trimmed)");
    }
    out.push_str("\nACTIONS:");

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
