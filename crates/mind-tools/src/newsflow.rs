//! BREAKING-NEWS DATAFLOW — peek, decide, and mostly let it go.
//!
//! A firehose read in full is not attention, it is expense. So headlines stream past a CHEAP
//! deterministic peek — no model, no fetch, string work only — and the small fraction that touches
//! something the mind is actually waiting on gets read properly. Everything else is dropped and
//! COUNTED, because a filter whose rejections are invisible cannot be told apart from a broken one.
//!
//! ## What makes a headline interesting
//!
//! The best answer is not a topic list somebody maintains. It is the mind's own OPEN QUESTIONS.
//! When a claim is pending in the judgment ledger — "memory names will sustain the bid on the gap
//! up" — then news about Micron is worth reading, because it bears on something the mind has
//! already committed to being graded on. Attention follows outstanding questions, and the interest
//! list maintains itself: a claim resolves, its symbols stop mattering, and the noise floor drops
//! without anyone editing a config.
//!
//! Tracked topics and known entities are weaker signals kept behind that, in that order.

use serde::{Deserialize, Serialize};

/// What the mind currently cares about, assembled from live state rather than configuration.
#[derive(Debug, Clone, Default)]
pub struct Watchlist {
    /// Tickers named by claims awaiting a grade — the strongest signal there is.
    pub claim_symbols: Vec<String>,
    /// Explicitly tracked subjects (`ym news track …`).
    pub topics: Vec<String>,
    /// People/companies/projects the mind holds beliefs about.
    pub entities: Vec<String>,
}

/// Why a headline survived the peek. Ordered by how much it earns a read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Interest {
    /// Bears on a claim awaiting a grade.
    PendingClaim(String),
    /// Matches something explicitly tracked.
    TrackedTopic(String),
    /// Mentions someone or something the mind knows.
    KnownEntity(String),
}

impl Interest {
    pub fn why(&self) -> String {
        match self {
            Interest::PendingClaim(s) => format!("bears on an open claim about {s}"),
            Interest::TrackedTopic(t) => format!("tracked topic: {t}"),
            Interest::KnownEntity(e) => format!("mentions {e}"),
        }
    }
}

/// Lowercased text padded with spaces and stripped of punctuation, so matching can require whole
/// words. Substring matching would make "IT" match "with" and "AI" match "said" — the exact
/// false-positive class that turns a filter into a passthrough.
fn bounded(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push(' ');
    let mut prev_space = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
            prev_space = false;
        } else if !prev_space {
            out.push(' ');
            prev_space = true;
        }
    }
    if !out.ends_with(' ') {
        out.push(' ');
    }
    out
}

fn has_word(hay: &str, needle: &str) -> bool {
    let n = needle.trim().to_lowercase();
    if n.is_empty() {
        return false;
    }
    hay.contains(&format!(" {n} "))
}

/// A ticker in prose is only credible as `$TSLA` or as a standalone capitalised token. Bare
/// two-letter symbols match far too much English to be trusted on their own.
fn mentions_symbol(raw: &str, bounded_text: &str, symbol: &str) -> bool {
    let s = symbol.trim();
    if s.len() < 2 {
        return false;
    }
    if raw.contains(&format!("${}", s.to_uppercase())) {
        return true;
    }
    if s.len() <= 2 {
        // Only the explicit $ form counts for very short symbols.
        return false;
    }
    has_word(bounded_text, s)
}

/// THE PEEK. Cheap, deterministic, no network and no model — this runs on every headline, so it
/// must cost nothing. Returns why the headline earns a read, or None to drop it.
pub fn peek(headline: &str, w: &Watchlist) -> Option<Interest> {
    let b = bounded(headline);
    for s in &w.claim_symbols {
        if mentions_symbol(headline, &b, s) {
            return Some(Interest::PendingClaim(s.to_uppercase()));
        }
    }
    for t in &w.topics {
        // A topic may be a phrase; require every word of it to appear.
        if !t.trim().is_empty() && t.split_whitespace().all(|word| has_word(&b, word)) {
            return Some(Interest::TrackedTopic(t.clone()));
        }
    }
    for e in &w.entities {
        if !e.trim().is_empty()
            && e.split_whitespace()
                .all(|word| word.len() >= 3 && has_word(&b, word))
        {
            return Some(Interest::KnownEntity(e.clone()));
        }
    }
    None
}

/// The outcome of one pass over a batch of headlines. `dropped` is reported, never hidden: a
/// filter that quietly discards is indistinguishable from one that is broken, and the ratio is
/// the only evidence that the peek is doing any work at all.
#[derive(Debug, Clone, Default)]
pub struct PeekPass {
    pub kept: Vec<(String, Interest)>,
    pub dropped: usize,
}

impl PeekPass {
    pub fn summary(&self) -> String {
        let seen = self.kept.len() + self.dropped;
        if seen == 0 {
            return "no headlines in this pass.".to_string();
        }
        format!(
            "peeked {seen} headline(s), read {}, let {} go ({:.0}% dropped)",
            self.kept.len(),
            self.dropped,
            self.dropped as f64 * 100.0 / seen as f64
        )
    }
}

/// Run the peek over a batch.
pub fn peek_batch(headlines: &[String], w: &Watchlist) -> PeekPass {
    let mut pass = PeekPass::default();
    for h in headlines {
        match peek(h, w) {
            Some(i) => pass.kept.push((h.clone(), i)),
            None => pass.dropped += 1,
        }
    }
    pass
}

#[cfg(test)]
mod tests {
    use super::*;

    fn watchlist() -> Watchlist {
        Watchlist {
            claim_symbols: vec!["MU".into(), "SNDK".into(), "SPY".into()],
            topics: vec!["interest rates".into()],
            entities: vec!["Anthropic".into()],
        }
    }

    #[test]
    fn an_open_claim_earns_the_read() {
        let w = watchlist();
        let hit = peek("Micron ($MU) guides higher on memory demand", &w);
        assert_eq!(hit, Some(Interest::PendingClaim("MU".into())));
        assert!(hit.unwrap().why().contains("open claim"));
        // Longer symbols match as bare words too.
        assert_eq!(
            peek("SNDK rallies after earnings", &w),
            Some(Interest::PendingClaim("SNDK".into()))
        );
    }

    #[test]
    fn short_symbols_need_the_dollar_form_or_they_swallow_english() {
        let w = Watchlist {
            claim_symbols: vec!["IT".into(), "AI".into(), "MU".into()],
            ..Default::default()
        };
        // These would match constantly on substrings; they must not.
        assert_eq!(peek("The council said it will review the policy", &w), None);
        assert_eq!(
            peek("Analysts said AI spending is rising", &w),
            None,
            "bare two-letter tickers are not credible"
        );
        // The explicit form is unambiguous and does match.
        assert_eq!(
            peek("Shares of $AI jumped 9%", &w),
            Some(Interest::PendingClaim("AI".into()))
        );
    }

    #[test]
    fn claims_outrank_topics_and_topics_outrank_entities() {
        let w = Watchlist {
            claim_symbols: vec!["SPY".into()],
            topics: vec!["interest rates".into()],
            entities: vec!["Anthropic".into()],
        };
        // All three could match; the pending claim wins because it is the thing awaiting a grade.
        let h = "SPY slips as interest rates rise and Anthropic raises";
        assert_eq!(peek(h, &w), Some(Interest::PendingClaim("SPY".into())));
        // Without the symbol, the topic wins over the entity.
        let h2 = "Anthropic comments as interest rates rise";
        assert_eq!(
            peek(h2, &w),
            Some(Interest::TrackedTopic("interest rates".into()))
        );
    }

    #[test]
    fn a_multiword_topic_needs_all_of_its_words() {
        let w = Watchlist {
            topics: vec!["interest rates".into()],
            ..Default::default()
        };
        assert!(
            peek("Interest in the sector rose", &w).is_none(),
            "half a phrase is not the topic"
        );
        assert!(
            peek("Rates and interest both climbed", &w).is_some(),
            "all words present, any order"
        );
    }

    #[test]
    fn the_pass_reports_what_it_threw_away() {
        let w = watchlist();
        let heads: Vec<String> = vec![
            "Micron ($MU) guides higher".into(),
            "Local bakery wins award".into(),
            "Weather warning for the coast".into(),
            "Traffic delays downtown".into(),
        ];
        let pass = peek_batch(&heads, &w);
        assert_eq!(pass.kept.len(), 1);
        assert_eq!(pass.dropped, 3);
        // The selectivity is stated, because an invisible rejection rate hides a broken filter.
        assert!(pass.summary().contains("read 1"), "{}", pass.summary());
        assert!(pass.summary().contains("75% dropped"), "{}", pass.summary());
        assert!(peek_batch(&[], &w).summary().contains("no headlines"));
    }

    #[test]
    fn an_empty_watchlist_reads_nothing_rather_than_everything() {
        // A mind with no open questions and no topics should go quiet, not consume the firehose.
        let pass = peek_batch(
            &["Anything at all".to_string(), "Something else".to_string()],
            &Watchlist::default(),
        );
        assert!(pass.kept.is_empty());
        assert_eq!(pass.dropped, 2);
    }
}
