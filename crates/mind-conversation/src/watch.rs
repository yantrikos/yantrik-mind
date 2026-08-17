//! WATCH — the mind's reach for audio and video, wired to the senses it already had.
//!
//! Nothing here perceives anything new. Hearing is the same local whisper that has handled voice
//! notes since July; seeing is the same `VisionClient` that reads photos and page screenshots. What
//! this adds is the missing middle: fetch the media, decide what it IS, and take a video apart into
//! pictures and a voice note so the existing senses can do their jobs.
//!
//! The order of preference is deliberate and it is about honesty as much as cost. Published
//! captions win whenever they exist, because they are the speaker's actual words rather than a
//! CPU's guess at them. Local whisper is next, bounded hard, because the box has no GPU and an
//! eight-hour broadcast is impossible rather than slow — and refusing it with the numbers beats
//! starting a job that never ends. Frames are sampled for material whose content is on screen: a
//! trading desk that broadcasts "no commentary" says nothing worth transcribing and shows
//! everything worth seeing.

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
                match tokio::task::spawn_blocking(move || mind_tools::media::captions(&u)).await {
                    Ok(Ok(t)) => {
                        out.push_str("📝 Read the published captions (the speaker's own words).\n");
                        Some(t)
                    }
                    Ok(Err(e)) => {
                        out.push_str(&format!("📝 Captions were advertised but unreadable ({e}).\n"));
                        None
                    }
                    Err(_) => None,
                }
            }
            mind_tools::media::MediaPlan::Transcribe { secs } | mind_tools::media::MediaPlan::LiveWindow { secs } => {
                let (u, s) = (url.to_string(), *secs);
                let live = matches!(plan, mind_tools::media::MediaPlan::LiveWindow { .. });
                match tokio::task::spawn_blocking(move || mind_tools::media::transcribe_segments(&u, s)).await {
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
            mind_tools::media::MediaPlan::TooLong { duration_secs, cap_secs } => {
                out.push_str(&format!(
                    "🎧 Too long to hear locally: {}m of audio against a {}m cap, and this box has no GPU — whisper runs at about real time, so I'd still be listening hours from now. I can watch a window of it, or you can raise YM_MEDIA_MAX_SECS if you want to spend the time.\n",
                    duration_secs / 60,
                    cap_secs / 60
                ));
                None
            }
        };

        // ── SEEING ─────────────────────────────────────────────────────────────────────────
        // Always sample frames: for screen-content media this IS the information, and it is the
        // only modality available when the audio is refused or silent.
        let window = match &plan {
            mind_tools::media::MediaPlan::LiveWindow { secs } => *secs,
            mind_tools::media::MediaPlan::TooLong { cap_secs, .. } => *cap_secs,
            mind_tools::media::MediaPlan::Transcribe { secs } => *secs,
            mind_tools::media::MediaPlan::Captions => probe.duration_secs.min(cap).max(60),
        };
        let (u, want) = (url.to_string(), frame_budget());
        let frames = match tokio::task::spawn_blocking(move || mind_tools::media::keyframes(&u, want, window)).await {
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
