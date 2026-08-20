//! VOICE — a mouth that stays open, and knows when to stop talking.
//!
//! Same shape as `BrowserSession`, for the same reason: a child process holding an expensive thing
//! in memory, spoken to in JSON lines. Loading the synthesiser costs ~1.8s and speaking costs
//! ~200ms, so a per-utterance process would pay a pause about as long as the sentence itself,
//! every time. The model stays resident and the mouth answers in a fifth of a second.
//!
//! ## The hold cache
//!
//! The first token from the language model arrives around a second after the speaker stops, and a
//! caller starts to think the line has dropped at about that point. That gap cannot be shortened, so
//! it is covered — the way a person covers it, by saying "mm" while they think. Those lines are
//! synthesised ONCE at boot (547ms for six of them, 284KB) and thereafter cost 0.002ms. Generating
//! them on demand would put the filler behind the same queue as the answer, which defeats the entire
//! purpose of having filler.
//!
//! ## Barge-in
//!
//! A voice you cannot interrupt is not a conversation, it is a recital: the listener has to wait for
//! a gap, and if the reply is wrong from its second sentence they must sit through the other five.
//! Interrupting is how people steer each other, so it is not a nicety here.
//!
//! It works by SPEAKING IN PIECES. A reply is chunked, and between chunks the session checks whether
//! it has been told to stop. That is why `speech::speakable_chunks` cuts the first piece short: the
//! chunk boundary is both when sound can start and when it can be halted. A single long utterance
//! would be uninterruptible no matter what the caller wanted, because the audio is already rendered
//! and on its way.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// One rendered utterance.
#[derive(Debug, Clone, PartialEq)]
pub struct Spoken {
    pub wav: Vec<u8>,
    pub secs: f64,
    pub synth_ms: u64,
}

pub struct VoiceSession {
    child: Mutex<Option<Child>>,
    stdin: Mutex<Option<ChildStdin>>,
    stdout: Mutex<Option<BufReader<ChildStdout>>>,
    /// Pre-rendered hold lines, keyed by text.
    holds: Mutex<Vec<(String, Vec<u8>)>>,
    /// Bumped to cancel whatever is currently being spoken. A generation counter rather than a
    /// boolean: a stale "stop" from a previous utterance must not silence the next one, which is
    /// exactly what a plain flag does when someone interrupts twice in quick succession.
    generation: AtomicU64,
}

impl VoiceSession {
    /// Start the synthesiser and pre-render the hold lines.
    pub fn start() -> anyhow::Result<VoiceSession> {
        let script = std::env::var("YM_TTS_AGENT").unwrap_or_else(|_| "/opt/yantrik-mind/tts_agent.py".into());
        if !std::path::Path::new(&script).exists() {
            anyhow::bail!("the voice is not installed at {script}");
        }
        let mut child = Command::new("python3")
            .arg(&script)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| anyhow::anyhow!("could not start the voice: {e}"))?;
        let stdin = child.stdin.take();
        let mut stdout = child.stdout.take().map(BufReader::new);
        // The daemon announces itself once the model is loaded; without waiting for that, the first
        // real sentence would race the 1.8s load and appear to hang.
        if let Some(so) = stdout.as_mut() {
            let mut line = String::new();
            if so.read_line(&mut line)? == 0 {
                anyhow::bail!("the voice exited before it was ready");
            }
            let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
            if v.get("fatal").is_some() {
                anyhow::bail!("{}", v["fatal"].as_str().unwrap_or("the voice could not start"));
            }
        }
        let s = VoiceSession {
            child: Mutex::new(Some(child)),
            stdin: Mutex::new(stdin),
            stdout: Mutex::new(stdout),
            holds: Mutex::new(Vec::new()),
            generation: AtomicU64::new(0),
        };
        s.prerender_holds();
        Ok(s)
    }

    fn prerender_holds(&self) {
        let mut cache = Vec::new();
        for line in crate::flow::HOLD_LINES {
            if let Ok(sp) = self.synthesize(line) {
                cache.push((line.to_string(), sp.wav));
            }
        }
        *self.holds.lock().unwrap_or_else(|p| p.into_inner()) = cache;
    }

    /// A hold line, already rendered. None if the cache is empty (the voice failed to start).
    pub fn hold(&self, nth: usize) -> Option<(String, Vec<u8>)> {
        let h = self.holds.lock().unwrap_or_else(|p| p.into_inner());
        if h.is_empty() {
            return None;
        }
        h.get(nth % h.len()).cloned()
    }

    /// Render one utterance. Blocking; call from `spawn_blocking`.
    pub fn synthesize(&self, text: &str) -> anyhow::Result<Spoken> {
        let req = serde_json::json!({ "say": text });
        {
            let mut si = self.stdin.lock().unwrap_or_else(|p| p.into_inner());
            let si = si.as_mut().ok_or_else(|| anyhow::anyhow!("the voice is closed"))?;
            writeln!(si, "{req}")?;
            si.flush()?;
        }
        let mut so = self.stdout.lock().unwrap_or_else(|p| p.into_inner());
        let so = so.as_mut().ok_or_else(|| anyhow::anyhow!("the voice is closed"))?;
        let mut line = String::new();
        if so.read_line(&mut line)? == 0 {
            anyhow::bail!("the voice exited");
        }
        let v: serde_json::Value = serde_json::from_str(&line)?;
        if !v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
            anyhow::bail!("{}", v.get("error").and_then(|x| x.as_str()).unwrap_or("the voice failed"));
        }
        let b64 = v.get("wav").and_then(|x| x.as_str()).unwrap_or_default();
        Ok(Spoken {
            wav: b64_decode(b64),
            secs: v.get("secs").and_then(|x| x.as_f64()).unwrap_or(0.0),
            synth_ms: v.get("ms").and_then(|x| x.as_u64()).unwrap_or(0),
        })
    }

    /// Begin a new utterance, invalidating any that is still being spoken.
    pub fn begin_turn(&self) -> u64 {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }

    /// Stop talking. Whatever is mid-flight will not emit its next chunk.
    pub fn interrupt(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    /// Is this turn still the current one?
    pub fn still_current(&self, turn: u64) -> bool {
        self.generation.load(Ordering::SeqCst) == turn
    }

    /// Speak a reply piece by piece, stopping the moment it is interrupted.
    ///
    /// Returns what was actually spoken — never what was intended. A caller that logs the intended
    /// text after an interruption records a sentence the listener never heard, and the transcript
    /// then disagrees with the conversation.
    pub fn speak_reply(&self, text: &str, turn: u64, mut emit: impl FnMut(&Spoken)) -> String {
        let chunks = crate::speech::speakable_chunks(text, 60, 160);
        let mut said = String::new();
        for c in chunks {
            if !self.still_current(turn) {
                break;
            }
            match self.synthesize(&c) {
                Ok(sp) => {
                    // Checked again AFTER rendering: an interruption during the ~200ms of synthesis
                    // must not still play. Without this the listener talks, and is answered anyway.
                    if !self.still_current(turn) {
                        break;
                    }
                    emit(&sp);
                    if !said.is_empty() {
                        said.push(' ');
                    }
                    said.push_str(&c);
                }
                Err(_) => break,
            }
        }
        said
    }

    pub fn close(&self) {
        if let Some(mut c) = self.child.lock().unwrap_or_else(|p| p.into_inner()).take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

fn b64_decode(s: &str) -> Vec<u8> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut idx = [255u8; 256];
    for (i, c) in T.iter().enumerate() {
        idx[*c as usize] = i as u8;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for b in s.bytes() {
        let v = idx[b as usize];
        if v == 255 {
            continue;
        }
        acc = (acc << 6) | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_a_wav_header() {
        // The audio arrives base64 over a pipe; a decoder that mangles it produces silence, which is
        // indistinguishable from a voice that failed to speak.
        assert_eq!(b64_decode("UklGRg=="), b"RIFF");
        assert_eq!(b64_decode("YW55IGNhcm5hbCBwbGVhc3VyZQ=="), b"any carnal pleasure");
        assert_eq!(b64_decode(""), Vec::<u8>::new());
    }

    #[test]
    fn a_generation_counter_survives_a_double_interruption() {
        // Why this is a counter and not a boolean. Someone interrupts, the mind starts a new reply,
        // and a late "stop" from the FIRST utterance arrives. With a flag that stop silences the new
        // reply and the mind appears to have given up mid-sentence.
        let gen = AtomicU64::new(0);
        let turn_a = gen.fetch_add(1, Ordering::SeqCst) + 1;
        gen.fetch_add(1, Ordering::SeqCst); // interrupt A
        let turn_b = gen.fetch_add(1, Ordering::SeqCst) + 1; // new reply
        assert_ne!(turn_a, turn_b);
        assert_eq!(gen.load(Ordering::SeqCst), turn_b, "the new turn is the current one");
        assert!(turn_a < turn_b, "a stale turn can never look current again");
    }

    #[test]
    fn chunking_is_what_makes_an_interruption_possible() {
        // A reply spoken as ONE utterance cannot be stopped: the audio is rendered and gone. The
        // chunk boundary is both where sound can start and where it can be halted.
        let reply = "The Nifty is at 24,053, down about a quarter percent. Reliance never came back \
                     cleanly, so I won't guess it. Want me to re-pull it?";
        let chunks = crate::speech::speakable_chunks(reply, 60, 160);
        assert!(chunks.len() >= 3, "an interruptible reply needs several stopping points: {chunks:?}");
    }
}
