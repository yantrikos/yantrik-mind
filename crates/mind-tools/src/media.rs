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
    /// Longer than the box can hear without a GPU. Refused with the numbers, not attempted.
    TooLong { duration_secs: u64, cap_secs: u64 },
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
        return MediaPlan::TooLong { duration_secs: probe.duration_secs, cap_secs };
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

/// Is an external binary actually present? (`Command` failing to spawn is the check — asking the
/// tool for its version is cheap and unambiguous.)
pub fn have(bin: &str) -> bool {
    Command::new(bin).arg("--version").output().map(|o| o.status.success()).unwrap_or(false)
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

/// Parse yt-dlp's metadata JSON. Split out so the shape can be tested without the binary.
pub fn probe_from_json(v: &serde_json::Value) -> MediaProbe {
    let subs = v.get("subtitles").and_then(|s| s.as_object()).map(|m| !m.is_empty()).unwrap_or(false);
    let auto = v.get("automatic_captions").and_then(|s| s.as_object()).map(|m| !m.is_empty()).unwrap_or(false);
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

/// HEAR: pull `secs` of audio and run the LOCAL whisper over it. Nothing leaves the house — the
/// same whisper that already handles voice notes, pointed at a different source.
pub fn transcribe(url: &str, secs: u64) -> anyhow::Result<String> {
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
        &["-y", "-i", &src, "-t", &secs.to_string(), "-vn", "-ar", "16000", "-ac", "1", wav.to_str().unwrap_or("a.wav")],
    )?;
    if !wav.exists() {
        anyhow::bail!("could not extract audio: {}", String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("").trim());
    }
    let out = run_bounded(&whisper, &["-m", &model, "-f", wav.to_str().unwrap_or("a.wav"), "-nt", "-np"])?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        anyhow::bail!("the audio produced no words — it may be music, silence, or a language the local model does not cover");
    }
    Ok(text)
}

/// SEE: sample scene-change frames as JPEGs, ready for `VisionClient`. Returns (approx_second, bytes).
///
/// Scene detection rather than a fixed interval: on material that holds one shot for minutes (a
/// chart, a slide, a trading desk) a fixed sample returns the same picture N times, while scene
/// change returns the moments something actually happened.
pub fn keyframes(url: &str, want: usize, within_secs: u64) -> anyhow::Result<Vec<(u64, Vec<u8>)>> {
    crate::ssrf_check_pub(url)?;
    if !have(&ffmpeg_bin()) {
        anyhow::bail!("ffmpeg is not installed on this host");
    }
    if !have(&ytdlp_bin()) {
        anyhow::bail!("yt-dlp is not installed on this host");
    }
    let want = want.clamp(1, 16);
    let scratch = Scratch::new("frames")?;
    let pattern = scratch.path().join("f_%03d.jpg");
    let src = stream_url(url, false)?;
    let out = run_bounded(
        &ffmpeg_bin(),
        &[
            "-y",
            "-i",
            &src,
            "-t",
            &within_secs.to_string(),
            "-vf",
            "select='gt(scene,0.3)',scale=768:-1",
            "-vsync",
            "vfr",
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
    let n = entries.len().max(1) as u64;
    for (i, p) in entries.iter().enumerate() {
        if let Ok(bytes) = std::fs::read(p) {
            // Even spacing is an approximation; scene frames carry no timestamp of their own.
            frames.push((within_secs * (i as u64) / n, bytes));
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
    fn too_long_is_refused_with_the_numbers_not_attempted() {
        // The box has no GPU: an 8-hour job is impossible, not slow. Say so before starting.
        assert_eq!(
            plan(&probe_of(28_800, false, false), 1800, 180),
            MediaPlan::TooLong { duration_secs: 28_800, cap_secs: 1800 }
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

    #[test]
    fn caps_are_env_tunable_with_honest_defaults() {
        // Defaults exist so the tool is safe with no configuration at all.
        assert!(cap_secs() >= 60);
        assert!(live_window_secs() >= 30);
    }
}
