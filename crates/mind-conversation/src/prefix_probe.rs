//! E.PREFIX1a: how much of a step's prompt is the same as the previous model call's -- the part a
//! server's prompt cache can reuse instead of reading again.
//!
//! The Mac's one-word turn on 14 October spent 20 s reading a 6,543-token prompt with nothing
//! reused, and on VM 541 a turn's first step cost 3.4 times as much per token as its later ones.
//! The fix depends on WHERE two calls first differ, which nothing measured: this lays a call out in
//! the order a chat template renders it and reports the shared length and the segment it ends in.
//! It carries numbers and a segment name only, never the prompt's text.

use serde_json::Value;
use yantrik_ml::ChatMessage;

/// One call, laid out as the template renders it: the first system message (the persona), the
/// tool schemas the template places after it, the other system messages, then everything else.
pub(crate) struct Render {
    text: String,
    persona_end: usize,
    tools_end: usize,
    system_end: usize,
}

impl Render {
    pub(crate) fn len(&self) -> usize {
        self.text.len()
    }
}

pub(crate) fn render(messages: &[ChatMessage], schemas: &[Value]) -> Render {
    let mut text = String::new();
    let mut rest = messages;
    if let Some((first, after)) = messages.split_first().filter(|(m, _)| m.role == "system") {
        text.push_str(&first.content);
        rest = after;
    }
    let persona_end = text.len();
    text.push('\n');
    text.push_str(&serde_json::to_string(schemas).unwrap_or_default());
    let tools_end = text.len();
    for m in rest.iter().filter(|m| m.role == "system") {
        text.push('\n');
        text.push_str(&m.content);
    }
    let system_end = text.len();
    for m in rest.iter().filter(|m| m.role != "system") {
        text.push('\n');
        text.push_str(&m.content);
    }
    Render { text, persona_end, tools_end, system_end }
}

/// The bytes `now` shares with `prev` from the start, and the segment of `now` where they first
/// differ (`nothing` when `now` is all shared).
pub(crate) fn shared(prev: &Render, now: &Render) -> (usize, &'static str) {
    let n = prev.text.bytes().zip(now.text.bytes()).take_while(|(a, b)| a == b).count();
    let at = if n >= now.text.len() {
        "nothing"
    } else if n < now.persona_end {
        "persona"
    } else if n < now.tools_end {
        "tools"
    } else if n < now.system_end {
        "system"
    } else {
        "prompt"
    };
    (n, at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(persona: &str, tool: &str, rules: &str, prompt: &str) -> Render {
        let schemas = vec![serde_json::json!({"type": "function", "function": {"name": tool}})];
        render(
            &[ChatMessage::system(persona), ChatMessage::system(rules), ChatMessage::user(prompt)],
            &schemas,
        )
    }

    #[test]
    fn the_first_difference_is_named_by_its_segment() {
        let base = call("You are YM.", "weather", "Act, don't talk.", "Current date/time: Wed 14 Oct 16:00.\nUser: hi");
        let same = call("You are YM.", "weather", "Act, don't talk.", "Current date/time: Wed 14 Oct 16:00.\nUser: hi");
        assert_eq!(shared(&base, &same), (same.len(), "nothing"));
        assert_eq!(shared(&base, &call("You are Ada.", "weather", "Act, don't talk.", "Current date/time: Wed 14 Oct 16:00.\nUser: hi")).1, "persona");
        assert_eq!(shared(&base, &call("You are YM.", "notes", "Act, don't talk.", "Current date/time: Wed 14 Oct 16:00.\nUser: hi")).1, "tools");
        assert_eq!(shared(&base, &call("You are YM.", "weather", "Act now.", "Current date/time: Wed 14 Oct 16:00.\nUser: hi")).1, "system");
        let later = call("You are YM.", "weather", "Act, don't talk.", "Current date/time: Wed 14 Oct 16:01.\nUser: hi");
        let (n, at) = shared(&base, &later);
        assert_eq!(at, "prompt");
        assert!(n > later.tools_end && n < later.len(), "{n}");
    }
}
