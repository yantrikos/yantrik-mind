//! markets — keyless quotes: crypto via CoinGecko (search → price), stocks via stooq CSV. No API key.
//! Numbers are reference data; the caller presents them (not financial advice).

use async_trait::async_trait;

/// A raw, structured quote — the numbers, so callers (e.g. a portfolio) can do math. The formatted
/// chat strings (`crypto`/`stock`) are rendered on top of this.
#[derive(Clone, Debug)]
pub struct Quote {
    pub name: String,
    pub symbol: String,
    pub price: f64,
    pub change_pct: f64,
    pub currency: String, // a symbol like "$"/"₹"/"€", or a bare code for the rest
}

impl Quote {
    fn render(&self, icon: &str, span: &str) -> String {
        let arrow = if self.change_pct >= 0.0 { "▲" } else { "▼" };
        format!("{icon} {} ({}): {}{} {arrow}{:.2}% ({span})", self.name, self.symbol, self.currency, fmt_money(self.price), self.change_pct.abs())
    }
    pub fn render_crypto(&self) -> String {
        self.render("💰", "24h")
    }
    pub fn render_stock(&self) -> String {
        self.render("📈", "today")
    }
}

#[async_trait]
pub trait MarketsClient: Send + Sync {
    /// Structured crypto quote for a free-text coin query (e.g. "btc", "ethereum").
    async fn crypto_quote(&self, query: &str) -> anyhow::Result<Quote>;
    /// Structured stock quote for a ticker (e.g. "AAPL"); US tickers assumed.
    async fn stock_quote(&self, symbol: &str) -> anyhow::Result<Quote>;

    /// Crypto price as a chat-ready line. Default = render the structured quote.
    async fn crypto(&self, query: &str) -> anyhow::Result<String> {
        Ok(self.crypto_quote(query).await?.render_crypto())
    }
    /// Stock quote as a chat-ready line. Default = render the structured quote.
    async fn stock(&self, symbol: &str) -> anyhow::Result<String> {
        Ok(self.stock_quote(symbol).await?.render_stock())
    }
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
    async fn crypto_quote(&self, query: &str) -> anyhow::Result<Quote> {
        let q = query.trim().to_string();
        if q.is_empty() {
            anyhow::bail!("which coin?");
        }
        tokio::task::spawn_blocking(move || -> anyhow::Result<Quote> {
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
            let change_pct = row["usd_24h_change"].as_f64().unwrap_or(0.0);
            Ok(Quote { name, symbol: sym, price, change_pct, currency: "$".into() })
        })
        .await?
    }

    async fn stock_quote(&self, symbol: &str) -> anyhow::Result<Quote> {
        let sym = symbol.trim().to_uppercase();
        if sym.is_empty() {
            anyhow::bail!("which ticker?");
        }
        tokio::task::spawn_blocking(move || -> anyhow::Result<Quote> {
            // Yahoo Finance chart endpoint (no key) — needs a browser UA or it 401/429s.
            let url = format!("https://query1.finance.yahoo.com/v8/finance/chart/{sym}");
            let v: serde_json::Value = ureq::get(&url)
                .timeout(std::time::Duration::from_secs(15))
                .set("User-Agent", "Mozilla/5.0 (compatible; yantrik-mind/1.0)")
                .query("interval", "1d")
                .query("range", "1d")
                .call()?
                .into_json()?;
            let meta = v["chart"]["result"].get(0).map(|r| r["meta"].clone()).ok_or_else(|| anyhow::anyhow!("no quote for \"{sym}\" (check the ticker)"))?;
            let price = meta["regularMarketPrice"].as_f64().ok_or_else(|| anyhow::anyhow!("no quote for \"{sym}\""))?;
            let prev = meta["chartPreviousClose"].as_f64().or_else(|| meta["previousClose"].as_f64()).unwrap_or(price);
            let change_pct = if prev != 0.0 { (price - prev) / prev * 100.0 } else { 0.0 };
            let name = meta["shortName"].as_str().or_else(|| meta["longName"].as_str()).unwrap_or(&sym).to_string();
            let currency = match meta["currency"].as_str().unwrap_or("USD") {
                "USD" => "$".to_string(),
                "INR" => "₹".to_string(),
                "EUR" => "€".to_string(),
                "GBP" => "£".to_string(),
                "JPY" => "¥".to_string(),
                other => format!("{other} "),
            };
            Ok(Quote { name, symbol: sym, price, change_pct, currency })
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

/// Deterministic markets for tests. `crypto`/`stock` are the canned chat lines; `price` is the
/// number every `*_quote` returns (so a portfolio can be valued deterministically).
pub struct ScriptedMarkets {
    pub crypto: String,
    pub stock: String,
    pub price: f64,
}

#[async_trait]
impl MarketsClient for ScriptedMarkets {
    async fn crypto_quote(&self, q: &str) -> anyhow::Result<Quote> {
        Ok(Quote { name: q.to_string(), symbol: q.to_uppercase(), price: self.price, change_pct: 0.0, currency: "$".into() })
    }
    async fn stock_quote(&self, s: &str) -> anyhow::Result<Quote> {
        Ok(Quote { name: s.to_string(), symbol: s.to_uppercase(), price: self.price, change_pct: 0.0, currency: "$".into() })
    }
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
