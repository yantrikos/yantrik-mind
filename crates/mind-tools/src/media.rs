//! EYES AND EARS FOR MEDIA THE MIND CAN REACH.
//!
//! The senses already existed and were wired to things HANDED to the mind: a Telegram voice note
//! became text through the local whisper at `/opt/voice`, and a photo (or a page screenshot)
//! became a description through `VisionClient`. What was missing was reach — nothing could go and
//! GET media behind a URL, and nothing took a video apart. So a link to an hour of video came back
//! as "I learned nothing", which was honest and useless at the same time.
//!
//! A video is pictures plus a voice note. This module decomposes it into the two senses that
//! already work, and does nothing clever beyond that:
//!
//! - **probe** — what IS this (title, duration, live, does it already have captions).
//! - **captions** — if the publisher shipped a transcript, read THAT: free, instant, exact, and no
//!   compute at all. Most of YouTube lands here.
//! - **transcribe** — otherwise pull audio only, downmix to what whisper wants, and run the local
//!   whisper. Bounded by a hard duration cap because the box has no GPU.
//! - **keyframes** — sample scene-change frames for `VisionClient`. For material whose information
//!   is ON SCREEN (a trading desk that broadcasts "no commentary"), this is the modality that
//!   carries the content, and audio carries almost nothing.
//!
//! Three rules this module refuses to break, all learned the hard way elsewhere in this codebase:
//! every external binary is spawned with **argv, never a shell** (a URL is untrusted input);
//! every remote URL passes the same **SSRF guard** as `fetch`, so an injected link cannot make the
//! mind pull from the home network; and a **missing tool degrades honestly** — it says what is not
//! installed rather than pretending it looked. Media files are temporary and deleted after
//! extraction: the transcript is knowledge, the download is not.

use std::path::{Path, PathBuf};
use std::process::Command;

/// How long a media job may run before it is abandoned, per external process.
const PROC_TIMEOUT_SECS: u32 = 300;

/// What a piece of media IS, before deciding what to do with it.
#[derive(Debug, Clone, PartialEq)]
pub struct MediaProbe {
    pub title: String,
    pub uploader: String,
    /// Total seconds; 0 for a live broadcast (it has no end yet).
    pub duration_secs: u64,
    pub is_live: bool,
    /// The publisher already ships a transcript (manual or auto captions).
    pub has_captions: bool,
}

/// What the mind should DO with this media, decided before any expensive work starts.
#[derive(Debug, Clone, PartialEq)]
pub enum MediaPlan {
    /// The publisher's own transcript exists — read it. No compute, no cost, exact words.
    Captions,
    /// No captions, short enough to hear: run the local whisper over `secs` of audio.
    Transcribe { secs: u64 },
    /// A live broadcast has no end and no finished transcript; sample a window of it instead.
    LiveWindow { secs: u64 },
    /// Longer than the box can hear in full, so hear the FIRST `secs` and say so. Refusing a
    /// three-hour recording outright taught nothing, when thirty minutes of it was available the
    /// whole time — a partial listen honestly labelled beats a principled silence.
    PartialListen { secs: u64, of_secs: u64 },
}

/// Decide before spending anything. Captions beat transcription whenever they exist — they are
/// free, instant, and the publisher's own words rather than a CPU's guess at them.
pub fn plan(probe: &MediaProbe, cap_secs: u64, live_window_secs: u64) -> MediaPlan {
    if probe.is_live {
        return MediaPlan::LiveWindow { secs: live_window_secs.min(cap_secs) };
    }
    if probe.has_captions {
        return MediaPlan::Captions;
    }
    if probe.duration_secs > cap_secs {
        return MediaPlan::PartialListen { secs: cap_secs, of_secs: probe.duration_secs };
    }
    MediaPlan::Transcribe { secs: probe.duration_secs.max(1) }
}

/// The duration ceiling for local hearing. The box is CPU-only, and whisper on CPU runs at roughly
/// real time — so an eight-hour broadcast is not a long job, it is an impossible one, and saying so
/// beats queueing work that silently never finishes.
pub fn cap_secs() -> u64 {
    std::env::var("YM_MEDIA_MAX_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(1800)
}

/// How much of a live broadcast one sample covers.
pub fn live_window_secs() -> u64 {
    std::env::var("YM_MEDIA_LIVE_WINDOW_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(180)
}

fn ytdlp_bin() -> String {
    std::env::var("YM_YTDLP_BIN").unwrap_or_else(|_| "yt-dlp".into())
}

fn ffmpeg_bin() -> String {
    std::env::var("YM_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into())
}

/// Is an external binary actually present?
///
/// Presence is "can it be SPAWNED", never "did it like the flag I passed". The first version of
/// this asked every tool for `--version` and believed a non-zero exit meant absence — so ffmpeg,
/// which wants `-version` and exits 1 on the double-dash form, was reported as not installed while
/// sitting at /usr/bin/ffmpeg. The mind then told its owner it had no ears, which was false and
/// sounded exactly like the truth. A failed spawn is the only honest signal of a missing binary.
pub fn have(bin: &str) -> bool {
    Command::new(bin).arg("-version").output().is_ok()
}

/// Run a command under a hard wall-clock kill, so a hung download can never wedge the mind.
/// Mirrors the headless-fetch pattern: `timeout` owns the deadline, argv owns the safety.
fn run_bounded(bin: &str, args: &[&str]) -> anyhow::Result<std::process::Output> {
    let out = Command::new("timeout")
        .arg(PROC_TIMEOUT_SECS.to_string())
        .arg(bin)
        .args(args)
        .output()
        .map_err(|e| anyhow::anyhow!("could not run {bin}: {e}"))?;
    Ok(out)
}

/// A scratch directory for one media job, removed when the guard drops — the download is a means,
/// never a kept artifact.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> anyhow::Result<Scratch> {
        let mut p = std::env::temp_dir();
        p.push(format!("ym_media_{}_{}", std::process::id(), tag));
        std::fs::create_dir_all(&p)?;
        Ok(Scratch(p))
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Ask yt-dlp what this URL is, without downloading a byte of media.
pub fn probe(url: &str) -> anyhow::Result<MediaProbe> {
    crate::ssrf_check_pub(url)?;
    let bin = ytdlp_bin();
    if !have(&bin) {
        anyhow::bail!("yt-dlp is not installed on this host, so I cannot reach video or audio at a URL yet");
    }
    let out = run_bounded(&bin, &["-J", "--no-playlist", "--no-warnings", "--skip-download", url])?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("yt-dlp could not read that URL: {}", err.lines().next().unwrap_or("unknown error").trim());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout)?;
    Ok(probe_from_json(&v))
}

/// Not every "subtitle" track is speech. YouTube lists `live_chat` — the chat replay — alongside
/// real caption languages, and treating it as a transcript sends the whole pipeline off to read
/// an audience chat log instead of listening to anyone. Found by probing a real stream, which is
/// the only way this kind of thing is ever found.
fn has_speech_tracks(v: &serde_json::Value, key: &str) -> bool {
    v.get(key)
        .and_then(|s| s.as_object())
        .map(|m| m.keys().any(|k| k != "live_chat"))
        .unwrap_or(false)
}

/// Parse yt-dlp's metadata JSON. Split out so the shape can be tested without the binary.
pub fn probe_from_json(v: &serde_json::Value) -> MediaProbe {
    let subs = has_speech_tracks(v, "subtitles");
    let auto = has_speech_tracks(v, "automatic_captions");
    MediaProbe {
        title: v.get("title").and_then(|x| x.as_str()).unwrap_or("(untitled)").to_string(),
        uploader: v.get("uploader").or_else(|| v.get("channel")).and_then(|x| x.as_str()).unwrap_or("").to_string(),
        duration_secs: v.get("duration").and_then(|x| x.as_f64()).unwrap_or(0.0).max(0.0) as u64,
        is_live: v.get("is_live").and_then(|x| x.as_bool()).unwrap_or(false),
        has_captions: subs || auto,
    }
}

/// Strip a WebVTT/SRT caption file down to readable prose with coarse timestamps.
///
/// Auto-captions repeat each line as they roll, so consecutive duplicates are collapsed — without
/// that, a transcript is three times its real length and reads as a stutter.
pub fn captions_to_text(vtt: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut stamp = String::new();
    let mut last = String::new();
    for raw in vtt.lines() {
        let line = raw.trim();
        if line.is_empty() || line == "WEBVTT" || line.starts_with("Kind:") || line.starts_with("Language:") {
            continue;
        }
        if line.contains("-->") {
            // "00:01:23.400 --> 00:01:26.900" → keep mm:ss of the start as a coarse anchor.
            if let Some(start) = line.split("-->").next() {
                let t = start.trim();
                let hhmmss: Vec<&str> = t.split(':').collect();
                stamp = match hhmmss.len() {
                    3 => format!("{}:{}", hhmmss[1], hhmmss[2].split('.').next().unwrap_or("00")),
                    2 => format!("{}:{}", hhmmss[0], hhmmss[1].split('.').next().unwrap_or("00")),
                    _ => String::new(),
                };
            }
            continue;
        }
        if line.chars().all(|c| c.is_ascii_digit()) {
            continue; // SRT cue number
        }
        // Drop inline caption markup (<c>, <00:00:01.000>) that auto-captions carry.
        let mut text = String::with_capacity(line.len());
        let mut skipping = false;
        for c in line.chars() {
            match c {
                '<' => skipping = true,
                '>' => skipping = false,
                _ if !skipping => text.push(c),
                _ => {}
            }
        }
        let text = text.trim();
        if text.is_empty() || text == last {
            continue;
        }
        last = text.to_string();
        if stamp.is_empty() {
            out.push(text.to_string());
        } else {
            out.push(format!("[{stamp}] {text}"));
            stamp.clear();
        }
    }
    out.join("\n")
}

/// Keep only the caption lines whose `[mm:ss]` anchor falls inside a window.
///
/// The captions path bypassed the seek fix entirely: a recording with published captions is read
/// from its first line, so seeking the AUDIO fixed nothing for the case that actually applies to
/// most of YouTube. Windowing here is the same correction applied to the other channel — a
/// three-hour show is not summarised by its first three minutes whichever way you read it.
pub fn captions_window(text: &str, from_secs: u64, secs: u64) -> String {
    let (lo, hi) = (from_secs, from_secs.saturating_add(secs));
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix('[') else { continue };
        let Some((stamp, _)) = rest.split_once(']') else { continue };
        let parts: Vec<&str> = stamp.split(':').collect();
        let at = match parts.len() {
            3 => parts[0].parse::<u64>().unwrap_or(0) * 3600 + parts[1].parse::<u64>().unwrap_or(0) * 60 + parts[2].parse::<u64>().unwrap_or(0),
            2 => parts[0].parse::<u64>().unwrap_or(0) * 60 + parts[1].parse::<u64>().unwrap_or(0),
            _ => continue,
        };
        if at >= lo && at <= hi {
            out.push(line);
        }
    }
    out.join("
")
}

/// Fetch the publisher's own transcript. Prefers manual captions, falls back to auto.
pub fn captions(url: &str) -> anyhow::Result<String> {
    crate::ssrf_check_pub(url)?;
    let bin = ytdlp_bin();
    if !have(&bin) {
        anyhow::bail!("yt-dlp is not installed on this host");
    }
    let scratch = Scratch::new("caps")?;
    let tmpl = scratch.path().join("cap.%(ext)s");
    let out = run_bounded(
        &bin,
        &[
            "--skip-download",
            "--write-subs",
            "--write-auto-subs",
            "--sub-langs",
            "en.*,en",
            "--sub-format",
            "vtt",
            "--no-playlist",
            "--no-warnings",
            "-o",
            tmpl.to_str().unwrap_or("cap.%(ext)s"),
            url,
        ],
    )?;
    if !out.status.success() {
        anyhow::bail!("caption download failed: {}", String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").trim());
    }
    let mut found: Option<String> = None;
    for entry in std::fs::read_dir(scratch.path())?.flatten() {
        let p = entry.path();
        if p.extension().map(|e| e == "vtt").unwrap_or(false) {
            if let Ok(text) = std::fs::read_to_string(&p) {
                found = Some(text);
                break;
            }
        }
    }
    let vtt = found.ok_or_else(|| anyhow::anyhow!("no captions were published for that URL"))?;
    let text = captions_to_text(&vtt);
    if text.trim().is_empty() {
        anyhow::bail!("the caption file was empty");
    }
    Ok(text)
}

/// The direct media stream URL, so ffmpeg can take a bounded window without downloading the whole
/// thing. This is what makes sampling a LIVE broadcast possible at all.
fn stream_url(url: &str, want_audio: bool) -> anyhow::Result<String> {
    let bin = ytdlp_bin();
    let fmt = if want_audio { "bestaudio/best" } else { "best[height<=720]/best" };
    let out = run_bounded(&bin, &["-g", "-f", fmt, "--no-playlist", "--no-warnings", url])?;
    if !out.status.success() {
        anyhow::bail!("could not resolve a media stream: {}", String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("").trim());
    }
    let s = String::from_utf8_lossy(&out.stdout);
    s.lines().find(|l| l.starts_with("http")).map(|l| l.to_string()).ok_or_else(|| anyhow::anyhow!("no stream url returned"))
}

/// One thing that was said, and when — the unit that lets speech line up with pictures.
#[derive(Debug, Clone, PartialEq)]
pub struct Utterance {
    pub at_secs: u64,
    pub text: String,
}

/// Parse whisper.cpp's timestamped output into utterances.
///
/// Whisper emits `[00:00:04.360 --> 00:00:06.780]   words` for free; the voice-note path passes
/// `-nt` to suppress it because a voice note only needs the words. Media needs the clock: without
/// it, speech and frames are two lists that cannot be laid against each other, and "what was on
/// screen when they said that" is unanswerable.
pub fn parse_whisper_segments(out: &str) -> Vec<Utterance> {
    let mut v = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix('[') else { continue };
        let Some((stamp, text)) = rest.split_once(']') else { continue };
        let start = stamp.split("-->").next().unwrap_or("").trim();
        let parts: Vec<&str> = start.split(':').collect();
        let secs = match parts.len() {
            3 => {
                let h: u64 = parts[0].trim().parse().unwrap_or(0);
                let m: u64 = parts[1].trim().parse().unwrap_or(0);
                let s: f64 = parts[2].trim().parse().unwrap_or(0.0);
                h * 3600 + m * 60 + s as u64
            }
            2 => {
                let m: u64 = parts[0].trim().parse().unwrap_or(0);
                let s: f64 = parts[1].trim().parse().unwrap_or(0.0);
                m * 60 + s as u64
            }
            _ => continue,
        };
        let text = text.trim();
        if !text.is_empty() {
            v.push(Utterance { at_secs: secs, text: text.to_string() });
        }
    }
    v
}

/// Flatten utterances back to plain prose (for consumers that only want the words).
pub fn utterances_to_text(u: &[Utterance]) -> String {
    u.iter().map(|x| x.text.as_str()).collect::<Vec<_>>().join(" ")
}

/// HEAR: pull `secs` of audio and run the LOCAL whisper over it. Nothing leaves the house — the
/// same whisper that already handles voice notes, pointed at a different source.
pub fn transcribe(url: &str, secs: u64) -> anyhow::Result<String> {
    Ok(utterances_to_text(&transcribe_segments(url, secs)?))
}

/// Where to start sampling a long recording. Sampling from zero is why two attempts at a trading
/// broadcast returned greetings and then a sofa: the opening of a market show is hellos and the
/// end is the wind-down, while everything worth hearing is in the middle. A third of the way in
/// is a better blind guess than the start, and for anything long it is a much better one.
pub fn sensible_offset(duration_secs: u64, window_secs: u64) -> u64 {
    if duration_secs <= window_secs * 2 {
        return 0; // short enough that the whole thing is the middle
    }
    (duration_secs / 3).min(duration_secs.saturating_sub(window_secs))
}

/// HEAR WITH A CLOCK: the same capture, keeping whisper's own timestamps so speech can be laid
/// against the frames on one timeline.
pub fn transcribe_segments(url: &str, secs: u64) -> anyhow::Result<Vec<Utterance>> {
    transcribe_segments_at(url, secs, 0)
}

/// Hear `secs` of audio starting `from_secs` into the recording.
pub fn transcribe_segments_at(url: &str, secs: u64, from_secs: u64) -> anyhow::Result<Vec<Utterance>> {
    crate::ssrf_check_pub(url)?;
    if !have(&ytdlp_bin()) {
        anyhow::bail!("yt-dlp is not installed on this host");
    }
    if !have(&ffmpeg_bin()) {
        anyhow::bail!("ffmpeg is not installed on this host");
    }
    let whisper = std::env::var("YM_WHISPER_BIN").unwrap_or_else(|_| "/opt/voice/whisper.cpp/build/bin/whisper-cli".into());
    let model = std::env::var("YM_WHISPER_MODEL").unwrap_or_else(|_| "/opt/voice/models/ggml-base.en.bin".into());
    if !Path::new(&model).exists() {
        anyhow::bail!("the local speech model is missing at {model}, so I cannot hear yet");
    }
    let scratch = Scratch::new("audio")?;
    let wav = scratch.path().join("a.wav");
    let src = stream_url(url, true)?;
    // 16 kHz mono is exactly what whisper.cpp wants; -t bounds the work at the source.
    let out = run_bounded(
        &ffmpeg_bin(),
        &["-y", "-ss", &from_secs.to_string(), "-i", &src, "-t", &secs.to_string(), "-vn", "-ar", "16000", "-ac", "1", wav.to_str().unwrap_or("a.wav")],
    )?;
    if !wav.exists() {
        anyhow::bail!("could not extract audio: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").trim());
    }
    // Timestamps KEPT (no `-nt`): they are what lets speech line up with the frames.
    //
    // FOUR THREADS, NOT EIGHT — measured, and the opposite of the obvious choice. On 60s of
    // audio: `-t 8` finished in 10.9s wall but burned 80.5 CPU-seconds; `-t 4` took 16.9s wall
    // for 64.8 CPU-seconds. Background listening has a whole 60-second window to finish in, so
    // wall time is free and total CPU is what actually competes with the mind for the box. More
    // threads bought latency nobody needed at a 24% premium in cores.
    let threads = std::env::var("YM_WHISPER_THREADS").unwrap_or_else(|_| "4".into());
    let out = run_bounded(&whisper, &["-m", &model, "-f", wav.to_str().unwrap_or("a.wav"), "-np", "-t", &threads])?;
    let raw = String::from_utf8_lossy(&out.stdout);
    let segments = parse_whisper_segments(&raw);
    if segments.is_empty() {
        anyhow::bail!("the audio produced no words — it may be music, silence, or a language the local model does not cover");
    }
    Ok(segments)
}

/// SEE: sample frames as JPEGs across the window, ready for `VisionClient`. Returns (second, bytes).
///
/// EVEN INTERVALS, not scene detection — and that is the opposite of what I first built. The
/// reasoning for scene change was that a static shot would return the same picture N times; the
/// measurement said otherwise. Pointed at a real trading broadcast, scene detection returned ONE
/// frame from twenty seconds, because a trading desk's layout never changes even as every number
/// on it does. The information on that screen is precisely what a scene detector is built to
/// ignore. An even interval returns the whole window and gives each frame an exact timestamp.
pub fn keyframes(url: &str, want: usize, within_secs: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
    keyframes_at(url, want, within_secs, 0)
}

/// Sample frames starting `from_secs` into the recording.
pub fn keyframes_at(url: &str, want: usize, within_secs: u64, from_secs: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
    crate::ssrf_check_pub(url)?;
    if !have(&ffmpeg_bin()) {
        anyhow::bail!("ffmpeg is not installed on this host");
    }
    if !have(&ytdlp_bin()) {
        anyhow::bail!("yt-dlp is not installed on this host");
    }
    let want = want.clamp(1, 16);
    let within_secs = within_secs.max(1);
    let scratch = Scratch::new("frames")?;
    let pattern = scratch.path().join("f_%03d.jpg");
    let src = stream_url(url, false)?;
    // `fps=want/window` spaces the samples evenly across the window, so the count is what was
    // asked for and each frame's second is known rather than guessed.
    let vf = format!("fps={want}/{within_secs},scale=768:-1");
    let out = run_bounded(
        &ffmpeg_bin(),
        &[
            "-y",
            "-ss",
            &from_secs.to_string(),
            "-i",
            &src,
            "-t",
            &within_secs.to_string(),
            "-vf",
            &vf,
            "-frames:v",
            &want.to_string(),
            "-q:v",
            "4",
            pattern.to_str().unwrap_or("f_%03d.jpg"),
        ],
    )?;
    let mut frames: Vec<(u64, Vec<u8>)> = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(scratch.path())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "jpg").unwrap_or(false))
        .collect();
    entries.sort();
    // Evenly spaced by construction, so the second is exact: frame i sits at i·window/want.
    let step = within_secs / (want as u64).max(1);
    for (i, p) in entries.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(p) {
            frames.push((from_secs + step * i as u64, bytes));
        }
    }
    if frames.is_empty() {
        anyhow::bail!(
            "no frames could be sampled: {}",
            String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").trim()
        );
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn probe_of(dur: u64, live: bool, caps: bool) -> MediaProbe {
        MediaProbe { title: "t".into(), uploader: "u".into(), duration_secs: dur, is_live: live, has_captions: caps }
    }

    #[test]
    fn captions_beat_transcription_whenever_they_exist() {
        // Free, instant, and the publisher's own words — never spend CPU to re-derive them.
        assert_eq!(plan(&probe_of(600, false, true), 1800, 180), MediaPlan::Captions);
        // …even for something long, where transcription would be refused outright.
        assert_eq!(plan(&probe_of(28_800, false, true), 1800, 180), MediaPlan::Captions);
    }

    #[test]
    fn a_live_broadcast_is_sampled_never_transcribed_whole() {
        // The live case that started this: an 8-hour trading stream has no end to transcribe.
        let p = probe_of(0, true, false);
        assert_eq!(plan(&p, 1800, 180), MediaPlan::LiveWindow { secs: 180 });
        // The window can never exceed the hearing cap.
        assert_eq!(plan(&p, 60, 180), MediaPlan::LiveWindow { secs: 60 });
    }

    #[test]
    fn a_long_recording_is_partly_heard_not_refused() {
        // Refusing a 3-hour recording outright taught nothing when 30 minutes of it was
        // available the whole time. Hear the window; label it honestly.
        assert_eq!(
            plan(&probe_of(28_800, false, false), 1800, 180),
            MediaPlan::PartialListen { secs: 1800, of_secs: 28_800 }
        );
        assert_eq!(plan(&probe_of(900, false, false), 1800, 180), MediaPlan::Transcribe { secs: 900 });
    }

    #[test]
    fn probe_reads_the_shape_yt_dlp_actually_returns() {
        let v = serde_json::json!({
            "title": "MARKETS PAUSE",
            "channel": "TraderTV Live",
            "duration": 612.4,
            "is_live": false,
            "automatic_captions": {"en": [{"ext": "vtt"}]},
            "subtitles": {}
        });
        let p = probe_from_json(&v);
        assert_eq!(p.title, "MARKETS PAUSE");
        assert_eq!(p.uploader, "TraderTV Live");
        assert_eq!(p.duration_secs, 612);
        assert!(p.has_captions, "automatic captions count as captions");
        assert!(!p.is_live);
        // A live broadcast with no duration and no captions.
        let live = probe_from_json(&serde_json::json!({"title": "x", "is_live": true}));
        assert!(live.is_live && live.duration_secs == 0 && !live.has_captions);
    }

    /// The real TraderTV probe returned `subtitles: {live_chat: …}` and nothing else. A chat
    /// replay is not speech: counting it as captions would send a finished stream down the
    /// caption path to read an audience chat log instead of hearing anyone talk.
    #[test]
    fn a_live_chat_replay_is_not_a_transcript() {
        let v = serde_json::json!({
            "title": "MARKETS PAUSE | Stock Market Live",
            "channel": "TraderTV Live",
            "is_live": true,
            "duration": serde_json::Value::Null,
            "subtitles": {"live_chat": [{"ext": "json"}]},
            "automatic_captions": {}
        });
        let p = probe_from_json(&v);
        assert!(!p.has_captions, "live_chat must not count as captions");
        assert!(p.is_live && p.duration_secs == 0);
        // …so even once it ends, it is heard rather than mis-read as a transcript.
        let ended = probe_from_json(&serde_json::json!({
            "title": "t", "duration": 600.0, "subtitles": {"live_chat": []}, "automatic_captions": {}
        }));
        assert_eq!(plan(&ended, 1800, 180), MediaPlan::Transcribe { secs: 600 });
        // A real language track still counts.
        let real = probe_from_json(&serde_json::json!({
            "title": "t", "duration": 600.0, "subtitles": {"live_chat": [], "en": [{"ext":"vtt"}]}
        }));
        assert!(real.has_captions);
    }

    #[test]
    fn captions_become_readable_prose_with_anchors() {
        let vtt = "WEBVTT\nKind: captions\nLanguage: en\n\n\
                   00:00:01.000 --> 00:00:03.000\nthe first thing said\n\n\
                   00:00:03.000 --> 00:00:05.000\nthe first thing said\n\n\
                   00:01:07.500 --> 00:01:09.000\n<c>the second</c> thing said\n";
        let text = captions_to_text(vtt);
        assert!(text.contains("[00:01] the first thing said"), "keeps a coarse anchor: {text}");
        assert!(text.contains("[01:07] the second thing said"), "strips inline markup: {text}");
        // Rolling auto-captions repeat every line; a stutter is not a transcript.
        assert_eq!(text.matches("the first thing said").count(), 1, "collapses duplicates: {text}");
        assert!(!text.contains("WEBVTT") && !text.contains("Kind:"), "drops the header: {text}");
    }

    #[test]
    fn an_srt_style_cue_number_is_not_dialogue() {
        let srt = "1\n00:00:01,000 --> 00:00:02,000\nreal words\n";
        let text = captions_to_text(srt);
        assert!(text.contains("real words"));
        assert!(!text.starts_with('1'), "the cue number is not speech: {text}");
    }

    /// A tool that runs and dislikes the flag is still installed. `ffmpeg --version` exits 1 (it
    /// wants `-version`), and reading that as absence made the mind claim it had no ears while
    /// ffmpeg sat on the PATH.
    #[test]
    fn presence_is_spawnability_not_exit_status() {
        assert!(!have("ym-definitely-not-a-real-binary-9f3a"), "a missing binary is absent");
        // The check must not depend on the exit code: a spawnable tool counts even when the flag
        // is wrong for it. `cargo` runs these tests, so it is present by construction here.
        if Command::new("cargo").arg("-version").output().is_ok() {
            assert!(have("cargo"), "a spawnable binary is present regardless of how it exits");
        }
    }

    /// The clock is the whole point. This is whisper's real output, captured from 60s of the
    /// live trading stream — the voice-note path suppresses these stamps with `-nt`, and without
    /// them "what was on screen when they said that" cannot be answered at all.
    #[test]
    fn whisper_timestamps_become_utterances() {
        let raw = "\n[00:00:00.000 --> 00:00:04.360]   thought about that i mean like who does stuff like that man\n\
                   [00:00:04.360 --> 00:00:06.780]   reverted if you're contrarian\n\
                   [00:01:07.760 --> 00:01:11.240]   so many different ways to make money in this market\n";
        let u = parse_whisper_segments(raw);
        assert_eq!(u.len(), 3, "{u:?}");
        assert_eq!(u[0].at_secs, 0);
        assert_eq!(u[1].at_secs, 4, "seconds come from the START of the segment");
        assert_eq!(u[2].at_secs, 67, "hh:mm:ss carries minutes correctly");
        assert!(u[1].text.starts_with("reverted if you're contrarian"), "{:?}", u[1].text);
        // Flattening drops the clock but keeps every word, for callers that only want prose.
        let flat = utterances_to_text(&u);
        assert!(flat.contains("contrarian") && flat.contains("make money"), "{flat}");
    }

    #[test]
    fn non_segment_noise_is_ignored() {
        // whisper prints load/system lines around the segments; none of it is speech.
        let raw = "whisper_init_from_file: loading model\n[00:00:02.000 --> 00:00:03.000]   real words\nsystem_info: n_threads = 8\n";
        let u = parse_whisper_segments(raw);
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].text, "real words");
        assert!(parse_whisper_segments("no timestamps at all here").is_empty());
    }

    /// Three attempts at learning from a three-hour broadcast all read its opening, the third
    /// because seeking the AUDIO left the captions path untouched. Both channels must window.
    #[test]
    fn captions_can_be_windowed_to_mid_session() {
        let text = "[00:30] hello everyone welcome
[45:00] I am long CRWV here
[46:10] taking profit at 110
[178:00] see you tomorrow";
        let mid = captions_window(text, 2700, 600); // 45m in, 10m wide
        assert!(mid.contains("long CRWV"), "the mid-session content must survive: {mid}");
        assert!(mid.contains("taking profit"), "{mid}");
        assert!(!mid.contains("hello everyone"), "the opening must be excluded: {mid}");
        assert!(!mid.contains("see you tomorrow"), "the wind-down must be excluded: {mid}");
        // An empty window is empty, not the whole file.
        assert_eq!(captions_window(text, 10_000, 60), "");
    }

    #[test]
    fn the_offset_targets_the_middle_and_leaves_short_media_alone() {
        assert_eq!(sensible_offset(600, 1800), 0, "short enough that the whole thing is the middle");
        assert_eq!(sensible_offset(10_878, 1800), 3626, "a 181m show is sampled from ~60m in");
        // Never seeks past the point where the window would run off the end.
        assert!(sensible_offset(2000, 1800) + 1800 <= 2000);
    }

    #[test]
    fn caps_are_env_tunable_with_honest_defaults() {
        // Defaults exist so the tool is safe with no configuration at all.
        assert!(cap_secs() >= 60);
        assert!(live_window_secs() >= 30);
    }
}
