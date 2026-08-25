//! WATCH — the mind's reach for audio and video, wired to the senses it already had.
//!
//! Nothing here perceives anything new. Hearing is the same local whisper that has handled voice
//! notes since July; seeing is the same `VisionClient` that reads photos and page screenshots. What
//! this adds is the missing middle: fetch the media, decide what it IS, and take a video apart into
//! pictures and a voice note so the existing senses can do their jobs.
//!
//! The order of preference is deliberate and it is about honesty as much as cost. Published
//! captions win whenever they exist, because they are the speaker's actual words rather than a
//! CPU's guess at them. Local whisper is next, bounded hard, because the box has no GPU — a
//! three-hour recording is heard as a labelled opening sample rather than refused, since refusing
//! it outright taught nothing when thirty minutes of it was available the whole time. Frames are
//! always sampled, because for material whose information is on screen they carry the content that
//! the audio does not.

use super::*;

/// How many frames one look costs (each is a vision call).
fn frame_budget() -> usize {
    std::env::var("YM_MEDIA_FRAMES").ok().and_then(|s| s.parse().ok()).unwrap_or(4)
}

impl super::ConversationEngine {
    /// `watch <url> [question]` — the whole pipeline, with every refusal stated in the terms that
    /// caused it. Returns what it actually perceived, never a summary of what it assumes is there.
    pub async fn watch_media(&self, url: &str, question: &str) -> String {
        let url = url.trim();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            return "Give me a full URL to watch or listen to, e.g. `ym watch https://youtube.com/watch?v=… what do they do?`".to_string();
        }
        let u = url.to_string();
        let probe = match tokio::task::spawn_blocking(move || mind_tools::media::probe(&u)).await {
            Ok(Ok(p)) => p,
            Ok(Err(e)) => return format!("I can't reach that media: {e}."),
            Err(_) => return "The probe task failed to run.".to_string(),
        };
        let cap = mind_tools::media::cap_secs();
        let plan = mind_tools::media::plan(&probe, cap, mind_tools::media::live_window_secs());

        let mut out = format!("🎬 {}", probe.title);
        if !probe.uploader.is_empty() {
            out.push_str(&format!(" — {}", probe.uploader));
        }
        if probe.is_live {
            out.push_str(" (LIVE)");
        } else if probe.duration_secs > 0 {
            out.push_str(&format!(" ({}m{:02}s)", probe.duration_secs / 60, probe.duration_secs % 60));
        }
        out.push('\n');

        // ── HEARING ────────────────────────────────────────────────────────────────────────
        let mut spoken: Vec<mind_tools::media::Utterance> = Vec::new();
        let heard: Option<String> = match &plan {
            mind_tools::media::MediaPlan::Captions => {
                let u = url.to_string();
                let dur = probe.duration_secs;
                match tokio::task::spawn_blocking(move || mind_tools::media::captions(&u)).await {
                    Ok(Ok(t)) => {
                        // WINDOW THE CAPTIONS. Seeking the audio fixed nothing for the case that
                        // applies to most of YouTube: a captioned recording was still read from
                        // its first line, which is how four attempts at a three-hour trading show
                        // all came back with the greeting segment.
                        if dur > cap {
                            let off = mind_tools::media::sensible_offset(dur, cap);
                            let win = mind_tools::media::captions_window(&t, off, cap);
                            if win.trim().is_empty() {
                                // Never substitute a different sample and stay quiet about it —
                                // that is what made a broken window look like a clean read.
                                out.push_str(&format!(
                                    "📝 The caption window from {}m in came back EMPTY — reporting nothing rather than quietly serving the opening.\n",
                                    off / 60
                                ));
                                None
                            } else {
                                out.push_str(&format!(
                                    "📝 Read the published captions — the {}m window from {}m in (mid-session, not the opening).\n",
                                    cap / 60,
                                    off / 60
                                ));
                                Some(win)
                            }
                        } else {
                            out.push_str("📝 Read the published captions (the speaker's own words).\n");
                            Some(t)
                        }
                    }
                    Ok(Err(e)) => {
                        out.push_str(&format!("📝 Captions were advertised but unreadable ({e}).\n"));
                        None
                    }
                    Err(_) => None,
                }
            }
            mind_tools::media::MediaPlan::Transcribe { secs }
            | mind_tools::media::MediaPlan::LiveWindow { secs }
            | mind_tools::media::MediaPlan::PartialListen { secs, .. } => {
                let (u, s) = (url.to_string(), *secs);
                let live = matches!(plan, mind_tools::media::MediaPlan::LiveWindow { .. });
                // Seek into the MIDDLE of a long recording. Sampling from zero is why two attempts
                // at a trading broadcast returned greetings and then a sofa: a market show opens
                // with hellos and closes with the wind-down, and the trading is in between.
                let offset = match &plan {
                    mind_tools::media::MediaPlan::PartialListen { secs, of_secs } => {
                        mind_tools::media::sensible_offset(*of_secs, *secs)
                    }
                    _ => 0,
                };
                match tokio::task::spawn_blocking(move || mind_tools::media::transcribe_segments_at(&u, s, offset)).await {
                    Ok(Ok(segs)) => {
                        spoken = segs;
                        let t = mind_tools::media::utterances_to_text(&spoken);
                        out.push_str(&if live {
                            format!("🎧 Heard a {s}s sample of the live broadcast (there is no finished recording to hear in full).\n")
                        } else {
                            format!("🎧 Heard {s}s of audio through the local speech model (nothing left the house).\n")
                        });
                        Some(t)
                    }
                    Ok(Err(e)) => {
                        out.push_str(&format!("🎧 Could not hear it: {e}.\n"));
                        None
                    }
                    Err(_) => None,
                }
            }
        };
        if let mind_tools::media::MediaPlan::PartialListen { secs, of_secs } = &plan {
            let off = mind_tools::media::sensible_offset(*of_secs, *secs);
            out.push_str(&format!(
                "   (a {}m sample of a {}m recording, taken from {}m in — mid-session, not the opening)\n",
                secs / 60,
                of_secs / 60,
                off / 60
            ));
        }

        // ── SEEING ─────────────────────────────────────────────────────────────────────────
        // Always sample frames: for screen-content media this IS the information, and it is the
        // only modality available when the audio is refused or silent.
        let window = match &plan {
            mind_tools::media::MediaPlan::LiveWindow { secs } => *secs,
            mind_tools::media::MediaPlan::PartialListen { secs, .. } => *secs,
            mind_tools::media::MediaPlan::Transcribe { secs } => *secs,
            mind_tools::media::MediaPlan::Captions => probe.duration_secs.min(cap).max(60),
        };
        let (u, want) = (url.to_string(), frame_budget());
        let frame_offset = match &plan {
            mind_tools::media::MediaPlan::PartialListen { secs, of_secs } => mind_tools::media::sensible_offset(*of_secs, *secs),
            _ => 0,
        };
        let frames = match tokio::task::spawn_blocking(move || mind_tools::media::keyframes_at(&u, want, window, frame_offset)).await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => {
                out.push_str(&format!("👁 Could not sample frames: {e}.\n"));
                Vec::new()
            }
            Err(_) => Vec::new(),
        };
        let mut seen: Vec<String> = Vec::new();
        let mut seen_at: Vec<(u64, String)> = Vec::new();
        if !frames.is_empty() {
            let q = if question.trim().is_empty() {
                "Describe what is on screen. Read any text, prices, tickers, or numbers exactly as shown."
            } else {
                question.trim()
            };
            for (at, bytes) in frames {
                let caption = self.analyze_image_bytes(bytes, "image/jpeg", q).await;
                seen.push(format!("[{}:{:02}] {}", at / 60, at % 60, caption.trim()));
                seen_at.push((at, caption.trim().to_string()));
            }
            out.push_str(&format!("👁 Looked at {} frame(s) with the local vision model.\n", seen.len()));
        }

        if heard.is_none() && seen.is_empty() {
            out.push_str("\nI perceived nothing from this one — so I have nothing to tell you about it.");
            return out;
        }

        // ── ONE TIMELINE ───────────────────────────────────────────────────────────────────
        // Speech and pictures are laid against each other by second, not listed separately.
        // Two parallel lists cannot answer "what was on screen when they said that"; a single
        // ordered timeline answers it by construction, which is the whole point of keeping
        // whisper's timestamps.
        if !spoken.is_empty() && !seen_at.is_empty() {
            let mut rows: Vec<(u64, String)> = Vec::new();
            for (at, caption) in &seen_at {
                rows.push((*at, format!("👁 {}", caption.chars().take(400).collect::<String>())));
            }
            for u in &spoken {
                rows.push((u.at_secs, format!("🗣 {}", u.text)));
            }
            rows.sort_by_key(|(at, _)| *at);
            out.push_str("\nTIMELINE (screen and speech, aligned by second):\n");
            for (at, row) in rows.iter().take(60) {
                out.push_str(&format!("[{}:{:02}] {}\n", at / 60, at % 60, row));
            }
        } else {
            if !seen.is_empty() {
                out.push_str("\nON SCREEN:\n");
                for s in &seen {
                    out.push_str(&format!("{}\n", s.chars().take(600).collect::<String>()));
                }
            }
            if let Some(t) = &heard {
                let excerpt: String = t.chars().take(4000).collect();
                out.push_str(&format!("\nSPOKEN:\n{excerpt}\n"));
            }
        }

        // The perception is a machine-derived OBSERVATION, never a naked belief — it enters memory
        // through the same gated inward boundary as every other tool result.
        let note = format!(
            "MEDIA: \"{}\" by {} — {}{}",
            probe.title,
            if probe.uploader.is_empty() { "unknown" } else { &probe.uploader },
            seen.first().map(|s| s.chars().take(300).collect::<String>()).unwrap_or_default(),
            heard.as_ref().map(|t| format!(" | said: {}", t.chars().take(300).collect::<String>())).unwrap_or_default()
        );
        let _ = self.memory.remember_observation(&note, mind_types::ProvenanceCategory::ToolResult).await;
        out
    }

    /// LEARN from what was watched — the step that turns perception into memory that thinks.
    ///
    /// `watch_media` records a single observation; observations are never beliefs, so a thing
    /// watched ten times was no better established than a thing watched once. This routes what was
    /// perceived through the same reconciler research already uses, with one addition that matters
    /// more here than anywhere else in the system:
    ///
    /// **What was OBSERVED becomes a belief. What was CLAIMED becomes a prediction.**
    ///
    /// A broadcast's own description of itself is checkable and can be believed. A trader saying
    /// "this breaks 110 today", or that some setup works, is not knowledge — it is an assertion
    /// whose truth arrives later. Absorbing those as beliefs is precisely how watching hours of
    /// market television turns into confident folklore, because the winners are narrated and the
    /// losers are silent. Sent to the judgment ledger instead, each one gets a deadline and a
    /// grade, so the mind ends up knowing whether this source is worth believing rather than
    /// merely believing it. That is the difference between learning from a stream and being
    /// trained by one.
    ///
    /// Watched facts are also capped in weight: seeing something once on a broadcast is weaker
    /// evidence than being told it, and repeat viewings accumulate Bayesian evidence on their own.
    pub async fn learn_from_watch(&self, url: &str, focus: &str) -> String {
        let perception = self.watch_media(url, focus).await;
        if perception.contains("I perceived nothing") {
            return format!("{perception}\n\n(nothing perceived, so nothing learned)");
        }
        let seen: String = perception.chars().take(6000).collect();
        let priors = self
            .memory
            .beliefs_matching_n("trading market stream broadcast", 12, &mind_types::AccessContext::operator_audit())
            .await
            .unwrap_or_default()
            .iter()
            .map(|b| format!("- {} ({:.2})", b.statement, b.confidence))
            .collect::<Vec<_>>()
            .join("\n");
        let prior_list = if priors.is_empty() { "(none yet)".to_string() } else { priors };

        let prompt = format!(
            "You WATCHED a segment of a live broadcast. Below is what was seen on screen and heard, with timestamps.\n\n\
             PRIOR BELIEFS:\n{prior_list}\n\nWHAT YOU PERCEIVED:\n{seen}\n\n\
             Separate what you perceived into two kinds, and be strict about the difference:\n\
             1. FACTS — durable, checkable, third-person statements about the world or about this source that were TRUE AT THE TIME OF WATCHING (e.g. \"TraderTV Live broadcasts weekdays 8:00-16:00 ET with two traders trading real money\"). A price printed on screen at a moment is NOT durable — skip it.\n\
             2. CLAIMS — forward-looking or strategy assertions that someone MADE and that could later be shown right or wrong (e.g. \"CRWV breaks 110 today\", \"buying the first pullback after a gap works\").\n\
             NEVER put a prediction, an opinion, or a trading strategy in facts. A fact was true when you watched it; a claim might be true later.\n\
             Output ONLY JSON:\n\
             {{\"facts\":[{{\"statement\":\"...\",\"certainty\":0.0-1.0}}], \
             \"claims\":[{{\"claim\":\"...\",\"confidence\":0.0-1.0,\"resolve_in_days\":1-30}}], \
             \"revisions\":[{{\"old\":\"<a prior belief above now contradicted>\",\"new\":\"...\",\"certainty\":0.0-1.0}}]}}\n\
             Empty arrays if none."
        );
        let messages = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::system("You separate what was observed from what was merely asserted. Output ONLY the JSON object."),
            ChatMessage::user(&prompt),
        ];
        // PRIVATE-GROUNDED: the perception may carry household content, so it takes the private lane.
        let text = match self.inference.chat_grounded(messages, GenerationConfig::default()).await {
            Ok(r) => r.text,
            Err(e) => return format!("{perception}\n\n(could not reconcile what I saw: {e})"),
        };
        let body_owned = crate::strip_reasoning(&text);
        let body = body_owned.as_str();
        let body = body.split("```").find(|s| s.contains('{')).unwrap_or(body);
        let obj = match (body.find('{'), body.rfind('}')) {
            (Some(s), Some(e)) if e > s => &body[s..=e],
            _ => "{}",
        };
        let v: serde_json::Value = serde_json::from_str(obj).unwrap_or(serde_json::json!({}));

        let mut learned: Vec<String> = Vec::new();
        let mut logged: Vec<String> = Vec::new();

        for f in v.get("facts").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let stmt = f.get("statement").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if stmt.len() < 8 {
                continue;
            }
            // Capped: one viewing of a broadcast is weaker than being told something. Repeat
            // viewings accumulate their own evidence, which is the point.
            let cert = f.get("certainty").and_then(|x| x.as_f64()).unwrap_or(0.5).clamp(0.1, 0.6);
            if self
                .memory
                .remember_as_belief(BeliefAssertion {
                    statement: stmt.clone(),
                    polarity: 1.0,
                    weight: 0.4 + cert,
                    source_event: Some("watched".into()),
                    provenance: "watched".into(),
                })
                .await
                .is_ok()
            {
                learned.push(stmt);
            }
        }
        for r in v.get("revisions").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let old = r.get("old").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let new = r.get("new").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if old.len() < 8 || new.len() < 8 {
                continue;
            }
            let w = 0.4 + r.get("certainty").and_then(|x| x.as_f64()).unwrap_or(0.5).clamp(0.1, 0.6);
            let _ = self.memory.remember_as_belief(BeliefAssertion { statement: new.clone(), polarity: 1.0, weight: w, source_event: Some("watched".into()), provenance: "watched".into() }).await;
            let _ = self.memory.remember_as_belief(BeliefAssertion { statement: old.clone(), polarity: -1.0, weight: w, source_event: Some("watched".into()), provenance: "watched".into() }).await;
            let _ = self.memory.relate(&new, &old, "contradicts", 0.9).await;
            learned.push(format!("revised: \"{old}\" → \"{new}\""));
        }
        // The claims do NOT become beliefs. They become gradeable predictions.
        let now = chrono::Utc::now().timestamp_millis();
        for c in v.get("claims").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let claim = c.get("claim").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if claim.len() < 8 {
                continue;
            }
            let p = c.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.5).clamp(0.05, 0.95);
            let days = c.get("resolve_in_days").and_then(|x| x.as_i64()).unwrap_or(7).clamp(1, 30);
            self.judgment_log("watched", "trading", &claim, p, now + days * 86_400_000, url).await;
            logged.push(format!("{claim} (p={p:.2}, grades in {days}d)"));
        }

        let mut out = perception;
        out.push_str("\n\n── WHAT I LEARNED ──\n");
        if learned.is_empty() {
            out.push_str("Nothing durable enough to believe.\n");
        } else {
            for l in &learned {
                out.push_str(&format!("📚 {l}\n"));
            }
        }
        if logged.is_empty() {
            out.push_str("No forward-looking claims to grade.\n");
        } else {
            out.push_str("\n── CLAIMS I DID NOT BELIEVE, BUT WILL GRADE ──\n");
            for l in &logged {
                out.push_str(&format!("⚖ {l}\n"));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The refusals must be reachable without any media tooling installed — a host with no yt-dlp
    /// must say so, not fail obscurely. (The perception paths need real binaries and are exercised
    /// on the box, not in unit tests.)
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_non_url_is_refused_before_any_work() {
        let mem: Arc<dyn MemoryFacade> = Arc::new(mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap());
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("x")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let conv = ConversationEngine::new(mem, pool, "JARVIS");
        let out = conv.watch_media("not a url", "").await;
        assert!(out.contains("full URL"), "{out}");
    }
}

impl super::ConversationEngine {
    /// Sample the position bar once and append it to the tape.
    ///
    /// One reading answers nothing; a few thousand answer whether shadowing these traders would
    /// have paid. So this is deliberately cheap — one frame, one vision call, one line appended —
    /// because it has to run on a tight cadence for weeks without becoming the reason the box is
    /// busy.
    pub async fn tape_sample(&self, url: &str) -> String {
        let traders: Vec<String> = std::env::var("YM_TAPE_TRADERS")
            .unwrap_or_else(|_| "CHERIF,CHEIF,OBI,SHARE".into())
            .split(',')
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        let (u, window) = (url.to_string(), 30u64);
        let frames = match tokio::task::spawn_blocking(move || mind_tools::media::keyframes(&u, 1, window)).await {
            Ok(Ok(f)) => f,
            Ok(Err(e)) => return format!("tape: could not sample a frame ({e})"),
            Err(_) => return "tape: frame task failed".to_string(),
        };
        let Some((_, bytes)) = frames.into_iter().next() else {
            return "tape: no frame returned".to_string();
        };
        let caption = self
            .analyze_image_bytes(
                bytes,
                "image/jpeg",
                "Read ONLY the traders' position bar at the bottom. For each trader give: name, LONG or SHORT, the ticker symbol, or the words 'no positions'. Copy the text exactly; do not infer.",
            )
            .await;
        // Discover the roster from the bar; the hint only seeds it. The shift changes and a
        // configured list would silently stop recording the day it does.
        let states = mind_tools::tape::parse_bar_auto(&caption, &traders);
        if states.is_empty() {
            return format!("tape: no trader state could be read (kept nothing rather than guessing)\n{}", caption.chars().take(200).collect::<String>());
        }
        let sample = mind_tools::tape::TapeSample {
            at_ms: chrono::Utc::now().timestamp_millis(),
            source: url.to_string(),
            states: states.clone(),
        };
        let path = std::path::PathBuf::from(
            std::env::var("YM_TAPE_PATH").unwrap_or_else(|_| "/var/lib/yantrik-mind/tape.jsonl".into()),
        );
        let stored = mind_tools::tape::append_sample(&path, &sample).is_ok();
        let mut out = String::from("📼 tape: ");
        out.push_str(
            &states
                .iter()
                .map(|s| match (&s.side, &s.symbol) {
                    (mind_tools::Side::Flat, _) => format!("{} flat", s.trader),
                    (side, Some(sym)) => format!("{} {:?} {}", s.trader, side, sym),
                    (side, None) => format!("{} {:?}", s.trader, side),
                })
                .collect::<Vec<_>>()
                .join(" · "),
        );
        if !stored {
            out.push_str("  (NOT recorded — the ledger write failed)");
        }
        out
    }

    /// `ym shadow` — the counterfactual over everything recorded so far.
    pub async fn shadow_report(&self) -> String {
        let path = std::path::PathBuf::from(
            std::env::var("YM_TAPE_PATH").unwrap_or_else(|_| "/var/lib/yantrik-mind/tape.jsonl".into()),
        );
        let tape = mind_tools::tape::read_tape(&path);
        if tape.is_empty() {
            return "No tape recorded yet — nothing to compute. `ym tape <url>` samples the position bar.".to_string();
        }
        let trans = mind_tools::tape::transitions(&tape);
        if trans.is_empty() {
            return format!(
                "{} tape sample(s) recorded, but no entry/exit transition yet — every reading so far showed the same state.",
                tape.len()
            );
        }
        // Price every symbol that appears, over the window the tape covers.
        let symbols: std::collections::HashSet<String> =
            trans.iter().filter_map(|t| t.symbol.clone()).collect();
        let (lo, hi) = (
            tape.iter().map(|s| s.at_ms).min().unwrap_or(0),
            tape.iter().map(|s| s.at_ms).max().unwrap_or(0),
        );
        let start = chrono::DateTime::from_timestamp_millis(lo - 3_600_000).map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default();
        let end = chrono::DateTime::from_timestamp_millis(hi + 3_600_000).map(|d| d.format("%Y-%m-%dT%H:%M:%SZ").to_string()).unwrap_or_default();
        let bars = match tokio::task::spawn_blocking(move || {
            // Route per symbol: Alpaca for US equities, Yahoo for Indian listings (and as the
            // fallback whenever Alpaca has nothing). An NSE symbol handed to Alpaca returns an
            // empty series, which the counterfactual would then score as "unpriceable" — a real
            // signal lost to the wrong data source rather than to missing data.
            let client = mind_tools::MarketClient::from_env().ok();
            let mut m = std::collections::HashMap::new();
            for s in symbols {
                if mind_tools::is_indian(&s) {
                    if let Ok(ser) = mind_tools::yahoo_series(&s, "5d", "1m") {
                        m.insert(s, ser.bars);
                    }
                    continue;
                }
                let via_alpaca = client.as_ref().and_then(|c| c.bars(&s, "1Min", &start, &end).ok()).filter(|b| !b.is_empty());
                match via_alpaca {
                    Some(b) => {
                        m.insert(s, b);
                    }
                    None => {
                        if let Ok(ser) = mind_tools::yahoo_series(&s, "5d", "1m") {
                            m.insert(s, ser.bars);
                        }
                    }
                }
            }
            Ok::<_, anyhow::Error>(m)
        })
        .await
        {
            Ok(Ok(m)) => m,
            Ok(Err(e)) => return format!("Recorded {} sample(s) and {} transition(s), but I can't price them: {e}", tape.len(), trans.len()),
            Err(_) => return "The pricing task failed.".to_string(),
        };
        let curve = mind_tools::lag_curve(&trans, &bars, &[0, 60, 120, 180, 300, 600], 15.0);
        format!(
            "📼 {} sample(s), {} transition(s), {} symbol(s) priced\n\n{}\nEvery leg is lagged, both entry AND exit, with 15bp charged each side.",
            tape.len(),
            trans.len(),
            bars.len(),
            mind_tools::render_curve(&curve)
        )
    }
}

impl super::ConversationEngine {
    /// Drain the bar-watcher's spool: every frame in it is a moment the position bar CHANGED,
    /// so each earns the vision call that polling was spending on unchanged screens.
    ///
    /// The timestamp recorded is the FRAME's, not the moment vision happened to run. The detector
    /// saw the change seconds ago; dating the event by when the expensive step got around to it
    /// would smear every entry and exit by the queue depth, and the whole counterfactual is built
    /// on those timings being right.
    pub async fn bar_drain(&self, max_frames: usize) -> String {
        let spool = std::path::PathBuf::from(
            std::env::var("YM_BAR_SPOOL").unwrap_or_else(|_| "/var/lib/yantrik-mind/barspool".into()),
        );
        let mut frames: Vec<(std::time::SystemTime, std::path::PathBuf)> = match std::fs::read_dir(&spool) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|x| x == "jpg").unwrap_or(false))
                .filter_map(|p| p.metadata().ok().and_then(|m| m.modified().ok()).map(|t| (t, p)))
                .collect(),
            Err(_) => return "bar-drain: no spool yet (the watcher has not run).".to_string(),
        };
        if frames.is_empty() {
            return "bar-drain: spool empty — no bar changes detected since the last drain.".to_string();
        }
        frames.sort_by_key(|(t, _)| *t);
        let total = frames.len();
        frames.truncate(max_frames.max(1));
        let traders: Vec<String> = std::env::var("YM_TAPE_TRADERS")
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
        let tape_path = std::path::PathBuf::from(
            std::env::var("YM_TAPE_PATH").unwrap_or_else(|_| "/var/lib/yantrik-mind/tape.jsonl".into()),
        );
        let mut recorded = 0usize;
        let mut unreadable = 0usize;
        let mut last = String::new();
        for (mtime, path) in &frames {
            let Ok(bytes) = std::fs::read(path) else { continue };
            let caption = self
                .analyze_image_bytes(
                    bytes,
                    "image/jpeg",
                    "Read ONLY the traders' position bar. For each trader: name, LONG or SHORT, the ticker, or the words 'no positions'. Copy exactly; do not infer.",
                )
                .await;
            let states = mind_tools::tape::parse_bar_auto(&caption, &traders);
            let _ = std::fs::remove_file(path);
            if states.is_empty() {
                unreadable += 1;
                continue;
            }
            let at_ms = mtime
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as i64)
                .unwrap_or_else(|_| chrono::Utc::now().timestamp_millis());
            let sample = mind_tools::tape::TapeSample { at_ms, source: "bar-watch".into(), states: states.clone() };
            if mind_tools::tape::append_sample(&tape_path, &sample).is_ok() {
                recorded += 1;
                last = states
                    .iter()
                    .map(|s| match (&s.side, &s.symbol) {
                        (mind_tools::Side::Flat, _) => format!("{} flat", s.trader),
                        (side, Some(sym)) => format!("{} {:?} {}", s.trader, side, sym),
                        (side, None) => format!("{} {:?}", s.trader, side),
                    })
                    .collect::<Vec<_>>()
                    .join(" · ");
            }
        }
        format!(
            "📼 bar-drain: {recorded} change event(s) recorded{}{} — {} still spooled\n{last}",
            if unreadable > 0 { format!(", {unreadable} unreadable (dropped, not guessed)") } else { String::new() },
            if total > frames.len() { format!(", {} deferred to the next drain", total - frames.len()) } else { String::new() },
            total.saturating_sub(frames.len())
        )
    }
}

impl super::ConversationEngine {
    /// `ym quote SYM[,SYM…]` — live prices, routed per symbol.
    ///
    /// Exists because the mind was refusing quote questions with "I have no market-data tool
    /// wired up", which was TRUE and was the honest answer to a capability that existed in the
    /// code and not in its hands. The pipeline could price a symbol; the mind could not ask.
    /// HUNT — the mind's own trade, from its own reading of the tape.
    ///
    /// Copying a broadcast borrows someone else's judgment and arrives late to it. This is the
    /// independent version: what moved, why it moved, is it even tradeable, and only then a view.
    ///
    /// The order matters and is not the obvious one. Filtering comes BEFORE the thesis, because a
    /// model handed FIXX +1378% will happily write a compelling paragraph about it, and no amount of
    /// good reasoning rescues a symbol that cannot be exited. Eligibility is arithmetic; judgment is
    /// for the survivors.
    ///
    /// Every position is filed as a prediction on the same ledger as any other claim, so a run of
    /// these is a measurable strategy rather than a sequence of anecdotes.
    pub async fn hunt(&self, act: bool) -> String {
        let pull = tokio::task::spawn_blocking(|| mind_tools::hunt::fetch_movers(20).map_err(|e| e.to_string()))
            .await
            .unwrap_or_else(|e| Err(format!("join failed: {e}")));

        let movers = match pull {
            Ok(x) => x,
            Err(e) => return format!("🎯 Hunt aborted: {e}"),
        };
        let bounds = mind_tools::hunt::Bounds::default();
        let (keep, dropped) = mind_tools::hunt::shortlist(&movers, &bounds);

        // News is asked for THESE symbols, after the shortlist — never the general firehose, which
        // is dominated by large caps and answers "no catalyst" for every small-cap mover.
        let syms: Vec<String> = keep.iter().take(6).map(|m| m.symbol.clone()).collect();
        let news = tokio::task::spawn_blocking(move || mind_tools::hunt::fetch_news_for(&syms, 50))
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or_default();

        let mut out = format!("🎯 HUNT — {} movers scanned, {} tradeable\n", movers.len(), keep.len());
        if !dropped.is_empty() {
            out.push_str(&format!("  filtered out {} (the filter IS the strategy here):\n", dropped.len()));
            for (s, r) in dropped.iter().take(6) {
                out.push_str(&format!("    · {s}: {r}\n"));
            }
        }
        if keep.is_empty() {
            out.push_str("\nNothing eligible today. A hunt that finds nothing is a result — the alternative is trading the junk.\n");
            return out;
        }

        out.push_str("\n  candidates:\n");
        let mut brief = String::new();
        for m in keep.iter().take(6) {
            // Only a SPECIFIC headline counts as a catalyst. A roundup tagging a dozen tickers
            // sitting beside a candidate reads as the explanation and gets reasoned about as one,
            // which is a false premise dressed as evidence.
            let head = mind_tools::hunt::catalyst_for(&m.symbol, &news)
                .map(|h| h.headline.clone())
                .unwrap_or_else(|| "(no company-specific news — an unexplained move)".into());
            out.push_str(&format!("    {} {:>8.2} {:+6.2}%  {}\n", m.symbol, m.price, m.percent_change, head.chars().take(70).collect::<String>()));
            brief.push_str(&format!("- {} at {:.2}, {:+.2}% today. News: {}\n", m.symbol, m.price, m.percent_change, head));
        }

        let prompt = format!(
            "You are deciding your OWN trades, not copying anyone. Today's tradeable movers:\n\n{brief}\n\
             For each, decide whether there is a same-day edge and which way.\n\
             A FRESH, SPECIFIC, COMPANY-LEVEL catalyst with the move still under way IS an edge, and \
             it is the case you are here for: an earnings reaction, guidance, trial data, a deal, an \
             analyst action that is moving the stock. Name the direction and take it.\n\
             NOT an edge: a move with no explanation, a move that has already fully played out, an \
             index or leveraged ETF drifting with the market, or 'it is going up'.\n\
             Judge each name on its own. An empty array is right when nothing has a catalyst — but \
             it is an answer, not a safe default, and passing on a clean setup costs as much as \
             taking a bad one.\n\
             Output ONLY JSON: {{\"trades\":[{{\"symbol\":\"X\",\"side\":\"long\"|\"short\",\
             \"conviction\":0.0-1.0,\"thesis\":\"one specific sentence\"}}]}}"
        );
        // NO PERSONA on this call, and the reason is measured rather than stylistic.
        //
        // With the persona attached the hunt declined the same candidates twice at temperature 0.
        // The identical prompt without it returned "BULL long, 0.65 — Rosenblatt's target raise
        // supports the current move". The persona is built around caution: measured data, never a
        // guess, say when you cannot see the number. That is exactly right for talking to a person
        // about their life and it suppresses an analytical judgment, because forming a view on
        // incomplete evidence is what analysis IS.
        //
        // The judgment is still bounded — filters upstream, conviction floor and position cap
        // downstream, every view logged to the ledger whether taken or not. The caution lives in
        // the machinery, where it can be measured, rather than in a voice telling the model to
        // hesitate.
        let messages = vec![
            ChatMessage::system(
                "You are a trading analyst. Decide from the evidence given. Output ONLY the JSON                  object. An empty array is a valid answer when nothing has a catalyst.",
            ),
            ChatMessage::user(&prompt),
        ];
        // GREEDY, not sampled. Two runs over the SAME candidates, minutes apart, disagreed
        // completely: one returned MRNA short at 0.85 conviction and WMT short at 0.75, each with a
        // written thesis; the other returned nothing at all. Same prices, same headlines, same
        // prompt. That is the sampler talking, not judgment.
        //
        // It breaks the part that matters most: a view that flips between identical inputs cannot be
        // graded, because the ledger would be scoring a coin toss and reporting it as skill. And it
        // removes a temptation that is hard to see from the inside — with a variable decision,
        // re-running the hunt until it agrees with you looks exactly like more analysis.
        let decide = GenerationConfig { temperature: 0.0, ..GenerationConfig::default() };
        let text = match self.inference.chat_grounded(messages, decide).await {
            Ok(r) => r.text,
            Err(e) => return format!("{out}\n(could not form a view: {e})"),
        };
        let b_owned = crate::strip_reasoning(&text);
        let b = b_owned.as_str();
        let b = b.split("```").find(|s| s.contains('{')).unwrap_or(b);
        let obj = match (b.find('{'), b.rfind('}')) {
            (Some(s), Some(e)) if e > s => &b[s..=e],
            _ => "{}",
        };
        let v: serde_json::Value = serde_json::from_str(obj).unwrap_or(serde_json::json!({}));
        let trades = v.get("trades").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if trades.is_empty() {
            out.push_str("\n📉 No thesis worth a position today. Declining is the discipline this is supposed to have.\n");
            return out;
        }

        let floor: f64 = std::env::var("YM_TRADE_MIN_CONVICTION").ok().and_then(|s| s.parse().ok()).unwrap_or(0.6);
        let stake: f64 = std::env::var("YM_PAPER_STAKE_USD").ok().and_then(|s| s.parse().ok()).unwrap_or(250.0);
        let now = chrono::Utc::now().timestamp_millis();
        out.push_str("\n📈 VIEW:\n");
        for t in trades.into_iter().take(3) {
            let sym = t.get("symbol").and_then(|x| x.as_str()).unwrap_or("").trim().to_uppercase();
            let side = t.get("side").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
            let conv = t.get("conviction").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let thesis = t.get("thesis").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if sym.is_empty() || !matches!(side.as_str(), "long" | "short") {
                continue;
            }
            out.push_str(&format!("  {sym} {side} (conviction {conv:.2}) — {thesis}\n"));
            // The view is recorded whether or not it is acted on. A thesis that is only logged when
            // it becomes a trade produces a track record of exactly the trades that were taken,
            // which is how a strategy grades itself generously.
            self.judgment_log("hunt", "trading", &format!("{sym} {side}: {thesis}"), conv.clamp(0.05, 0.95), now + 86_400_000, &sym).await;
            if !act {
                out.push_str("      (logged as a prediction; not traded — pass `act` to take it)\n");
                continue;
            }
            if conv < floor {
                out.push_str(&format!("      not taken: conviction {conv:.2} below the {floor:.2} floor\n"));
                continue;
            }
            let (s2, side2) = (sym.clone(), side.clone());
            let placed = tokio::task::spawn_blocking(move || -> std::result::Result<(f64, f64, String), String> {
                let broker = mind_tools::broker::PaperBroker::from_env().map_err(|e| e.to_string())?;
                let acct = broker.account().map_err(|e| e.to_string())?;
                let px = mind_tools::MarketClient::from_env()
                    .ok()
                    .and_then(|c| c.last_price(&s2).ok())
                    .ok_or_else(|| "no live price — refusing to size blind".to_string())?;
                let qty = (stake / px).floor();
                mind_tools::broker::check_order(qty, px, acct.equity).map_err(|r| r.to_string())?;
                let sd = if side2 == "long" { mind_tools::broker::Side::Buy } else { mind_tools::broker::Side::Sell };
                let ack = broker.submit_market(&s2, qty, sd).map_err(|e| e.to_string())?;
                Ok((qty, px, format!("{} {}", ack.status, ack.id)))
            })
            .await
            .unwrap_or_else(|e| Err(format!("join failed: {e}")));
            match placed {
                Ok((qty, px, ack)) => {
                    // RECORD THE TRADE, not just the order. A broker position carries no entry
                    // time and no link to the prediction it was betting on, so without this the
                    // horizon rule can never come due (every position reads as brand new) and the
                    // prediction can never be graded (nothing knows which position resolves it).
                    // Both failures ran live for five days on the WMT short.
                    let signed = if side == "long" { qty } else { -qty };
                    self.record_open_trade(mind_tools::trades::OpenTrade {
                        symbol: sym.clone(),
                        qty: signed,
                        entry: px,
                        opened_at_ms: now,
                        judgment_ref: sym.clone(),
                        thesis: thesis.clone(),
                    })
                    .await;
                    out.push_str(&format!("      ✓ {side} {qty} {sym} @ ~{px:.2} — {ack}
"));
                }
                Err(e) => out.push_str(&format!("      ✗ not filled: {e}\n")),
            }
        }
        out
    }

    /// Persist a trade record beside the broker position.
    pub(crate) async fn record_open_trade(&self, t: mind_tools::trades::OpenTrade) {
        let raw = self.memory.profile_get("open_trades").await.ok().flatten().unwrap_or_default();
        let mut book = mind_tools::trades::parse_book(&raw);
        mind_tools::trades::upsert(&mut book, t);
        let _ = self.memory.profile_set("open_trades", &mind_tools::trades::render_book(&book)).await;
    }

    pub(crate) async fn open_trade_book(&self) -> Vec<mind_tools::trades::OpenTrade> {
        let raw = self.memory.profile_get("open_trades").await.ok().flatten().unwrap_or_default();
        mind_tools::trades::parse_book(&raw)
    }

    /// GRADE what has come due — the other half of logging a prediction.
    ///
    /// Six `hunt` predictions sat "awaiting their deadline" days after a 24-hour deadline had
    /// passed, because nothing ever graded a trading claim. The ledger recorded what the mind
    /// believed and never found out whether it was right: logged, expired, silent. A trust score
    /// built on that is a score of predictions nobody checked.
    ///
    /// Graded on DIRECTION against the tape. The claim was "this should be profitable", so a
    /// position that moved the predicted way was a correct read even if the exit was mistimed.
    pub async fn grade_due_trades(&self) -> String {
        let book = self.open_trade_book().await;
        if book.is_empty() {
            return "no open trades on record to grade".to_string();
        }
        let now = chrono::Utc::now().timestamp_millis();
        let horizon = mind_tools::exit::ExitRule::default().horizon_ms;
        let mut lines: Vec<String> = Vec::new();
        for t in book {
            if now.saturating_sub(t.opened_at_ms) < horizon {
                lines.push(format!("  {} — inside its horizon, not due yet", t.symbol));
                continue;
            }
            let sym = t.symbol.clone();
            let price = tokio::task::spawn_blocking(move || {
                mind_tools::MarketClient::from_env().ok().and_then(|c| c.last_price(&sym).ok())
            })
            .await
            .ok()
            .flatten();
            let Some(price) = price else {
                // No price is not a verdict. Grading against a failed quote would record a guess
                // as an outcome, which is worse than leaving the claim pending.
                lines.push(format!("  {} — no price, cannot grade honestly", t.symbol));
                continue;
            };
            let right = t.was_right(price);
            self.judgment_grade(&t.judgment_ref, right).await;
            lines.push(format!(
                "  {} {} — entry {:.2}, now {:.2} ({:+.2}%) -> {}",
                t.symbol,
                if t.is_short() { "short" } else { "long" },
                t.entry,
                price,
                t.favour_pct(price),
                if right { "RIGHT" } else { "WRONG" }
            ));
        }
        format!("⚖️  GRADED WHAT CAME DUE
{}", lines.join("
"))
    }

    /// SOURCES — who has earned attention, from the record rather than the impression.
    ///
    /// Reads the judgment ledger the mind already keeps, rolls the GRADED rows up by source, and
    /// reports where each one stands. This is the step that makes trust a measurement instead of a
    /// type: claims were already logged with their origin and graded later, but nothing joined those
    /// two facts, so a source could be wrong indefinitely without anything noticing.
    ///
    /// Pending claims are excluded and counted separately. A prediction whose deadline has not
    /// arrived is not a wrong prediction, and folding the two together would quietly punish whoever
    /// makes the longest-horizon calls.
    pub async fn source_standing(&self) -> String {
        let led: Vec<serde_json::Value> = self
            .memory
            .profile_get("judgment_ledger")
            .await
            .ok()
            .flatten()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if led.is_empty() {
            return "📚 No judgment ledger yet — nothing has been claimed, so nothing can be trusted.".to_string();
        }
        let mut pending: std::collections::BTreeMap<String, u32> = Default::default();
        let graded: Vec<(String, bool)> = led
            .iter()
            .filter_map(|r| {
                let src = r.get("source").and_then(|x| x.as_str()).unwrap_or("(unknown)").to_string();
                // The ledger writes an outcome as 1/0, not true/false. Reading it with as_bool()
                // returned None for every graded row, so this reported "803 claims, 0 graded" — the
                // mind looked as though it had never learned from a single outcome in its life. The
                // reader was wrong, not the ledger, and it is worth noticing that the false version
                // was entirely believable.
                let outcome = r
                    .get("outcome")
                    .and_then(|x| x.as_bool().or_else(|| x.as_i64().map(|n| n != 0)));
                match outcome {
                    Some(o) => Some((src, o)),
                    None => {
                        *pending.entry(src).or_default() += 1;
                        None
                    }
                }
            })
            .collect();
        let tallied = mind_tools::scout::tally(graded.iter().map(|(s, o)| (s.as_str(), *o)));

        let mut out = format!(
            "📚 SOURCE STANDING — {} claims logged, {} graded\n",
            led.len(),
            graded.len()
        );
        if tallied.is_empty() {
            out.push_str("  nothing graded yet: every source is unproven, which is a fact about the ledger, not about them\n");
        }
        for (src, rec) in &tallied {
            let st = mind_tools::scout::standing(rec);
            let hit = if rec.graded > 0 { rec.correct as f64 / rec.graded as f64 * 100.0 } else { 0.0 };
            out.push_str(&format!(
                "  {:<28} {}/{} correct ({hit:.0}%) — {}\n",
                src,
                rec.correct,
                rec.graded,
                match st {
                    mind_tools::scout::Standing::Trusted => "TRUSTED (act on it)",
                    mind_tools::scout::Standing::Dropped => "DROPPED (stop spending attention)",
                    mind_tools::scout::Standing::Provisional if rec.graded < mind_tools::scout::MIN_GRADED =>
                        "provisional (too few calls to judge)",
                    mind_tools::scout::Standing::Provisional => "provisional (no edge over a coin flip)",
                }
            ));
        }
        if !pending.is_empty() {
            out.push_str("  awaiting their deadline (not counted against anyone):\n");
            for (src, n) in pending.iter().take(8) {
                out.push_str(&format!("    {src}: {n}\n"));
            }
        }
        out
    }

    /// SURF — look at every live feed in the rotation, not one.
    ///
    /// A person watches one screen because they have one pair of eyes; that limit is not a law of
    /// markets, it is a fact about people. The mind's version of an edge is not seeing one feed
    /// better, it is seeing five at once and noticing which one just changed.
    ///
    /// So this checks what is live across the roster, looks at each, and — crucially — DIFFS each
    /// reading against the last one stored for that feed. A single look at a trading desk is nearly
    /// worthless: a trader who is flat tells you nothing, as this afternoon demonstrated twice. The
    /// signal is the transition, and a transition needs a memory of the previous look, which is why
    /// each sighting is stored under the feed's own key.
    pub async fn surf_feeds(&self, spec: &str) -> String {
        let feeds = if spec.trim().is_empty() {
            mind_tools::surf::default_feeds()
        } else {
            mind_tools::surf::parse_feeds(spec)
        };
        let mut out = String::from("📡 SURFING the rotation (each feed diffed against its own last look)\n");
        let mut changes: Vec<String> = Vec::new();

        // WHICH feeds are live is asked of all of them AT ONCE. Sequentially this cost one probe's
        // latency per channel and made "watch many feeds" behave exactly like watching them one at
        // a time — the limit the whole module exists to remove. Probes are cheap metadata calls, so
        // there is nothing to stagger.
        let mut probes = Vec::new();
        for f in feeds.iter().take(6) {
            let u = mind_tools::surf::live_url(&f.handle);
            probes.push(tokio::task::spawn_blocking(move || mind_tools::media::probe(&u)));
        }
        let mut live: Vec<(&mind_tools::surf::Feed, String, String, String)> = Vec::new();
        for (f, h) in feeds.iter().take(6).zip(probes) {
            match h.await.ok().and_then(|r| r.ok()) {
                Some(p) if p.is_live => live.push((f, mind_tools::surf::live_url(&f.handle), p.title, p.id)),
                _ => out.push_str(&format!("  · {} — not live now\n", f.handle)),
            }
        }
        // A handle is followed rather than a video id precisely because a desk ENDS one broadcast
        // and starts another when the shift changes — TraderTV ran "Moderna Goes Parabolic" and
        // then "Wall Street Bounces" the same afternoon, same traders, different id. Following the
        // id would have the mind watching a finished recording while the desk trades on.

        for (f, url, title, vid) in live {
            let p_title = title;
            // Identity is the VIDEO ID, not the title: short, ASCII and stable, where a 103-character
            // emoji-laden title compared unequal to itself across a storage round trip and reported
            // a NEW BROADCAST on a stream that had not changed.
            let p_id = vid;
            // A GLANCE, not a viewing: one whole frame and one vision call, no audio at all.
            //
            // A full watch spends most of its minutes pulling a 180-second audio window and running
            // whisper over it, and for surfing that is spent on the wrong modality. Today's evidence
            // is unambiguous: the position banner and the watchlist both came off FRAMES, while the
            // audio carried commentary — a wedge forming, someone's opinion of a cancer vaccine —
            // that named no ticker and no direction. Paying five minutes a feed for that reduces a
            // rotation to one channel again, which is the limit this exists to remove.
            //
            // Whole frame, no crop: what makes this work on a channel nobody tuned is that the model
            // is asked what it SEES rather than told where to look.
            let u = url.clone();
            let frame = tokio::task::spawn_blocking(move || mind_tools::media::keyframes(&u, 1, 20))
                .await
                .ok()
                .and_then(|r| r.ok())
                .and_then(|f| f.into_iter().next());
            // A CONSTRAINED reading, not a description.
            //
            // The first version asked the model to say what it saw and diffed the prose. Three
            // consecutive passes over the same feed all reported CHANGED, because free text is
            // never stable: the model picks a different chart to mention, reads a different
            // headline, rephrases itself. Stripping digits stopped price ticks from counting and
            // did nothing about the prose itself — the same failure as the scene-detector that fired
            // 776 times in 25 seconds, one abstraction level up, and just as invisible.
            //
            // So the answer is pinned to a closed vocabulary. POSITIONS is a handful of names and
            // three possible states; a list of tickers is a set of symbols. Those are stable across
            // two readings of an unchanged screen, and they change exactly when the thing worth
            // knowing changes.
            // The LENS supplies both the question and what counts as a change. Surfing is not a
            // trading feature — a desk is watched for who is holding what, a news channel for which
            // headlines are up — so the roster names the lens and this loop stays domain-free.
            let lens = mind_tools::surf::lens_named(&f.lens);
            let digest = match frame {
                Some((_, bytes)) => self.analyze_image_bytes(bytes, "image/jpeg", lens.prompt).await,
                None => String::new(),
            };
            let digest: String = digest.chars().take(600).collect();
            // The stored look is stamped with the BROADCAST it came from. When a desk restarts its
            // stream, the previous reading describes a different broadcast — often a different
            // shift with different traders — and diffing across that seam compares two unrelated
            // screens. That is a transition the mind would report and act on, and it never happened.
            let key = format!("surf_last_{}", f.handle.trim_start_matches('@'));
            let stored = self.memory.profile_get(&key).await.ok().flatten().unwrap_or_default();
            let (prev_id, before) = stored.split_once('\n').unwrap_or(("", ""));
            let same_broadcast = !prev_id.is_empty() && prev_id == p_id;
            let moved = same_broadcast && mind_tools::surf::changed_by(&lens, before, &digest);
            let _ = self.memory.profile_set(&key, &format!("{p_title}\n{digest}")).await;
            out.push_str(&format!(
                "  {} {} — {}\n",
                if moved { "🔔" } else { "·" },
                f.handle,
                p_title.chars().take(60).collect::<String>()
            ));
            if prev_id.is_empty() {
                out.push_str("      (first look — nothing to diff against yet)\n");
            } else if !same_broadcast {
                out.push_str(&format!(
                    "      NEW BROADCAST (was \"{}\") — seeded, not a position change\n",
                    prev_id.chars().take(40).collect::<String>()
                ));
            } else if moved {
                changes.push(f.handle.clone());
                // SHOW THE WORK. Four rounds of fixes to this detector all ended the same way: six
                // passes, six bells, and no way to tell a real transition from a misread without a
                // live stream to re-run against. A detector that announces a change and cannot say
                // WHAT changed is unfalsifiable, and an unfalsifiable signal is the one thing this
                // must never be — it would send the mind to trade on a rephrasing.
                let b4 = (lens.reduce)(&before).unwrap_or_default();
                let now = (lens.reduce)(&digest).unwrap_or_default();
                let opened: Vec<&String> = now.difference(&b4).collect();
                let closed: Vec<&String> = b4.difference(&now).collect();
                out.push_str("      CHANGED since the last look:\n");
                if !opened.is_empty() {
                    out.push_str(&format!("        + {}\n", opened.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
                }
                if !closed.is_empty() {
                    out.push_str(&format!("        - {}\n", closed.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")));
                }
                if opened.is_empty() && closed.is_empty() {
                    out.push_str("        (nothing added or removed — the reducer is unstable, NOT a real transition)\n");
                }
            }
        }
        if changes.is_empty() {
            out.push_str("\nNothing changed across the rotation. That is a real observation, not a failure — \
                          most looks at most feeds should be quiet, and a surfer that always finds something \
                          is finding noise.\n");
        } else {
            out.push_str(&format!("\nChanged: {}. Run `ym copy-trade <url>` on those.\n", changes.join(", ")));
        }
        out
    }

    /// WATCH → TYPED SIGNAL → PAPER POSITION → GRADEABLE PREDICTION.
    ///
    /// `learn_from_watch` already turns a broadcast into beliefs and claims. This closes the loop:
    /// a claim that can be ACTED on becomes a position, and the position is logged as a prediction
    /// so it is graded like any other. That pairing is the whole point — a trade that is not a
    /// recorded prediction teaches nothing when it wins or loses, and a prediction with no position
    /// never meets a fill, a spread or a queue. Neither half alone earns anything.
    ///
    /// Every signal that was NOT acted on is reported with the reason. A tape that lists only the
    /// trades taken looks like a strategy with perfect discipline; the refusals are where the
    /// selection actually lives, and hiding them is how a backtest flatters itself.
    pub async fn trade_from_watch(&self, url: &str, focus: &str) -> String {
        let perception = self.watch_media(url, focus).await;
        if perception.contains("I perceived nothing") {
            return format!("{perception}\n\n(nothing perceived, so nothing traded)");
        }
        let seen: String = perception.chars().take(6000).collect();
        let prompt = format!(
            "You WATCHED a segment of a live trading broadcast. Below is what was seen on screen and heard.\n\n\
             {seen}\n\n\
             Extract only ACTIONABLE directional signals — a specific ticker someone is trading or \
             calling, with a direction. A watchlist name with no stated view is NOT a signal. A \
             general market comment is NOT a signal. If nobody expressed a direction on a specific \
             ticker, return an empty array; that is a correct and common answer.\n\
             Output ONLY JSON:\n\
             {{\"signals\":[{{\"symbol\":\"TICKER\",\"side\":\"long\"|\"short\",\"conviction\":0.0-1.0,\
             \"level\":\"the price level or trigger mentioned, or empty\",\"why\":\"one short line, their reasoning\"}}]}}"
        );
        let messages = vec![
            ChatMessage::system(&self.persona),
            ChatMessage::system("You extract typed trading signals. Output ONLY the JSON object. An empty array is a valid answer."),
            ChatMessage::user(&prompt),
        ];
        let text = match self.inference.chat_grounded(messages, GenerationConfig::default()).await {
            Ok(r) => r.text,
            Err(e) => return format!("{perception}\n\n(could not read signals from what I saw: {e})"),
        };
        let body_owned = crate::strip_reasoning(&text);
        let body = body_owned.as_str();
        let body = body.split("```").find(|s| s.contains('{')).unwrap_or(body);
        let obj = match (body.find('{'), body.rfind('}')) {
            (Some(s), Some(e)) if e > s => &body[s..=e],
            _ => "{}",
        };
        let v: serde_json::Value = serde_json::from_str(obj).unwrap_or(serde_json::json!({}));
        let signals = v.get("signals").and_then(|x| x.as_array()).cloned().unwrap_or_default();
        if signals.is_empty() {
            return format!("{perception}\n\n📈 No actionable directional signal in this window — nothing traded. (A watchlist is not a call.)");
        }

        // Conviction floor. Acting on everything heard would measure the broadcast's chattiness
        // rather than its skill.
        let floor: f64 = std::env::var("YM_TRADE_MIN_CONVICTION").ok().and_then(|s| s.parse().ok()).unwrap_or(0.6);
        let stake: f64 = std::env::var("YM_PAPER_STAKE_USD").ok().and_then(|s| s.parse().ok()).unwrap_or(250.0);

        let mut acted: Vec<(String, String, f64, f64, String, String)> = Vec::new(); // sym, side, qty, px, why, ack
        let mut refused: Vec<String> = Vec::new();
        let now = chrono::Utc::now().timestamp_millis();

        for s in signals.into_iter().take(4) {
            let sym = s.get("symbol").and_then(|x| x.as_str()).unwrap_or("").trim().trim_start_matches('$').to_uppercase();
            let side_s = s.get("side").and_then(|x| x.as_str()).unwrap_or("").trim().to_lowercase();
            let conv = s.get("conviction").and_then(|x| x.as_f64()).unwrap_or(0.0);
            let why = s.get("why").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            let level = s.get("level").and_then(|x| x.as_str()).unwrap_or("").trim().to_string();
            if sym.is_empty() || sym.len() > 8 {
                refused.push(format!("(unnamed symbol) — no usable ticker"));
                continue;
            }
            if !matches!(side_s.as_str(), "long" | "short") {
                refused.push(format!("{sym} — no clear direction ({side_s:?})"));
                continue;
            }
            if conv < floor {
                refused.push(format!("{sym} {side_s} — conviction {conv:.2} below the {floor:.2} floor"));
                continue;
            }
            let sym2 = sym.clone();
            let side2 = side_s.clone();
            // Price, sizing, bound-check and submission all happen off the async runtime.
            let placed = tokio::task::spawn_blocking(move || -> std::result::Result<(f64, f64, String), String> {
                let broker = mind_tools::broker::PaperBroker::from_env().map_err(|e| e.to_string())?;
                let acct = broker.account().map_err(|e| e.to_string())?;
                let px = mind_tools::MarketClient::from_env()
                    .ok()
                    .and_then(|c| c.last_price(&sym2).ok())
                    .ok_or_else(|| "no live price — refusing to size a position blind".to_string())?;
                let qty = (stake / px).floor();
                // The bound is checked BEFORE the order exists, so a refusal names which limit hit.
                mind_tools::broker::check_order(qty, px, acct.equity).map_err(|r| r.to_string())?;
                let side = if side2 == "long" {
                    mind_tools::broker::Side::Buy
                } else {
                    mind_tools::broker::Side::Sell
                };
                let ack = broker.submit_market(&sym2, qty, side).map_err(|e| e.to_string())?;
                Ok((qty, px, format!("{} {}", ack.status, ack.id)))
            })
            .await
            .unwrap_or_else(|e| Err(format!("join failed: {e}")));

            match placed {
                Ok((qty, px, ack)) => {
                    // The position IS a prediction, so it is filed as one and graded on the same
                    // ledger as everything else the mind asserts.
                    let claim = format!(
                        "Copy-trade from a live broadcast: {sym} {side_s} entered at {px:.2}{}{} should be profitable",
                        if level.is_empty() { String::new() } else { format!(" (level {level})") },
                        if why.is_empty() { String::new() } else { format!(" — {why}") },
                    );
                    // Attribute the claim to the SOURCE, not to the mechanism. Logging every copied trade
                    // under "copy_trade" would pool a good desk and a bad one into one meaningless
                    // record, and the whole point of a record is to tell them apart.
                    let src = mind_tools::scout::source_label(url);
                    self.judgment_log(&src, "trading", &claim, conv.clamp(0.05, 0.95), now + 86_400_000, url).await;
                    acted.push((sym, side_s, qty, px, why, ack));
                }
                Err(e) => refused.push(format!("{sym} {side_s} — {e}")),
            }
        }

        let mut out = perception;
        out.push_str("\n\n📈 SIGNALS → PAPER POSITIONS (sandbox account; every fill is also a logged prediction)\n");
        if acted.is_empty() {
            out.push_str("  nothing was traded.\n");
        }
        for (sym, side, qty, px, why, ack) in &acted {
            out.push_str(&format!("  ✓ {side} {qty} {sym} @ ~{px:.2} — {ack}{}\n", if why.is_empty() { String::new() } else { format!(" · {why}") }));
        }
        if !refused.is_empty() {
            out.push_str("  not acted on (the refusals are where the selection lives):\n");
            for r in &refused {
                out.push_str(&format!("    · {r}\n"));
            }
        }
        out
    }

    /// FOLLOW — check every open position against its exit rule, and close what is due.
    ///
    /// The first trade this mind took had an entry and no plan to leave. It was a same-day thesis
    /// and it sat overnight, which quietly turned it into a swing trade nobody chose. Entering is a
    /// judgment; continuing to hold has to be one too, and until this existed it was simply the
    /// default.
    ///
    /// Every close names the rule that fired, because "stopped out" and "the thesis expired" are
    /// different facts about the same trade and only one of them argues the view was wrong.
    pub async fn follow_positions(&self, act: bool) -> String {
        // ADOPT what we find. A position with no record fell back to "opened now" on every pass,
        // so its horizon restarted every time it was looked at and it could never age out — a
        // permanent orphan, which is exactly what the WMT short became: opened before this record
        // existed, five days old, and still reading as brand new.
        //
        // We cannot know when an unrecorded position opened. We do know when we FIRST SAW it, and
        // ageing from first sight is honest and terminates; the alternative is a position that is
        // never due by construction. Adoption is stamped once and then never moves.
        let mut book = self.open_trade_book().await;
        {
            let known: Vec<String> = book.iter().map(|t| t.symbol.to_uppercase()).collect();
            let live = tokio::task::spawn_blocking(|| {
                mind_tools::broker::PaperBroker::from_env().ok().and_then(|b| b.positions().ok())
            })
            .await
            .ok()
            .flatten()
            .unwrap_or_default();
            let stamp = chrono::Utc::now().timestamp_millis();
            let mut adopted = false;
            for p in live {
                if known.contains(&p.symbol.to_uppercase()) {
                    continue;
                }
                mind_tools::trades::upsert(
                    &mut book,
                    mind_tools::trades::OpenTrade {
                        symbol: p.symbol.clone(),
                        qty: p.qty,
                        entry: p.avg_entry_price,
                        opened_at_ms: stamp,
                        judgment_ref: p.symbol.clone(),
                        thesis: "adopted — opened before trades were recorded; ageing from first sight".into(),
                    },
                );
                adopted = true;
            }
            if adopted {
                let _ = self
                    .memory
                    .profile_set("open_trades", &mind_tools::trades::render_book(&book))
                    .await;
            }
        }
        let res = tokio::task::spawn_blocking(move || -> std::result::Result<Vec<String>, String> {
            let broker = mind_tools::broker::PaperBroker::from_env().map_err(|e| e.to_string())?;
            let positions = broker.positions().map_err(|e| e.to_string())?;
            if positions.is_empty() {
                return Ok(vec!["no open positions — nothing to follow".to_string()]);
            }
            let market = mind_tools::MarketClient::from_env().ok();
            let now = chrono::Utc::now().timestamp_millis();
            let mut lines = Vec::new();
            for p in positions {
                let Some(price) = market.as_ref().and_then(|c| c.last_price(&p.symbol).ok()) else {
                    // No price means no judgment. Closing a position because the quote failed would
                    // be acting on the absence of information.
                    lines.push(format!("  {} — no live price, leaving it alone", p.symbol));
                    continue;
                };
                // Entry time comes from OUR record, not the broker's position — the broker has
                // none, so before this every position read as zero seconds old and the horizon
                // rule could never come due. A same-day thesis ran for five days that way.
                // Absent a record we still fall back to "opened now", which errs toward HOLDING
                // rather than closing something whose age is genuinely unknown.
                let opened = book
                    .iter()
                    .find(|t| t.symbol.eq_ignore_ascii_case(&p.symbol))
                    .map(|t| t.opened_at_ms)
                    .unwrap_or(now);
                let pos = mind_tools::exit::OpenPosition {
                    symbol: p.symbol.clone(),
                    qty: p.qty,
                    entry: p.avg_entry_price,
                    entered_at_ms: opened,
                    rule: mind_tools::exit::ExitRule::default(),
                };
                let fav = pos.favour_pct(price);
                match mind_tools::exit::should_close(&pos, price, now) {
                    Some(reason) => {
                        if !act {
                            lines.push(format!("  {} {:+} @ {:.2} — now {:.2} ({fav:+.2}%) — WOULD CLOSE: {}",
                                p.symbol, p.qty, p.avg_entry_price, price, reason.as_str()));
                            continue;
                        }
                        // Closing a short is a buy, and a long is a sell.
                        let side = if p.qty < 0.0 { mind_tools::broker::Side::Buy } else { mind_tools::broker::Side::Sell };
                        match broker.submit_market(&p.symbol, p.qty.abs(), side) {
                            Ok(ack) => lines.push(format!("  {} CLOSED @ ~{:.2} ({fav:+.2}%) — {} [{}]",
                                p.symbol, price, reason.as_str(), ack.status)),
                            Err(e) => lines.push(format!("  {} — close FAILED: {e}", p.symbol)),
                        }
                    }
                    None => lines.push(format!("  {} {:+} @ {:.2} — now {:.2} ({fav:+.2}%) — holding, no rule fired",
                        p.symbol, p.qty, p.avg_entry_price, price)),
                }
            }
            Ok(lines)
        })
        .await
        .unwrap_or_else(|e| Err(format!("join failed: {e}")));
        match res {
            Ok(lines) => format!("👣 FOLLOW{}
{}", if act { " (closing what is due)" } else { " (dry run — pass `act` to close)" }, lines.join("
")),
            Err(e) => format!("👣 Follow failed: {e}"),
        }
    }

    /// The sandbox book: what the paper account holds and what it is worth.
    ///
    /// Reported as MEASURED, like every other number the mind states about the world. The paper
    /// balance is the copy-trade experiment's readout, so a stale or recalled figure here would not
    /// be a small inaccuracy — it would be the instrument lying about itself.
    pub async fn paper_book(&self) -> String {
        tokio::task::spawn_blocking(|| {
            let b = match mind_tools::broker::PaperBroker::from_env() {
                Ok(b) => b,
                Err(e) => return format!("No paper account reachable: {e}"),
            };
            let acct = match b.account() {
                Ok(a) => a,
                Err(e) => return format!("Could not read the paper account: {e}"),
            };
            let mut out = format!(
                "📒 Paper account {} ({}) — measured, not recalled\n  equity ${:.2} · cash ${:.2} · buying power ${:.2}\n",
                acct.account_number, acct.status, acct.equity, acct.cash, acct.buying_power
            );
            match b.positions() {
                Ok(ps) if ps.is_empty() => out.push_str("  no open positions\n"),
                Ok(ps) => {
                    for p in ps {
                        out.push_str(&format!(
                            "  {} {:+} @ {:.2} · now ${:.2} · unrealised {:+.2}\n",
                            p.symbol, p.qty, p.avg_entry_price, p.market_value, p.unrealized_pl
                        ));
                    }
                }
                Err(e) => out.push_str(&format!("  (positions unreadable: {e})\n")),
            }
            out
        })
        .await
        .unwrap_or_else(|e| format!("paper book failed: {e}"))
    }

    pub async fn quote_symbols(&self, spec: &str) -> String {
        let syms: Vec<String> = spec
            .split(|c: char| c == ',' || c.is_whitespace())
            .map(|s| s.trim().trim_start_matches('$').to_uppercase())
            .filter(|s| !s.is_empty() && s.len() <= 12)
            .take(8)
            .collect();
        if syms.is_empty() {
            return "Which symbols? e.g. `ym quote SPY, RELIANCE.NS, ^NSEI`".to_string();
        }
        let lines = tokio::task::spawn_blocking(move || {
            let client = mind_tools::MarketClient::from_env().ok();
            let mut out: Vec<String> = Vec::new();
            for s in syms {
                // Indian listings and indices go to Yahoo; US equities try Alpaca first.
                if !mind_tools::is_indian(&s) {
                    if let Some(px) = client.as_ref().and_then(|c| c.last_price(&s).ok()) {
                        out.push(format!("  {s}: {px:.2} USD (Alpaca)"));
                        continue;
                    }
                }
                match mind_tools::yahoo_series(&s, "1d", "1m") {
                    Ok(ser) => match ser.bars.last() {
                        Some(b) => {
                            let first = ser.bars.first().map(|f| f.close).unwrap_or(b.close);
                            let chg = if first > 0.0 { (b.close - first) / first * 100.0 } else { 0.0 };
                            out.push(format!(
                                "  {}: {:.2} {} ({:+.2}% on the session, {} bars, {})",
                                ser.symbol, b.close, ser.currency, chg, ser.bars.len(), ser.exchange_tz
                            ));
                        }
                        None => out.push(format!("  {s}: no bars returned (market may be closed with no session data)")),
                    },
                    Err(e) => out.push(format!("  {s}: unavailable — {e}")),
                }
            }
            out
        })
        .await
        .unwrap_or_default();
        if lines.is_empty() {
            return "No quotes came back — reporting nothing rather than guessing.".to_string();
        }
        format!("💹 Quotes (measured, not recalled):\n{}", lines.join("\n"))
    }
}
