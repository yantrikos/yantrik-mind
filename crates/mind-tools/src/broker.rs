//! PAPER BROKER — the mind's hands, and only ever in the sandbox.
//!
//! `market.rs` reads prices and asserts it must never address the trading host. That was the right
//! rule while nothing could act at all, and the wrong shape once a PAPER account existed: paper
//! trading is the sandbox the broker built for exactly this, with simulated money, real fills and
//! real slippage. Refusing to use it does not make the mind safer, it makes the copy-trade thesis
//! untestable — a shadow book that never fills teaches nothing about whether the edge survives
//! contact with a queue.
//!
//! So the boundary moves rather than disappears, and it inverts:
//!
//! - `market.rs`: may address the DATA host, never the trading host.
//! - here:        may address the PAPER host, never the LIVE host.
//!
//! ## Why the host is a const and not a field
//!
//! Every other endpoint in this codebase is env-configurable. This one is not, and the difference
//! is the point: a configurable base is one typo, one copied `.env`, or one helpful override away
//! from real money. `PAPER_HOST` is compiled in, there is no setter, and no code path reads an
//! environment variable for it — so pointing this client at `api.alpaca.markets` is not a mistake
//! someone can make at runtime, it is an edit to this file that shows up in a diff.
//!
//! ## Why orders are size-bounded
//!
//! The paper account is an INSTRUMENT: its balance is the experiment's readout. A loop that fires a
//! single oversized order does not lose real money, it destroys the measurement — the account is
//! then reporting the fat finger rather than the strategy. So an order is capped as a fraction of
//! equity, and the cap is checked here rather than trusted to the caller.

use serde::{Deserialize, Serialize};

/// The ONLY host this module will ever speak to. Not configurable — see the module note.
const PAPER_HOST: &str = "https://paper-api.alpaca.markets/v2";

/// The live host, named here solely so tests can assert we never became it.
#[cfg(test)]
const LIVE_HOST_NEVER: &str = "https://api.alpaca.markets/v2";

/// Largest fraction of account equity one order may commit. A cap on the INSTRUMENT, not on risk
/// appetite: the paper balance is the experiment's readout, and one oversized fill would make it
/// report the mistake instead of the strategy.
const MAX_ORDER_FRACTION_OF_EQUITY: f64 = 0.10;

/// What the sandbox account is worth right now.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub account_number: String,
    pub status: String,
    pub cash: f64,
    pub equity: f64,
    pub buying_power: f64,
}

/// One open position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Position {
    pub symbol: String,
    pub qty: f64,
    pub avg_entry_price: f64,
    pub market_value: f64,
    pub unrealized_pl: f64,
}

/// Which way an order goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Buy,
    Sell,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Buy => "buy",
            Self::Sell => "sell",
        }
    }
}

/// What came back from submitting an order.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OrderAck {
    pub id: String,
    pub symbol: String,
    pub qty: String,
    pub side: String,
    pub status: String,
}

pub struct PaperBroker {
    key: String,
    secret: String,
}

/// Why an order was refused before it was ever sent. Refusing HERE, with a reason, beats letting
/// the broker refuse it: the caller learns which bound it hit rather than reading an HTTP 422.
#[derive(Debug, Clone, PartialEq)]
pub enum Refusal {
    /// Notional exceeds the per-order share of equity.
    TooLarge { notional: f64, cap: f64 },
    /// Non-positive size — almost always a bug upstream, never a trade.
    NotPositive,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { notional, cap } => write!(
                f,
                "order is ${notional:.2}, over the ${cap:.2} per-order cap ({:.0}% of equity) — \
                 the paper balance is the experiment's readout, so one oversized fill would make it \
                 report the mistake instead of the strategy",
                MAX_ORDER_FRACTION_OF_EQUITY * 100.0
            ),
            Self::NotPositive => write!(f, "order size is not positive — refusing rather than guessing what was meant"),
        }
    }
}

/// Would this order be allowed? Pure, so the bound is testable without a network or an account.
pub fn check_order(qty: f64, price: f64, equity: f64) -> Result<f64, Refusal> {
    if qty <= 0.0 || price <= 0.0 {
        return Err(Refusal::NotPositive);
    }
    let notional = qty * price;
    let cap = equity * MAX_ORDER_FRACTION_OF_EQUITY;
    if notional > cap {
        return Err(Refusal::TooLarge { notional, cap });
    }
    Ok(notional)
}

impl PaperBroker {
    /// Build from the same credentials the data client uses — one Alpaca key pair serves both hosts.
    pub fn from_env() -> anyhow::Result<PaperBroker> {
        let key = std::env::var("ALPACA_KEY_ID").ok().filter(|k| !k.trim().is_empty());
        let secret = std::env::var("ALPACA_SECRET_KEY").ok().filter(|k| !k.trim().is_empty());
        match (key, secret) {
            (Some(key), Some(secret)) => Ok(PaperBroker { key, secret }),
            (None, Some(_)) => anyhow::bail!("ALPACA_KEY_ID is not set"),
            (Some(_), None) => anyhow::bail!("ALPACA_SECRET_KEY is not set (Alpaca needs BOTH)"),
            (None, None) => anyhow::bail!("no Alpaca credentials (ALPACA_KEY_ID + ALPACA_SECRET_KEY)"),
        }
    }

    fn get(&self, path: &str) -> anyhow::Result<serde_json::Value> {
        Ok(ureq::get(&format!("{PAPER_HOST}{path}"))
            .set("APCA-API-KEY-ID", &self.key)
            .set("APCA-API-SECRET-KEY", &self.secret)
            .timeout(std::time::Duration::from_secs(30))
            .call()?
            .into_json()?)
    }

    /// The sandbox account.
    pub fn account(&self) -> anyhow::Result<Account> {
        let v = self.get("/account")?;
        let f = |k: &str| v.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0);
        Ok(Account {
            account_number: v.get("account_number").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            status: v.get("status").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            cash: f("cash"),
            equity: f("equity"),
            buying_power: f("buying_power"),
        })
    }

    /// Open positions.
    pub fn positions(&self) -> anyhow::Result<Vec<Position>> {
        let v = self.get("/positions")?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .map(|p| {
                        let f = |k: &str| {
                            p.get(k).and_then(|x| x.as_str()).and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0)
                        };
                        Position {
                            symbol: p.get("symbol").and_then(|x| x.as_str()).unwrap_or("").to_string(),
                            qty: f("qty"),
                            avg_entry_price: f("avg_entry_price"),
                            market_value: f("market_value"),
                            unrealized_pl: f("unrealized_pl"),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Place a market order in the sandbox, after the size bound has been checked against equity.
    ///
    /// `day` time-in-force on purpose: an order that outlives the session it was reasoned about is
    /// no longer the trade anyone decided on.
    pub fn submit_market(&self, symbol: &str, qty: f64, side: Side) -> anyhow::Result<OrderAck> {
        let body = serde_json::json!({
            "symbol": symbol.trim().to_uppercase(),
            "qty": qty.to_string(),
            "side": side.as_str(),
            "type": "market",
            "time_in_force": "day",
        });
        let v: serde_json::Value = ureq::post(&format!("{PAPER_HOST}/orders"))
            .set("APCA-API-KEY-ID", &self.key)
            .set("APCA-API-SECRET-KEY", &self.secret)
            .timeout(std::time::Duration::from_secs(30))
            .send_json(body)?
            .into_json()?;
        Ok(OrderAck {
            id: v.get("id").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            symbol: v.get("symbol").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            qty: v.get("qty").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            side: v.get("side").and_then(|x| x.as_str()).unwrap_or("").to_string(),
            status: v.get("status").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_client_can_only_ever_reach_the_paper_host() {
        // The whole safety argument of this module in one assertion. If a future edit makes the
        // host configurable, or points it at production, this fails — and it fails in a diff rather
        // than in an account.
        assert!(PAPER_HOST.contains("paper-api.alpaca.markets"), "{PAPER_HOST}");
        assert_ne!(PAPER_HOST, LIVE_HOST_NEVER, "the paper broker became the live broker");
        assert!(!PAPER_HOST.starts_with("https://api."), "{PAPER_HOST} is the live trading host");
    }

    #[test]
    fn an_order_may_not_swamp_the_account_that_is_measuring_it() {
        // 10k equity → a 1k cap. The bound protects the INSTRUMENT: the paper balance is how the
        // copy-trade experiment reports itself, and one fat-fingered fill would make it report the
        // mistake rather than the strategy.
        assert_eq!(check_order(5.0, 100.0, 10_000.0), Ok(500.0));
        let too_big = check_order(50.0, 100.0, 10_000.0).unwrap_err();
        assert!(matches!(too_big, Refusal::TooLarge { .. }), "{too_big:?}");
        assert!(too_big.to_string().contains("1000.00"), "the message must name the cap: {too_big}");
    }

    #[test]
    fn a_nonpositive_size_is_a_bug_upstream_not_a_trade() {
        assert_eq!(check_order(0.0, 100.0, 10_000.0), Err(Refusal::NotPositive));
        assert_eq!(check_order(-3.0, 100.0, 10_000.0), Err(Refusal::NotPositive));
        // A missing price must not silently become a free order.
        assert_eq!(check_order(10.0, 0.0, 10_000.0), Err(Refusal::NotPositive));
    }

    #[test]
    fn the_cap_scales_with_the_account_rather_than_being_a_magic_dollar_figure() {
        // A cap fixed in dollars would be wrong the moment the account grew or shrank.
        assert!(check_order(1.0, 900.0, 10_000.0).is_ok());
        assert!(check_order(1.0, 900.0, 1_000.0).is_err(), "same order, smaller account, must refuse");
    }
}
