//! quota — how full the provider's rolling usage windows are, before they run out.
//!
//! This exists because of 2026-08-13: a delegation loop moved 42.7M tokens in a day, exhausted a
//! one-week token-plan quota, and nobody noticed until the API started answering 429. The spend
//! ledger says what was spent; it cannot say how much room is left, because the room is defined by
//! the provider's window, not by our own arithmetic.
//!
//! WHY WINDOWS AND NOT A TOTAL. These are rolling buckets, not calendar periods. A seven-day window
//! at 83% does not reset in seven days — it resets when enough old usage ages out, so heavy usage
//! pushes the reset LATER. That behaviour is invisible to a spend total and obvious in a window,
//! which is exactly why the qwen reset slid from 08-18 to 08-20 while it looked like nothing was
//! happening.
//!
//! HONEST COVERAGE: only Anthropic publishes a usage endpoint we can read. Qwen's token-plan
//! reveals its window solely in the body of a 429 — at which point it is already too late to be
//! a warning. We report what we can actually measure and say nothing about the rest, rather than
//! inventing a number that would be trusted.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// One provider usage window.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct QuotaWindow {
    /// The provider's own name for the window (`five_hour`, `seven_day`).
    pub name: String,
    /// Percent of the window consumed, 0–100.
    pub utilization: f64,
    /// ISO-8601 instant the window rolls over, when the provider states one.
    pub resets_at: Option<String>,
}

/// Everything we can currently measure about remaining headroom.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct QuotaReport {
    /// Highest utilization first — the window about to bite is the one worth showing.
    pub windows: Vec<QuotaWindow>,
    /// Providers we deliberately cannot report on, and why. Naming the gap keeps an unmonitored
    /// provider from reading as a healthy one.
    pub unmonitored: Vec<String>,
}

impl QuotaReport {
    /// The window closest to full, if any. What a single status chip should show.
    pub fn tightest(&self) -> Option<&QuotaWindow> {
        self.windows.first()
    }
}

/// Parse the Anthropic OAuth usage payload.
///
/// Deliberately GENERIC over the top-level keys: alongside `five_hour` and `seven_day` the endpoint
/// carries a rotating set of internal codenames (`tangelo`, `nimbus_quill`, `iguana_necktie`, …),
/// almost all null. Hardcoding today's names would silently miss tomorrow's window and break on a
/// rename, so the rule is structural instead — any object with a numeric `utilization` is a window.
///
/// A window at exactly 0.0 with no reset time is a placeholder for a plan the account does not
/// have; those are dropped, because rendering a row of 0% chips buries the one that matters.
pub fn parse_usage(v: &serde_json::Value) -> Vec<QuotaWindow> {
    let Some(obj) = v.as_object() else {
        return Vec::new();
    };
    let mut out: Vec<QuotaWindow> = obj
        .iter()
        .filter_map(|(name, val)| {
            let u = val.get("utilization")?.as_f64()?;
            let resets_at = val
                .get("resets_at")
                .and_then(|r| r.as_str())
                .map(str::to_string);
            if u == 0.0 && resets_at.is_none() {
                return None; // an inactive plan, not a window with room
            }
            Some(QuotaWindow {
                name: name.clone(),
                utilization: u,
                resets_at,
            })
        })
        .collect();
    // Fullest first, then by name so equal windows keep a stable order across polls (a chip that
    // reshuffles on every refresh reads as flapping).
    out.sort_by(|a, b| {
        b.utilization
            .partial_cmp(&a.utilization)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Cached so a UI can poll freely without generating an outbound request per paint. Usage windows
/// move on the order of minutes; a 60s TTL is far finer than any decision made from them.
static CACHE: Mutex<Option<(Instant, QuotaReport)>> = Mutex::new(None);
const TTL: Duration = Duration::from_secs(60);

/// Fetch (or serve from cache) the current quota picture. Blocking — call under `spawn_blocking`.
///
/// Fails SOFT: an unreachable endpoint yields an empty window list plus an `unmonitored` note, not
/// an error. A quota probe is an instrument, and an instrument that takes the panel down with it
/// when it breaks is worse than one that admits it cannot see.
pub fn quota_report() -> QuotaReport {
    if let Ok(c) = CACHE.lock() {
        if let Some((at, report)) = c.as_ref() {
            if at.elapsed() < TTL {
                return report.clone();
            }
        }
    }
    let report = fetch_uncached();
    if let Ok(mut c) = CACHE.lock() {
        *c = Some((Instant::now(), report.clone()));
    }
    report
}

fn fetch_uncached() -> QuotaReport {
    let mut report = QuotaReport::default();
    // Same endpoint and headers the self-build tick already uses for its hot-window guard, so
    // there is one way to ask this question rather than two that can disagree.
    match std::env::var("CLAUDE_CODE_OAUTH_TOKEN") {
        Ok(token) if !token.trim().is_empty() => {
            let res = ureq::get("https://api.anthropic.com/api/oauth/usage")
                .timeout(Duration::from_secs(12))
                .set("Authorization", &format!("Bearer {}", token.trim()))
                .set("anthropic-beta", "oauth-2025-04-20")
                .call();
            match res.and_then(|r| r.into_json::<serde_json::Value>().map_err(Into::into)) {
                Ok(v) => report.windows = parse_usage(&v),
                Err(e) => report.unmonitored.push(format!(
                    "anthropic: usage endpoint unreachable ({})",
                    short(&e.to_string())
                )),
            }
        }
        _ => report
            .unmonitored
            .push("anthropic: no CLAUDE_CODE_OAUTH_TOKEN configured".to_string()),
    }
    // Stated, not measured. The token-plan publishes no usage endpoint — its window surfaces only
    // in the body of a 429, which arrives after the budget is already gone. Saying so is the whole
    // point: an unmonitored provider must not be mistaken for one with headroom.
    if std::env::var("QWEN_API_KEY")
        .map(|k| !k.trim().is_empty())
        .unwrap_or(false)
    {
        report.unmonitored.push(
            "qwen token-plan: no usage endpoint — the window is only visible in a 429".to_string(),
        );
    }
    report
}

fn short(s: &str) -> String {
    s.chars().take(80).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact shape the live endpoint returned on 2026-08-14, codenames and all.
    #[test]
    fn parses_real_windows_and_ignores_the_codename_placeholders() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{
              "five_hour":  {"utilization": 20.0, "resets_at": "2026-08-14T03:20:00Z"},
              "seven_day":  {"utilization": 83.0, "resets_at": "2026-08-15T10:00:00Z"},
              "seven_day_opus": null,
              "tangelo": null,
              "nimbus_quill": {"utilization": 0.0, "resets_at": null},
              "extra_usage": {"is_enabled": false, "utilization": null}
            }"#,
        )
        .unwrap();
        let w = parse_usage(&v);

        assert_eq!(
            w.len(),
            2,
            "nulls, inactive placeholders and non-window objects are all dropped"
        );
        assert_eq!(
            w[0].name, "seven_day",
            "the window closest to full must lead — it is the one about to bite"
        );
        assert_eq!(w[0].utilization, 83.0);
        assert_eq!(w[0].resets_at.as_deref(), Some("2026-08-15T10:00:00Z"));
        assert_eq!(w[1].name, "five_hour");

        // nimbus_quill is 0.0 with no reset: a plan the account does not have, not headroom.
        assert!(!w.iter().any(|x| x.name == "nimbus_quill"));
        // extra_usage carries a NULL utilization, so it is not a window at all.
        assert!(!w.iter().any(|x| x.name == "extra_usage"));
    }

    #[test]
    fn a_shapeless_payload_yields_no_windows_rather_than_a_panic() {
        assert!(parse_usage(&serde_json::json!("nope")).is_empty());
        assert!(parse_usage(&serde_json::json!({})).is_empty());
        assert!(
            QuotaReport::default().tightest().is_none(),
            "no windows means no claim about headroom"
        );
    }

    /// Equal utilization must not reshuffle between polls, or the chip flaps.
    #[test]
    fn equal_windows_keep_a_stable_order() {
        let v = serde_json::json!({
            "zulu":  {"utilization": 50.0, "resets_at": "x"},
            "alpha": {"utilization": 50.0, "resets_at": "x"},
        });
        let w = parse_usage(&v);
        assert_eq!(
            w.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(),
            vec!["alpha", "zulu"]
        );
    }
}
