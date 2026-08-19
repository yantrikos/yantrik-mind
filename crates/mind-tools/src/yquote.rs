//! YAHOO CHART — prices for everything Alpaca does not cover, notably India.
//!
//! Alpaca is US equities on a free feed that returns very little history. Yahoo's chart endpoint
//! needs no key and no client library, covers NSE with the `.NS` suffix and indices with `^`, and
//! returns intraday bars. That is the entire data dependency for grading a claim about an Indian
//! symbol, satisfied for nothing.
//!
//! It is deliberately the FALLBACK rather than the primary: it is an unofficial endpoint that has
//! changed shape before and will again. So Alpaca is asked first where it applies, this covers the
//! rest, and both are behind the same `Bar` type so a caller never has to care which answered.
//!
//! ## The currency trap
//!
//! These bars are in INR for `.NS` symbols and USD for US ones, and the endpoint says which. A
//! return in basis points is currency-agnostic, but a PRICE is not, and quietly mixing the two
//! would produce a P&L that looks like a number and means nothing. The currency is therefore
//! carried on the series rather than assumed, and the exchange timezone with it — an Indian
//! session runs 03:45–10:00 UTC, so a "market hours" check written for New York is simply wrong
//! here rather than approximately right.

use crate::market::Bar;
use serde::{Deserialize, Serialize};

/// A price series with the facts needed to interpret it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Series {
    pub symbol: String,
    pub currency: String,
    pub exchange_tz: String,
    pub bars: Vec<Bar>,
}

/// Parse Yahoo's chart envelope into a series. Split out so the shape is testable without network.
pub fn parse_chart(body: &serde_json::Value) -> anyhow::Result<Series> {
    let result = body
        .get("chart")
        .and_then(|c| c.get("result"))
        .and_then(|r| r.as_array())
        .and_then(|a| a.first())
        .ok_or_else(|| {
            // Yahoo reports its own errors inside the same envelope; surface that rather than a
            // generic parse failure, because "symbol not found" and "we changed the shape" need
            // very different responses from a caller.
            let desc = body
                .get("chart")
                .and_then(|c| c.get("error"))
                .and_then(|e| e.get("description"))
                .and_then(|d| d.as_str())
                .unwrap_or("no result in the chart envelope");
            anyhow::anyhow!("{desc}")
        })?;
    let meta = result.get("meta").cloned().unwrap_or_default();
    let symbol = meta.get("symbol").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let currency = meta.get("currency").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let exchange_tz = meta.get("exchangeTimezoneName").and_then(|s| s.as_str()).unwrap_or("").to_string();
    let stamps: Vec<i64> = result
        .get("timestamp")
        .and_then(|t| t.as_array())
        .map(|a| a.iter().filter_map(|v| v.as_i64()).collect())
        .unwrap_or_default();
    let q = result
        .get("indicators")
        .and_then(|i| i.get("quote"))
        .and_then(|q| q.as_array())
        .and_then(|a| a.first())
        .cloned()
        .unwrap_or_default();
    let col = |name: &str| -> Vec<Option<f64>> {
        q.get(name).and_then(|v| v.as_array()).map(|a| a.iter().map(|x| x.as_f64()).collect()).unwrap_or_default()
    };
    let (o, h, l, c, v) = (col("open"), col("high"), col("low"), col("close"), col("volume"));
    let mut bars = Vec::new();
    for (i, ts) in stamps.iter().enumerate() {
        // A bar with no close is a gap in Yahoo's series (halts, holidays, pre-open padding).
        // Dropping it keeps the series honest; carrying it forward would invent a print.
        let Some(close) = c.get(i).copied().flatten() else { continue };
        bars.push(Bar {
            time: iso_from_epoch(*ts),
            open: o.get(i).copied().flatten().unwrap_or(close),
            high: h.get(i).copied().flatten().unwrap_or(close),
            low: l.get(i).copied().flatten().unwrap_or(close),
            close,
            volume: v.get(i).copied().flatten().unwrap_or(0.0),
        });
    }
    Ok(Series { symbol, currency, exchange_tz, bars })
}

/// Epoch seconds → the RFC-3339 form the rest of the pipeline speaks.
pub fn iso_from_epoch(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z", y, m, d, rem / 3600, (rem % 3600) / 60, rem % 60)
}

/// Fetch a series. `range` like "1d"/"5d"/"1mo"; `interval` like "1m"/"5m"/"1d".
pub fn series(symbol: &str, range: &str, interval: &str) -> anyhow::Result<Series> {
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{}?range={range}&interval={interval}",
        urlencoding::encode(symbol.trim())
    );
    let body: serde_json::Value = ureq::get(&url)
        // A bare request is refused; this endpoint expects to be talking to a browser.
        .set("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .timeout(std::time::Duration::from_secs(25))
        .call()?
        .into_json()?;
    parse_chart(&body)
}

/// Is this an Indian listing? `.NS` (NSE) and `.BO` (BSE), plus the Nifty index.
pub fn is_indian(symbol: &str) -> bool {
    let s = symbol.trim().to_uppercase();
    s.ends_with(".NS") || s.ends_with(".BO") || s == "^NSEI" || s == "^BSESN"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> serde_json::Value {
        // Shaped exactly like the live response verified against RELIANCE.NS.
        serde_json::json!({"chart":{"result":[{
            "meta":{"symbol":"RELIANCE.NS","currency":"INR","exchangeTimezoneName":"Asia/Kolkata"},
            "timestamp":[1787000000,1787000060,1787000120],
            "indicators":{"quote":[{
                "open":[1310.0,1311.0,null],
                "high":[1315.0,1316.0,null],
                "low":[1309.0,1310.5,null],
                "close":[1313.9,1315.5,null],
                "volume":[1000.0,2000.0,null]}]}
        }],"error":null}})
    }

    #[test]
    fn an_indian_series_keeps_its_currency_and_timezone() {
        // A P&L that silently mixes INR and USD prices looks like a number and means nothing,
        // and an Indian session is 03:45-10:00 UTC, so a New York market-hours check is simply
        // wrong here rather than approximately right.
        let s = parse_chart(&envelope()).unwrap();
        assert_eq!(s.symbol, "RELIANCE.NS");
        assert_eq!(s.currency, "INR");
        assert_eq!(s.exchange_tz, "Asia/Kolkata");
    }

    #[test]
    fn a_gap_bar_is_dropped_rather_than_invented() {
        let s = parse_chart(&envelope()).unwrap();
        assert_eq!(s.bars.len(), 2, "the null-close bar is a gap, not a print: {:?}", s.bars);
        assert_eq!(s.bars[0].close, 1313.9);
        assert_eq!(s.bars[1].close, 1315.5);
    }

    #[test]
    fn timestamps_convert_to_the_form_the_rest_of_the_pipeline_speaks() {
        assert_eq!(iso_from_epoch(0), "1970-01-01T00:00:00Z");
        // And the round trip through the shadow parser must agree, or every timing is off.
        let iso = iso_from_epoch(1_787_047_200);
        assert_eq!(crate::shadow::parse_rfc3339_ms(&iso), Some(1_787_047_200_000));
    }

    #[test]
    fn yahoos_own_error_is_surfaced_not_swallowed() {
        // "No data found, symbol may be delisted" and "we changed the shape" demand different
        // responses from a caller, so the endpoint's own words are preserved.
        let e = parse_chart(&serde_json::json!({"chart":{"result":null,"error":{"description":"No data found, symbol may be delisted"}}}))
            .unwrap_err()
            .to_string();
        assert!(e.contains("delisted"), "{e}");
    }

    #[test]
    fn indian_listings_are_recognised() {
        for s in ["RELIANCE.NS", "tcs.ns", "SENSEX.BO", "^NSEI"] {
            assert!(is_indian(s), "{s}");
        }
        for s in ["SPY", "MU", "AAPL"] {
            assert!(!is_indian(s), "{s}");
        }
    }
}
