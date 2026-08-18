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
