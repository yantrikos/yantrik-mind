//! Deterministic emotional-continuity ledger (the "Samantha rung").
//!
//! On each primary-owner turn: infer a coarse valence + energy label from message text using a
//! word-list heuristic (no LLM cost), persist a rolling 14-day per-person baseline in the profile
//! KV, and if a sustained 3-day flat-or-negative deviation from that baseline is detected, record
//! one wellbeing `Tension` (surfaced by the existing proactive digest path, rate-limited to once
//! per 3 days so it never pesters).

use mind_types::{MemoryFacade, Result, TensionKind};

// ── Word lists ────────────────────────────────────────────────────────────────

static POSITIVE: &[&str] = &[
    "happy", "great", "good", "awesome", "wonderful", "love", "excited", "excellent",
    "fantastic", "amazing", "joyful", "pleased", "glad", "delighted", "superb", "brilliant",
    "perfect", "enjoy", "enjoying", "beautiful", "nice", "lovely", "thankful", "grateful",
    "thrilled", "cheerful", "content", "satisfied", "proud", "winning", "success", "celebrate",
    "celebrated", "laugh", "laughing", "smile", "smiling", "fun", "hopeful", "optimistic",
    "eager", "energized", "inspired", "motivated", "confident", "blessed", "accomplished",
];

static NEGATIVE: &[&str] = &[
    "sad", "terrible", "awful", "horrible", "hate", "depressed", "depression", "anxious",
    "anxiety", "worried", "stress", "angry", "frustrated", "upset", "miserable", "unhappy",
    "disappointed", "exhausted", "sick", "pain", "hurt", "hurting", "failed", "failure",
    "scared", "afraid", "alone", "lonely", "hopeless", "overwhelmed", "drained", "defeated",
    "suffering", "struggling", "crying", "sucks", "terrible", "worst", "broken", "pointless",
    "empty", "numb", "bleak", "rough",
];

static HIGH_ENERGY: &[&str] = &[
    "excited", "pumped", "motivated", "ready", "fired", "busy", "productive", "energized",
    "eager",
];

static LOW_ENERGY: &[&str] = &[
    "tired", "exhausted", "drained", "sleepy", "fatigued", "lethargic", "weak", "burnout",
    "dragging",
];

// ── Public types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Valence {
    Positive,
    Neutral,
    Negative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Energy {
    High,
    Medium,
    Low,
}

pub struct ValenceReading {
    pub valence: Valence,
    #[allow(dead_code)] // inferred and tested; reserved for callers + future baseline storage
    pub energy: Energy,
}

impl Valence {
    fn score(self) -> f64 {
        match self {
            Valence::Positive => 1.0,
            Valence::Neutral => 0.0,
            Valence::Negative => -1.0,
        }
    }
}

// ── Inference ─────────────────────────────────────────────────────────────────

/// Deterministic word-list valence + energy inference. O(words) — no LLM cost.
pub fn infer(text: &str) -> ValenceReading {
    let lower = text.to_lowercase();
    let pos = POSITIVE.iter().filter(|&&w| lower.contains(w)).count();
    let neg = NEGATIVE.iter().filter(|&&w| lower.contains(w)).count();
    let valence = if pos > neg + 1 {
        Valence::Positive
    } else if neg > pos {
        Valence::Negative
    } else {
        Valence::Neutral
    };
    let hi = HIGH_ENERGY.iter().filter(|&&w| lower.contains(w)).count();
    let lo = LOW_ENERGY.iter().filter(|&&w| lower.contains(w)).count();
    let energy = if hi > lo { Energy::High } else if lo > hi { Energy::Low } else { Energy::Medium };
    ValenceReading { valence, energy }
}

// ── Persistence ───────────────────────────────────────────────────────────────

const WINDOW_DAYS: usize = 14;
const DEVIATION_DAYS: usize = 3;
/// Baseline avg must exceed this for a negative streak to count as a "drop from positive".
const MIN_BASELINE_SCORE: f64 = 0.2;
/// Minimum gap between wellbeing nudges: 3 days in milliseconds.
pub(crate) const WELLBEING_RATE_LIMIT_MS: u64 = 3 * 24 * 3600 * 1000;

fn baseline_key(owner: &str) -> String {
    format!("emotion_baseline:{owner}")
}

fn nudge_ts_key(owner: &str) -> String {
    format!("wellbeing_nudge_ms:{owner}")
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn current_day() -> u64 {
    now_ms() / (24 * 3600 * 1000)
}

/// A single day's valence readings, stored as (day_index, sum, count) to allow merging
/// intra-day readings without floating-point churn.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct DayEntry {
    pub(crate) day: u64,   // days since Unix epoch
    pub(crate) sum: f64,
    pub(crate) count: u64,
}

impl DayEntry {
    pub(crate) fn avg(&self) -> f64 {
        if self.count == 0 { 0.0 } else { self.sum / self.count as f64 }
    }
}

/// Rolling 14-day emotional baseline, serialised as compact JSON in the profile KV.
#[derive(Debug, Clone, Default)]
pub(crate) struct Baseline {
    pub(crate) entries: Vec<DayEntry>, // chronological, deduplicated by day, max WINDOW_DAYS
}

impl Baseline {
    pub(crate) fn load(raw: &str) -> Self {
        let entries = serde_json::from_str::<serde_json::Value>(raw)
            .ok()
            .and_then(|v| v.get("d").and_then(|d| d.as_array()).cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| {
                let day = e.get(0)?.as_u64()?;
                let sum = e.get(1)?.as_f64()?;
                let count = e.get(2)?.as_u64()?;
                Some(DayEntry { day, sum, count })
            })
            .collect();
        Self { entries }
    }

    pub(crate) fn save(&self) -> String {
        let arr: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| serde_json::json!([e.day, e.sum, e.count]))
            .collect();
        serde_json::json!({ "d": arr }).to_string()
    }

    /// Add one reading, merging into today's bucket or appending a new day entry.
    pub(crate) fn add_reading(&mut self, valence_score: f64) {
        let today = current_day();
        if let Some(e) = self.entries.iter_mut().find(|e| e.day == today) {
            e.sum += valence_score;
            e.count += 1;
        } else {
            self.entries.push(DayEntry { day: today, sum: valence_score, count: 1 });
            self.entries.sort_by_key(|e| e.day);
            if self.entries.len() > WINDOW_DAYS {
                let excess = self.entries.len() - WINDOW_DAYS;
                self.entries.drain(..excess);
            }
        }
    }

    /// True when the last DEVIATION_DAYS populated day-buckets are all ≤ 0 (flat or negative)
    /// AND the prior entries form a positive baseline (avg > MIN_BASELINE_SCORE). Requires at
    /// least DEVIATION_DAYS entries to avoid false positives on a cold ledger.
    pub(crate) fn has_sustained_deviation(&self) -> bool {
        if self.entries.len() < DEVIATION_DAYS {
            return false;
        }
        let recent: Vec<f64> = self.entries.iter().rev().take(DEVIATION_DAYS).map(|e| e.avg()).collect();
        if !recent.iter().all(|&v| v <= 0.0) {
            return false;
        }
        let prior: Vec<f64> = self.entries.iter().rev().skip(DEVIATION_DAYS).map(|e| e.avg()).collect();
        if prior.is_empty() {
            // No prior baseline yet — require strictly negative (not just neutral) to surface.
            return recent.iter().any(|&v| v < 0.0);
        }
        let baseline_avg = prior.iter().sum::<f64>() / prior.len() as f64;
        baseline_avg > MIN_BASELINE_SCORE
    }
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Infer valence from `text`, update the rolling baseline for `owner`, and if a sustained
/// 3-day flat/negative deviation from that baseline is detected, record one wellbeing
/// `Tension` (rate-limited to once per `WELLBEING_RATE_LIMIT_MS`).
///
/// Errors are intentionally swallowed by the caller so a ledger failure never breaks a turn.
pub async fn record_turn(memory: &dyn MemoryFacade, owner: &str, text: &str) -> Result<()> {
    let reading = infer(text);
    let key = baseline_key(owner);
    let mut baseline = match memory.profile_get(&key).await? {
        Some(raw) => Baseline::load(&raw),
        None => Baseline::default(),
    };
    baseline.add_reading(reading.valence.score());
    memory.profile_set(&key, &baseline.save()).await?;

    if baseline.has_sustained_deviation() {
        let rl_key = nudge_ts_key(owner);
        let last_ms: u64 = memory
            .profile_get(&rl_key)
            .await?
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        let now = now_ms();
        if now.saturating_sub(last_ms) >= WELLBEING_RATE_LIMIT_MS {
            memory
                .record_tension(
                    TensionKind::Curiosity,
                    0.85,
                    "you've seemed low-energy or down the past few days — just checking in, how are you?",
                )
                .await?;
            memory.profile_set(&rl_key, &now.to_string()).await?;
        }
    }

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Inference ────────────────────────────────────────────────────────────

    #[test]
    fn positive_message_detected() {
        assert_eq!(infer("I'm so happy and excited about today, it's amazing!").valence, Valence::Positive);
    }

    #[test]
    fn negative_message_detected() {
        assert_eq!(infer("I feel awful and depressed, everything is terrible").valence, Valence::Negative);
    }

    #[test]
    fn neutral_message_detected() {
        assert_eq!(infer("What time is the meeting tomorrow?").valence, Valence::Neutral);
    }

    #[test]
    fn low_energy_detected() {
        assert_eq!(infer("I'm so exhausted and drained today").energy, Energy::Low);
    }

    #[test]
    fn high_energy_detected() {
        assert_eq!(infer("I'm so pumped and motivated, let's do this!").energy, Energy::High);
    }

    // ── Baseline update ──────────────────────────────────────────────────────

    #[test]
    fn baseline_merges_intraday_readings() {
        let mut b = Baseline::default();
        let today = current_day();
        b.entries.push(DayEntry { day: today, sum: 0.0, count: 1 });
        b.add_reading(1.0);
        assert_eq!(b.entries.len(), 1, "intra-day reading must merge, not append");
        assert_eq!(b.entries[0].count, 2);
        assert_eq!(b.entries[0].avg(), 0.5);
    }

    #[test]
    fn baseline_trims_to_window() {
        let mut b = Baseline::default();
        for i in 0..20u64 {
            b.entries.push(DayEntry { day: i, sum: 0.5, count: 1 });
        }
        b.add_reading(0.5); // today will be a new day unless today == 19
        assert!(b.entries.len() <= WINDOW_DAYS, "window must not exceed {WINDOW_DAYS} days");
    }

    #[test]
    fn baseline_serialization_roundtrip() {
        let mut b = Baseline::default();
        b.entries.push(DayEntry { day: 1000, sum: 1.5, count: 2 });
        b.entries.push(DayEntry { day: 1001, sum: -0.5, count: 1 });
        let raw = b.save();
        let b2 = Baseline::load(&raw);
        assert_eq!(b2.entries.len(), 2);
        assert_eq!(b2.entries[0].day, 1000);
        assert_eq!(b2.entries[1].avg(), -0.5);
    }

    // ── Deviation detection ──────────────────────────────────────────────────

    fn make_baseline(day_scores: &[(u64, f64)]) -> Baseline {
        Baseline {
            entries: day_scores
                .iter()
                .map(|&(day, score)| DayEntry { day, sum: score, count: 1 })
                .collect(),
        }
    }

    #[test]
    fn no_deviation_without_enough_history() {
        let b = make_baseline(&[(1, -1.0), (2, -1.0)]);
        assert!(!b.has_sustained_deviation(), "need at least {DEVIATION_DAYS} days");
    }

    #[test]
    fn deviation_detected_positive_baseline_then_three_negative_days() {
        let mut scores: Vec<(u64, f64)> = (1..=7).map(|d| (d, 1.0)).collect();
        scores.push((8, -1.0));
        scores.push((9, -1.0));
        scores.push((10, -1.0));
        let b = make_baseline(&scores);
        assert!(b.has_sustained_deviation());
    }

    #[test]
    fn no_deviation_all_negative_baseline_and_recent() {
        // Person is consistently negative — no meaningful drop FROM a positive baseline.
        let scores: Vec<(u64, f64)> = (1..=10).map(|d| (d, -1.0)).collect();
        let b = make_baseline(&scores);
        assert!(!b.has_sustained_deviation());
    }

    #[test]
    fn no_deviation_neutral_baseline() {
        // Baseline is 0.0 (below MIN_BASELINE_SCORE) → flat recent days aren't a drop.
        let scores: Vec<(u64, f64)> = (1..=10).map(|d| (d, 0.0)).collect();
        let b = make_baseline(&scores);
        assert!(!b.has_sustained_deviation());
    }

    #[test]
    fn deviation_clears_when_recent_day_turns_positive() {
        // 7 positive days, 2 negative, then 1 positive — the streak is broken.
        let mut scores: Vec<(u64, f64)> = (1..=7).map(|d| (d, 1.0)).collect();
        scores.push((8, -1.0));
        scores.push((9, -1.0));
        scores.push((10, 1.0)); // bounce back
        let b = make_baseline(&scores);
        assert!(!b.has_sustained_deviation());
    }

    // ── Rate limit ───────────────────────────────────────────────────────────

    #[test]
    fn rate_limit_allows_when_never_nudged() {
        let last_ms: u64 = 0;
        let now = now_ms();
        assert!(now.saturating_sub(last_ms) >= WELLBEING_RATE_LIMIT_MS);
    }

    #[test]
    fn rate_limit_blocks_within_three_days() {
        let just_now = now_ms();
        assert!(now_ms().saturating_sub(just_now) < WELLBEING_RATE_LIMIT_MS);
    }

    #[test]
    fn rate_limit_allows_after_three_days() {
        let three_days_ago = now_ms().saturating_sub(WELLBEING_RATE_LIMIT_MS + 1_000);
        assert!(now_ms().saturating_sub(three_days_ago) >= WELLBEING_RATE_LIMIT_MS);
    }
}
