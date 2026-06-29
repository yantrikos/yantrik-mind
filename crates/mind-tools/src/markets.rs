//! markets — keyless quotes: crypto via CoinGecko (search → price), stocks via stooq CSV. No API key.
//! Numbers are reference data; the caller presents them (not financial advice).

use async_trait::async_trait;

#[async_trait]
pub trait MarketsClient: Send + Sync {
    /// Crypto price for a free-text coin query (e.g. "btc", "ethereum").
    async fn crypto(&self, query: &str) -> anyhow::Result<String>;
    /// Stock quote for a ticker (e.g. "AAPL"); US tickers assumed.
    async fn stock(&self, symbol: &str) -> anyhow::Result<String>;
}

/// Live markets (CoinGecko + stooq).
pub struct LiveMarkets;

impl LiveMarkets {
    pub fn new() -> Self {
        Self
    }
}

impl Default for LiveMarkets {
    fn default() -> Self {
        Self
    }
}

#[async_trait]
impl MarketsClient for LiveMarkets {
    async fn crypto(&self, query: &str) -> anyhow::Result<String> {
        let q = query.trim().to_string();
        if q.is_empty() {
            anyhow::bail!("which coin?");
        }
        tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            // search → the top (highest market-cap) coin's id/name/symbol
            let s: serde_json::Value = ureq::get("https://api.coingecko.com/api/v3/search")
                .timeout(std::time::Duration::from_secs(15))
                .query("query", &q)
                .call()?
                .into_json()?;
            let coin = s["coins"].get(0).cloned().ok_or_else(|| anyhow::anyhow!("no coin matching \"{q}\""))?;
            let id = coin["id"].as_str().unwrap_or("").to_string();
            let name = coin["name"].as_str().unwrap_or(&q).to_string();
            let sym = coin["symbol"].as_str().unwrap_or("").to_uppercase();
            // price + 24h change
            let p: serde_json::Value = ureq::get("https://api.coingecko.com/api/v3/simple/price")
                .timeout(std::time::Duration::from_secs(15))
                .query("ids", &id)
                .query("vs_currencies", "usd")
                .query("include_24hr_change", "true")
                .call()?
                .into_json()?;
            let row = &p[&id];
            let price = row["usd"].as_f64().ok_or_else(|| anyhow::anyhow!("no price for {name}"))?;
            let chg = row["usd_24h_change"].as_f64().unwrap_or(0.0);
            let arrow = if chg >= 0.0 { "▲" } else { "▼" };
            Ok(format!("💰 {name} ({sym}): ${} {arrow}{:.2}% (24h)", fmt_money(price), chg.abs()))
        })
        .await?
    }

    async fn stock(&self, symbol: &str) -> anyhow::Result<String> {
        let sym = symbol.trim().to_lowercase();
        if sym.is_empty() {
            anyhow::bail!("which ticker?");
        }
        // stooq wants an exchange suffix; default US.
        let s = if sym.contains('.') { sym.clone() } else { format!("{sym}.us") };
        tokio::task::spawn_blocking(move || -> anyhow::Result<String> {
            let csv = ureq::get("https://stooq.com/q/l/")
                .timeout(std::time::Duration::from_secs(15))
                .query("s", &s)
                .query("f", "sd2t2ohlcvn")
                .query("h", "")
                .query("e", "csv")
                .call()?
                .into_string()?;
            // header line + one data line: Symbol,Date,Time,Open,High,Low,Close,Volume,Name
            let data = csv.lines().nth(1).ok_or_else(|| anyhow::anyhow!("no quote for {s}"))?;
            let cols: Vec<&str> = data.split(',').collect();
            if cols.len() < 7 || cols[6] == "N/D" {
                anyhow::bail!("no quote for \"{}\" (try the exact ticker)", s.trim_end_matches(".us"));
            }
            let close: f64 = cols[6].parse().map_err(|_| anyhow::anyhow!("no price for {s}"))?;
            let name = cols.get(8).map(|n| n.trim()).filter(|n| !n.is_empty()).unwrap_or(cols[0]);
            Ok(format!("📈 {name} ({}): ${}", cols[0].to_uppercase(), fmt_money(close)))
        })
        .await?
    }
}

/// Money with thousands separators + sensible precision (sub-$1 → more decimals).
fn fmt_money(v: f64) -> String {
    let dp = if v.abs() >= 1.0 { 2 } else { 6 };
    let s = format!("{v:.dp$}", dp = dp);
    // add thousands commas to the integer part
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let neg = int.starts_with('-');
    let digits = int.trim_start_matches('-');
    let mut grouped = String::new();
    for (i, c) in digits.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let int_fmt: String = grouped.chars().rev().collect();
    format!("{}{}{}", if neg { "-" } else { "" }, int_fmt, if frac.is_empty() { String::new() } else { format!(".{frac}") })
}

/// Deterministic markets for tests.
pub struct ScriptedMarkets {
    pub crypto: String,
    pub stock: String,
}

#[async_trait]
impl MarketsClient for ScriptedMarkets {
    async fn crypto(&self, _q: &str) -> anyhow::Result<String> {
        Ok(self.crypto.clone())
    }
    async fn stock(&self, _s: &str) -> anyhow::Result<String> {
        Ok(self.stock.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn money_formatting() {
        assert_eq!(fmt_money(67000.5), "67,000.50");
        assert_eq!(fmt_money(211.3), "211.30");
        assert!(fmt_money(0.00012345).starts_with("0.0001"));
        assert_eq!(fmt_money(-1234.0), "-1,234.00");
    }
}
