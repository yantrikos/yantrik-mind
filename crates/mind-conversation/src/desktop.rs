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

#[cfg(test)]
mod tests {
    use super::*;

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
