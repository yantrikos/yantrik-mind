//! THE COUNTERFACTUAL — what shadowing these traders would actually have paid.
//!
//! The tape records what they held and when. This asks the only question that settles the idea:
//! if you had copied every entry and exit N minutes late, what would the arithmetic have been?
//!
//! I am writing this for myself to rely on, so it is built to be believed rather than to look
//! encouraging. Four decisions carry that, and each one makes the number worse:
//!
//! **The lag applies to BOTH sides.** You see their exit late for exactly the same reason you see
//! their entry late. Almost every naive copy-trade backtest lags only the entry, which quietly
//! grants the shadow a perfect exit it could never have taken — and that single asymmetry is
//! usually the whole apparent edge.
//!
//! **Costs come out of every round trip.** Commission-free is not cost-free: you cross a spread
//! going in and again coming out, and the small, thin names day traders favour have wide ones.
//!
//! **A missing price is an excluded trade, never a zero.** Scoring an unmeasurable trade as
//! break-even is how a backtest launders its own gaps into a flat, respectable-looking line.
//!
//! **Buy-and-hold is reported beside it.** A positive number means nothing if the tape covers a
//! day the whole market rose; the only interesting result is the part that is not the market.
//! This is the same denominator discipline the scoreboard already refuses to bend.
//!
//! Nothing here touches an order. It reads two series and does arithmetic.

use crate::market::Bar;
use crate::tape::{Side, Transition};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// How the shadow is assumed to behave.
#[derive(Debug, Clone, Copy)]
pub struct ShadowConfig {
    /// How late the shadow sees, and therefore acts. Applied to entries AND exits.
    pub lag_secs: i64,
    /// Half-spread plus fees, per side, in basis points.
    pub cost_bps_per_side: f64,
}

impl Default for ShadowConfig {
    fn default() -> Self {
        // 15bp a side is not pessimism, it is what a thin small-cap actually costs to cross.
        Self { lag_secs: 180, cost_bps_per_side: 15.0 }
    }
}

/// One shadowed round trip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ShadowTrade {
    pub symbol: String,
    pub side: String,
    pub entry_ms: i64,
    pub exit_ms: i64,
    pub entry_px: f64,
    pub exit_px: f64,
    /// Return in basis points, AFTER costs, signed for the direction taken.
    pub net_bps: f64,
}

/// What the whole tape would have paid at one lag.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShadowReport {
    pub lag_secs: i64,
    pub trades: Vec<ShadowTrade>,
    /// Round trips that could not be priced — reported, never silently dropped or zeroed.
    pub unpriced: usize,
    pub total_net_bps: f64,
    pub win_rate: f64,
    /// The same symbols held passively across the same windows, for comparison.
    pub buy_hold_bps: f64,
}

/// How many round trips before a result is worth reading at all. Below this the sign of the total
/// is noise, and reporting it as a finding would be the "spam wearing a metric" failure in another
/// costume.
pub const MIN_TRADES_TO_CONCLUDE: usize = 30;

impl ShadowReport {
    pub fn render(&self) -> String {
        let n = self.trades.len();
        let mut s = format!(
            "SHADOW at {}s lag — {} round trip(s){}\n",
            self.lag_secs,
            n,
            if self.unpriced > 0 { format!(", {} unpriced and excluded", self.unpriced) } else { String::new() }
        );
        if n == 0 {
            s.push_str("  nothing to report — no round trip could be priced.\n");
            return s;
        }
        s.push_str(&format!(
            "  net {:+.1} bps total, {:.0}% winners · passive hold of the same names: {:+.1} bps\n",
            self.total_net_bps,
            self.win_rate * 100.0,
            self.buy_hold_bps
        ));
        s.push_str(&format!(
            "  edge over simply holding: {:+.1} bps\n",
            self.total_net_bps - self.buy_hold_bps
        ));
        if n < MIN_TRADES_TO_CONCLUDE {
            s.push_str(&format!(
                "  NOT YET CONCLUSIVE: {n} of {MIN_TRADES_TO_CONCLUDE} round trips. The sign of this number is still noise.\n"
            ));
        }
        s
    }
}

/// The first printed price at or after `at_ms` — what a shadow acting then could actually have got.
/// Returns None rather than reaching backwards for a price that had already happened.
pub fn price_at_or_after(bars: &[Bar], at_ms: i64) -> Option<f64> {
    let mut best: Option<(i64, f64)> = None;
    for b in bars {
        let t = parse_rfc3339_ms(&b.time)?;
        if t >= at_ms && best.map(|(bt, _)| t < bt).unwrap_or(true) {
            best = Some((t, b.close));
        }
    }
    best.map(|(_, px)| px)
}

/// Minimal RFC-3339 → epoch millis. Only the shapes Alpaca emits.
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let time = rest.trim_end_matches('Z');
    let time = time.split(['+', '-']).next().unwrap_or(time);
    let mut t = time.split(':');
    let (h, mi): (i64, i64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    let sec: f64 = t.next().unwrap_or("0").parse().unwrap_or(0.0);
    // days since epoch (civil algorithm)
    let (y2, mo2) = if mo <= 2 { (y - 1, mo + 12) } else { (y, mo) };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let doy = (153 * (mo2 - 3) + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + h * 3600 + mi * 60) as f64 * 1000.0 + sec * 1000.0) as i64)
}

/// Pair entries with the matching exit, per trader and symbol.
fn round_trips(transitions: &[Transition]) -> Vec<(Transition, Transition)> {
    let mut open: HashMap<(String, String), Transition> = HashMap::new();
    let mut out = Vec::new();
    for t in transitions {
        let key = (t.trader.clone(), t.symbol.clone().unwrap_or_default());
        if t.kind == "entry" {
            open.insert(key, t.clone());
        } else if t.kind == "exit" {
            if let Some(entry) = open.remove(&key) {
                out.push((entry, t.clone()));
            }
        }
    }
    out
}

/// Run the counterfactual at one lag.
pub fn simulate(transitions: &[Transition], bars: &HashMap<String, Vec<Bar>>, cfg: ShadowConfig) -> ShadowReport {
    let lag_ms = cfg.lag_secs * 1000;
    let mut rep = ShadowReport { lag_secs: cfg.lag_secs, ..Default::default() };
    let mut wins = 0usize;
    let mut hold_total = 0.0;
    for (entry, exit) in round_trips(transitions) {
        let Some(sym) = entry.symbol.clone() else {
            rep.unpriced += 1;
            continue;
        };
        let Some(series) = bars.get(&sym) else {
            rep.unpriced += 1;
            continue;
        };
        // The lag applies to BOTH legs — the shadow sees the exit late for the same reason.
        let (Some(epx), Some(xpx)) = (
            price_at_or_after(series, entry.at_ms + lag_ms),
            price_at_or_after(series, exit.at_ms + lag_ms),
        ) else {
            rep.unpriced += 1;
            continue;
        };
        if epx <= 0.0 {
            rep.unpriced += 1;
            continue;
        }
        let raw_bps = (xpx - epx) / epx * 10_000.0;
        let signed = if entry.side == Side::Short { -raw_bps } else { raw_bps };
        let net = signed - cfg.cost_bps_per_side * 2.0;
        if net > 0.0 {
            wins += 1;
        }
        rep.total_net_bps += net;
        // Passive comparison over the SAME window: what holding it would have done, uncharged.
        if let (Some(h0), Some(h1)) = (price_at_or_after(series, entry.at_ms), price_at_or_after(series, exit.at_ms)) {
            if h0 > 0.0 {
                hold_total += (h1 - h0) / h0 * 10_000.0;
            }
        }
        rep.trades.push(ShadowTrade {
            symbol: sym,
            side: format!("{:?}", entry.side),
            entry_ms: entry.at_ms,
            exit_ms: exit.at_ms,
            entry_px: epx,
            exit_px: xpx,
            net_bps: net,
        });
    }
    let n = rep.trades.len();
    rep.win_rate = if n == 0 { 0.0 } else { wins as f64 / n as f64 };
    rep.buy_hold_bps = hold_total;
    rep
}

/// The answer to "how fast would I have to be" — the same tape at several delays.
pub fn lag_curve(transitions: &[Transition], bars: &HashMap<String, Vec<Bar>>, lags: &[i64], cost_bps: f64) -> Vec<ShadowReport> {
    lags.iter()
        .map(|&l| simulate(transitions, bars, ShadowConfig { lag_secs: l, cost_bps_per_side: cost_bps }))
        .collect()
}

/// Render the curve, which is the artefact that actually decides the question.
pub fn render_curve(reports: &[ShadowReport]) -> String {
    let mut s = String::from("LAG CURVE — net basis points after costs, by how late the shadow acts\n");
    for r in reports {
        s.push_str(&format!(
            "  {:>4}s  {:>8.1} bps  ({} trips, {:.0}% win, vs hold {:+.1})\n",
            r.lag_secs,
            r.total_net_bps,
            r.trades.len(),
            r.win_rate * 100.0,
            r.buy_hold_bps
        ));
    }
    if reports.iter().all(|r| r.trades.len() < MIN_TRADES_TO_CONCLUDE) {
        s.push_str(&format!("  NOT YET CONCLUSIVE at any lag — under {MIN_TRADES_TO_CONCLUDE} round trips.\n"));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(t: &str, close: f64) -> Bar {
        Bar { time: t.into(), open: close, high: close, low: close, close, volume: 1.0 }
    }
    fn tr(kind: &str, at_ms: i64, side: Side) -> Transition {
        Transition { at_ms, trader: "CHERIF".into(), kind: kind.into(), symbol: Some("OSHR".into()), side }
    }
    fn series() -> HashMap<String, Vec<Bar>> {
        // A rising minute series from 10:00 to 10:09 at $100 → $109.
        let mut m = HashMap::new();
        let bars: Vec<Bar> = (0..10).map(|i| bar(&format!("2026-08-18T10:{:02}:00Z", i), 100.0 + i as f64)).collect();
        m.insert("OSHR".to_string(), bars);
        m
    }

    #[test]
    fn epoch_parsing_is_right_or_every_number_here_is_wrong() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_rfc3339_ms("2026-08-18T10:00:00Z"), Some(1_787_047_200_000));
    }

    #[test]
    fn the_lag_applies_to_the_exit_too_and_that_is_the_whole_point() {
        let s = series();
        let t0 = parse_rfc3339_ms("2026-08-18T10:00:00Z").unwrap();
        // They enter at 10:00 and exit at 10:05 — a clean +5 move for them.
        let trans = vec![tr("entry", t0, Side::Long), tr("exit", t0 + 5 * 60_000, Side::Long)];
        // A shadow 3 minutes late buys at 10:03 ($103) and SELLS at 10:08 ($108) — still +5 here,
        // because this series only rises. The point is that the exit moved too.
        let r = simulate(&trans, &s, ShadowConfig { lag_secs: 180, cost_bps_per_side: 0.0 });
        assert_eq!(r.trades.len(), 1);
        assert_eq!(r.trades[0].entry_px, 103.0, "entry lagged");
        assert_eq!(r.trades[0].exit_px, 108.0, "EXIT lagged too — lagging only the entry invents a perfect exit");
    }

    #[test]
    fn costs_are_charged_on_both_sides() {
        let s = series();
        let t0 = parse_rfc3339_ms("2026-08-18T10:00:00Z").unwrap();
        let trans = vec![tr("entry", t0, Side::Long), tr("exit", t0 + 60_000, Side::Long)];
        let free = simulate(&trans, &s, ShadowConfig { lag_secs: 0, cost_bps_per_side: 0.0 });
        let charged = simulate(&trans, &s, ShadowConfig { lag_secs: 0, cost_bps_per_side: 15.0 });
        assert!((free.total_net_bps - charged.total_net_bps - 30.0).abs() < 1e-6, "two sides of 15bp");
    }

    #[test]
    fn a_short_makes_money_when_the_price_falls() {
        let mut m = HashMap::new();
        m.insert("OSHR".to_string(), vec![bar("2026-08-18T10:00:00Z", 100.0), bar("2026-08-18T10:01:00Z", 90.0)]);
        let t0 = parse_rfc3339_ms("2026-08-18T10:00:00Z").unwrap();
        let trans = vec![tr("entry", t0, Side::Short), tr("exit", t0 + 60_000, Side::Short)];
        let r = simulate(&trans, &m, ShadowConfig { lag_secs: 0, cost_bps_per_side: 0.0 });
        assert!(r.total_net_bps > 900.0, "a 10% drop shorted is ~+1000bp: {}", r.total_net_bps);
    }

    #[test]
    fn an_unpriceable_trip_is_excluded_and_counted_never_zeroed() {
        // Scoring an unmeasurable trade as break-even launders a gap into a respectable flat line.
        let s = series();
        let t0 = parse_rfc3339_ms("2026-08-18T10:00:00Z").unwrap();
        let mut a = tr("entry", t0, Side::Long);
        let mut b = tr("exit", t0 + 60_000, Side::Long);
        a.symbol = Some("NOPRICES".into());
        b.symbol = Some("NOPRICES".into());
        let r = simulate(&[a, b], &s, ShadowConfig::default());
        assert!(r.trades.is_empty());
        assert_eq!(r.unpriced, 1, "the gap is reported, not absorbed");
        assert!(r.render().contains("unpriced and excluded"));
    }

    #[test]
    fn a_thin_sample_refuses_to_conclude() {
        let s = series();
        let t0 = parse_rfc3339_ms("2026-08-18T10:00:00Z").unwrap();
        let trans = vec![tr("entry", t0, Side::Long), tr("exit", t0 + 60_000, Side::Long)];
        let r = simulate(&trans, &s, ShadowConfig::default());
        assert!(r.render().contains("NOT YET CONCLUSIVE"), "{}", r.render());
        assert!(render_curve(&[r]).contains("NOT YET CONCLUSIVE"));
    }

    #[test]
    fn buy_and_hold_is_reported_so_a_rising_tide_cannot_pass_as_edge() {
        let s = series();
        let t0 = parse_rfc3339_ms("2026-08-18T10:00:00Z").unwrap();
        let trans = vec![tr("entry", t0, Side::Long), tr("exit", t0 + 5 * 60_000, Side::Long)];
        let r = simulate(&trans, &s, ShadowConfig { lag_secs: 0, cost_bps_per_side: 0.0 });
        assert!(r.buy_hold_bps > 0.0, "the passive comparison is computed");
        assert!(r.render().contains("edge over simply holding"), "{}", r.render());
    }

    #[test]
    fn the_curve_answers_how_fast_you_would_need_to_be() {
        let s = series();
        let t0 = parse_rfc3339_ms("2026-08-18T10:00:00Z").unwrap();
        let trans = vec![tr("entry", t0, Side::Long), tr("exit", t0 + 5 * 60_000, Side::Long)];
        let c = lag_curve(&trans, &s, &[0, 60, 180], 15.0);
        assert_eq!(c.len(), 3);
        assert!(render_curve(&c).contains("LAG CURVE"));
    }
}
