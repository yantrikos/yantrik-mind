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
use crate::telegram::{in_quiet_hours_now, now_ms, tg_send_mirrored};
use mind_conversation::{ConversationEngine, EngagementMarker};
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
    TelegramAccepted {
        chars: usize,
    },
    ConsoleQueued {
        notice_id: String,
        fresh: bool,
    },
    Undelivered,
    /// L3c: an engaging line with nobody there to see it — nothing queued, nothing spoken.
    HeldNoPresence,
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
            Self::HeldNoPresence => DeliveryOutcome::HeldNoPresence,
        }
    }
}

/// L3c-2: where an ENGAGING line goes, decided once before anything is banded or rendered, so
/// the band, the probability and the marker always belong to the surface that shows the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EngagingRoute {
    Telegram,
    Console,
    None,
}

/// The pure rule: a reachable chat outside quiet hours; else an open cockpit; else nowhere.
pub(crate) fn engaging_route(
    telegram_reachable: bool,
    quiet: bool,
    console_present: bool,
) -> EngagingRoute {
    if telegram_reachable && !quiet {
        EngagingRoute::Telegram
    } else if console_present {
        EngagingRoute::Console
    } else {
        EngagingRoute::None
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

    /// L3c: the cockpit is open right now — a machine view polled within the presence window —
    /// and there is a queue to put a line in.
    pub(crate) fn console_present(&self) -> bool {
        self.console_present_at(now_ms())
    }

    /// The same, at a given instant — the door revalidates with the caller's clock.
    pub(crate) fn console_present_at(&self, now_ms: u64) -> bool {
        self.conv.has_notice_queue() && self.conv.turns().console_view_recent(now_ms)
    }

    /// L3c: someone can see a line NOW: a pinned chat, or an open cockpit. The engagement loops'
    /// `chat_present`; a queue nobody is looking at is not presence.
    pub(crate) fn has_presence(&self) -> bool {
        self.telegram_reachable() || self.console_present()
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

    /// L3c-2: the engaging route for this instant — the same rule the loops band and render by.
    pub(crate) fn engaging_route(&self, quiet: bool) -> EngagingRoute {
        engaging_route(self.telegram_reachable(), quiet, self.console_present())
    }

    /// L3c: deliver a line that PREDICTS engagement, along the route decided for it. Telegram:
    /// send; a rejected send is `Undelivered` — NEVER a fallback to the console, because the
    /// line was banded for Telegram. Console: queue with the marker and a show-by bound.
    /// None: held — nothing queued, nothing spoken. One ledger record per call; the prediction is
    /// never committed here (a Telegram caller commits after acceptance, the console at `shown`).
    pub(crate) async fn deliver_engaging(
        &self,
        route: EngagingRoute,
        kind: DeliveryKind,
        text: &str,
        marker: &EngagementMarker,
        show_by_ms: u64,
        now_ms: u64,
    ) -> Delivered {
        let chars = text.chars().count();
        let outcome = match route {
            // The chosen chat is revalidated at the door too: still pinned, still outside quiet
            // hours (a long generation can cross into the night); a rejected send is undelivered
            // and never falls back.
            EngagingRoute::Telegram => match &self.telegram {
                Some(t) => {
                    let chat = t.active_chat.load(Ordering::Relaxed);
                    if chat != 0
                        && !in_quiet_hours_now()
                        && tg_send_mirrored(&self.conv, &t.api, chat, text)
                            .await
                            .is_ok()
                    {
                        Delivered::TelegramAccepted { chars }
                    } else {
                        Delivered::Undelivered
                    }
                }
                None => Delivered::Undelivered,
            },
            // The chosen console is revalidated at the door: a cockpit that went stale while the
            // line was being generated gets nothing (never a fallback — the line was banded for
            // the console, and no other surface may show it).
            EngagingRoute::Console if !self.console_present_at(now_ms) => Delivered::HeldNoPresence,
            EngagingRoute::Console => {
                match self
                    .conv
                    .queue_engaging_notice(kind, text, marker, show_by_ms)
                {
                    Ok(q) => Delivered::ConsoleQueued {
                        notice_id: q.notice_id,
                        fresh: q.fresh,
                    },
                    Err(error) => {
                        eprintln!("[delivery] undelivered {}: {error}", kind.as_str());
                        Delivered::Undelivered
                    }
                }
            }
            EngagingRoute::None => Delivered::HeldNoPresence,
        };
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
            2,
            "two send sites: the plain door and the engaging one"
        );
        // L3c: the engaging door never commits a prediction, queues only on the Console route,
        // and never falls back from a rejected Telegram send to the console.
        let engaging = &body[body.find("pub(crate) async fn deliver_engaging(").unwrap()..];
        assert!(!engaging.contains("commit_"), "the seam commits nothing");
        assert!(engaging.contains("EngagingRoute::None => Delivered::HeldNoPresence"));
        let tg_arm = &engaging[engaging.find("EngagingRoute::Telegram =>").unwrap()
            ..engaging.find("EngagingRoute::Console =>").unwrap()];
        assert!(
            !tg_arm.contains("queue_engaging_notice"),
            "no console fallback from the Telegram route"
        );
        assert!(
            tg_arm.find("in_quiet_hours_now()").unwrap()
                < tg_arm.find("tg_send_mirrored(").unwrap(),
            "quiet hours are rechecked at the Telegram door"
        );
        assert!(
            !engaging.contains("console_present()"),
            "the route is decided by the caller"
        );
        let console_arm = &engaging[engaging.find("EngagingRoute::Console if").unwrap()..];
        assert!(
            console_arm.find("console_present_at(now_ms)").unwrap()
                < console_arm.find("queue_engaging_notice").unwrap(),
            "the chosen console is revalidated before anything is queued"
        );
        assert!(body.contains("matches!(self, Self::TelegramAccepted { .. })"));
        assert!(
            !body.contains("note_proactive_sent"),
            "the seam never marks anything as spoken"
        );
        assert_eq!(
            body.matches("self.conv.record_delivery(").count(),
            2,
            "one ledger record per call, in each door"
        );
        for other in [include_str!("loops.rs"), include_str!("web.rs")] {
            let prod = &other[..other.find("#[cfg(test)]").unwrap_or(other.len())];
            assert!(!prod.contains("tg_send"), "no sender outside the seam");
        }
    }

    /// L3c-2: the route is a pure rule of the three facts, decided once.
    #[test]
    fn the_engaging_route_is_decided_once_by_three_facts() {
        use super::{engaging_route, EngagingRoute};
        assert_eq!(engaging_route(true, false, true), EngagingRoute::Telegram);
        assert_eq!(engaging_route(true, false, false), EngagingRoute::Telegram);
        assert_eq!(
            engaging_route(true, true, true),
            EngagingRoute::Console,
            "quiet: not the phone"
        );
        assert_eq!(engaging_route(false, false, true), EngagingRoute::Console);
        assert_eq!(engaging_route(false, false, false), EngagingRoute::None);
        assert_eq!(engaging_route(true, true, false), EngagingRoute::None);
    }

    /// Codex's L3c-2 addendum (A): the cockpit is present when the route is chosen and stale by
    /// the time the door is reached — held, nothing queued, no fallback.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cockpit_that_went_stale_between_the_route_and_the_door_gets_nothing() {
        use super::{Delivered, Delivery, EngagingRoute};
        use mind_conversation::{ConversationEngine, EngagementMarker};
        use std::sync::Arc;
        struct NoTools;
        #[mind_recipes::async_trait_rt::async_trait]
        impl mind_recipes::RecipeHost for NoTools {
            async fn call_tool(
                &self,
                _tool: &str,
                _args: &serde_json::Value,
            ) -> anyhow::Result<String> {
                anyhow::bail!("no tools in this fixture")
            }
        }
        let mem: Arc<dyn mind_types::MemoryFacade> =
            Arc::new(mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap());
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("unused")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let store = Arc::new(mind_recipes::RecipeStore::open(":memory:").unwrap());
        let recipes = mind_recipes::RecipeEngine::new(pool.clone(), Arc::new(NoTools), "JARVIS")
            .with_store(store.clone());
        let conv =
            Arc::new(ConversationEngine::new(mem, pool, "JARVIS").with_recipes(Arc::new(recipes)));
        let delivery = Delivery::new(conv.clone(), None);
        let now = crate::telegram::now_ms();
        // The cockpit polls: present now.
        {
            let _view = conv.turns().begin_view_on("cli:loops_json", now);
        }
        assert_eq!(delivery.engaging_route(false), EngagingRoute::Console);
        let marker = EngagementMarker::digest_line("0123456789abcdef", 400).unwrap();
        // Generation took long enough for the stamp to age past the window: held at the door.
        let late = now + 200_000;
        let out = delivery
            .deliver_engaging(
                EngagingRoute::Console,
                mind_observability::DeliveryKind::Digest,
                "a digest line",
                &marker,
                late + 600_000,
                late,
            )
            .await;
        assert_eq!(out, Delivered::HeldNoPresence);
        assert_eq!(conv.notice_queue_depth().unwrap(), (0, 0), "nothing queued");
        // Still present at the door: queued.
        let out = delivery
            .deliver_engaging(
                EngagingRoute::Console,
                mind_observability::DeliveryKind::Digest,
                "a digest line",
                &marker,
                now + 600_000,
                now + 1,
            )
            .await;
        assert!(matches!(out, Delivered::ConsoleQueued { .. }));
        assert_eq!(conv.notice_queue_depth().unwrap(), (1, 0));
    }
}
