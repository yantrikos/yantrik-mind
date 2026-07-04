//! mail — the mind's email capability. READ is here (inbox triage/digest, safe like browsing);
//! its output is untrusted (email bodies are an injection surface, wrapped by the caller). SEND is
//! an outward effect and is deliberately NOT here — it must ride the harm-gate + confirmation, so it
//! lands once `mind-governance`'s real gate exists.
//!
//! `MailClient` is the injectable seam (real IMAP vs scripted-for-tests). The real transport is
//! blocking `imap`, run on the blocking pool so it never stalls the async runtime.

use async_trait::async_trait;
use std::sync::Mutex;

/// One inbox message, reduced to what a digest needs (headers only — no body fetch in v1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmailMsg {
    pub id: String,
    pub from: String,
    pub subject: String,
    pub date: String,
}

#[async_trait]
pub trait MailClient: Send + Sync {
    /// Most-recent `limit` messages from the inbox (newest first).
    async fn inbox(&self, limit: usize) -> anyhow::Result<Vec<EmailMsg>>;

    /// Body text of specific messages by sequence id — BODY.PEEK, so nothing is ever marked read.
    /// Returns (id, cleaned text). Default: unsupported → empty.
    /// Full-mailbox search (subject/body TEXT), newest first, with cleaned body snippets.
    /// Default: unsupported (scripted clients return empty).
    async fn search(&self, needle: &str, limit: usize) -> anyhow::Result<Vec<(EmailMsg, String)>> {
        let _ = (needle, limit);
        Ok(vec![])
    }

    async fn peek_bodies(&self, ids: &[String], max_chars: usize) -> anyhow::Result<Vec<(String, String)>> {
        let _ = (ids, max_chars);
        Ok(Vec::new())
    }
}

/// Strip an email body to readable text: drop MIME scaffolding, base64 payloads, HTML tags,
/// quoted-printable soft breaks — bounded to `max_chars`.
fn clean_body(raw: &[u8], max_chars: usize) -> String {
    let s = String::from_utf8_lossy(raw);
    let mut out = String::new();
    let mut in_tag = false;
    for line in s.lines() {
        let t = line.trim_end_matches('\r').trim();
        if t.len() > 100 && t.chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '=') {
            continue; // base64 payload line
        }
        if t.starts_with("Content-") || t.starts_with("--") || t.starts_with("MIME-") || t.starts_with("charset") {
            continue;
        }
        let mut cleaned = String::new();
        for c in t.chars() {
            match c {
                '<' => in_tag = true,
                '>' => in_tag = false,
                c if !in_tag => cleaned.push(c),
                _ => {}
            }
        }
        let cleaned = cleaned
            .replace("=20", " ")
            .replace("=E2=80=99", "'")
            .replace("&nbsp;", " ")
            .replace("&amp;", "&")
            .replace("&#39;", "'");
        let cleaned = cleaned.trim();
        if cleaned.len() > 1 {
            out.push_str(cleaned);
            out.push('\n');
        }
        if out.len() >= max_chars {
            break;
        }
    }
    out.chars().take(max_chars).collect()
}

/// Render an inbox as a compact, untrusted digest block for grounding a reply.
pub fn render_inbox_digest(msgs: &[EmailMsg]) -> String {
    if msgs.is_empty() {
        return "Inbox is empty (no recent messages).".to_string();
    }
    let mut s = format!("{} recent message(s):\n", msgs.len());
    for m in msgs {
        let subj = if m.subject.trim().is_empty() { "(no subject)" } else { m.subject.trim() };
        s.push_str(&format!("- from {} — {} [{}]\n", m.from, subj, m.date));
    }
    s
}

/// Deterministic mail client for tests/evals.
pub struct ScriptedMailClient {
    pub msgs: Vec<EmailMsg>,
}

impl ScriptedMailClient {
    pub fn new(msgs: Vec<EmailMsg>) -> Self {
        Self { msgs }
    }
}

#[async_trait]
impl MailClient for ScriptedMailClient {
    async fn inbox(&self, limit: usize) -> anyhow::Result<Vec<EmailMsg>> {
        Ok(self.msgs.iter().take(limit).cloned().collect())
    }
}

/// Real IMAP read client (TLS). Built ready-to-use; live auth needs a working IMAP password
/// (Gmail requires a 16-char App Password — a regular account password is rejected).
pub struct ImapClient {
    host: String,
    port: u16,
    user: String,
    password: String,
}

impl ImapClient {
    pub fn new(host: impl Into<String>, port: u16, user: impl Into<String>, password: impl Into<String>) -> Self {
        Self { host: host.into(), port, user: user.into(), password: password.into() }
    }

    /// Convenience for common providers by the account address.
    pub fn for_address(addr: &str, password: impl Into<String>) -> Option<Self> {
        let host = if addr.ends_with("@gmail.com") {
            "imap.gmail.com"
        } else {
            // Best-effort: mail.<domain>. Override with ImapClient::new for non-standard hosts.
            return addr.split('@').nth(1).map(|d| Self::new(format!("mail.{d}"), 993, addr, password));
        };
        Some(Self::new(host, 993, addr, password))
    }
}

#[async_trait]
impl MailClient for ImapClient {
    async fn inbox(&self, limit: usize) -> anyhow::Result<Vec<EmailMsg>> {
        let (host, port, user, password) =
            (self.host.clone(), self.port, self.user.clone(), self.password.clone());
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<EmailMsg>> {
            let tls = native_tls::TlsConnector::builder().build()?;
            let client = imap::connect((host.as_str(), port), host.as_str(), &tls)?;
            let mut session = client.login(&user, &password).map_err(|(e, _)| e)?;
            let mailbox = session.select("INBOX")?;
            let total = mailbox.exists;
            if total == 0 {
                let _ = session.logout();
                return Ok(vec![]);
            }
            let start = total.saturating_sub(limit as u32 - 1).max(1);
            let set = format!("{start}:{total}");
            let fetches = session.fetch(set, "(ENVELOPE INTERNALDATE)")?;
            let mut out = Vec::new();
            for f in fetches.iter() {
                let env = match f.envelope() {
                    Some(e) => e,
                    None => continue,
                };
                let decode = |b: &[u8]| String::from_utf8_lossy(b).to_string();
                let subject = env.subject.as_ref().map(|s| decode(s)).unwrap_or_default();
                let from = env
                    .from
                    .as_ref()
                    .and_then(|addrs| addrs.first())
                    .map(|a| {
                        let mbox = a.mailbox.as_ref().map(|b| decode(b)).unwrap_or_default();
                        let host = a.host.as_ref().map(|b| decode(b)).unwrap_or_default();
                        if a.name.is_some() {
                            decode(a.name.as_ref().unwrap())
                        } else {
                            format!("{mbox}@{host}")
                        }
                    })
                    .unwrap_or_else(|| "(unknown)".into());
                let date = env.date.as_ref().map(|b| decode(b)).unwrap_or_default();
                out.push(EmailMsg { id: f.message.to_string(), from, subject, date });
            }
            let _ = session.logout();
            out.reverse(); // newest first
            Ok(out)
        })
        .await?
    }

    async fn search(&self, needle: &str, limit: usize) -> anyhow::Result<Vec<(EmailMsg, String)>> {
        let (host, port, user, password) =
            (self.host.clone(), self.port, self.user.clone(), self.password.clone());
        let needle: String = needle.chars().filter(|c| *c != '"' && *c != '\\').collect();
        let limit = limit.clamp(1, 10);
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(EmailMsg, String)>> {
            let tls = native_tls::TlsConnector::builder().build()?;
            let client = imap::connect((host.as_str(), port), host.as_str(), &tls)?;
            let mut session = client.login(&user, &password).map_err(|(e, _)| e)?;
            // Gmail's All Mail covers INBOX + archive in one place; plain hosts get INBOX + Archive.
            let mailboxes: Vec<&str> = if host.contains("gmail") {
                vec!["[Gmail]/All Mail", "INBOX"]
            } else {
                vec!["INBOX", "Archive"]
            };
            let mut out: Vec<(EmailMsg, String)> = Vec::new();
            for mb in mailboxes {
                if session.select(mb).is_err() {
                    continue;
                }
                let Ok(ids) = session.search(format!("TEXT \"{needle}\"")) else { continue };
                let mut idv: Vec<u32> = ids.into_iter().collect();
                idv.sort_unstable();
                let take: Vec<u32> = idv.into_iter().rev().take(limit).collect();
                if take.is_empty() {
                    continue;
                }
                let set = take.iter().map(u32::to_string).collect::<Vec<_>>().join(",");
                let fetches = session.fetch(&set, "(ENVELOPE INTERNALDATE BODY.PEEK[TEXT])")?;
                for fmsg in fetches.iter() {
                    let Some(env) = fmsg.envelope() else { continue };
                    let decode = |b: &[u8]| String::from_utf8_lossy(b).to_string();
                    let subject = env.subject.as_ref().map(|s| decode(s)).unwrap_or_default();
                    let from = env
                        .from
                        .as_ref()
                        .and_then(|a| a.first())
                        .map(|a| {
                            let mbox = a.mailbox.as_ref().map(|b| decode(b)).unwrap_or_default();
                            let h = a.host.as_ref().map(|b| decode(b)).unwrap_or_default();
                            if a.name.is_some() { decode(a.name.as_ref().unwrap()) } else { format!("{mbox}@{h}") }
                        })
                        .unwrap_or_else(|| "(unknown)".into());
                    let date = env.date.as_ref().map(|b| decode(b)).unwrap_or_default();
                    let body = fmsg.text().map(|t| clean_body(t, 700)).unwrap_or_default();
                    out.push((EmailMsg { id: fmsg.message.to_string(), from, subject, date }, body));
                }
                if !out.is_empty() {
                    break; // first mailbox with hits wins (All Mail already spans the account)
                }
            }
            let _ = session.logout();
            out.reverse();
            Ok(out)
        })
        .await?
    }

    async fn peek_bodies(&self, ids: &[String], max_chars: usize) -> anyhow::Result<Vec<(String, String)>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let (host, port, user, password) =
            (self.host.clone(), self.port, self.user.clone(), self.password.clone());
        let set = ids.join(",");
        tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<(String, String)>> {
            let tls = native_tls::TlsConnector::builder().build()?;
            let client = imap::connect((host.as_str(), port), host.as_str(), &tls)?;
            let mut session = client.login(&user, &password).map_err(|(e, _)| e)?;
            session.select("INBOX")?;
            let fetches = session.fetch(set, "BODY.PEEK[TEXT]")?;
            let mut out = Vec::new();
            for f in fetches.iter() {
                if let Some(text) = f.text() {
                    out.push((f.message.to_string(), clean_body(text, max_chars)));
                }
            }
            let _ = session.logout();
            Ok(out)
        })
        .await?
    }
}

// ---------------------------------------------------------------------------------------------
// Sending — an OUTWARD effect. This is the transport only; whether a send is allowed/confirmed is
// the harm-gate + ActionRuntime's job, never the sender's.
// ---------------------------------------------------------------------------------------------

#[async_trait]
pub trait MailSender: Send + Sync {
    async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()>;
}

/// Real SMTP sender (TLS). For gmail use `for_address` (-> smtp.gmail.com) with the App Password.
pub struct SmtpMailSender {
    host: String,
    user: String,
    password: String,
    from: String,
}

impl SmtpMailSender {
    pub fn new(host: impl Into<String>, user: impl Into<String>, password: impl Into<String>, from: impl Into<String>) -> Self {
        Self { host: host.into(), user: user.into(), password: password.into(), from: from.into() }
    }

    pub fn for_address(addr: &str, password: impl Into<String>) -> Self {
        let host = if addr.ends_with("@gmail.com") {
            "smtp.gmail.com".to_string()
        } else {
            addr.split('@').nth(1).map(|d| format!("mail.{d}")).unwrap_or_else(|| "localhost".into())
        };
        Self::new(host, addr, password, addr)
    }
}

#[async_trait]
impl MailSender for SmtpMailSender {
    async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()> {
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::{Message, SmtpTransport, Transport};
        let (host, user, password, from) =
            (self.host.clone(), self.user.clone(), self.password.clone(), self.from.clone());
        let (to, subject, body) = (to.to_string(), subject.to_string(), body.to_string());
        tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
            let email = Message::builder()
                .from(from.parse()?)
                .to(to.parse()?)
                .subject(subject)
                .body(body)?;
            let creds = Credentials::new(user, password);
            let mailer = SmtpTransport::relay(&host)?.credentials(creds).build();
            mailer.send(&email)?;
            Ok(())
        })
        .await?
    }
}

/// Records sends instead of performing them — for tests/dry-runs.
#[derive(Default)]
pub struct ScriptedMailSender {
    pub sent: Mutex<Vec<(String, String, String)>>,
}

impl ScriptedMailSender {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl MailSender for ScriptedMailSender {
    async fn send(&self, to: &str, subject: &str, body: &str) -> anyhow::Result<()> {
        self.sent.lock().unwrap().push((to.into(), subject.into(), body.into()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(from: &str, subj: &str) -> EmailMsg {
        EmailMsg { id: "1".into(), from: from.into(), subject: subj.into(), date: "today".into() }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn scripted_inbox_respects_limit() {
        let c = ScriptedMailClient::new(vec![msg("a@x", "one"), msg("b@x", "two"), msg("c@x", "three")]);
        assert_eq!(c.inbox(2).await.unwrap().len(), 2);
    }

    #[test]
    fn digest_lists_senders_and_subjects() {
        let d = render_inbox_digest(&[msg("alice@x", "Invoice"), msg("bob@y", "Lunch?")]);
        assert!(d.contains("alice@x") && d.contains("Invoice"));
        assert!(d.contains("bob@y") && d.contains("Lunch?"));
        assert!(render_inbox_digest(&[]).contains("empty"));
    }

    #[test]
    fn gmail_address_maps_to_gmail_imap() {
        let c = ImapClient::for_address("yantrikdb@gmail.com", "pw").unwrap();
        assert_eq!(c.host, "imap.gmail.com");
        assert_eq!(c.port, 993);
    }
}
