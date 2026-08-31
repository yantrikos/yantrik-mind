//! A bounded, professional-style intraday agent connected to the Mind.
//!
//! It has one transparent playbook, derives size from price-level risk, stops after three entries
//! or a one-percent paper-account drawdown, and owns only the positions it opened. The only broker
//! it can construct is `PaperBroker`, whose compile-time host is Alpaca's sandbox.

use super::*;

const DAY_TRADER_PROFILE: &str = "day_trader_config";
const DAY_SCAN_MS: i64 = 5 * 60_000;
const DAY_MANAGE_MS: i64 = 60_000;
type ManageResult = (
    Vec<String>,
    Vec<String>,
    Vec<mind_tools::trades::ClosedTrade>,
    Vec<(String, String, f64)>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DayTraderMode {
    #[default]
    Shadow,
    Paper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DayTraderAction {
    Scan,
    Manage,
    Flatten,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrackedDayPlan {
    pub(crate) symbol: String,
    pub(crate) plan: mind_tools::daytrade::DayTradePlan,
    pub(crate) opened_at_ms: i64,
    #[serde(default)]
    pub(crate) close_submitted_ms: i64,
    #[serde(default)]
    pub(crate) close_order_id: String,
    /// Broker position cost basis captured before submitting the exit. A position that has
    /// disappeared cannot supply this later, so reconciliation must carry it across polls.
    #[serde(default)]
    pub(crate) close_entry: f64,
}

fn reconciled_day_close(
    plan: &TrackedDayPlan,
    order: &mind_tools::broker::OrderSnapshot,
    closed_at_ms: i64,
) -> std::result::Result<mind_tools::trades::ClosedTrade, String> {
    if order.id != plan.close_order_id || !order.symbol.eq_ignore_ascii_case(&plan.symbol) {
        return Err("broker exit identity does not match the tracked plan".to_string());
    }
    let expected_side = match plan.plan.side {
        mind_tools::daytrade::TradeSide::Long => "sell",
        mind_tools::daytrade::TradeSide::Short => "buy",
    };
    if !order.side.eq_ignore_ascii_case(expected_side) {
        return Err("broker exit side does not match the tracked plan".to_string());
    }
    let (filled_qty, exit) = order
        .completed_fill()
        .ok_or_else(|| format!("broker exit is still {}", order.status))?;
    let entry = if plan.close_entry.is_finite() && plan.close_entry > 0.0 {
        plan.close_entry
    } else {
        plan.plan.entry
    };
    let qty = match plan.plan.side {
        mind_tools::daytrade::TradeSide::Long => filled_qty,
        mind_tools::daytrade::TradeSide::Short => -filled_qty,
    };
    Ok(mind_tools::trades::ClosedTrade {
        desk: "day".to_string(),
        symbol: plan.symbol.clone(),
        qty,
        entry,
        exit,
        fees: 0.0,
        opened_at_ms: plan.opened_at_ms,
        closed_at_ms: order.filled_at_ms.unwrap_or(closed_at_ms),
        exit_order_id: order.id.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct DayTraderConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) mode: DayTraderMode,
    #[serde(default)]
    pub(crate) risk: mind_tools::daytrade::DayRiskState,
    #[serde(default)]
    pub(crate) last_scan_ms: i64,
    #[serde(default)]
    pub(crate) last_manage_ms: i64,
    #[serde(default)]
    pub(crate) last_flatten_date: String,
    #[serde(default)]
    pub(crate) last_summary: String,
    #[serde(default)]
    pub(crate) plans: Vec<TrackedDayPlan>,
}

#[derive(Debug, Clone)]
struct DayCandidate {
    symbol: String,
    plan: mind_tools::daytrade::DayTradePlan,
    catalyst: String,
}

pub(crate) fn day_trader_action_at(
    cfg: &DayTraderConfig,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<DayTraderAction> {
    use chrono::{Datelike, Timelike};

    if !cfg.enabled {
        return None;
    }
    let ny = now.with_timezone(&chrono_tz::America::New_York);
    if matches!(ny.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun) {
        return None;
    }
    let minute = ny.hour() * 60 + ny.minute();
    let session = ny.format("%Y-%m-%d").to_string();
    if cfg.mode == DayTraderMode::Paper
        && (15 * 60 + 50..16 * 60).contains(&minute)
        && cfg.last_flatten_date != session
    {
        return Some(DayTraderAction::Flatten);
    }
    if !(9 * 60 + 45..15 * 60 + 50).contains(&minute) {
        return None;
    }
    let now_ms = now.timestamp_millis();
    let scan_due = minute < 15 * 60 + 30
        && cfg.risk.halted_reason.is_empty()
        && cfg.risk.entries
            < mind_tools::daytrade::DayRiskLimits::default().max_entries_per_session
        && now_ms.saturating_sub(cfg.last_scan_ms) >= DAY_SCAN_MS;
    if cfg.mode == DayTraderMode::Shadow {
        return scan_due.then_some(DayTraderAction::Scan);
    }
    let manage_due = now_ms.saturating_sub(cfg.last_manage_ms) >= DAY_MANAGE_MS;
    match (scan_due, manage_due) {
        (true, true) if cfg.last_manage_ms <= cfg.last_scan_ms => Some(DayTraderAction::Manage),
        (true, _) => Some(DayTraderAction::Scan),
        (_, true) => Some(DayTraderAction::Manage),
        _ => None,
    }
}

fn session_window(now: chrono::DateTime<chrono::Utc>) -> Option<(String, String, String)> {
    use chrono::TimeZone;

    let ny = now.with_timezone(&chrono_tz::America::New_York);
    let date = ny.date_naive();
    let start_local = date.and_hms_opt(9, 30, 0)?;
    let start = chrono_tz::America::New_York
        .from_local_datetime(&start_local)
        .single()?
        .with_timezone(&chrono::Utc)
        .to_rfc3339();
    Some((date.to_string(), start, now.to_rfc3339()))
}

fn entry_window_open(now: chrono::DateTime<chrono::Utc>) -> bool {
    use chrono::{Datelike, Timelike};

    let ny = now.with_timezone(&chrono_tz::America::New_York);
    !matches!(ny.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
        && (9 * 60 + 45..15 * 60 + 30).contains(&(ny.hour() * 60 + ny.minute()))
}

fn day_exit_needs_fresh_quote(flatten: bool) -> bool {
    !flatten
}

fn has_staked_day_trade(book: &[mind_tools::trades::OpenTrade], symbol: &str) -> bool {
    book.iter()
        .any(|trade| trade.staked && trade.symbol.eq_ignore_ascii_case(symbol))
}

fn management_window_open(now: chrono::DateTime<chrono::Utc>) -> bool {
    use chrono::{Datelike, Timelike};

    let ny = now.with_timezone(&chrono_tz::America::New_York);
    !matches!(ny.weekday(), chrono::Weekday::Sat | chrono::Weekday::Sun)
        && (9 * 60 + 30..16 * 60).contains(&(ny.hour() * 60 + ny.minute()))
}

impl ConversationEngine {
    async fn load_day_trader(&self) -> std::result::Result<DayTraderConfig, String> {
        let raw = self
            .memory
            .profile_get(DAY_TRADER_PROFILE)
            .await
            .map_err(|_| {
                "Day-trader state is unavailable; refusing autonomous work.".to_string()
            })?;
        match raw {
            None => Ok(DayTraderConfig::default()),
            Some(raw) => serde_json::from_str(&raw).map_err(|_| {
                "Day-trader state is corrupt; refusing to overwrite or run it.".to_string()
            }),
        }
    }

    async fn save_day_trader(&self, cfg: &DayTraderConfig) -> std::result::Result<(), String> {
        let raw = serde_json::to_string(cfg)
            .map_err(|_| "Day-trader state could not be encoded.".to_string())?;
        self.memory
            .profile_set(DAY_TRADER_PROFILE, &raw)
            .await
            .map_err(|_| {
                "Day-trader state could not be persisted; no action was confirmed.".to_string()
            })
    }

    fn render_day_trader(cfg: &DayTraderConfig) -> String {
        if !cfg.enabled {
            return "📊 Pro day trader: OFF\n  `ym day-trader shadow` observes and grades a deterministic opening-range setup.\n  `ym day-trader paper` adds risk-sized SANDBOX orders. Live trading is not supported."
                .to_string();
        }
        let mode = match cfg.mode {
            DayTraderMode::Shadow => "SHADOW — signals and grading, no orders",
            DayTraderMode::Paper => "PAPER — sandbox execution only",
        };
        let limits = mind_tools::daytrade::DayRiskLimits::default();
        let mut out = format!(
            "📊 Pro day trader: ON ({mode})\n  playbook: 15-minute opening-range breakout, fresh company catalyst, volume confirmation\n  gates: {:.2}% equity risk/trade · {:.1}% daily loss · {} entries/session · 2R minimum\n  clock: scan every 5m, manage every 1m, no entries after 15:30 ET, flatten owned positions at 15:50 ET\n  boundary: Alpaca PAPER host only; no live-broker path",
            limits.risk_fraction_per_trade * 100.0,
            limits.max_daily_loss_fraction * 100.0,
            limits.max_entries_per_session,
        );
        if !cfg.risk.session_date.is_empty() {
            out.push_str(&format!(
                "\n  session: {} · entries {}/{}",
                cfg.risk.session_date, cfg.risk.entries, limits.max_entries_per_session
            ));
        }
        if !cfg.risk.halted_reason.is_empty() {
            out.push_str(&format!("\n  HALTED: {}", cfg.risk.halted_reason));
        }
        if !cfg.last_summary.is_empty() {
            out.push_str(&format!("\n  last: {}", cfg.last_summary));
        }
        out
    }

    pub async fn day_trader_cmd(&self, spec: &str) -> String {
        let cmd = spec.trim().to_lowercase();
        let mut cfg = match self.load_day_trader().await {
            Ok(cfg) => cfg,
            Err(message) => return message,
        };
        match cmd.as_str() {
            "" | "status" | "show" => Self::render_day_trader(&cfg),
            "off" | "stop" | "disable" => {
                if !cfg.plans.is_empty() {
                    return format!(
                        "Day-trader shutdown refused: {} owned paper position(s) still require reconciliation. Use `ym day-trader flatten` during market hours, then retry `off` after the broker reports them closed.",
                        cfg.plans.len()
                    );
                }
                cfg.enabled = false;
                if let Err(message) = self.save_day_trader(&cfg).await {
                    return message;
                }
                Self::render_day_trader(&cfg)
            }
            "shadow" | "on" | "enable" => {
                if !cfg.plans.is_empty() && cfg.mode != DayTraderMode::Shadow {
                    return "Day-trader mode change refused while owned paper positions remain open. Flatten and reconcile them first."
                        .to_string();
                }
                if let Err(message) = self.stop_crypto_trader_for_other_desk().await {
                    return format!("Could not isolate the day trader from crypto: {message}");
                }
                let old = self.paper_desk_cmd("off").await;
                if !old.contains("Paper desk: OFF") {
                    return format!("Could not isolate the day trader from the session desk: {old}");
                }
                cfg.enabled = true;
                cfg.mode = DayTraderMode::Shadow;
                cfg.risk = Default::default();
                cfg.last_scan_ms = 0;
                cfg.last_manage_ms = 0;
                cfg.last_flatten_date.clear();
                if let Err(message) = self.save_day_trader(&cfg).await {
                    return message;
                }
                Self::render_day_trader(&cfg)
            }
            "paper" | "paper on" | "enable paper" => {
                if !cfg.plans.is_empty() && cfg.mode != DayTraderMode::Paper {
                    return "Day-trader mode change refused while owned paper positions remain open. Flatten and reconcile them first."
                        .to_string();
                }
                if let Err(message) = self.stop_crypto_trader_for_other_desk().await {
                    return format!("Could not isolate the day trader from crypto: {message}");
                }
                let old = self.paper_desk_cmd("off").await;
                if !old.contains("Paper desk: OFF") {
                    return format!("Could not isolate the day trader from the session desk: {old}");
                }
                cfg.enabled = true;
                cfg.mode = DayTraderMode::Paper;
                cfg.risk = Default::default();
                cfg.last_scan_ms = 0;
                cfg.last_manage_ms = 0;
                cfg.last_flatten_date.clear();
                if let Err(message) = self.save_day_trader(&cfg).await {
                    return message;
                }
                Self::render_day_trader(&cfg)
            }
            "run" | "run shadow" => self.day_scan(false, &mut cfg).await,
            "run paper" | "paper run" => {
                if !cfg.enabled || cfg.mode != DayTraderMode::Paper {
                    return "Day-trader paper run refused: enable `ym day-trader paper` first so every submitted order remains under persistent management."
                        .to_string();
                }
                if !entry_window_open(chrono::Utc::now()) {
                    return "Day-trader paper entries are allowed only 09:45–15:30 New York time on weekdays."
                        .to_string();
                }
                self.day_scan(true, &mut cfg).await
            }
            "flatten" | "close all" => {
                if cfg.mode != DayTraderMode::Paper {
                    return "Day-trader flatten is only meaningful in paper mode.".to_string();
                }
                if !management_window_open(chrono::Utc::now()) {
                    return "Day-trader flatten refused outside 09:30–16:00 New York time on weekdays; no queued market order was created."
                        .to_string();
                }
                self.day_manage(&mut cfg, true, true).await
            }
            "live" | "real" | "live on" => "Live trading is not supported. The pro day trader can observe in shadow mode or execute only against the compile-time paper broker."
                .to_string(),
            _ => "Usage: `ym day-trader status|shadow|paper|off|run|run paper|flatten`. Live trading is unavailable."
                .to_string(),
        }
    }

    pub(crate) async fn stop_day_trader_for_other_desk(&self) -> std::result::Result<(), String> {
        let mut cfg = self.load_day_trader().await?;
        if cfg.enabled {
            if !cfg.plans.is_empty() {
                return Err(format!(
                    "{} owned day-trader position(s) still require reconciliation",
                    cfg.plans.len()
                ));
            }
            cfg.enabled = false;
            cfg.last_summary = "stopped because the session paper desk was enabled".to_string();
            self.save_day_trader(&cfg).await?;
        }
        Ok(())
    }

    pub async fn day_trader_tick(&self) -> Option<String> {
        let now = chrono::Utc::now();
        let (session, _, _) = session_window(now)?;
        let mut cfg = self.load_day_trader().await.ok()?;
        if !cfg.enabled {
            return None;
        }
        if cfg.risk.session_date != session {
            cfg.risk = mind_tools::daytrade::DayRiskState {
                session_date: session.clone(),
                ..Default::default()
            };
            cfg.last_scan_ms = 0;
            cfg.last_manage_ms = 0;
            cfg.last_flatten_date.clear();
            if cfg.mode == DayTraderMode::Paper {
                let equity = tokio::task::spawn_blocking(|| {
                    mind_tools::broker::PaperBroker::from_env()
                        .and_then(|broker| broker.account())
                        .map(|account| account.equity)
                })
                .await
                .ok()
                .and_then(|result| result.ok())?;
                cfg.risk.session_start_equity = equity;
            }
            self.save_day_trader(&cfg).await.ok()?;
        }

        // Establish that the exchange clock permits work before any risk response can submit an
        // order. A loss gate noticed after hours remains halted and is handled next session; it may
        // not create a queued market order while the market is closed.
        let scheduled = day_trader_action_at(&cfg, now)?;

        let carried_position = cfg.plans.iter().any(|plan| {
            chrono::DateTime::from_timestamp_millis(plan.opened_at_ms)
                .map(|opened| {
                    opened
                        .with_timezone(&chrono_tz::America::New_York)
                        .format("%Y-%m-%d")
                        .to_string()
                        != session
                })
                .unwrap_or(true)
        });
        if cfg.mode == DayTraderMode::Paper && carried_position {
            cfg.risk.halted_reason =
                "carried position from a prior session; new entries halted".to_string();
            if self.save_day_trader(&cfg).await.is_err() {
                return None;
            }
            return Some(self.day_manage(&mut cfg, true, true).await);
        }

        if cfg.mode == DayTraderMode::Paper && cfg.risk.session_start_equity > 0.0 {
            let equity = tokio::task::spawn_blocking(|| {
                mind_tools::broker::PaperBroker::from_env()
                    .and_then(|broker| broker.account())
                    .map(|account| account.equity)
            })
            .await
            .ok()
            .and_then(|result| result.ok())?;
            let loss_floor = cfg.risk.session_start_equity
                * (1.0 - mind_tools::daytrade::DayRiskLimits::default().max_daily_loss_fraction);
            if equity <= loss_floor && cfg.risk.halted_reason.is_empty() {
                cfg.risk.halted_reason = format!(
                    "daily loss gate: equity {:.2} fell to or below {:.2}",
                    equity, loss_floor
                );
                self.save_day_trader(&cfg).await.ok()?;
                let report = self.day_manage(&mut cfg, true, true).await;
                return Some(report);
            }
        }

        match scheduled {
            DayTraderAction::Scan => Some(
                self.day_scan(cfg.mode == DayTraderMode::Paper, &mut cfg)
                    .await,
            ),
            DayTraderAction::Manage => Some(self.day_manage(&mut cfg, true, false).await),
            DayTraderAction::Flatten => Some(self.day_manage(&mut cfg, true, true).await),
        }
    }

    async fn day_scan(&self, act: bool, cfg: &mut DayTraderConfig) -> String {
        let now = chrono::Utc::now();
        let Some((session, start, end)) = session_window(now) else {
            return "Day scan unavailable: could not establish the New York session window."
                .to_string();
        };
        if cfg.risk.session_date != session {
            cfg.risk = mind_tools::daytrade::DayRiskState {
                session_date: session,
                ..Default::default()
            };
        }
        if act && cfg.risk.session_start_equity <= 0.0 {
            cfg.risk.session_start_equity = match tokio::task::spawn_blocking(|| {
                mind_tools::broker::PaperBroker::from_env()
                    .and_then(|broker| broker.account())
                    .map(|account| account.equity)
            })
            .await
            {
                Ok(Ok(equity)) if equity.is_finite() && equity > 0.0 => equity,
                Ok(Ok(_)) => return "Day scan refused: invalid paper-account equity.".to_string(),
                Ok(Err(error)) => {
                    return format!("Day scan refused: paper account unavailable ({error}).")
                }
                Err(error) => {
                    return format!("Day scan refused: paper account task failed ({error}).")
                }
            };
        }
        cfg.last_scan_ms = now.timestamp_millis();
        if let Err(message) = self.save_day_trader(cfg).await {
            return format!("Day scan skipped: {message}");
        }

        let candidates = tokio::task::spawn_blocking(
            move || -> std::result::Result<Vec<DayCandidate>, String> {
                let movers =
                    mind_tools::hunt::fetch_movers(30).map_err(|error| error.to_string())?;
                let (eligible, _) =
                    mind_tools::hunt::shortlist(&movers, &mind_tools::hunt::Bounds::default());
                let symbols: Vec<String> = eligible
                    .iter()
                    .take(8)
                    .map(|mover| mover.symbol.clone())
                    .collect();
                let news = mind_tools::hunt::fetch_news_for(&symbols, 50).unwrap_or_default();
                let market =
                    mind_tools::MarketClient::from_env().map_err(|error| error.to_string())?;
                let read_at = chrono::Utc::now().timestamp_millis();
                let mut out = Vec::new();
                for symbol in symbols {
                    let Some(headline) = mind_tools::hunt::catalyst_for(&symbol, &news) else {
                        continue;
                    };
                    if !mind_tools::hunt::is_fresh(&headline.at, read_at) {
                        continue;
                    }
                    let Ok(bars) = market.bars(&symbol, "5Min", &start, &end) else {
                        continue;
                    };
                    let Some(plan) = mind_tools::daytrade::opening_range_breakout(&bars) else {
                        continue;
                    };
                    out.push(DayCandidate {
                        symbol,
                        plan,
                        catalyst: headline.headline.clone(),
                    });
                }
                Ok(out)
            },
        )
        .await
        .unwrap_or_else(|error| Err(format!("join failed: {error}")));
        let candidates = match candidates {
            Ok(candidates) => candidates,
            Err(error) => return format!("📊 Day scan failed: {error}"),
        };
        if candidates.is_empty() {
            cfg.last_summary = "scan: no confirmed opening-range setup".to_string();
            let _ = self.save_day_trader(cfg).await;
            return "📊 DAY SCAN — no fresh, liquid opening-range breakout. No trade is a valid result."
                .to_string();
        }

        let existing = self.open_trade_book().await;
        let mut lines = Vec::new();
        for candidate in candidates {
            if has_staked_day_trade(&existing, &candidate.symbol)
                || cfg
                    .plans
                    .iter()
                    .any(|plan| plan.symbol.eq_ignore_ascii_case(&candidate.symbol))
            {
                lines.push(format!("{} skipped — already tracked", candidate.symbol));
                continue;
            }
            let side = match candidate.plan.side {
                mind_tools::daytrade::TradeSide::Long => "long",
                mind_tools::daytrade::TradeSide::Short => "short",
            };
            let thesis = format!(
                "{}; fresh catalyst: {}",
                candidate.plan.setup, candidate.catalyst
            );
            self.judgment_log(
                "day-trader",
                "trading",
                &format!("{} {side}: {thesis}", candidate.symbol),
                0.65,
                now.timestamp_millis() + 8 * 60 * 60_000,
                &candidate.symbol,
            )
            .await;
            if !act {
                self.record_open_trade(mind_tools::trades::OpenTrade {
                    symbol: candidate.symbol.clone(),
                    qty: if side == "long" { 1.0 } else { -1.0 },
                    entry: candidate.plan.entry,
                    opened_at_ms: now.timestamp_millis(),
                    judgment_ref: candidate.symbol.clone(),
                    thesis,
                    staked: false,
                })
                .await;
                lines.push(format!(
                    "{} {side} @ {:.2} · invalid {:.2} · target {:.2} · SHADOW",
                    candidate.symbol,
                    candidate.plan.entry,
                    candidate.plan.invalidation,
                    candidate.plan.target,
                ));
                continue;
            }

            let plan = candidate.plan.clone();
            let symbol = candidate.symbol.clone();
            let state = cfg.risk.clone();
            let preflight =
                tokio::task::spawn_blocking(move || -> std::result::Result<f64, String> {
                    let broker = mind_tools::broker::PaperBroker::from_env()
                        .map_err(|error| error.to_string())?;
                    let account = broker.account().map_err(|error| error.to_string())?;
                    if broker
                        .positions()
                        .map_err(|error| error.to_string())?
                        .iter()
                        .any(|position| position.symbol.eq_ignore_ascii_case(&symbol))
                    {
                        return Err("broker already has a position in this symbol".to_string());
                    }
                    let qty = mind_tools::daytrade::size_for_risk(
                        account.equity,
                        account.equity,
                        &state,
                        mind_tools::daytrade::DayRiskLimits::default(),
                        &plan,
                    )
                    .map_err(|reason| reason.to_string())?;
                    mind_tools::broker::check_order(qty, plan.entry, account.equity)
                        .map_err(|reason| reason.to_string())?;
                    Ok(qty)
                })
                .await
                .unwrap_or_else(|error| Err(format!("join failed: {error}")));
            let qty = match preflight {
                Ok(qty) => qty,
                Err(error) => {
                    lines.push(format!("{} refused — {error}", candidate.symbol));
                    continue;
                }
            };

            // Persist ownership and consume the entry slot before the outward order. If the
            // process or response disappears after submission, the next management pass still
            // knows which symbol it owns and reconciles it against the broker.
            cfg.risk.entries += 1;
            cfg.plans.push(TrackedDayPlan {
                symbol: candidate.symbol.clone(),
                plan: candidate.plan.clone(),
                opened_at_ms: now.timestamp_millis(),
                close_submitted_ms: 0,
                close_order_id: String::new(),
                close_entry: 0.0,
            });
            if let Err(message) = self.save_day_trader(cfg).await {
                cfg.risk.entries = cfg.risk.entries.saturating_sub(1);
                cfg.plans
                    .retain(|tracked| !tracked.symbol.eq_ignore_ascii_case(&candidate.symbol));
                lines.push(format!(
                    "{} refused before submission — {message}",
                    candidate.symbol
                ));
                continue;
            }

            let submit_symbol = candidate.symbol.clone();
            let broker_side = match candidate.plan.side {
                mind_tools::daytrade::TradeSide::Long => mind_tools::broker::Side::Buy,
                mind_tools::daytrade::TradeSide::Short => mind_tools::broker::Side::Sell,
            };
            let placed = tokio::task::spawn_blocking(move || {
                let broker = mind_tools::broker::PaperBroker::from_env()
                    .map_err(|error| error.to_string())?;
                broker
                    .submit_market(&submit_symbol, qty, broker_side)
                    .map(|ack| format!("{} {}", ack.status, ack.id))
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap_or_else(|error| Err(format!("join failed: {error}")));
            match placed {
                Ok(ack) => {
                    self.record_open_trade(mind_tools::trades::OpenTrade {
                        symbol: candidate.symbol.clone(),
                        qty: if side == "long" { qty } else { -qty },
                        entry: candidate.plan.entry,
                        opened_at_ms: now.timestamp_millis(),
                        judgment_ref: candidate.symbol.clone(),
                        thesis,
                        staked: true,
                    })
                    .await;
                    lines.push(format!(
                        "{} {side} {qty} @ ~{:.2} · invalid {:.2} · target {:.2} · {ack}",
                        candidate.symbol,
                        candidate.plan.entry,
                        candidate.plan.invalidation,
                        candidate.plan.target,
                    ));
                }
                Err(error) => lines.push(format!(
                    "{} submission result uncertain — {error}; ownership reservation retained for broker reconciliation",
                    candidate.symbol
                )),
            }
        }
        cfg.last_summary = format!("scan: {} result(s)", lines.len());
        if let Err(message) = self.save_day_trader(cfg).await {
            lines.push(message);
        }
        format!("📊 DAY SCAN\n{}", lines.join("\n"))
    }

    async fn day_manage(&self, cfg: &mut DayTraderConfig, act: bool, flatten: bool) -> String {
        let now = chrono::Utc::now();
        let now_ms = now.timestamp_millis();
        cfg.last_manage_ms = now_ms;
        let tracked = cfg.plans.clone();
        let result =
            tokio::task::spawn_blocking(move || -> std::result::Result<ManageResult, String> {
                let broker = mind_tools::broker::PaperBroker::from_env()
                    .map_err(|error| error.to_string())?;
                let positions = broker.positions().map_err(|error| error.to_string())?;
                let market =
                    mind_tools::MarketClient::from_env().map_err(|error| error.to_string())?;
                let mut lines = Vec::new();
                let mut closed = Vec::new();
                let mut reconciled = Vec::new();
                let mut submitted = Vec::new();
                for tracked_plan in tracked {
                    let Some(position) = positions.iter().find(|position| {
                        position.symbol.eq_ignore_ascii_case(&tracked_plan.symbol)
                    }) else {
                        if tracked_plan.close_order_id.is_empty() {
                            lines.push(format!(
                                "{} no longer open; no owned exit id, so no realized result was inferred",
                                tracked_plan.symbol
                            ));
                            closed.push(tracked_plan.symbol);
                        } else {
                            match broker.order(&tracked_plan.close_order_id) {
                                Ok(order) => match reconciled_day_close(&tracked_plan, &order, now_ms)
                                {
                                    Ok(trade) => {
                                        lines.push(format!(
                                            "{} exit fill broker-verified @ {:.2}",
                                            tracked_plan.symbol, trade.exit
                                        ));
                                        reconciled.push(trade);
                                    }
                                    Err(error) => lines.push(format!(
                                        "{} exit reconciliation pending: {error}",
                                        tracked_plan.symbol
                                    )),
                                },
                                Err(error) => lines.push(format!(
                                    "{} exit reconciliation failed; remains tracked ({error})",
                                    tracked_plan.symbol
                                )),
                            }
                        }
                        continue;
                    };
                    let price = if day_exit_needs_fresh_quote(flatten) {
                        match market.last_price(&tracked_plan.symbol) {
                            Ok(price) => Some(price),
                            Err(error) => {
                                lines.push(format!(
                                    "{} quote unavailable; position remains tracked ({error})",
                                    tracked_plan.symbol
                                ));
                                continue;
                            }
                        }
                    } else {
                        None
                    };
                    let level_exit = price.is_some_and(|price| match tracked_plan.plan.side {
                        mind_tools::daytrade::TradeSide::Long => {
                            price <= tracked_plan.plan.invalidation
                                || price >= tracked_plan.plan.target
                        }
                        mind_tools::daytrade::TradeSide::Short => {
                            price >= tracked_plan.plan.invalidation
                                || price <= tracked_plan.plan.target
                        }
                    });
                    if !(flatten || level_exit) {
                        let price = price.expect("a level-based hold always has a fresh quote");
                        lines.push(format!(
                            "{} {:.2} · invalid {:.2} · target {:.2} · holding",
                            tracked_plan.symbol,
                            price,
                            tracked_plan.plan.invalidation,
                            tracked_plan.plan.target,
                        ));
                        continue;
                    }
                    let reason = if flatten {
                        "session flatten"
                    } else {
                        "planned level fired"
                    };
                    let price_context = price
                        .map(|price| format!("@ ~{price:.2}"))
                        .unwrap_or_else(|| "without a fresh quote".to_string());
                    if !act {
                        lines.push(format!(
                            "{} would close {price_context} — {reason}",
                            tracked_plan.symbol
                        ));
                        continue;
                    }
                    if tracked_plan.close_submitted_ms > 0
                        && now_ms.saturating_sub(tracked_plan.close_submitted_ms) < 5 * 60_000
                    {
                        lines.push(format!(
                            "{} exit pending broker reconciliation — {reason}",
                            tracked_plan.symbol
                        ));
                        continue;
                    }
                    let side = if position.qty < 0.0 {
                        mind_tools::broker::Side::Buy
                    } else {
                        mind_tools::broker::Side::Sell
                    };
                    match broker.submit_market(&tracked_plan.symbol, position.qty.abs(), side) {
                        Ok(ack) => {
                            lines.push(format!(
                                "{} close submitted {price_context} — {reason} [{}]",
                                tracked_plan.symbol, ack.status
                            ));
                            submitted.push((
                                tracked_plan.symbol,
                                ack.id,
                                position.avg_entry_price,
                            ));
                        }
                        Err(error) => {
                            lines.push(format!("{} close failed: {error}", tracked_plan.symbol))
                        }
                    }
                }
                Ok((lines, closed, reconciled, submitted))
            })
            .await
            .unwrap_or_else(|error| Err(format!("join failed: {error}")));
        let (mut lines, mut closed, reconciled, submitted) = match result {
            Ok(result) => result,
            Err(error) => return format!("📊 Day management failed: {error}"),
        };
        for trade in reconciled {
            let symbol = trade.symbol.clone();
            match self.record_closed_trade(trade).await {
                Ok(()) => {
                    lines.push(format!("{symbol} realized result recorded"));
                    closed.push(symbol);
                }
                Err(error) => lines.push(format!(
                    "{symbol} result not recorded; ownership retained for retry ({error})"
                )),
            }
        }
        for symbol in &closed {
            cfg.plans
                .retain(|plan| !plan.symbol.eq_ignore_ascii_case(symbol));
            self.remove_open_trade(symbol).await;
        }
        for plan in &mut cfg.plans {
            if let Some((_, order_id, entry)) = submitted
                .iter()
                .find(|(symbol, _, _)| plan.symbol.eq_ignore_ascii_case(symbol))
            {
                plan.close_submitted_ms = now_ms;
                plan.close_order_id = order_id.clone();
                plan.close_entry = *entry;
            }
        }
        if flatten && cfg.plans.is_empty() {
            cfg.last_flatten_date = now
                .with_timezone(&chrono_tz::America::New_York)
                .format("%Y-%m-%d")
                .to_string();
        }
        if lines.is_empty() {
            lines.push("no owned paper positions to manage".to_string());
        }
        cfg.last_summary = if flatten && cfg.plans.is_empty() {
            "manage: session flatten complete".to_string()
        } else if flatten {
            format!(
                "manage: flatten retry pending for {} position(s)",
                cfg.plans.len()
            )
        } else {
            "manage: stops and targets checked".to_string()
        };
        if let Err(message) = self.save_day_trader(cfg).await {
            lines.push(message);
        }
        format!("📊 DAY MANAGE\n{}", lines.join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(value: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(value)
            .unwrap()
            .with_timezone(&chrono::Utc)
    }

    #[test]
    fn the_intraday_clock_waits_for_the_range_and_flattens_before_close() {
        let mut cfg = DayTraderConfig {
            enabled: true,
            mode: DayTraderMode::Paper,
            ..Default::default()
        };
        assert_eq!(day_trader_action_at(&cfg, at("2026-08-31T13:44:00Z")), None);
        assert_eq!(
            day_trader_action_at(&cfg, at("2026-08-31T13:45:00Z")),
            Some(DayTraderAction::Manage),
            "paper positions are reconciled before the first entry scan"
        );
        cfg.last_manage_ms = at("2026-08-31T13:45:00Z").timestamp_millis();
        assert_eq!(
            day_trader_action_at(&cfg, at("2026-08-31T13:46:00Z")),
            Some(DayTraderAction::Scan)
        );
        assert_eq!(
            day_trader_action_at(&cfg, at("2026-08-31T19:50:00Z")),
            Some(DayTraderAction::Flatten)
        );
        cfg.last_flatten_date = "2026-08-31".to_string();
        assert_eq!(day_trader_action_at(&cfg, at("2026-08-31T19:51:00Z")), None);
    }

    #[test]
    fn mandatory_session_flatten_does_not_depend_on_a_fresh_quote() {
        assert!(!day_exit_needs_fresh_quote(true));
        assert!(day_exit_needs_fresh_quote(false));
    }

    #[test]
    fn gradeable_shadow_views_do_not_block_a_later_paper_entry() {
        let mut trade = mind_tools::trades::OpenTrade {
            symbol: "AAPL".to_string(),
            qty: 1.0,
            entry: 100.0,
            opened_at_ms: 1,
            judgment_ref: "AAPL".to_string(),
            thesis: "shadow view".to_string(),
            staked: false,
        };
        assert!(!has_staked_day_trade(&[trade.clone()], "aapl"));
        trade.staked = true;
        assert!(has_staked_day_trade(&[trade], "aapl"));
    }

    #[test]
    fn a_day_close_is_accounted_only_from_the_matching_broker_fill() {
        let plan = TrackedDayPlan {
            symbol: "AAPL".to_string(),
            plan: mind_tools::daytrade::DayTradePlan {
                side: mind_tools::daytrade::TradeSide::Short,
                entry: 101.0,
                invalidation: 102.0,
                target: 99.0,
                setup: "test".to_string(),
            },
            opened_at_ms: 10,
            close_submitted_ms: 20,
            close_order_id: "exit-1".to_string(),
            close_entry: 100.5,
        };
        let order = mind_tools::broker::OrderSnapshot {
            id: "exit-1".to_string(),
            symbol: "AAPL".to_string(),
            side: "buy".to_string(),
            status: "filled".to_string(),
            filled_qty: 4.0,
            filled_avg_price: Some(98.0),
            filled_at_ms: Some(25),
        };

        let trade = reconciled_day_close(&plan, &order, 30).unwrap();
        assert_eq!(trade.qty, -4.0);
        assert_eq!(trade.entry, 100.5);
        assert_eq!(trade.closed_at_ms, 25);
        assert_eq!(trade.net_pnl(), Some(10.0));

        let mut wrong = order;
        wrong.id = "another-exit".to_string();
        assert!(reconciled_day_close(&plan, &wrong, 30).is_err());
    }
}
