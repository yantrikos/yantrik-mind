//! L3b (ARCH7 §4 L3, second slice): the process's ONE outward door for loop output.
//!
//! A loop that speaks used to call the Telegram sender directly, so on a box with no phone its
//! line died in the journal and — worse — a loop could mark itself as having spoken. This seam
//! is the honest contract: Telegram when a chat is reachable and it is not quiet hours; else the
//! durable console notice queue, which the cockpit leases, renders once by id and acknowledges;
//! else nothing. Only `TelegramAccepted` is DELIVERED. A queued notice is a promise the cockpit
//! keeps later, so it may not set `spoke`, place the proactive-sent mark or commit an engagement
//! prediction (E.G1c's wall, made structural). Every call records exactly one typed `delivery`
//! decision event: kind, outcome, receipt id, size — never the text.
use crate::telegram::{in_quiet_hours_now, tg_send_mirrored};
use mind_conversation::ConversationEngine;
use mind_observability::{DeliveryKind, DeliveryOutcome, DeliveryTick};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// The Telegram surface, present only when the poll loop runs on this box.
pub(crate) struct TelegramTarget {
    pub api: String,
    pub active_chat: Arc<AtomicI64>,
}

/// What one delivery did. `is_delivered` is true for Telegram acceptance alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Delivered {
    TelegramAccepted { chars: usize },
    ConsoleQueued { notice_id: String, fresh: bool },
    Undelivered,
}

impl Delivered {
    pub(crate) fn is_delivered(&self) -> bool {
        matches!(self, Self::TelegramAccepted { .. })
    }
    fn ledger(&self) -> DeliveryOutcome {
        match self {
            Self::TelegramAccepted { .. } => DeliveryOutcome::TelegramAccepted,
            Self::ConsoleQueued { .. } => DeliveryOutcome::ConsoleQueued,
            Self::Undelivered => DeliveryOutcome::Undelivered,
        }
    }
}

pub(crate) struct Delivery {
    conv: Arc<ConversationEngine>,
    telegram: Option<TelegramTarget>,
}

impl Delivery {
    pub(crate) fn new(conv: Arc<ConversationEngine>, telegram: Option<TelegramTarget>) -> Self {
        Self { conv, telegram }
    }

    /// A chat is configured and pinned right now.
    pub(crate) fn telegram_reachable(&self) -> bool {
        self.telegram
            .as_ref()
            .is_some_and(|t| t.active_chat.load(Ordering::Relaxed) != 0)
    }

    /// Somewhere a line can land: a reachable chat or the durable console queue. The process
    /// runner's `chat_present`, so a headless box with a cockpit is not "no chat".
    pub(crate) fn has_surface(&self) -> bool {
        self.telegram_reachable() || self.conv.has_notice_queue()
    }

    /// Deliver one line. Telegram first (reachable, outside quiet hours, accepted by the API);
    /// otherwise the console queue; otherwise undelivered. One ledger record per call.
    pub(crate) async fn deliver(&self, kind: DeliveryKind, text: &str) -> Delivered {
        let chars = text.chars().count();
        let mut outcome = Delivered::Undelivered;
        if let Some(t) = &self.telegram {
            let chat = t.active_chat.load(Ordering::Relaxed);
            if chat != 0
                && !in_quiet_hours_now()
                && tg_send_mirrored(&self.conv, &t.api, chat, text)
                    .await
                    .is_ok()
            {
                outcome = Delivered::TelegramAccepted { chars };
            }
        }
        if outcome == Delivered::Undelivered {
            outcome = match self.conv.queue_notice(kind, text) {
                Ok(q) => Delivered::ConsoleQueued {
                    notice_id: q.notice_id,
                    fresh: q.fresh,
                },
                Err(error) => {
                    eprintln!("[delivery] undelivered {}: {error}", kind.as_str());
                    Delivered::Undelivered
                }
            };
        }
        let receipt_id = match &outcome {
            Delivered::ConsoleQueued { notice_id, .. } => Some(notice_id.clone()),
            _ => None,
        };
        self.conv.record_delivery(DeliveryTick {
            kind,
            outcome: outcome.ledger(),
            receipt_id,
            chars: u32::try_from(chars).unwrap_or(u32::MAX),
        });
        outcome
    }
}

#[cfg(test)]
mod tests {
    /// The seam is the only sender the runner can reach, and it decides delivery by the API's
    /// acceptance alone: a queued notice never counts as spoken.
    #[test]
    fn the_seam_is_the_runner_s_only_sender_and_only_telegram_counts_as_delivered() {
        let src = include_str!("delivery.rs");
        let body = &src[..src.find("#[cfg(test)]").unwrap()];
        assert_eq!(
            body.matches("tg_send_mirrored(").count(),
            1,
            "one send site"
        );
        assert!(body.contains("matches!(self, Self::TelegramAccepted { .. })"));
        assert!(
            !body.contains("note_proactive_sent"),
            "the seam never marks anything as spoken"
        );
        assert_eq!(
            body.matches("self.conv.record_delivery(").count(),
            1,
            "one ledger record per call"
        );
        for other in [include_str!("loops.rs"), include_str!("web.rs")] {
            let prod = &other[..other.find("#[cfg(test)]").unwrap_or(other.len())];
            assert!(!prod.contains("tg_send"), "no sender outside the seam");
        }
    }
}
