//! mail — the mind's email capability. READ is here (inbox triage/digest, safe like browsing);
//! its output is untrusted (email bodies are an injection surface, wrapped by the caller). SEND is
//! an outward effect and is deliberately NOT here — it must ride the harm-gate + confirmation, so it
//! lands once `mind-governance`'s real gate exists.
//!
//! `MailClient` is the injectable seam (real IMAP vs scripted-for-tests). The real transport is
//! blocking `imap`, run on the blocking pool so it never stalls the async runtime.

use async_trait::async_trait;

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
