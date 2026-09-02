//! L3b (ARCH7 §4 L3, second slice): the console notice queue's accounting.
//!
//! A loop that speaks on a box with no phone has nowhere to deliver; today its line dies in the
//! journal. The notice queue is the durable, per-operator surface the cockpit reads instead. Its
//! honesty is in the receipts: `queued` when the loop wrote it, `leased` when a cockpit took it to
//! render, `shown` when that cockpit acknowledged the paint. "Shown" is a receipt, never a mutable
//! column, and the store allows one per notice. What is promised is at-least-once delivery until
//! the acknowledgement plus idempotent rendering by id — exactly-once across a browser crash is
//! not provable and is not claimed.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The bounded kinds a loop may queue. No free text reaches the kind column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeKind {
    /// Resolve: one graded prediction's verdict line.
    Verdict,
    /// ProfileRefresh: a re-learn summary.
    ProfileRefresh,
    /// Patterns: a grounded pattern line (only when one was found).
    Pattern,
    /// A horizon tick outcome (E.F3's expiry included) on a box with no phone.
    HorizonTick,
    /// L3c: the calibrated knock — an engaging line (carries a marker, expires unshown).
    Knock,
    /// L3c: the proactive digest — engaging.
    Digest,
    /// L3c: a get-to-know-you question — engaging.
    Ask,
}

impl NoticeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verdict => "verdict",
            Self::ProfileRefresh => "profile_refresh",
            Self::Pattern => "pattern",
            Self::HorizonTick => "horizon_tick",
            Self::Knock => "knock",
            Self::Digest => "digest",
            Self::Ask => "ask",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "verdict" => Some(Self::Verdict),
            "profile_refresh" => Some(Self::ProfileRefresh),
            "pattern" => Some(Self::Pattern),
            "horizon_tick" => Some(Self::HorizonTick),
            "knock" => Some(Self::Knock),
            "digest" => Some(Self::Digest),
            "ask" => Some(Self::Ask),
            _ => None,
        }
    }
    /// L3c: the kinds that predict engagement and therefore carry a marker and a show-by bound.
    pub const fn is_engaging(self) -> bool {
        matches!(self, Self::Knock | Self::Digest | Self::Ask)
    }
}

/// L3c: the engagement marker, version 1 — a canonical typed record, never free JSON. It rides on
/// an engaging notice so the prediction can be committed at the instant the line was SHOWN, with
/// the same probability the loop decided on. Integer probability units (0..=1000 = 0.000..1.000),
/// strict per-kind ref shapes, bounded ids, no text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngagementMarker {
    pub version: u8,
    pub kind: NoticeKind,
    /// `knock:<packet id>` · `digest:<16 hex>` · `ask:<slot>` — see `valid_ref`.
    pub r#ref: String,
    pub p_units: u16,
    /// The knock's spoken band (60 / 75 / 90); 0 for the other kinds.
    pub band: u8,
    /// The world-shadow evaluation id the knock was decided under; empty for the other kinds.
    pub eval_id: String,
}

impl EngagementMarker {
    pub const VERSION: u8 = 1;

    pub fn knock(pkt_id: &str, p_units: u16, band: u8, eval_id: &str) -> Option<Self> {
        Self::new(
            NoticeKind::Knock,
            format!("knock:{pkt_id}"),
            p_units,
            band,
            eval_id,
        )
    }
    pub fn digest_line(digest_hex16: &str, p_units: u16) -> Option<Self> {
        Self::new(
            NoticeKind::Digest,
            format!("digest:{digest_hex16}"),
            p_units,
            0,
            "",
        )
    }
    pub fn ask(slot: &str, p_units: u16) -> Option<Self> {
        Self::new(NoticeKind::Ask, format!("ask:{slot}"), p_units, 0, "")
    }
    fn new(kind: NoticeKind, r#ref: String, p_units: u16, band: u8, eval_id: &str) -> Option<Self> {
        let marker = Self {
            version: Self::VERSION,
            kind,
            r#ref,
            p_units,
            band,
            eval_id: eval_id.to_string(),
        };
        marker.validate().then_some(marker)
    }

    pub fn validate(&self) -> bool {
        self.version == Self::VERSION
            && self.kind.is_engaging()
            && self.p_units <= 1000
            && valid_ref(self.kind, &self.r#ref)
            && match self.kind {
                NoticeKind::Knock => {
                    matches!(self.band, 60 | 75 | 90)
                        && !self.eval_id.is_empty()
                        && self.eval_id.len() <= 64
                        && self
                            .eval_id
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_'))
                }
                _ => self.band == 0 && self.eval_id.is_empty(),
            }
    }

    /// The canonical form: fixed field order, no whitespace. Stored, digested and compared as is.
    pub fn canonical_json(&self) -> String {
        format!(
            "{{\"version\":{},\"kind\":\"{}\",\"ref\":\"{}\",\"p_units\":{},\"band\":{},\"eval_id\":\"{}\"}}",
            self.version,
            self.kind.as_str(),
            self.r#ref,
            self.p_units,
            self.band,
            self.eval_id
        )
    }
    pub fn digest(&self) -> String {
        sha256_hex(self.canonical_json().as_bytes())
    }
    /// Parse a stored marker; anything that is not exactly a valid canonical v1 record is `None`.
    pub fn parse(stored: &str) -> Option<Self> {
        let marker: Self = serde_json::from_str(stored).ok()?;
        (marker.validate() && marker.canonical_json() == stored).then_some(marker)
    }
    pub fn p(&self) -> f64 {
        f64::from(self.p_units) / 1000.0
    }
}

fn valid_ref(kind: NoticeKind, r#ref: &str) -> bool {
    let bounded = |s: &str, max: usize, extra: &[char]| {
        !s.is_empty()
            && s.len() <= max
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || extra.contains(&c))
    };
    match kind {
        NoticeKind::Knock => r#ref
            .strip_prefix("knock:")
            .is_some_and(|id| bounded(id, 64, &[':', '-', '_'])),
        NoticeKind::Digest => r#ref
            .strip_prefix("digest:")
            .is_some_and(|h| h.len() == 16 && h.chars().all(|c| c.is_ascii_hexdigit())),
        NoticeKind::Ask => r#ref
            .strip_prefix("ask:")
            .is_some_and(|slot| bounded(slot, 32, &[':', '-', '_'])),
        _ => false,
    }
}

/// The most characters a notice may carry. A loop's own line fits; a memory dump does not.
pub const NOTICE_MAX_CHARS: usize = 2000;

/// Bounded text: control characters other than newline stripped, capped at NOTICE_MAX_CHARS.
pub fn bounded_notice_text(raw: &str) -> String {
    raw.chars()
        .filter(|c| !c.is_control() || *c == '\n')
        .take(NOTICE_MAX_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeEvent {
    Queued,
    Leased,
    Shown,
    /// L3c: an engaging notice's show-by bound passed unshown — terminal; never leased again.
    Expired,
    /// L3c: the prediction the shown notice carried was committed by the engine — the durable
    /// outbox completion; follows `shown` only; nothing follows it.
    Committed,
}

impl NoticeEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Leased => "leased",
            Self::Shown => "shown",
            Self::Expired => "expired",
            Self::Committed => "committed",
        }
    }
}

/// One append-only, hash-chained accounting receipt for a notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoticeReceipt {
    pub notice_id: String,
    pub operator_id: String,
    pub event: NoticeEvent,
    pub occurred_at_ms: u64,
    /// Present on `leased` and on the `shown` that closes that lease.
    pub lease_id: Option<String>,
    /// Present on `leased` only: the instant after which another cockpit may lease again.
    pub lease_until_ms: Option<u64>,
    pub previous_receipt_sha256: Option<String>,
    pub receipt_sha256: String,
}

impl NoticeReceipt {
    /// Issue a receipt, or `None` when the shape is not one the chain admits.
    pub fn issue(
        notice_id: impl Into<String>,
        operator_id: impl Into<String>,
        event: NoticeEvent,
        occurred_at_ms: u64,
        lease_id: Option<String>,
        lease_until_ms: Option<u64>,
        previous_receipt_sha256: Option<String>,
    ) -> Option<Self> {
        let mut receipt = Self {
            notice_id: notice_id.into(),
            operator_id: operator_id.into(),
            event,
            occurred_at_ms,
            lease_id,
            lease_until_ms,
            previous_receipt_sha256,
            receipt_sha256: String::new(),
        };
        if !receipt.valid_shape() {
            return None;
        }
        receipt.receipt_sha256 = receipt.digest();
        Some(receipt)
    }

    pub fn verify(&self) -> bool {
        self.valid_shape() && self.receipt_sha256 == self.digest()
    }

    fn valid_shape(&self) -> bool {
        let ids_ok = valid_id(&self.notice_id)
            && valid_id(&self.operator_id)
            && self
                .lease_id
                .as_deref()
                .is_none_or(|id| id.len() == 16 && id.chars().all(|c| c.is_ascii_hexdigit()))
            && self
                .previous_receipt_sha256
                .as_deref()
                .is_none_or(|d| d.len() == 64 && d.chars().all(|c| c.is_ascii_hexdigit()));
        let event_ok = match self.event {
            // Queued opens the chain: no lease, no predecessor.
            NoticeEvent::Queued => {
                self.lease_id.is_none()
                    && self.lease_until_ms.is_none()
                    && self.previous_receipt_sha256.is_none()
            }
            NoticeEvent::Leased => {
                self.lease_id.is_some()
                    && self
                        .lease_until_ms
                        .is_some_and(|until| until > self.occurred_at_ms)
                    && self.previous_receipt_sha256.is_some()
            }
            NoticeEvent::Shown => {
                self.lease_id.is_some()
                    && self.lease_until_ms.is_none()
                    && self.previous_receipt_sha256.is_some()
            }
            // Expired and Committed close the chain with no lease and a predecessor.
            NoticeEvent::Expired | NoticeEvent::Committed => {
                self.lease_id.is_none()
                    && self.lease_until_ms.is_none()
                    && self.previous_receipt_sha256.is_some()
            }
        };
        ids_ok && event_ok
    }

    fn digest(&self) -> String {
        let canonical = format!(
            "notice-receipt-v1\n{}\n{}\n{}\n{}\n{}\n{}\n{}",
            self.notice_id,
            self.operator_id,
            self.event.as_str(),
            self.occurred_at_ms,
            self.lease_id.as_deref().unwrap_or("-"),
            self.lease_until_ms
                .map(|v| v.to_string())
                .unwrap_or_else(|| "-".into()),
            self.previous_receipt_sha256.as_deref().unwrap_or("-"),
        );
        format!("{:x}", Sha256::digest(canonical.as_bytes()))
    }
}

/// Lower-hex SHA-256 of bytes: the one digest every notice id, lease id and receipt uses.
pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn valid_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '-' | '_' | '.' | '@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chain_of_three_verifies_and_every_wrong_shape_is_refused() {
        let queued = NoticeReceipt::issue(
            "notice:0011223344556677",
            "primary",
            NoticeEvent::Queued,
            10,
            None,
            None,
            None,
        )
        .expect("queued opens a chain");
        assert!(queued.verify());
        // Queued may carry no lease and no predecessor.
        assert!(NoticeReceipt::issue(
            "notice:1",
            "primary",
            NoticeEvent::Queued,
            10,
            Some("0123456789abcdef".into()),
            None,
            None
        )
        .is_none());
        assert!(NoticeReceipt::issue(
            "notice:1",
            "primary",
            NoticeEvent::Queued,
            10,
            None,
            None,
            Some(queued.receipt_sha256.clone())
        )
        .is_none());
        // Leased needs a lease id, a future expiry and a predecessor.
        assert!(NoticeReceipt::issue(
            "notice:1",
            "primary",
            NoticeEvent::Leased,
            20,
            Some("0123456789abcdef".into()),
            Some(20),
            Some(queued.receipt_sha256.clone())
        )
        .is_none());
        assert!(NoticeReceipt::issue(
            "notice:1",
            "primary",
            NoticeEvent::Leased,
            20,
            None,
            Some(120),
            Some(queued.receipt_sha256.clone())
        )
        .is_none());
        let leased = NoticeReceipt::issue(
            "notice:0011223344556677",
            "primary",
            NoticeEvent::Leased,
            20,
            Some("0123456789abcdef".into()),
            Some(120),
            Some(queued.receipt_sha256.clone()),
        )
        .unwrap();
        assert!(leased.verify());
        // Shown closes with the lease id and no expiry.
        assert!(NoticeReceipt::issue(
            "notice:1",
            "primary",
            NoticeEvent::Shown,
            30,
            Some("0123456789abcdef".into()),
            Some(120),
            Some(leased.receipt_sha256.clone())
        )
        .is_none());
        let shown = NoticeReceipt::issue(
            "notice:0011223344556677",
            "primary",
            NoticeEvent::Shown,
            30,
            Some("0123456789abcdef".into()),
            None,
            Some(leased.receipt_sha256.clone()),
        )
        .unwrap();
        assert!(shown.verify());
        // A single altered field breaks the digest.
        let mut forged = shown.clone();
        forged.occurred_at_ms = 31;
        assert!(!forged.verify());
        let mut forged = shown;
        forged.lease_id = Some("fedcba9876543210".into());
        assert!(!forged.verify());
    }

    #[test]
    fn notice_text_is_bounded_and_control_free() {
        let raw = format!("a\u{7}b\r\nc{}", "x".repeat(NOTICE_MAX_CHARS * 2));
        let bounded = bounded_notice_text(&raw);
        assert!(bounded.starts_with("ab\nc"));
        assert_eq!(bounded.chars().count(), NOTICE_MAX_CHARS);
        assert!(!bounded.chars().any(|c| c.is_control() && c != '\n'));
        assert_eq!(bounded_notice_text("  \t "), "");
    }

    /// L3c: the marker is canonical and strict — a wrong band, an unbounded ref, a stray field
    /// order or a probability over 1.000 is not a marker.
    #[test]
    fn the_engagement_marker_is_canonical_and_strict() {
        let knock = EngagementMarker::knock("pkt:0123abcd", 612, 75, "eval:abc").unwrap();
        let stored = knock.canonical_json();
        assert_eq!(EngagementMarker::parse(&stored), Some(knock.clone()));
        assert_eq!(knock.p(), 0.612);
        assert_eq!(knock.digest().len(), 64);
        // Re-ordered fields are not canonical.
        let reordered = stored.replace(
            "{\"version\":1,\"kind\":\"knock\"",
            "{\"kind\":\"knock\",\"version\":1",
        );
        assert_ne!(reordered, stored);
        assert_eq!(EngagementMarker::parse(&reordered), None);
        assert!(
            EngagementMarker::knock("pkt", 612, 70, "eval").is_none(),
            "band off the ladder"
        );
        assert!(
            EngagementMarker::knock("pkt", 1001, 75, "eval").is_none(),
            "over 1.000"
        );
        assert!(
            EngagementMarker::knock("", 612, 75, "eval").is_none(),
            "empty packet"
        );
        assert!(
            EngagementMarker::knock("pkt", 612, 75, "").is_none(),
            "knock needs its eval id"
        );
        assert!(EngagementMarker::digest_line("0123456789abcdef", 300).is_some());
        assert!(EngagementMarker::digest_line("0123", 300).is_none());
        assert!(EngagementMarker::ask("interest:music", 300).is_some());
        assert!(EngagementMarker::ask("interest music", 300).is_none());
        // A non-engaging kind can never carry a marker.
        let mut forged = knock;
        forged.kind = NoticeKind::Pattern;
        assert!(!forged.validate());
        // Expired closes a chain with no lease.
        let queued = NoticeReceipt::issue(
            "notice:1",
            "primary",
            NoticeEvent::Queued,
            10,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(NoticeReceipt::issue(
            "notice:1",
            "primary",
            NoticeEvent::Expired,
            20,
            None,
            None,
            Some(queued.receipt_sha256.clone())
        )
        .is_some());
        assert!(NoticeReceipt::issue(
            "notice:1",
            "primary",
            NoticeEvent::Expired,
            20,
            Some("0123456789abcdef".into()),
            None,
            Some(queued.receipt_sha256)
        )
        .is_none());
    }

    #[test]
    fn kinds_round_trip_and_nothing_else_parses() {
        for kind in [
            NoticeKind::Verdict,
            NoticeKind::ProfileRefresh,
            NoticeKind::Pattern,
            NoticeKind::HorizonTick,
            NoticeKind::Knock,
            NoticeKind::Digest,
            NoticeKind::Ask,
        ] {
            assert_eq!(NoticeKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(NoticeKind::parse("memory"), None);
    }
}
