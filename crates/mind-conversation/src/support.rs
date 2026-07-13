//! Support-nudge command surface -- the `support` CLI verb + the candidate/audit helpers that
//! back the proactive support nudges. Extracted from lib.rs.

use super::*;

impl super::ConversationEngine {
    /// ---------- SUPPORT-NOT-REPLACE (CR-1) ----------
    /// The opt-in/mute/status surface for the support-nudge class. The class is
    /// OFF until the owner opts in — a companion that pushes you toward real
    /// people must be chosen, never defaulted on. Storage is plain profile KV.
    pub async fn support_cmd(&self, arg: &str) -> String {
        let a = arg.trim();
        let (verb, rest) = a.split_once(' ').unwrap_or((a, ""));
        match verb {
            "on" | "enable" => {
                let _ = self.memory.profile_set("snr_optin", "1").await;
                let _ = self.memory.profile_set("snr_class_mute", "0").await;
                "🤝 Support nudges ON. When someone you know has a birthday coming and it's not yet handled, I'll offer to help you show up for them — an offer, never a nudge to \"reach out more.\" Turn off anytime with `support off`; mute one person with `support mute <name>`.".into()
            }
            "off" | "disable" => {
                let _ = self.memory.profile_set("snr_optin", "0").await;
                "🤝 Support nudges OFF. I won't raise them again unless you turn them back on with `support on`.".into()
            }
            "mute" if !rest.trim().is_empty() => {
                let who = rest.trim().to_lowercase();
                let mut muted = self.support_muted_people().await;
                if !muted.contains(&who) {
                    muted.push(who.clone());
                    let _ = self.memory.profile_set("snr_muted_people", &muted.join(",")).await;
                }
                format!("Muted support nudges for {who}. I'll never raise them for that person again.")
            }
            "mute" => {
                let _ = self.memory.profile_set("snr_class_mute", "1").await;
                "Muted the whole support-nudge class. `support on` re-enables it.".into()
            }
            "feedback" if !rest.trim().is_empty() => {
                // Grades the most recent ungraded nudge. "pressured"/"monitored"/"creepy"
                // feed the kill switch; "helpful"/"neutral" feed the trust bound. This is
                // the metric — how it FELT, never whether you acted.
                let word = rest.trim().to_lowercase();
                let mut audits = self.support_audits().await;
                let Some(last) = audits.iter_mut().rev().find(|a| a.feedback.is_none()) else {
                    return "No ungraded support nudge on record.".into();
                };
                last.feedback = Some(word.clone());
                let person = last.person.clone();
                let harm = support_nudge::feedback_is_harm(&word);
                if harm {
                    // A harm report immediately pauses that person (sol's kill rule).
                    let mut muted = self.support_muted_people().await;
                    let p = person.to_lowercase();
                    if !muted.contains(&p) {
                        muted.push(p);
                        let _ = self.memory.profile_set("snr_muted_people", &muted.join(",")).await;
                    }
                }
                let health = support_nudge::class_health(&audits);
                let _ = self.memory.profile_set("snr_audits", &serde_json::to_string(&audits).unwrap_or_default()).await;
                if health == support_nudge::ClassHealth::KillDisabled {
                    let _ = self.memory.profile_set("snr_class_mute", "1").await;
                    return "Understood — and that's twice recently, so I've disabled support nudges entirely pending your review (`support on` re-enables). Thank you for telling me.".into();
                }
                if harm {
                    format!("Understood — I'm sorry it landed that way. I've muted support nudges for {person} and recorded the report.")
                } else {
                    "Noted, thank you — that grades the last nudge.".into()
                }
            }
            _ => {
                let on = self.memory.profile_get("snr_optin").await.ok().flatten().as_deref() == Some("1");
                let muted = self.support_muted_people().await;
                let audits = self.support_audits().await;
                let health = support_nudge::class_health(&audits);
                let graded = audits.iter().filter(|x| x.feedback.is_some()).count();
                format!(
                    "🤝 Support-not-replace — {}\n· muted people: {}\n· sends audited: {} ({} with feedback)\n· class health: {:?}\nA companion that helps you show up for the people you love, opportunity-first — never guilt. `support on`/`off` · `support mute <name>`.",
                    if on { "ON" } else { "OFF (opt-in with `support on`)" },
                    if muted.is_empty() { "none".into() } else { muted.join(", ") },
                    audits.len(), graded, health,
                )
            }
        }
    }

    async fn support_muted_people(&self) -> Vec<String> {
        self.memory.profile_get("snr_muted_people").await.ok().flatten()
            .map(|s| s.split(',').map(|x| x.trim().to_lowercase()).filter(|x| !x.is_empty()).collect())
            .unwrap_or_default()
    }

    async fn support_audits(&self) -> Vec<support_nudge::NudgeAudit> {
        self.memory.profile_get("snr_audits").await.ok().flatten()
            .and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// Build the one eligible support nudge, if any — a candidate for the
    /// proactive path. Reuses the birthday horizon (observed/told events),
    /// applies the full [`support_nudge::NudgeGate`], renders opportunity-first,
    /// and writes an audit record. Returns None (silence) by default.
    pub async fn support_nudge_candidate(&self, quiet_hours: bool, emotion_heavy: bool) -> Option<String> {
        if self.memory.profile_get("snr_optin").await.ok().flatten().as_deref() != Some("1") {
            return None; // opt-out by default
        }
        let class_muted = self.memory.profile_get("snr_class_mute").await.ok().flatten().as_deref() == Some("1");
        if class_muted {
            return None;
        }
        let muted = self.support_muted_people().await;
        let mut audits = self.support_audits().await;
        if support_nudge::class_health(&audits) == support_nudge::ClassHealth::KillDisabled {
            return None; // kill switch tripped — stay silent pending review
        }
        let sent_keys: std::collections::HashSet<String> = audits.iter().map(|a| a.event_key.clone()).collect();

        // Observed/told birthdays within the next 9 days whose prep is unmet.
        for n in self.future_scan(9).await {
            if n.get("kind").and_then(|x| x.as_str()) != Some("birthday") {
                continue;
            }
            let title = n.get("title").and_then(|x| x.as_str()).unwrap_or("").to_string();
            let person = title.split(['\u{2019}', '\'']).next().unwrap_or(&title).trim().to_string();
            if person.is_empty() || muted.contains(&person.to_lowercase()) {
                continue;
            }
            let when = n.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            let key = support_nudge::event_key(&person, when);
            let readiness_unmet = n.get("readiness").and_then(|r| r.get("prepared-note"))
                .and_then(|v| v.as_bool()) != Some(true);
            let gate = support_nudge::NudgeGate {
                opted_in: true, class_muted: false,
                person_muted: false, // filtered above
                already_sent: sent_keys.contains(&key),
                readiness_unmet,
                provenance_authorized: true, // birthday horizon is observed/told by construction
                quiet_hours, emotion_heavy,
            };
            if !gate.eligible() {
                continue;
            }
            let today = local_now();
            let date_phrase = chrono::DateTime::from_timestamp_millis(when)
                .map(|t| t.with_timezone(today.offset()).format("%A").to_string())
                .unwrap_or_else(|| "coming up".into());
            let gift_hint = self.memory.beliefs_matching(&format!("{person} likes"), &mind_types::AccessContext::Operator).await.ok()
                .and_then(|b| b.first().map(|x| x.statement.clone()));
            let rendered = support_nudge::render(&person, &date_phrase, gift_hint.as_deref());
            if !support_nudge::is_clean(&rendered) {
                continue; // belt-and-suspenders: never emit guilt framing
            }
            // Audit BEFORE returning — captures eligibility/provenance/controls, never a predicted action.
            audits.push(support_nudge::NudgeAudit {
                event_key: key, person: person.clone(), provenance: "told".into(),
                controls_shown: true,
                ts_ms: chrono::Utc::now().timestamp_millis(),
                feedback: None,
            });
            // Keep the audit trail bounded.
            let n_keep = audits.len().saturating_sub(200);
            let trimmed = audits[n_keep..].to_vec();
            let _ = self.memory.profile_set("snr_audits", &serde_json::to_string(&trimmed).unwrap_or_default()).await;
            return Some(rendered);
        }
        None
    }

}
