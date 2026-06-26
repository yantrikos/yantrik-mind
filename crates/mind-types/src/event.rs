//! `Event` — the normalized envelope every observable thing flows as. The OpenClaw split keeps
//! model-facing text separate from command parsing and the raw original (directive stripping
//! applies to the current message only, so history stays intact and untrusted metadata is never
//! treated as instructions).
use crate::clock::UnixMillis;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum EventSource {
    Chat {
        channel: String,
        chat_id: String,
        user: String,
    },
    System {
        kind: String,
    },
    Cron {
        job: String,
    },
    SelfReflection,
    Tool {
        name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBody {
    /// Model-facing prompt text (directives stripped).
    pub for_agent: String,
    /// Parsed command/directive, if any (current message only).
    pub command: Option<String>,
    /// Raw original text.
    pub raw: String,
}

impl EventBody {
    pub fn plain(text: impl Into<String>) -> Self {
        let raw = text.into();
        Self {
            for_agent: raw.clone(),
            command: None,
            raw,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub id: String,
    pub trace_id: String,
    pub source: EventSource,
    pub body: EventBody,
    pub ts: UnixMillis,
}
