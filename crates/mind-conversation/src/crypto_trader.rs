//! Bounded 24/7 spot-crypto agent: BTC/USD and ETH/USD, long/flat, paper only.

use super::*;

const CRYPTO_PROFILE: &str = "crypto_trader_config";
const CRYPTO_SCAN_MS: i64 = 60 * 60_000;
const CRYPTO_MANAGE_MS: i64 = 5 * 60_000;
const CRYPTO_MAX_HOLD_MS: i64 = 24 * 60 * 60_000;
const CRYPTO_UNIVERSE: [&str; 2] = ["BTC/USD", "ETH/USD"];
type CryptoManageResult = (
    Vec<String>,
    Vec<String>,
    Vec<mind_tools::trades::ClosedTrade>,
    Vec<(String, String, f64)>,
);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CryptoTraderMode {
    #[default]
    Shadow,
    Paper,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CryptoTraderAction {
    Scan,
    Manage,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct TrackedCryptoPlan {
    pub(crate) symbol: String,
    pub(crate) plan: mind_tools::crypto::CryptoTradePlan,
    pub(crate) opened_at_ms: i64,
    #[serde(default)]
    pub(crate) close_submitted_ms: i64,
    #[serde(default)]
    pub(crate) close_order_id: String,
    #[serde(default)]
    pub(crate) close_entry: f64,
}

fn reconciled_crypto_close(
    plan: &TrackedCryptoPlan,
    order: &mind_tools::broker::OrderSnapshot,
    closed_at_ms: i64,
) -> std::result::Result<mind_tools::trades::ClosedTrade, String> {
    if order.id != plan.close_order_id || !same_symbol(&order.symbol, &plan.symbol) {
        return Err("broker exit identity does not match the tracked plan".to_string());
    }
    if !order.side.eq_ignore_ascii_case("sell") {
        return Err("broker exit side does not match the long-only plan".to_string());
    }
    let (filled_qty, exit) = order
        .completed_fill()
        .ok_or_else(|| format!("broker exit is still {}", order.status))?;
    let entry = if plan.close_entry.is_finite() && plan.close_entry > 0.0 {
        plan.close_entry
    } else {
        plan.plan.entry
    };
    Ok(mind_tools::trades::ClosedTrade {
        desk: "crypto".to_string(),
        symbol: plan.symbol.clone(),
        qty: filled_qty,
        entry,
        exit,
        fees: 0.0,
        opened_at_ms: plan.opened_at_ms,
        closed_at_ms: order.filled_at_ms.unwrap_or(closed_at_ms),
        exit_order_id: order.id.clone(),
    })
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub(crate) struct CryptoTraderConfig {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) mode: CryptoTraderMode,
    #[serde(default)]
    pub(crate) risk: mind_tools::crypto::CryptoRiskState,
    #[serde(default)]
    pub(crate) last_scan_ms: i64,
    #[serde(default)]
    pub(crate) last_manage_ms: i64,
    #[serde(default)]
    pub(crate) last_summary: String,
    #[serde(default)]
    pub(crate) plans: Vec<TrackedCryptoPlan>,
}

pub(crate) fn crypto_trader_action_at(
    cfg: &CryptoTraderConfig,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<CryptoTraderAction> {
    if !cfg.enabled {
        return None;
    }
    let now_ms = now.timestamp_millis();
    let scan_due = cfg.plans.is_empty()
        && cfg.risk.halted_reason.is_empty()
        && cfg.risk.entries
            < mind_tools::crypto::CryptoRiskLimits::default().max_entries_per_utc_day
        && now_ms.saturating_sub(cfg.last_scan_ms) >= CRYPTO_SCAN_MS;
    if cfg.mode == CryptoTraderMode::Shadow {
        return scan_due.then_some(CryptoTraderAction::Scan);
    }
    let manage_due = now_ms.saturating_sub(cfg.last_manage_ms) >= CRYPTO_MANAGE_MS;
    match (scan_due, manage_due) {
        (_, true) => Some(CryptoTraderAction::Manage),
        (true, false) => Some(CryptoTraderAction::Scan),
        _ => None,
    }
}

fn completed_crypto_window(now: chrono::DateTime<chrono::Utc>) -> Option<(String, String)> {
    let completed_end = chrono::DateTime::from_timestamp(
        now.timestamp() - now.timestamp().rem_euclid(15 * 60) - 1,
        0,
    )?;
    let start = completed_end - chrono::Duration::hours(10);
    Some((start.to_rfc3339(), completed_end.to_rfc3339()))
}

fn same_symbol(left: &str, right: &str) -> bool {
    let normalize = |value: &str| {
        value
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .flat_map(char::to_uppercase)
            .collect::<String>()
    };
    normalize(left) == normalize(right)
}

fn crypto_exit_needs_fresh_quote(flatten: bool, expired: bool) -> bool {
    !flatten && !expired
}

fn has_staked_crypto_trade(book: &[mind_tools::trades::OpenTrade], symbol: &str) -> bool {
    book.iter()
        .any(|trade| trade.staked && same_symbol(&trade.symbol, symbol))
}

impl ConversationEngine {
    async fn load_crypto_trader(&self) -> std::result::Result<CryptoTraderConfig, String> {
        let raw = self.memory.profile_get(CRYPTO_PROFILE).await.map_err(|_| {
            "Crypto-trader state is unavailable; refusing autonomous work.".to_string()
        })?;
        match raw {
            None => Ok(CryptoTraderConfig::default()),
            Some(raw) => serde_json::from_str(&raw).map_err(|_| {
                "Crypto-trader state is corrupt; refusing to overwrite or run it.".to_string()
            }),
        }
    }

    async fn save_crypto_trader(
        &self,
        cfg: &CryptoTraderConfig,
    ) -> std::result::Result<(), String> {
        let raw = serde_json::to_string(cfg)
            .map_err(|_| "Crypto-trader state could not be encoded.".to_string())?;
        self.memory
            .profile_set(CRYPTO_PROFILE, &raw)
            .await
            .map_err(|_| {
                "Crypto-trader state could not be persisted; no action was confirmed.".to_string()
            })
    }

    fn render_crypto_trader(cfg: &CryptoTraderConfig) -> String {
        if !cfg.enabled {
            return "₿ Crypto trader: OFF\n  `ym crypto-trader shadow` observes BTC/USD and ETH/USD around the clock.\n  `ym crypto-trader paper` adds fractional SANDBOX orders. Live trading is not supported."
                .to_string();
        }
        let mode = match cfg.mode {
            CryptoTraderMode::Shadow => "SHADOW — signals and grading, no orders",
            CryptoTraderMode::Paper => "PAPER — sandbox execution only",
        };
        let limits = mind_tools::crypto::CryptoRiskLimits::default();
        let mut output = format!(
            "₿ Crypto trader: ON ({mode})\n  playbook: long/flat 15-minute breakout above the prior 8-hour high; BTC/USD and ETH/USD only\n  gates: {:.2}% equity risk/trade · {:.2}% UTC-day loss · {} entries/UTC day · {:.0}% max notional\n  clock: scan hourly, manage every 5m, maximum 24h hold; weekends included\n  boundary: Alpaca PAPER host, fractional GTC orders; spot crypto cannot short",
            limits.risk_fraction_per_trade * 100.0,
            limits.max_daily_loss_fraction * 100.0,
            limits.max_entries_per_utc_day,
            limits.max_notional_fraction * 100.0,
        );
        if !cfg.risk.utc_date.is_empty() {
            output.push_str(&format!(
                "\n  UTC day: {} · entries {}/{}",
                cfg.risk.utc_date, cfg.risk.entries, limits.max_entries_per_utc_day
            ));
        }
        if !cfg.risk.halted_reason.is_empty() {
            output.push_str(&format!("\n  HALTED: {}", cfg.risk.halted_reason));
        }
        if !cfg.last_summary.is_empty() {
            output.push_str(&format!("\n  last: {}", cfg.last_summary));
        }
        output
    }

    pub async fn crypto_trader_cmd(&self, spec: &str) -> String {
        let cmd = spec.trim().to_lowercase();
        let mut cfg = match self.load_crypto_trader().await {
            Ok(cfg) => cfg,
            Err(message) => return message,
        };
        match cmd.as_str() {
            "" | "status" | "show" => Self::render_crypto_trader(&cfg),
            "off" | "stop" | "disable" => {
                if !cfg.plans.is_empty() {
                    return format!(
                        "Crypto-trader shutdown refused: {} owned paper position(s) still require reconciliation. Use `ym crypto-trader flatten`, then retry after the broker reports them closed.",
                        cfg.plans.len()
                    );
                }
                cfg.enabled = false;
                if let Err(message) = self.save_crypto_trader(&cfg).await {
                    return message;
                }
                Self::render_crypto_trader(&cfg)
            }
            "shadow" | "on" | "enable" => {
                if !cfg.plans.is_empty() && cfg.mode != CryptoTraderMode::Shadow {
                    return "Crypto mode change refused while owned paper positions remain open."
                        .to_string();
                }
                if let Err(message) = self.isolate_crypto_desk().await {
                    return message;
                }
                cfg.enabled = true;
                cfg.mode = CryptoTraderMode::Shadow;
                cfg.risk = Default::default();
                cfg.last_scan_ms = 0;
                cfg.last_manage_ms = 0;
                if let Err(message) = self.save_crypto_trader(&cfg).await {
                    return message;
                }
                Self::render_crypto_trader(&cfg)
            }
            "paper" | "paper on" | "enable paper" => {
                if !cfg.plans.is_empty() && cfg.mode != CryptoTraderMode::Paper {
                    return "Crypto mode change refused while owned paper positions remain open."
                        .to_string();
                }
                if let Err(message) = self.isolate_crypto_desk().await {
                    return message;
                }
                cfg.enabled = true;
                cfg.mode = CryptoTraderMode::Paper;
                cfg.risk = Default::default();
                cfg.last_scan_ms = 0;
                cfg.last_manage_ms = 0;
                if let Err(message) = self.save_crypto_trader(&cfg).await {
                    return message;
                }
                Self::render_crypto_trader(&cfg)
            }
            "run" | "run shadow" => self.crypto_scan(false, &mut cfg).await,
            "run paper" | "paper run" => {
                if !cfg.enabled || cfg.mode != CryptoTraderMode::Paper {
                    return "Crypto paper run refused: enable `ym crypto-trader paper` first so every order remains under persistent management."
                        .to_string();
                }
                self.crypto_scan(true, &mut cfg).await
            }
            "flatten" | "close all" => {
                if cfg.mode != CryptoTraderMode::Paper {
                    return "Crypto flatten is only meaningful in paper mode.".to_string();
                }
                self.crypto_manage(&mut cfg, true, true).await
            }
            "live" | "real" | "live on" => "Live crypto trading is not supported. This agent can observe or use only the compile-time paper broker."
                .to_string(),
            _ => "Usage: `ym crypto-trader status|shadow|paper|off|run|run paper|flatten`. Live trading is unavailable."
                .to_string(),
        }
    }

    async fn isolate_crypto_desk(&self) -> std::result::Result<(), String> {
        let session = self.paper_desk_cmd("off").await;
        if !session.contains("Paper desk: OFF") {
            return Err(format!(
                "Could not isolate crypto from the session desk: {session}"
            ));
        }
        let day = self.day_trader_cmd("off").await;
        if !day.contains("Pro day trader: OFF") {
            return Err(format!(
                "Could not isolate crypto from the day trader: {day}"
            ));
        }
        Ok(())
    }

    pub(crate) async fn stop_crypto_trader_for_other_desk(
        &self,
    ) -> std::result::Result<(), String> {
        let mut cfg = self.load_crypto_trader().await?;
        if cfg.enabled {
            if !cfg.plans.is_empty() {
                return Err(format!(
                    "{} owned crypto position(s) still require reconciliation",
                    cfg.plans.len()
                ));
            }
            cfg.enabled = false;
            cfg.last_summary = "stopped because an equities paper desk was enabled".to_string();
            self.save_crypto_trader(&cfg).await?;
        }
        Ok(())
    }

    pub async fn crypto_trader_tick(&self) -> Option<String> {
        let now = chrono::Utc::now();
        let mut cfg = self.load_crypto_trader().await.ok()?;
        if !cfg.enabled {
            return None;
        }
        let utc_date = now.format("%Y-%m-%d").to_string();
        if cfg.risk.utc_date != utc_date {
            cfg.risk = mind_tools::crypto::CryptoRiskState {
                utc_date,
                ..Default::default()
            };
            cfg.last_scan_ms = 0;
            cfg.last_manage_ms = 0;
            if cfg.mode == CryptoTraderMode::Paper {
                cfg.risk.start_equity = tokio::task::spawn_blocking(|| {
                    mind_tools::broker::PaperBroker::from_env()
                        .and_then(|broker| broker.account())
                        .map(|account| account.equity)
                })
                .await
                .ok()
                .and_then(|result| result.ok())?;
            }
            self.save_crypto_trader(&cfg).await.ok()?;
        }
        let scheduled = crypto_trader_action_at(&cfg, now)?;
        if cfg.mode == CryptoTraderMode::Paper && cfg.risk.start_equity > 0.0 {
            let equity = tokio::task::spawn_blocking(|| {
                mind_tools::broker::PaperBroker::from_env()
                    .and_then(|broker| broker.account())
                    .map(|account| account.equity)
            })
            .await
            .ok()
            .and_then(|result| result.ok())?;
            let floor = cfg.risk.start_equity
                * (1.0 - mind_tools::crypto::CryptoRiskLimits::default().max_daily_loss_fraction);
            if equity <= floor && cfg.risk.halted_reason.is_empty() {
                cfg.risk.halted_reason = format!(
                    "UTC-day loss gate: equity {:.2} fell to or below {:.2}",
                    equity, floor
                );
                self.save_crypto_trader(&cfg).await.ok()?;
                return Some(self.crypto_manage(&mut cfg, true, true).await);
            }
        }
        match scheduled {
            CryptoTraderAction::Scan => Some(
                self.crypto_scan(cfg.mode == CryptoTraderMode::Paper, &mut cfg)
                    .await,
            ),
            CryptoTraderAction::Manage => Some(self.crypto_manage(&mut cfg, true, false).await),
        }
    }

    async fn crypto_scan(&self, act: bool, cfg: &mut CryptoTraderConfig) -> String {
        let now = chrono::Utc::now();
        let Some((start, end)) = completed_crypto_window(now) else {
            return "Crypto scan unavailable: could not establish a completed bar window."
                .to_string();
        };
        let utc_date = now.format("%Y-%m-%d").to_string();
        if cfg.risk.utc_date != utc_date {
            cfg.risk = mind_tools::crypto::CryptoRiskState {
                utc_date,
                ..Default::default()
            };
        }
        if act && cfg.risk.start_equity <= 0.0 {
            cfg.risk.start_equity = match tokio::task::spawn_blocking(|| {
                mind_tools::broker::PaperBroker::from_env()
                    .and_then(|broker| broker.account())
                    .map(|account| account.equity)
            })
            .await
            {
                Ok(Ok(equity)) if equity.is_finite() && equity > 0.0 => equity,
                Ok(Ok(_)) => return "Crypto scan refused: invalid paper equity.".to_string(),
                Ok(Err(error)) => return format!("Crypto scan refused: {error}"),
                Err(error) => return format!("Crypto scan refused: paper task failed ({error})."),
            };
        }
        cfg.last_scan_ms = now.timestamp_millis();
        if let Err(message) = self.save_crypto_trader(cfg).await {
            return format!("Crypto scan skipped: {message}");
        }
        let candidates = tokio::task::spawn_blocking(move || {
            let market =
                mind_tools::CryptoMarketClient::from_env().map_err(|error| error.to_string())?;
            let mut output = Vec::new();
            for symbol in CRYPTO_UNIVERSE {
                let Ok(bars) = market.bars(symbol, "15Min", &start, &end) else {
                    continue;
                };
                if let Some(plan) = mind_tools::crypto::continuous_breakout(&bars) {
                    output.push((symbol.to_string(), plan));
                }
            }
            Ok::<_, String>(output)
        })
        .await
        .unwrap_or_else(|error| Err(format!("join failed: {error}")));
        let candidates = match candidates {
            Ok(candidates) => candidates,
            Err(error) => return format!("₿ Crypto scan failed: {error}"),
        };
        if candidates.is_empty() {
            cfg.last_summary = "scan: no confirmed crypto breakout".to_string();
            if let Err(message) = self.save_crypto_trader(cfg).await {
                return format!("₿ Crypto scan found no setup; {message}");
            }
            return "₿ CRYPTO SCAN — no confirmed BTC/USD or ETH/USD breakout. No trade is a valid result."
                .to_string();
        }
        let existing = self.open_trade_book().await;
        let mut lines = Vec::new();
        for (symbol, plan) in candidates {
            if !cfg.plans.is_empty() || has_staked_crypto_trade(&existing, &symbol) {
                lines.push(format!(
                    "{symbol} skipped — a crypto position is already tracked"
                ));
                continue;
            }
            let thesis = plan.setup.clone();
            self.judgment_log(
                "crypto-trader",
                "crypto",
                &format!("{symbol} long: {thesis}"),
                0.62,
                now.timestamp_millis() + CRYPTO_MAX_HOLD_MS,
                &symbol,
            )
            .await;
            if !act {
                self.record_open_trade(mind_tools::trades::OpenTrade {
                    symbol: symbol.clone(),
                    qty: 1.0,
                    entry: plan.entry,
                    opened_at_ms: now.timestamp_millis(),
                    judgment_ref: symbol.clone(),
                    thesis,
                    staked: false,
                })
                .await;
                lines.push(format!(
                    "{symbol} long @ {:.2} · invalid {:.2} · target {:.2} · SHADOW",
                    plan.entry, plan.invalidation, plan.target
                ));
                continue;
            }
            let preflight_symbol = symbol.clone();
            let preflight_plan = plan.clone();
            let state = cfg.risk.clone();
            let preflight = tokio::task::spawn_blocking(move || {
                let broker = mind_tools::broker::PaperBroker::from_env()
                    .map_err(|error| error.to_string())?;
                let account = broker.account().map_err(|error| error.to_string())?;
                if broker
                    .positions()
                    .map_err(|error| error.to_string())?
                    .iter()
                    .any(|position| same_symbol(&position.symbol, &preflight_symbol))
                {
                    return Err("broker already has this crypto position".to_string());
                }
                let notional = mind_tools::crypto::notional_for_risk(
                    account.equity,
                    account.equity,
                    &state,
                    mind_tools::crypto::CryptoRiskLimits::default(),
                    &preflight_plan,
                )
                .map_err(|reason| reason.to_string())?;
                mind_tools::broker::check_notional(notional, account.equity)
                    .map_err(|reason| reason.to_string())?;
                Ok::<_, String>(notional)
            })
            .await
            .unwrap_or_else(|error| Err(format!("join failed: {error}")));
            let notional = match preflight {
                Ok(notional) => notional,
                Err(error) => {
                    lines.push(format!("{symbol} refused — {error}"));
                    continue;
                }
            };
            cfg.risk.entries += 1;
            cfg.plans.push(TrackedCryptoPlan {
                symbol: symbol.clone(),
                plan: plan.clone(),
                opened_at_ms: now.timestamp_millis(),
                close_submitted_ms: 0,
                close_order_id: String::new(),
                close_entry: 0.0,
            });
            if let Err(message) = self.save_crypto_trader(cfg).await {
                cfg.risk.entries = cfg.risk.entries.saturating_sub(1);
                cfg.plans
                    .retain(|tracked| !same_symbol(&tracked.symbol, &symbol));
                lines.push(format!("{symbol} refused before submission — {message}"));
                continue;
            }
            let submit_symbol = symbol.clone();
            let placed = tokio::task::spawn_blocking(move || {
                mind_tools::broker::PaperBroker::from_env()
                    .and_then(|broker| {
                        broker.submit_crypto_market_notional(&submit_symbol, notional)
                    })
                    .map(|ack| format!("{} {}", ack.status, ack.id))
                    .map_err(|error| error.to_string())
            })
            .await
            .unwrap_or_else(|error| Err(format!("join failed: {error}")));
            match placed {
                Ok(ack) => {
                    self.record_open_trade(mind_tools::trades::OpenTrade {
                        symbol: symbol.clone(),
                        qty: notional / plan.entry,
                        entry: plan.entry,
                        opened_at_ms: now.timestamp_millis(),
                        judgment_ref: symbol.clone(),
                        thesis,
                        staked: true,
                    })
                    .await;
                    lines.push(format!(
                        "{symbol} long ${notional:.0} @ ~{:.2} · invalid {:.2} · target {:.2} · {ack}",
                        plan.entry, plan.invalidation, plan.target
                    ));
                }
                Err(error) => lines.push(format!(
                    "{symbol} submission uncertain — {error}; ownership retained for reconciliation"
                )),
            }
        }
        cfg.last_summary = format!("scan: {} result(s)", lines.len());
        if let Err(message) = self.save_crypto_trader(cfg).await {
            lines.push(message);
        }
        format!("₿ CRYPTO SCAN\n{}", lines.join("\n"))
    }

    async fn crypto_manage(
        &self,
        cfg: &mut CryptoTraderConfig,
        act: bool,
        flatten: bool,
    ) -> String {
        let now_ms = chrono::Utc::now().timestamp_millis();
        cfg.last_manage_ms = now_ms;
        let tracked = cfg.plans.clone();
        let result = tokio::task::spawn_blocking(
            move || -> std::result::Result<CryptoManageResult, String> {
                let broker = mind_tools::broker::PaperBroker::from_env()
                    .map_err(|error| error.to_string())?;
                let positions = broker.positions().map_err(|error| error.to_string())?;
                let market = mind_tools::CryptoMarketClient::from_env()
                    .map_err(|error| error.to_string())?;
                let mut lines = Vec::new();
                let mut closed = Vec::new();
                let mut reconciled = Vec::new();
                let mut submitted = Vec::new();
                for tracked_plan in tracked {
                    let Some(position) = positions
                        .iter()
                        .find(|position| same_symbol(&position.symbol, &tracked_plan.symbol))
                    else {
                        if tracked_plan.close_order_id.is_empty() {
                            lines.push(format!(
                                "{} no longer open; no owned exit id, so no realized result was inferred",
                                tracked_plan.symbol
                            ));
                            closed.push(tracked_plan.symbol);
                        } else {
                            match broker.order(&tracked_plan.close_order_id) {
                                Ok(order) => {
                                    match reconciled_crypto_close(&tracked_plan, &order, now_ms) {
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
                                    }
                                }
                                Err(error) => lines.push(format!(
                                    "{} exit reconciliation failed; remains tracked ({error})",
                                    tracked_plan.symbol
                                )),
                            }
                        }
                        continue;
                    };
                    let expired =
                        now_ms.saturating_sub(tracked_plan.opened_at_ms) >= CRYPTO_MAX_HOLD_MS;
                    let price = if crypto_exit_needs_fresh_quote(flatten, expired) {
                        match market.last_price(&tracked_plan.symbol) {
                            Ok(price) => Some(price),
                            Err(error) => {
                                lines.push(format!(
                                    "{} quote unavailable; remains tracked ({error})",
                                    tracked_plan.symbol
                                ));
                                continue;
                            }
                        }
                    } else {
                        None
                    };
                    let level_exit = price.is_some_and(|price| {
                        price <= tracked_plan.plan.invalidation || price >= tracked_plan.plan.target
                    });
                    if !(flatten || level_exit || expired) {
                        let price = price.expect("a level-based hold always has a fresh quote");
                        lines.push(format!(
                            "{} {:.2} · invalid {:.2} · target {:.2} · holding",
                            tracked_plan.symbol,
                            price,
                            tracked_plan.plan.invalidation,
                            tracked_plan.plan.target
                        ));
                        continue;
                    }
                    let reason = if flatten {
                        "risk flatten"
                    } else if expired {
                        "24-hour horizon"
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
                    if position.qty <= 0.0 {
                        lines.push(format!(
                            "{} has non-long broker quantity; refusing to guess how to close it",
                            tracked_plan.symbol
                        ));
                        continue;
                    }
                    match broker.submit_crypto_market_qty(
                        &tracked_plan.symbol,
                        position.qty,
                        mind_tools::broker::Side::Sell,
                    ) {
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
            },
        )
        .await
        .unwrap_or_else(|error| Err(format!("join failed: {error}")));
        let (mut lines, mut closed, reconciled, submitted) = match result {
            Ok(result) => result,
            Err(error) => return format!("₿ Crypto management failed: {error}"),
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
            cfg.plans.retain(|plan| !same_symbol(&plan.symbol, symbol));
            self.remove_open_trade(symbol).await;
        }
        for plan in &mut cfg.plans {
            if let Some((_, order_id, entry)) = submitted
                .iter()
                .find(|(symbol, _, _)| same_symbol(&plan.symbol, symbol))
            {
                plan.close_submitted_ms = now_ms;
                plan.close_order_id = order_id.clone();
                plan.close_entry = *entry;
            }
        }
        if lines.is_empty() {
            lines.push("no owned crypto positions to manage".to_string());
        }
        cfg.last_summary = if flatten && cfg.plans.is_empty() {
            "manage: crypto flatten complete".to_string()
        } else if flatten {
            format!(
                "manage: flatten pending for {} position(s)",
                cfg.plans.len()
            )
        } else {
            "manage: crypto stops, targets, and horizon checked".to_string()
        };
        if let Err(message) = self.save_crypto_trader(cfg).await {
            lines.push(message);
        }
        format!("₿ CRYPTO MANAGE\n{}", lines.join("\n"))
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
    fn crypto_clock_runs_on_weekends_and_manages_before_scanning() {
        let mut cfg = CryptoTraderConfig {
            enabled: true,
            mode: CryptoTraderMode::Paper,
            ..Default::default()
        };
        let saturday = at("2026-08-29T12:00:00Z");
        assert_eq!(
            crypto_trader_action_at(&cfg, saturday),
            Some(CryptoTraderAction::Manage)
        );
        cfg.last_manage_ms = saturday.timestamp_millis();
        assert_eq!(
            crypto_trader_action_at(&cfg, saturday + chrono::Duration::minutes(1)),
            Some(CryptoTraderAction::Scan)
        );
    }

    #[test]
    fn crypto_window_excludes_the_still_forming_fifteen_minute_bar() {
        let (_, end) = completed_crypto_window(at("2026-08-29T12:07:30Z")).unwrap();
        assert_eq!(end, "2026-08-29T11:59:59+00:00");
    }

    #[test]
    fn slash_and_legacy_broker_symbols_reconcile() {
        assert!(same_symbol("BTC/USD", "BTCUSD"));
        assert!(!same_symbol("BTC/USD", "ETHUSD"));
    }

    #[test]
    fn mandatory_crypto_exits_do_not_depend_on_a_fresh_quote() {
        assert!(!crypto_exit_needs_fresh_quote(true, false));
        assert!(!crypto_exit_needs_fresh_quote(false, true));
        assert!(!crypto_exit_needs_fresh_quote(true, true));
        assert!(crypto_exit_needs_fresh_quote(false, false));
    }

    #[test]
    fn gradeable_shadow_views_do_not_block_a_later_crypto_paper_entry() {
        let mut trade = mind_tools::trades::OpenTrade {
            symbol: "BTCUSD".to_string(),
            qty: 1.0,
            entry: 100_000.0,
            opened_at_ms: 1,
            judgment_ref: "BTC/USD".to_string(),
            thesis: "shadow view".to_string(),
            staked: false,
        };
        assert!(!has_staked_crypto_trade(&[trade.clone()], "BTC/USD"));
        trade.staked = true;
        assert!(has_staked_crypto_trade(&[trade], "BTC/USD"));
    }

    #[test]
    fn a_crypto_close_waits_for_the_exact_filled_sell() {
        let plan = TrackedCryptoPlan {
            symbol: "BTC/USD".to_string(),
            plan: mind_tools::crypto::CryptoTradePlan {
                entry: 100_000.0,
                invalidation: 99_000.0,
                target: 102_000.0,
                setup: "test".to_string(),
            },
            opened_at_ms: 10,
            close_submitted_ms: 20,
            close_order_id: "exit-1".to_string(),
            close_entry: 100_100.0,
        };
        let mut order = mind_tools::broker::OrderSnapshot {
            id: "exit-1".to_string(),
            symbol: "BTCUSD".to_string(),
            side: "sell".to_string(),
            status: "accepted".to_string(),
            filled_qty: 0.01,
            filled_avg_price: None,
            filled_at_ms: None,
        };

        assert!(reconciled_crypto_close(&plan, &order, 30).is_err());
        order.status = "filled".to_string();
        order.filled_avg_price = Some(101_100.0);
        order.filled_at_ms = Some(29);
        let trade = reconciled_crypto_close(&plan, &order, 30).unwrap();
        assert_eq!(trade.qty, 0.01);
        assert_eq!(trade.closed_at_ms, 29);
        assert_eq!(trade.net_pnl(), Some(10.0));
    }
}
