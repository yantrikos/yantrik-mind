//! E.DONE1 (Pranab, 1 Nov: "tasks should never fail unless its access issue or limitations"): before a
//! turn that used tools gives its answer, each part of the request is held against the turn's record:
//! done, not done for a reason the record gives, or asked. "Gave up" is not one of them.
//!
//! The check is a model, so it fails open: a reply that does not fit the schema changes nothing. Which
//! sentences it calls unsupported is journalled only (SHADOW) until a battery has graded them.

/// The part of the work log the check reads, from the end.
const RECORD_KEPT: usize = 16_000;
pub(crate) const MAX_TOKENS: usize = 900;
pub(crate) const CALL_CAP: std::time::Duration = std::time::Duration::from_secs(45);

pub(crate) const SYSTEM: &str = "You check a finished piece of work before it is said to the person. REQUEST is what they \
asked. RECORD is everything actually done this turn and what came back. REPLY is what will be said, by sentence.\n\
1. Split REQUEST into the separate things asked for. For each, status is \"done\" only if RECORD shows it happened, \
\"asked\" if the reply puts a question to the person that it needs answered first, otherwise \"not_done\". For not_done, \
reason is the reason RECORD gives (a refusal, a missing permission, a missing thing), quoted exactly and briefly, or \
\"none\" if RECORD gives no reason.\n\
2. List the numbers of REPLY sentences that state a fact, a comparison or an action RECORD does not support. Check \
comparisons against the numbers. Offers, questions, and sentences saying something was not done are never unsupported.\n\
Output ONLY JSON: {\"parts\":[{\"part\":\"...\",\"status\":\"done|not_done|asked\",\"reason\":\"...\"}],\"unsupported\":[numbers]}";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Status {
    Done,
    NotDone,
    Asked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Part {
    pub(crate) part: String,
    pub(crate) status: Status,
    pub(crate) reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Verdict {
    pub(crate) parts: Vec<Part>,
    pub(crate) unsupported: Vec<u64>,
}

/// The reply, by sentence: what the check numbers.
pub(crate) fn sentences(reply: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in reply.lines() {
        let mut piece = String::new();
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            piece.push(c);
            if matches!(c, '.' | '!' | '?') && chars.peek().is_some_and(|n| n.is_whitespace()) {
                out.push(std::mem::take(&mut piece));
            }
        }
        out.push(piece);
    }
    out.into_iter().map(|p| p.trim().to_string()).filter(|p| !p.is_empty()).collect()
}

/// What the check is asked: the request, the end of the record, and the numbered reply.
pub(crate) fn user_message(request: &str, record: &str, reply: &str) -> String {
    let chars: Vec<char> = record.chars().collect();
    let kept: String = chars[chars.len().saturating_sub(RECORD_KEPT)..].iter().collect();
    let numbered: Vec<String> = sentences(reply).iter().enumerate().map(|(i, p)| format!("[{}] {p}", i + 1)).collect();
    format!("REQUEST: {request}\n\nRECORD:\n{kept}\n\nREPLY, by sentence:\n{}\n\nOutput ONLY the JSON.", numbered.join("\n"))
}

/// The closed schema, or None: a part with another status, or no parts, is not a verdict.
pub(crate) fn parse(raw: &str) -> Option<Verdict> {
    let at = raw.rfind("</think>").map_or(0, |i| i + "</think>".len());
    let raw = &raw[at..];
    let body = &raw[raw.find('{')?..=raw.rfind('}')?];
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    let mut parts = Vec::new();
    for p in v.get("parts")?.as_array()? {
        let status = match p.get("status")?.as_str()? {
            "done" => Status::Done,
            "not_done" => Status::NotDone,
            "asked" => Status::Asked,
            _ => return None,
        };
        parts.push(Part {
            part: p.get("part")?.as_str()?.trim().to_string(),
            status,
            reason: p.get("reason").and_then(|r| r.as_str()).unwrap_or("").trim().to_string(),
        });
    }
    if parts.is_empty() {
        return None;
    }
    let unsupported = v.get("unsupported").and_then(|u| u.as_array()).map_or_else(Vec::new, |u| u.iter().filter_map(|n| n.as_u64()).collect());
    Some(Verdict { parts, unsupported })
}

/// Lowercase with whitespace folded, and the quoting and clipping a model adds around a quote removed.
fn folded(s: &str) -> String {
    s.to_lowercase().split_whitespace().collect::<Vec<_>>().join(" ").trim_matches(|c: char| "\"'`.\u{2026} ".contains(c)).to_string()
}

/// Does the record itself give this reason? A reason found only in the reply is no reason: 541's turn
/// 831 told the person "I don't have a tool for either" of two apps it could have used.
fn in_record(reason: &str, record: &str) -> bool {
    let reason = folded(reason);
    !reason.is_empty() && reason != "none" && folded(record).contains(&reason)
}

/// The parts not done with no reason the record gives.
pub(crate) fn unfinished(v: &Verdict, record: &str) -> Vec<String> {
    v.parts.iter().filter(|p| p.status == Status::NotDone && !in_record(&p.reason, record)).map(|p| p.part.clone()).collect()
}

/// Sent back to the loop, once. Names the desktop's tool only when there is a desktop to use it on.
pub(crate) fn nudge(step: usize, missing: &[String], desktop: bool) -> String {
    let look = if desktop { " (mcp.yantrik-os.os_describe {app} lists what an app can do)" } else { "" };
    format!(
        "\n[{step}] (checked against what they asked: not done yet: {}. Your notes give no reason for that. Do it now with the \
         right tool{look}, or, if it truly cannot be done, say what stopped you. Do not say you have no tool for it without \
         having looked.)",
        missing.join("; ")
    )
}

/// Said when it was sent back and still is not done.
pub(crate) fn not_finished_note(missing: &[String]) -> String {
    format!("(I did not finish: {}.)", missing.join("; "))
}

/// One journal line: counts, what is unfinished, and the sentence numbers (SHADOW).
pub(crate) fn journal_line(v: &Verdict, missing: &[String]) -> String {
    let count = |s: Status| v.parts.iter().filter(|p| p.status == s).count();
    format!(
        "parts done {} not_done {} asked {}; unfinished {:?}; would flag sentences {:?}",
        count(Status::Done),
        count(Status::NotDone),
        count(Status::Asked),
        missing,
        v.unsupported
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = "step 0: os_describe {\"app\":\"weather\"} -> Weather \u{2014} 5\u{b0}C in Seattle\nstep 4: os_act add_location -> REFUSED \u{2014} nothing was run. refused: the person at the machine was asked to allow weather.add_location and did not answer within 110s, so nothing was run.";

    fn part(part: &str, status: Status, reason: &str) -> Part {
        Part { part: part.to_string(), status, reason: reason.to_string() }
    }

    #[test]
    fn the_verdict_is_a_closed_schema() {
        let v = parse("<think>hm {not this}</think>\nHere: {\"parts\":[{\"part\":\"a\",\"status\":\"done\",\"reason\":\"none\"},{\"part\":\"b\",\"status\":\"not_done\",\"reason\":\"x\"}],\"unsupported\":[2,5]}").unwrap();
        assert_eq!(v.parts, vec![part("a", Status::Done, "none"), part("b", Status::NotDone, "x")]);
        assert_eq!(v.unsupported, vec![2, 5]);
        assert_eq!(parse("{\"parts\":[{\"part\":\"a\",\"status\":\"maybe\"}]}"), None, "another status");
        assert_eq!(parse("{\"parts\":[]}"), None, "no parts");
        assert_eq!(parse("I think it went well."), None);
        assert_eq!(parse("{\"parts\":[{\"part\":\"a\",\"status\":\"do"), None, "cut off");
    }

    /// The preflight's real words (541, turn 831): a reason from the reply is no reason.
    #[test]
    fn a_reason_counts_only_if_the_record_gives_it() {
        let v = Verdict {
            parts: vec![
                part("check the weather", Status::Done, "none"),
                part("put the hike on my calendar", Status::NotDone, "I don't have a tool for either of those actions"),
                part("write me a checklist note", Status::NotDone, "none"),
                part("add Bentonville", Status::NotDone, "\"Did not answer  within 110s\"\u{2026}"),
                part("which folder", Status::Asked, "none"),
            ],
            unsupported: vec![3],
        };
        let missing = unfinished(&v, RECORD);
        assert_eq!(missing, vec!["put the hike on my calendar", "write me a checklist note"]);
        assert_eq!(journal_line(&v, &missing), "parts done 1 not_done 3 asked 1; unfinished [\"put the hike on my calendar\", \"write me a checklist note\"]; would flag sentences [3]");
    }

    /// What the model is told it can use: the desktop's tool only with a desktop.
    #[test]
    fn the_nudge_names_only_what_is_there() {
        let missing = vec!["write the note".to_string()];
        assert!(nudge(3, &missing, true).contains("mcp.yantrik-os.os_describe"));
        let off = nudge(3, &missing, false);
        assert!(!off.contains("os_describe") && off.contains("not done yet: write the note"), "{off}");
    }

    #[test]
    fn a_reply_is_numbered_by_sentence() {
        assert_eq!(sentences("Done \u{2014} it is 14.5\u{b0}. See you!\n\n- Water\n- Is that it? Yes"), vec!["Done \u{2014} it is 14.5\u{b0}.", "See you!", "- Water", "- Is that it?", "Yes"]);
        let m = user_message("Ask", &"x".repeat(RECORD_KEPT + 5), "One. Two.");
        assert!(m.contains("[1] One.\n[2] Two."), "{m}");
        assert_eq!(m.matches('x').count(), RECORD_KEPT, "the record's end is kept");
    }
}
