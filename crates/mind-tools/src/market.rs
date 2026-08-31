//! MARKET DATA — the ground truth that turns a forecast into a grade.
//!
//! The mind's judgment organ currently reports skill −0.37 and a verdict of "NOT YET PROVABLE".
//! It cannot improve on that without outcomes, and outcomes are the one thing markets give away:
//! a dated, thresholded claim resolves against a printed price, unambiguously, for free, forever.
//! That is why this module exists — not to trade, and not to find edge, but to make the mind's
//! confidence mean something by settling its claims against what actually happened.
//!
//! ## Read-only by construction
//!
//! Every request here goes to `data.alpaca.markets`, the market-DATA host. That host has no order
//! endpoints at all: there is no path from this client to a position, because the server on the
//! other end does not implement one. The configured account is a paper account today, but the
//! boundary deliberately does not rest on that — paper keys get swapped for live keys eventually,
//! and the code must not be the thing that decides whether that is safe. Same principle as the
//! mail draft that cannot send and the browser that cannot press Buy: bound the capability by
//! what it is physically able to reach, not by a rule someone could relax later.
//!
//! Credentials are `ALPACA_KEY_ID` + `ALPACA_SECRET_KEY`. Absent either, the client refuses to
//! exist and says which one is missing, rather than failing at the first call.

use serde::{Deserialize, Serialize};

/// One OHLCV bar as Alpaca returns it (their field names are single letters).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Bar {
    #[serde(rename = "t")]
    pub time: String,
    #[serde(rename = "o")]
    pub open: f64,
    #[serde(rename = "h")]
    pub high: f64,
    #[serde(rename = "l")]
    pub low: f64,
    #[serde(rename = "c")]
    pub close: f64,
    #[serde(rename = "v", default)]
    pub volume: f64,
}

/// Which way a claim points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Direction {
    /// "will reach / break above / close above X"
    Above,
    /// "will fall to / break below / close below X"
    Below,
}

/// A market claim reduced to something a price series can settle. Anything that cannot be put in
/// this shape cannot be graded, and belongs nowhere near the judgment ledger.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvableClaim {
    pub symbol: String,
    pub direction: Direction,
    pub threshold: f64,
    /// Whether the CLOSE must satisfy it, or any touch intraday counts.
    pub on_close: bool,
}

/// How a claim came out against the bars in its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Hit,
    Miss,
    /// No price data covered the window — ungradeable, which is NOT a miss. Scoring an
    /// unmeasurable claim as wrong would quietly bias every calibration number built on it.
    NoData,
}

/// Settle a claim against its window's bars.
///
/// `on_close` claims are judged only on closing prices; touch claims are judged on the high/low,
/// because "it will hit 250" is true the moment it trades there even if it closes lower.
pub fn resolve(claim: &ResolvableClaim, bars: &[Bar]) -> Verdict {
    if bars.is_empty() {
        return Verdict::NoData;
    }
    let satisfied = bars
        .iter()
        .any(|b| match (claim.direction, claim.on_close) {
            (Direction::Above, true) => b.close >= claim.threshold,
            (Direction::Above, false) => b.high >= claim.threshold,
            (Direction::Below, true) => b.close <= claim.threshold,
            (Direction::Below, false) => b.low <= claim.threshold,
        });
    if satisfied {
        Verdict::Hit
    } else {
        Verdict::Miss
    }
}

/// Read-only market data client. Talks ONLY to the data host — see the module header.
pub struct MarketClient {
    key: String,
    secret: String,
    base: String,
    feed: String,
}

/// Read-only spot-crypto data client. The fixed host has market data and no order endpoint.
pub struct CryptoMarketClient {
    key: String,
    secret: String,
}

impl CryptoMarketClient {
    const BASE: &'static str = "https://data.alpaca.markets/v1beta3/crypto/us";

    pub fn from_env() -> anyhow::Result<Self> {
        let key = std::env::var("ALPACA_KEY_ID")
            .ok()
            .filter(|value| !value.trim().is_empty());
        let secret = std::env::var("ALPACA_SECRET_KEY")
            .ok()
            .filter(|value| !value.trim().is_empty());
        match (key, secret) {
            (Some(key), Some(secret)) => Ok(Self { key, secret }),
            (None, Some(_)) => anyhow::bail!("ALPACA_KEY_ID is not set"),
            (Some(_), None) => anyhow::bail!("ALPACA_SECRET_KEY is not set"),
            (None, None) => anyhow::bail!("no Alpaca credentials"),
        }
    }

    fn symbol_query(symbol: &str) -> String {
        symbol.trim().to_uppercase().replace('/', "%2F")
    }

    pub fn bars_url(symbol: &str, timeframe: &str, start: &str, end: &str) -> String {
        format!(
            "{}/bars?symbols={}&timeframe={timeframe}&start={start}&end={end}&sort=asc&limit=1000",
            Self::BASE,
            Self::symbol_query(symbol)
        )
    }

    pub fn latest_trade_url(symbol: &str) -> String {
        format!(
            "{}/latest/trades?symbols={}",
            Self::BASE,
            Self::symbol_query(symbol)
        )
    }

    pub fn bars(
        &self,
        symbol: &str,
        timeframe: &str,
        start: &str,
        end: &str,
    ) -> anyhow::Result<Vec<Bar>> {
        let body: serde_json::Value = ureq::get(&Self::bars_url(symbol, timeframe, start, end))
            .set("APCA-API-KEY-ID", &self.key)
            .set("APCA-API-SECRET-KEY", &self.secret)
            .timeout(std::time::Duration::from_secs(30))
            .call()?
            .into_json()?;
        Ok(parse_bars(&body))
    }

    pub fn last_price(&self, symbol: &str) -> anyhow::Result<f64> {
        let body: serde_json::Value = ureq::get(&Self::latest_trade_url(symbol))
            .set("APCA-API-KEY-ID", &self.key)
            .set("APCA-API-SECRET-KEY", &self.secret)
            .timeout(std::time::Duration::from_secs(20))
            .call()?
            .into_json()?;
        let canonical = symbol.trim().to_uppercase();
        let price = body
            .get("trades")
            .and_then(|trades| trades.get(&canonical))
            .and_then(|trade| trade.get("p"))
            .and_then(|price| price.as_f64())
            .ok_or_else(|| anyhow::anyhow!("no crypto trade price in the response"))?;
        if !price.is_finite() || price <= 0.0 {
            anyhow::bail!("invalid crypto trade price in the response");
        }
        Ok(price)
    }
}

impl MarketClient {
    /// Build from env, or say precisely which credential is missing.
    pub fn from_env() -> anyhow::Result<MarketClient> {
        let key = std::env::var("ALPACA_KEY_ID")
            .ok()
            .filter(|k| !k.trim().is_empty());
        let secret = std::env::var("ALPACA_SECRET_KEY")
            .ok()
            .filter(|k| !k.trim().is_empty());
        match (key, secret) {
            (Some(key), Some(secret)) => Ok(MarketClient {
                key,
                secret,
                // The DATA host, never the trading host. Not configurable on purpose.
                base: "https://data.alpaca.markets/v2".to_string(),
                // iex is the free feed; sip needs a paid subscription.
                feed: std::env::var("ALPACA_FEED").unwrap_or_else(|_| "iex".into()),
            }),
            (None, Some(_)) => anyhow::bail!("ALPACA_KEY_ID is not set"),
            (Some(_), None) => anyhow::bail!(
                "ALPACA_SECRET_KEY is not set (Alpaca needs BOTH the key id and the secret)"
            ),
            (None, None) => {
                anyhow::bail!("no Alpaca credentials (ALPACA_KEY_ID + ALPACA_SECRET_KEY)")
            }
        }
    }

    /// The bars URL for a window. Split out so the shape is testable without a network call.
    pub fn bars_url(
        base: &str,
        feed: &str,
        symbol: &str,
        timeframe: &str,
        start: &str,
        end: &str,
    ) -> String {
        format!(
            "{base}/stocks/{}/bars?timeframe={timeframe}&start={start}&end={end}&feed={feed}&limit=1000",
            symbol.trim().to_uppercase()
        )
    }

    /// Daily (or other timeframe) bars for a symbol across a window. RFC-3339 dates.
    pub fn bars(
        &self,
        symbol: &str,
        timeframe: &str,
        start: &str,
        end: &str,
    ) -> anyhow::Result<Vec<Bar>> {
        let url = Self::bars_url(&self.base, &self.feed, symbol, timeframe, start, end);
        let body: serde_json::Value = ureq::get(&url)
            .set("APCA-API-KEY-ID", &self.key)
            .set("APCA-API-SECRET-KEY", &self.secret)
            .timeout(std::time::Duration::from_secs(30))
            .call()?
            .into_json()?;
        Ok(parse_bars(&body))
    }

    /// The most recent trade price, for "where is it now" questions.
    pub fn last_price(&self, symbol: &str) -> anyhow::Result<f64> {
        let url = format!(
            "{}/stocks/{}/trades/latest?feed={}",
            self.base,
            symbol.trim().to_uppercase(),
            self.feed
        );
        let body: serde_json::Value = ureq::get(&url)
            .set("APCA-API-KEY-ID", &self.key)
            .set("APCA-API-SECRET-KEY", &self.secret)
            .timeout(std::time::Duration::from_secs(20))
            .call()?
            .into_json()?;
        body.get("trade")
            .and_then(|t| t.get("p"))
            .and_then(|p| p.as_f64())
            .ok_or_else(|| anyhow::anyhow!("no trade price in the response"))
    }
}

/// Pull the bar array out of Alpaca's envelope, tolerating both the single-symbol and
/// multi-symbol response shapes.
pub fn parse_bars(body: &serde_json::Value) -> Vec<Bar> {
    if let Some(arr) = body.get("bars").and_then(|b| b.as_array()) {
        return arr
            .iter()
            .filter_map(|b| serde_json::from_value(b.clone()).ok())
            .collect();
    }
    // multi-symbol: {"bars": {"SPY": [...]}}
    if let Some(map) = body.get("bars").and_then(|b| b.as_object()) {
        return map
            .values()
            .filter_map(|v| v.as_array())
            .flatten()
            .filter_map(|b| serde_json::from_value(b.clone()).ok())
            .collect();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            time: "2026-08-18T20:00:00Z".into(),
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 1.0,
        }
    }

    #[test]
    fn the_client_only_ever_addresses_the_data_host() {
        // The safety property is structural: there are no order endpoints on this host, so no
        // configuration mistake and no prompt can turn this client into a trade.
        let url = MarketClient::bars_url(
            "https://data.alpaca.markets/v2",
            "iex",
            "spy",
            "1Day",
            "2026-08-01",
            "2026-08-18",
        );
        assert!(
            url.starts_with("https://data.alpaca.markets/v2/stocks/SPY/bars"),
            "{url}"
        );
        assert!(
            !url.contains("paper-api"),
            "must never address the trading host: {url}"
        );
        assert!(!url.contains("/orders"), "{url}");
        assert!(url.contains("feed=iex"), "{url}");
    }

    #[test]
    fn crypto_urls_are_read_only_and_preserve_pair_symbology() {
        let bars = CryptoMarketClient::bars_url(
            "btc/usd",
            "15Min",
            "2026-08-29T00:00:00Z",
            "2026-08-30T00:00:00Z",
        );
        assert!(bars.starts_with("https://data.alpaca.markets/v1beta3/crypto/us/bars?"));
        assert!(bars.contains("symbols=BTC%2FUSD"), "{bars}");
        assert!(!bars.contains("paper-api"), "{bars}");
        let latest = CryptoMarketClient::latest_trade_url("ETH/USD");
        assert!(
            latest.contains("latest/trades?symbols=ETH%2FUSD"),
            "{latest}"
        );
    }

    #[test]
    fn a_close_claim_is_judged_on_closes_and_a_touch_claim_on_extremes() {
        // Touched 251 intraday but closed at 249.
        let bars = vec![bar(248.0, 251.0, 247.0, 249.0)];
        let touch = ResolvableClaim {
            symbol: "X".into(),
            direction: Direction::Above,
            threshold: 250.0,
            on_close: false,
        };
        let close = ResolvableClaim {
            symbol: "X".into(),
            direction: Direction::Above,
            threshold: 250.0,
            on_close: true,
        };
        assert_eq!(
            resolve(&touch, &bars),
            Verdict::Hit,
            "a touch claim is true the moment it trades there"
        );
        assert_eq!(
            resolve(&close, &bars),
            Verdict::Miss,
            "a close claim needs the close"
        );
    }

    #[test]
    fn downside_claims_use_the_low_and_the_close_respectively() {
        let bars = vec![bar(100.0, 101.0, 94.0, 99.0)];
        let touch = ResolvableClaim {
            symbol: "X".into(),
            direction: Direction::Below,
            threshold: 95.0,
            on_close: false,
        };
        let close = ResolvableClaim {
            symbol: "X".into(),
            direction: Direction::Below,
            threshold: 95.0,
            on_close: true,
        };
        assert_eq!(resolve(&touch, &bars), Verdict::Hit);
        assert_eq!(resolve(&close, &bars), Verdict::Miss);
    }

    #[test]
    fn no_data_is_not_a_miss() {
        // Scoring an unmeasurable claim as WRONG would quietly bias every calibration number
        // built on top of it — the mind would look worse than it is, for free.
        let c = ResolvableClaim {
            symbol: "X".into(),
            direction: Direction::Above,
            threshold: 1.0,
            on_close: true,
        };
        assert_eq!(resolve(&c, &[]), Verdict::NoData);
    }

    #[test]
    fn a_claim_is_hit_if_any_bar_in_the_window_satisfies_it() {
        let bars = vec![
            bar(10.0, 11.0, 9.0, 10.5),
            bar(10.5, 20.0, 10.0, 19.0),
            bar(19.0, 19.5, 18.0, 18.5),
        ];
        let c = ResolvableClaim {
            symbol: "X".into(),
            direction: Direction::Above,
            threshold: 18.0,
            on_close: true,
        };
        assert_eq!(
            resolve(&c, &bars),
            Verdict::Hit,
            "the middle bar closed above"
        );
    }

    #[test]
    fn both_response_shapes_parse() {
        let single = serde_json::json!({"bars":[{"t":"2026-08-18T20:00:00Z","o":1.0,"h":2.0,"l":0.5,"c":1.5,"v":10}]});
        assert_eq!(parse_bars(&single).len(), 1);
        let multi = serde_json::json!({"bars":{"SPY":[{"t":"2026-08-18T20:00:00Z","o":1.0,"h":2.0,"l":0.5,"c":1.5,"v":10}]}});
        assert_eq!(parse_bars(&multi).len(), 1);
        assert!(parse_bars(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn a_missing_secret_is_named_precisely() {
        // The live blocker: alpaca.txt carries a key and no secret, and a 401 from nginx does not
        // say which half is absent.
        std::env::set_var("ALPACA_KEY_ID", "PKTEST");
        std::env::remove_var("ALPACA_SECRET_KEY");
        let e = match MarketClient::from_env() {
            Ok(_) => panic!("must refuse without a secret"),
            Err(e) => e.to_string(),
        };
        assert!(e.contains("ALPACA_SECRET_KEY"), "{e}");
        std::env::remove_var("ALPACA_KEY_ID");
    }
}
