//! First run on the desktop: this mind has nowhere to think yet, so it asks.
//!
//! A freshly installed Yantrik OS machine starts this mind with no model configured. Before this
//! existed it attached to the desktop anyway, was listed in the mind picker, and answered every
//! question with a placeholder naming environment variables — to a person looking at a desktop,
//! with no terminal open and no idea what an environment variable is.
//!
//! So until a model is configured, the desktop channel is this conversation instead: it says what
//! is missing, asks where a model is, checks the answer against the real server, and saves it.
//!
//! # Why the OS does not do this
//!
//! Yantrik OS never holds a mind's model, endpoint or credentials — it offers a socket and nothing
//! else, so that those choices stay with whoever runs the mind. A setup screen in the OS would
//! break that on the first boot. This is the mind configuring itself, through the one channel it
//! already has, into its own file.
//!
//! # Why only a model on your own hardware, for now
//!
//! A cloud provider needs an API key, and a key typed here would not stay here: the desktop shows
//! the message in the chat panel, and its control surface reports the recent conversation to any
//! agent that asks — which on this machine includes other agents. A secret must not travel through
//! a transcript, so for a key this says where the file is instead of asking for it. An Ollama
//! address is not a secret, and a model on the person's own hardware is also the choice that keeps
//! private turns at home.
//!
//! # How it takes effect
//!
//! The model is chosen when the process starts, so after saving, a mind running under systemd
//! restarts itself (its unit restarts it) and attaches again with the model; the desktop picks it
//! back up. Anywhere else it says to restart it, rather than exiting under someone's terminal.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Ollama's default port.
const OLLAMA_PORT: u16 = 11434;
/// The most models listed at once; beyond this the person can type a name.
const LIST_MAX: usize = 12;
/// How long a model gets to answer the check. Shorter than the desktop's 90-second presence
/// window, since this mind is not polling while it waits.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(75);

/// What the harness does after sending a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    /// Wait for the next message.
    Wait,
    /// The model is saved: restart so it takes effect.
    Restart,
}

/// The two questions this asks an Ollama server. A trait so the conversation can be tested
/// without one.
pub trait Ollama {
    /// The models it has, by name.
    fn models(&self, url: &str) -> Result<Vec<String>, String>;
    /// Whether `model` actually answers a message.
    fn answers(&self, url: &str, model: &str) -> Result<(), String>;
}

#[derive(Debug, Clone, PartialEq)]
enum Step {
    /// Nothing known yet.
    Asking,
    /// An Ollama server was found; waiting for the person to pick a model.
    Choosing { url: String, models: Vec<String> },
    /// Saved; the restart is on its way.
    Saved { model: String },
}

/// The conversation's state. One per desktop session.
pub struct FirstRun<O: Ollama> {
    step: Step,
    env_path: PathBuf,
    /// Running under systemd, which will start this mind again when it exits.
    supervised: bool,
    ollama: O,
}

/// Where this mind's own settings live: `YM_ENV_PATH` when its unit says, otherwise the user's
/// config directory, since a desktop mind runs as the person rather than as a service account.
pub fn env_path() -> PathBuf {
    if let Some(p) = std::env::var_os("YM_ENV_PATH").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    config.join("yantrik-mind.env")
}

/// Whether systemd is supervising this process. systemd sets `INVOCATION_ID` for every unit it
/// starts, and nothing else does.
pub fn supervised() -> bool {
    std::env::var_os("INVOCATION_ID").is_some()
}

impl<O: Ollama> FirstRun<O> {
    pub fn new(ollama: O, env_path: PathBuf, supervised: bool) -> Self {
        Self { step: Step::Asking, env_path, supervised, ollama }
    }

    /// Answer one message. `say` receives the reply in parts — a line saying what is being checked
    /// goes out before a slow check, so the person is not left looking at nothing.
    pub fn respond(&mut self, text: &str, say: &mut dyn FnMut(&str)) -> Then {
        let text = text.trim();
        let lower = text.to_ascii_lowercase();

        if matches!(lower.as_str(), "start over" | "restart setup" | "reset" | "cancel") {
            self.step = Step::Asking;
            say(&intro(None));
            return Then::Wait;
        }

        if let Step::Saved { model } = &self.step {
            if self.supervised {
                say(&format!("I've saved {model} and I'm restarting to use it. Ask me again in a few seconds."));
                return Then::Restart;
            }
            say(&format!("I've saved {model}. Restart Yantrik Mind to start using it."));
            return Then::Wait;
        }

        // An address always wins: it is unambiguous, and it is how a person corrects a wrong one.
        if let Some(url) = parse_address(text) {
            return self.try_server(&url, say);
        }

        if let Step::Choosing { url, models } = self.step.clone() {
            return match pick(&models, text) {
                Some(model) => self.check_and_save(&url, &model, say),
                None => {
                    say(&format!(
                        "I don't see that one on {url}. Reply with a number from the list, or type \
                         the model's name exactly.\n\n{}",
                        numbered(&models)
                    ));
                    Then::Wait
                }
            };
        }

        if mentions_cloud(&lower) {
            say(&cloud_answer(&self.env_path));
            return Then::Wait;
        }

        // Nothing to go on. Look on this machine before asking — Ollama may already be here.
        let local = format!("http://127.0.0.1:{OLLAMA_PORT}");
        if let Ok(models) = self.ollama.models(&local) {
            let models = chat_models(models);
            if !models.is_empty() {
                return self.offer(&local, models, true, say);
            }
        }
        say(&intro((!is_greeting(&lower)).then_some(text)));
        Then::Wait
    }

    fn try_server(&mut self, url: &str, say: &mut dyn FnMut(&str)) -> Then {
        match self.ollama.models(url) {
            Ok(models) => {
                let models = chat_models(models);
                if models.is_empty() {
                    say(&format!(
                        "Ollama is running at {url}, but it has no chat models yet. Pull one on \
                         that machine — for example `ollama pull qwen3.5:9b` — then send me the \
                         address again."
                    ));
                    Then::Wait
                } else {
                    self.offer(url, models, false, say)
                }
            }
            Err(e) => {
                say(&format!(
                    "I couldn't reach Ollama at {url}: {e}\n\nCheck the address, that Ollama is \
                     running there, and — if it is another machine — that it accepts connections \
                     from the network (it listens only on its own machine unless started with \
                     OLLAMA_HOST=0.0.0.0)."
                ));
                Then::Wait
            }
        }
    }

    fn offer(&mut self, url: &str, models: Vec<String>, found_here: bool, say: &mut dyn FnMut(&str)) -> Then {
        if models.len() == 1 {
            let only = models[0].clone();
            if found_here {
                say(&format!("I found Ollama on this machine with one model, {only}."));
            }
            return self.check_and_save(url, &only, say);
        }
        let place = if found_here { "on this machine".to_string() } else { format!("at {url}") };
        say(&format!(
            "I found Ollama {place} with {} models. Which one should I think with? Reply with its \
             number or name.\n\n{}",
            models.len(),
            numbered(&models)
        ));
        self.step = Step::Choosing { url: url.to_string(), models };
        Then::Wait
    }

    fn check_and_save(&mut self, url: &str, model: &str, say: &mut dyn FnMut(&str)) -> Then {
        say(&format!("Checking that {model} answers — the first message can take a while if it has to load…"));
        if let Err(e) = self.ollama.answers(url, model) {
            say(&format!(
                "{model} didn't answer: {e}\n\nPick another model, or send a different address."
            ));
            return Then::Wait;
        }
        if let Err(e) = save_model(&self.env_path, url, model) {
            say(&format!(
                "{model} answered, but I couldn't save it to {}: {e}",
                self.env_path.display()
            ));
            return Then::Wait;
        }
        self.step = Step::Saved { model: model.to_string() };
        if self.supervised {
            say(&format!(
                "It answered. I'll think with {model} at {url} — saved to {}. Restarting to use \
                 it; ask me anything in a few seconds.",
                self.env_path.display()
            ));
            Then::Restart
        } else {
            say(&format!(
                "It answered. I'll think with {model} at {url} — saved to {}. Restart Yantrik \
                 Mind to start using it.",
                self.env_path.display()
            ));
            Then::Wait
        }
    }
}

fn intro(said: Option<&str>) -> String {
    let lead = match said {
        Some(s) if !s.is_empty() => "I can't answer that yet: I don't have a model to think with.",
        _ => "I don't have a model to think with yet.",
    };
    format!(
        "{lead}\n\nI think with a model running on your own hardware, through Ollama. Send me the \
         address of the machine running it — for example 192.168.1.20 or \
         http://ollama.local:11434 — and I'll find its models.\n\nNo Ollama yet? Install it from \
         ollama.com on any machine with a capable graphics card, pull a model, and send me that \
         machine's address."
    )
}

/// "hello" is not a question this mind failed to answer, so it should not be told "I can't answer
/// that yet" — seen on the first live run.
fn is_greeting(lower: &str) -> bool {
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    words.len() <= 3
        && words.first().is_some_and(|w| {
            ["hello", "hi", "hey", "hiya", "yo", "morning", "good", "greetings", "namaste", "howdy"].contains(w)
        })
}

fn cloud_answer(env_path: &Path) -> String {
    format!(
        "A cloud model needs an API key, and I won't ask for one here: this chat is shown on the \
         desktop and can be read back by other agents on this machine, and a key must not pass \
         through it.\n\nTo use one, add the key to {} — for example NANOGPT_KEY=…, \
         OLLAMA_CLOUD_KEY=… or MINIMAX_API_KEY=… — and restart Yantrik Mind. Or send me an Ollama \
         address to use a model on your own hardware.",
        env_path.display()
    )
}

fn mentions_cloud(lower: &str) -> bool {
    ["api key", "apikey", "cloud", "nanogpt", "nano-gpt", "minimax", "openai", "anthropic", "claude", "gemini", "key"]
        .iter()
        .any(|w| lower.split(|c: char| !c.is_ascii_alphanumeric() && c != '-').any(|t| t == *w) || (w.contains(' ') && lower.contains(w)))
}

fn numbered(models: &[String]) -> String {
    let mut out: Vec<String> = models
        .iter()
        .take(LIST_MAX)
        .enumerate()
        .map(|(i, m)| format!("{}. {m}", i + 1))
        .collect();
    if models.len() > LIST_MAX {
        out.push(format!("…and {} more — type a name to use one of those.", models.len() - LIST_MAX));
    }
    out.join("\n")
}

/// Drop models that cannot hold a conversation. Embedding models are listed by Ollama beside chat
/// models and fail the check anyway; offering them only wastes a slow round trip.
fn chat_models(models: Vec<String>) -> Vec<String> {
    models.into_iter().filter(|m| !m.to_ascii_lowercase().contains("embed")).collect()
}

/// A model chosen by its number in the list, its exact name, or a prefix only one name has.
fn pick(models: &[String], text: &str) -> Option<String> {
    let t = text.trim().trim_end_matches('.');
    if let Ok(n) = t.parse::<usize>() {
        return (1..=models.len()).contains(&n).then(|| models[n - 1].clone());
    }
    let lower = t.to_ascii_lowercase();
    if lower.is_empty() {
        return None;
    }
    if let Some(m) = models.iter().find(|m| m.to_ascii_lowercase() == lower) {
        return Some(m.clone());
    }
    let prefixed: Vec<&String> = models.iter().filter(|m| m.to_ascii_lowercase().starts_with(&lower)).collect();
    (prefixed.len() == 1).then(|| prefixed[0].clone())
}

/// The first thing in `text` that looks like a server address, as a base URL with a port and no
/// path: `192.168.1.20` → `http://192.168.1.20:11434`.
///
/// Deliberately narrow. A sentence is full of dotted words ("e.g.", "v2.5"), and treating one as
/// an address would send a request somewhere the person never named.
pub fn parse_address(text: &str) -> Option<String> {
    for raw in text.split_whitespace() {
        let token = raw.trim_matches(|c: char| matches!(c, ',' | ';' | '!' | '?' | '"' | '\'' | '(' | ')' | '<' | '>' | '`'));
        let token = token.trim_end_matches('.');
        let (scheme, rest) = if let Some(r) = token.strip_prefix("http://") {
            ("http", r)
        } else if let Some(r) = token.strip_prefix("https://") {
            ("https", r)
        } else {
            ("http", token)
        };
        let authority = rest.split('/').next().unwrap_or("");
        if authority.is_empty() || authority.contains('@') {
            continue;
        }
        let (host, port) = match authority.rsplit_once(':') {
            Some((h, p)) => match p.parse::<u16>() {
                Ok(p) if p > 0 => (h, Some(p)),
                _ => continue,
            },
            None => (authority, None),
        };
        let explicit_scheme = token.len() != rest.len();
        if !(is_ipv4(host) || host.eq_ignore_ascii_case("localhost") || is_hostname(host) || (explicit_scheme && is_label(host))) {
            continue;
        }
        let port = port.unwrap_or(if explicit_scheme && scheme == "https" { 443 } else { OLLAMA_PORT });
        return Some(format!("{scheme}://{host}:{port}"));
    }
    None
}

fn is_ipv4(host: &str) -> bool {
    let parts: Vec<&str> = host.split('.').collect();
    parts.len() == 4 && parts.iter().all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

fn is_label(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// A dotted hostname whose last label is a plausible top-level name (letters, two or more) — so
/// `ollama.local` and `gpu.example.com` count and `e.g` and `v2.5` do not.
fn is_hostname(host: &str) -> bool {
    let labels: Vec<&str> = host.split('.').collect();
    labels.len() >= 2
        && labels.iter().all(|l| is_label(l))
        && labels.last().is_some_and(|t| t.len() >= 2 && t.chars().all(|c| c.is_ascii_alphabetic()))
}

/// Record the model in the mind's own settings file, keeping everything else in it.
pub fn save_model(env_path: &Path, url: &str, model: &str) -> anyhow::Result<()> {
    if let Some(dir) = env_path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let existing = std::fs::read_to_string(env_path).unwrap_or_else(|_| {
        "# Yantrik Mind's own settings. Yantrik OS never reads this file.\n".to_string()
    });
    let mut body = crate::setup::upsert_env_line(&existing, "YM_LOCAL_OLLAMA_URL", url);
    body = crate::setup::upsert_env_line(&body, "YM_LOCAL_OLLAMA_MODEL", model);
    crate::setup::write_env_600(&env_path.display().to_string(), &body)
}

/// The real Ollama, over HTTP.
pub struct HttpOllama;

impl Ollama for HttpOllama {
    fn models(&self, url: &str) -> Result<Vec<String>, String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(4))
            .timeout(Duration::from_secs(10))
            .build();
        let reply: serde_json::Value = agent
            .get(&format!("{url}/api/tags"))
            .call()
            .map_err(describe_http_error)?
            .into_json()
            .map_err(|e| format!("that answered, but not like Ollama ({e})"))?;
        let models = reply["models"]
            .as_array()
            .ok_or_else(|| "that answered, but not like Ollama (no model list)".to_string())?;
        Ok(models
            .iter()
            // Embedding models cannot hold a conversation, and most are not named for it — the
            // first live run listed bge-m3 among the choices. Their architecture family is the
            // reliable tell: BERT-derived.
            .filter(|m| {
                !m["details"]["family"].as_str().unwrap_or_default().to_ascii_lowercase().contains("bert")
            })
            .filter_map(|m| m["name"].as_str().or_else(|| m["model"].as_str()).map(str::to_string))
            .collect())
    }

    fn answers(&self, url: &str, model: &str) -> Result<(), String> {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(4))
            .timeout(ANSWER_TIMEOUT)
            .build();
        // `think: false` because that is how this mind will call it, and a thinking preamble would
        // turn a one-word check into a minute of reasoning.
        let reply: serde_json::Value = agent
            .post(&format!("{url}/api/chat"))
            .send_json(serde_json::json!({
                "model": model,
                "messages": [{ "role": "user", "content": "Reply with the single word: ready" }],
                "stream": false,
                "think": false,
                "options": { "num_predict": 16 },
            }))
            .map_err(describe_http_error)?
            .into_json()
            .map_err(|e| format!("its reply could not be read ({e})"))?;
        if let Some(err) = reply["error"].as_str() {
            return Err(err.to_string());
        }
        let content = reply["message"]["content"].as_str().unwrap_or("");
        if content.trim().is_empty() && reply["done"].as_bool() != Some(true) {
            return Err("it replied with nothing".to_string());
        }
        Ok(())
    }
}

fn describe_http_error(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            let detail = serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or_else(|| body.chars().take(200).collect());
            format!("it answered HTTP {code}{}", if detail.is_empty() { String::new() } else { format!(" — {detail}") })
        }
        ureq::Error::Transport(t) => t.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashMap;

    #[derive(Default)]
    struct FakeOllama {
        servers: HashMap<String, Vec<String>>,
        broken: Vec<String>,
        asked: RefCell<Vec<String>>,
    }

    impl FakeOllama {
        fn with(mut self, url: &str, models: &[&str]) -> Self {
            self.servers.insert(url.to_string(), models.iter().map(|m| m.to_string()).collect());
            self
        }
    }

    impl Ollama for FakeOllama {
        fn models(&self, url: &str) -> Result<Vec<String>, String> {
            self.asked.borrow_mut().push(url.to_string());
            self.servers.get(url).cloned().ok_or_else(|| "connection refused".to_string())
        }
        fn answers(&self, _url: &str, model: &str) -> Result<(), String> {
            if self.broken.iter().any(|b| b == model) {
                Err("model failed to load".to_string())
            } else {
                Ok(())
            }
        }
    }

    fn run(fr: &mut FirstRun<FakeOllama>, text: &str) -> (String, Then) {
        let mut parts = Vec::new();
        let then = fr.respond(text, &mut |s: &str| parts.push(s.to_string()));
        (parts.join("\n\n"), then)
    }

    /// A directory that is removed when the test ends.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "first-run-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn setup(ollama: FakeOllama, supervised: bool) -> (FirstRun<FakeOllama>, TempDir) {
        let dir = TempDir::new();
        let env = dir.path().join("config").join("yantrik-mind.env");
        (FirstRun::new(ollama, env, supervised), dir)
    }

    #[test]
    fn addresses_are_recognised_and_sentences_are_not() {
        assert_eq!(parse_address("192.168.4.35").as_deref(), Some("http://192.168.4.35:11434"));
        assert_eq!(parse_address("it's on 192.168.4.35:8080, thanks").as_deref(), Some("http://192.168.4.35:8080"));
        assert_eq!(parse_address("http://gpu-box:11434/api/tags").as_deref(), Some("http://gpu-box:11434"));
        assert_eq!(parse_address("https://ollama.example.com").as_deref(), Some("https://ollama.example.com:443"));
        assert_eq!(parse_address("try ollama.local.").as_deref(), Some("http://ollama.local:11434"));
        assert_eq!(parse_address("localhost").as_deref(), Some("http://localhost:11434"));
        for sentence in ["hello there", "e.g. something", "use v2.5 please", "what is 3.14", "999.1.1.1", "me@host.com"] {
            assert_eq!(parse_address(sentence), None, "{sentence}");
        }
    }

    #[test]
    fn a_first_hello_explains_what_is_missing_and_asks_for_an_address() {
        let (mut fr, _d) = setup(FakeOllama::default(), true);
        let (said, then) = run(&mut fr, "hello");
        assert_eq!(then, Then::Wait);
        assert!(said.starts_with("I don't have a model to think with yet."), "{said}");
        assert!(!said.contains("can't answer that"), "a greeting is not an unanswered question: {said}");
        assert!(said.contains("address"), "{said}");
    }

    #[test]
    fn a_real_question_is_told_it_cannot_be_answered_yet() {
        let (mut fr, _d) = setup(FakeOllama::default(), true);
        let (said, _) = run(&mut fr, "what is the weather tomorrow");
        assert!(said.starts_with("I can't answer that yet"), "{said}");
    }

    /// Ollama on this machine is found without being asked about.
    #[test]
    fn ollama_on_this_machine_is_offered_before_asking() {
        let ollama = FakeOllama::default().with("http://127.0.0.1:11434", &["llama3.2:3b", "qwen3.5:9b", "nomic-embed-text"]);
        let (mut fr, _d) = setup(ollama, true);
        let (said, _) = run(&mut fr, "hi");
        assert!(said.contains("on this machine with 2 models"), "{said}");
        assert!(said.contains("1. llama3.2:3b") && said.contains("2. qwen3.5:9b"), "{said}");
        assert!(!said.contains("embed"), "embedding models are not offered: {said}");
    }

    #[test]
    fn choosing_a_model_checks_it_saves_it_privately_and_restarts() {
        let ollama = FakeOllama::default().with("http://192.168.4.35:11434", &["gemma3:12b", "qwen3.5:9b-8k"]);
        let (mut fr, dir) = setup(ollama, true);
        let (said, then) = run(&mut fr, "192.168.4.35");
        assert_eq!(then, Then::Wait);
        assert!(said.contains("at http://192.168.4.35:11434 with 2 models"), "{said}");

        let (said, then) = run(&mut fr, "2");
        assert_eq!(then, Then::Restart, "{said}");
        assert!(said.contains("Checking that qwen3.5:9b-8k answers"), "{said}");
        assert!(said.contains("Restarting"), "{said}");

        let env = std::fs::read_to_string(dir.path().join("config/yantrik-mind.env")).unwrap();
        assert!(env.contains("YM_LOCAL_OLLAMA_URL=http://192.168.4.35:11434\n"), "{env}");
        assert!(env.contains("YM_LOCAL_OLLAMA_MODEL=qwen3.5:9b-8k\n"), "{env}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join("config/yantrik-mind.env")).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn saving_keeps_what_was_already_in_the_file() {
        let dir = TempDir::new();
        let env = dir.path().join("yantrik-mind.env");
        std::fs::write(&env, "YM_DB=/x/mind.db\nYM_LOCAL_OLLAMA_MODEL=old\n").unwrap();
        save_model(&env, "http://10.0.0.2:11434", "new").unwrap();
        let body = std::fs::read_to_string(&env).unwrap();
        assert_eq!(body, "YM_DB=/x/mind.db\nYM_LOCAL_OLLAMA_MODEL=new\nYM_LOCAL_OLLAMA_URL=http://10.0.0.2:11434\n");
    }

    #[test]
    fn a_model_that_does_not_answer_is_not_saved() {
        let mut ollama = FakeOllama::default().with("http://10.0.0.2:11434", &["big:70b", "small:3b"]);
        ollama.broken.push("big:70b".into());
        let (mut fr, dir) = setup(ollama, true);
        run(&mut fr, "10.0.0.2");
        let (said, then) = run(&mut fr, "big");
        assert_eq!(then, Then::Wait);
        assert!(said.contains("didn't answer: model failed to load"), "{said}");
        assert!(!dir.path().join("config/yantrik-mind.env").exists());
        let (_, then) = run(&mut fr, "small:3b");
        assert_eq!(then, Then::Restart);
    }

    #[test]
    fn an_unreachable_address_says_why_and_what_to_check() {
        let (mut fr, _d) = setup(FakeOllama::default(), true);
        let (said, then) = run(&mut fr, "10.9.9.9");
        assert_eq!(then, Then::Wait);
        assert!(said.contains("couldn't reach Ollama at http://10.9.9.9:11434: connection refused"), "{said}");
        assert!(said.contains("OLLAMA_HOST=0.0.0.0"), "{said}");
    }

    #[test]
    fn a_server_with_one_model_needs_no_choice() {
        let ollama = FakeOllama::default().with("http://10.0.0.2:11434", &["qwen3.5:9b"]);
        let (mut fr, _d) = setup(ollama, true);
        let (said, then) = run(&mut fr, "10.0.0.2");
        assert_eq!(then, Then::Restart, "{said}");
    }

    #[test]
    fn an_unrecognised_choice_shows_the_list_again() {
        let ollama = FakeOllama::default().with("http://10.0.0.2:11434", &["a:1b", "b:1b"]);
        let (mut fr, _d) = setup(ollama, true);
        run(&mut fr, "10.0.0.2");
        let (said, then) = run(&mut fr, "7");
        assert_eq!(then, Then::Wait);
        assert!(said.contains("I don't see that one") && said.contains("1. a:1b"), "{said}");
    }

    /// Keys must not travel through the transcript, so none is asked for.
    #[test]
    fn a_cloud_request_points_at_the_file_instead_of_asking_for_a_key() {
        let (mut fr, _d) = setup(FakeOllama::default(), true);
        let (said, then) = run(&mut fr, "I want to use my NanoGPT api key");
        assert_eq!(then, Then::Wait);
        assert!(said.contains("won't ask for one here"), "{said}");
        assert!(said.contains("yantrik-mind.env"), "{said}");
    }

    #[test]
    fn without_systemd_it_says_to_restart_rather_than_exiting() {
        let ollama = FakeOllama::default().with("http://10.0.0.2:11434", &["qwen3.5:9b"]);
        let (mut fr, _d) = setup(ollama, false);
        let (said, then) = run(&mut fr, "10.0.0.2");
        assert_eq!(then, Then::Wait);
        assert!(said.contains("Restart Yantrik Mind"), "{said}");
    }

    #[test]
    fn start_over_forgets_the_server() {
        let ollama = FakeOllama::default().with("http://10.0.0.2:11434", &["a:1b", "b:1b"]);
        let (mut fr, _d) = setup(ollama, true);
        run(&mut fr, "10.0.0.2");
        let (said, _) = run(&mut fr, "start over");
        assert!(said.contains("address"), "{said}");
        let (said, _) = run(&mut fr, "1");
        assert!(!said.contains("Checking"), "a bare number means nothing once the list is gone: {said}");
    }

    #[test]
    fn the_settings_file_is_the_units_or_the_users_config() {
        // Only the fallback shape is checked; the environment is process-global and shared by tests.
        let p = env_path();
        assert!(p.ends_with("yantrik-mind.env"), "{}", p.display());
    }
}
