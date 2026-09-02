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
}

impl NoticeKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verdict => "verdict",
            Self::ProfileRefresh => "profile_refresh",
            Self::Pattern => "pattern",
            Self::HorizonTick => "horizon_tick",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "verdict" => Some(Self::Verdict),
            "profile_refresh" => Some(Self::ProfileRefresh),
            "pattern" => Some(Self::Pattern),
            "horizon_tick" => Some(Self::HorizonTick),
            _ => None,
        }
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
}

impl NoticeEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Leased => "leased",
            Self::Shown => "shown",
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

    #[test]
    fn kinds_round_trip_and_nothing_else_parses() {
        for kind in [
            NoticeKind::Verdict,
            NoticeKind::ProfileRefresh,
            NoticeKind::Pattern,
            NoticeKind::HorizonTick,
        ] {
            assert_eq!(NoticeKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(NoticeKind::parse("memory"), None);
    }
}
