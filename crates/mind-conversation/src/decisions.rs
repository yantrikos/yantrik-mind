//! Decision ledger -- future packets, node ticks, fragility scan, regret classification. Extracted from lib.rs.

use super::*;

const PACKET_DECISION_EVALUATOR_ID: &str = "owner-packet-decision-v1";
const PACKET_EXPIRY_EVALUATOR_ID: &str = "packet-expiry-clock-v1";
static PACKET_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn next_packet_id(now_ms: i64) -> String {
    let sequence = PACKET_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("pkt:{now_ms:x}-{:x}-{sequence:x}", std::process::id())
}

/// Packet confidence is persisted in both the mutable packet store and the immutable recorder.
/// Keep the two representations inside the probability contract even when a future producer
/// supplies an invalid float. Non-finite values become the conservative floor rather than JSON
/// `null`; finite out-of-range values are bounded without changing the packet API.
fn normalize_packet_confidence(confidence: f64) -> f64 {
    if confidence.is_finite() {
        confidence.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

impl super::ConversationEngine {
    pub(crate) async fn load_packets(&self) -> Vec<serde_json::Value> {
        self.memory
            .profile_get("action_packets")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_default()
    }

    pub(crate) async fn save_packets(&self, v: &[serde_json::Value]) {
        let _ = self
            .memory
            .profile_set(
                "action_packets",
                &serde_json::Value::Array(v.to_vec()).to_string(),
            )
            .await;
    }

    /// Author one packet (emissaries call this). Returns the packet id. If `satisfies` names a
    /// readiness criterion on the linked node, the node is ticked immediately — proposed work
    /// counts as readiness; a rejection un-ticks it.
    #[allow(clippy::too_many_arguments)]
    /// The ONE door that stamps `told` authority — used by the courier, whose threads can only be
    /// opened by an explicit statement from the user. A packet created here may justify a calibrated
    /// knock; everything from `packet_add` stays `inferred` and may not interrupt.
    #[allow(clippy::too_many_arguments)]
    pub async fn packet_add_told(
        &self,
        node_id: &str,
        satisfies: Option<&str>,
        kind: &str,
        title: &str,
        body: &str,
        reason: &str,
        evidence: Vec<String>,
        confidence: f64,
        confirmation_required: bool,
        expiry_ms: i64,
    ) -> String {
        self.packet_add_with_provenance(
            node_id,
            satisfies,
            kind,
            title,
            body,
            reason,
            evidence,
            confidence,
            confirmation_required,
            expiry_ms,
            "told",
        )
        .await
    }

    /// The door that stamps `observed` authority — for packets whose TRIGGER is a deterministic
    /// observation of a human-curated fact: a birthday or festival arriving on the calendar, where
    /// the date itself came from the family layer (entered by the user through `family_set`, the
    /// human-authoritative editor). Date arithmetic over a told fact contains zero inference; the
    /// inference lives only in the PREP CONTENT, whose uncertainty is what the knock band prices.
    ///
    /// Why this door exists (2026-08-06): the funnel showed 3,221/3,221 knock kills — the ONLY
    /// knock-eligible producer was the courier (user-uttered promises), so the mind's own initiative
    /// could never interrupt even when it had genuinely prepared a birthday gift plan triggered by a
    /// date the user personally entered. The authority rule (sol) bans PATTERN-NOTICED triggers from
    /// interrupting; it was never meant to ban the calendar. `packet_add` stays `inferred`.
    #[allow(clippy::too_many_arguments)]
    pub async fn packet_add_observed(
        &self,
        node_id: &str,
        satisfies: Option<&str>,
        kind: &str,
        title: &str,
        body: &str,
        reason: &str,
        evidence: Vec<String>,
        confidence: f64,
        confirmation_required: bool,
        expiry_ms: i64,
    ) -> String {
        self.packet_add_with_provenance(
            node_id,
            satisfies,
            kind,
            title,
            body,
            reason,
            evidence,
            confidence,
            confirmation_required,
            expiry_ms,
            "observed",
        )
        .await
    }

    /// Drop terminal packets (expired/rejected, or past expiry) older than 30 days — the store had
    /// accumulated 62 corpses that the knock search re-read 2,000+ times a day. Returns how many
    /// were removed. Called on every `packet_add` and by `ym packets prune`.
    pub async fn packets_prune(&self) -> usize {
        let now = chrono::Utc::now().timestamp_millis();
        let cutoff = now - 30 * 24 * 3600 * 1000;
        let mut store = self.load_packets().await;
        let before = store.len();
        store.retain(|p| {
            let status = p
                .get("status")
                .and_then(|x| x.as_str())
                .unwrap_or("proposed");
            let expiry = p
                .get("expiry_ms")
                .and_then(|x| x.as_i64())
                .unwrap_or(i64::MAX);
            let terminal = status == "expired" || status == "rejected" || expiry < now;
            !(terminal && expiry < cutoff)
        });
        let removed = before - store.len();
        if removed > 0 {
            let _ = self
                .memory
                .profile_set(
                    "action_packets",
                    &serde_json::to_string(&store).unwrap_or_default(),
                )
                .await;
        }
        removed
    }

    /// Record whether a packet contains REAL prepared work (the finished comparison/draft) rather
    /// than a restatement of the request. The calibrated knock says "I've prepared X" — this is what
    /// makes that claim structurally true instead of a hopeful phrase.
    pub(crate) async fn packet_mark_prepared(&self, id: &str, prepared: bool) {
        let mut store = self.load_packets().await;
        if let Some(p) = store
            .iter_mut()
            .find(|p| p.get("id").and_then(|x| x.as_str()) == Some(id))
        {
            p["prepared"] = serde_json::json!(prepared);
        }
        let _ = self
            .memory
            .profile_set(
                "action_packets",
                &serde_json::to_string(&store).unwrap_or_default(),
            )
            .await;
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "packet creation exposes the proof-carrying action contract explicitly rather than hiding fields in defaults"
    )]
    pub async fn packet_add(
        &self,
        node_id: &str,
        satisfies: Option<&str>,
        kind: &str, // checklist | plan | draft | cart | info
        title: &str,
        body: &str,
        reason: &str,
        evidence: Vec<String>,
        confidence: f64,
        confirmation_required: bool,
        expiry_ms: i64,
    ) -> String {
        self.packet_add_with_provenance(
            node_id,
            satisfies,
            kind,
            title,
            body,
            reason,
            evidence,
            confidence,
            confirmation_required,
            expiry_ms,
            "inferred",
        )
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the internal packet writer atomically persists the explicit trigger authority with the proof-carrying action contract"
    )]
    async fn packet_add_with_provenance(
        &self,
        node_id: &str,
        satisfies: Option<&str>,
        kind: &str,
        title: &str,
        body: &str,
        reason: &str,
        evidence: Vec<String>,
        confidence: f64,
        confirmation_required: bool,
        expiry_ms: i64,
        trigger_provenance: &str,
    ) -> String {
        let now = chrono::Utc::now().timestamp_millis();
        let id = next_packet_id(now);
        let confidence = normalize_packet_confidence(confidence);
        // Hygiene rides the write path: without it, terminal packets only left the store past the
        // 200 cap — 62 corpses sat being re-scanned 2,000+ times a day for five weeks.
        let _ = self.packets_prune().await;
        let mut store = self.load_packets().await;
        // Mint the causal root before persisting the packet so every later terminal event can
        // point back to the exact proposal it resolves, rather than relying on trace co-location.
        let mut created_event =
            mind_observability::DecisionEvent::span(&id, None, "packet_created");
        created_event.object_id = Some(id.clone());
        created_event.goal_id = Some(node_id.to_string());
        created_event.actor = Some("proactive".into());
        created_event.lane = Some("primary".into());
        created_event.goal = Some(title.to_string());
        created_event.trigger = Some(format!(
            "{node_id}{}",
            satisfies.map(|s| format!(" ({s})")).unwrap_or_default()
        ));
        created_event.evidence_ids = evidence.clone();
        created_event.chosen = Some(kind.to_string());
        created_event.confidence = Some(confidence);
        created_event.policy = vec![format!(
            "confirmation_required={confirmation_required} provenance={trigger_provenance} expiry_ms={expiry_ms}"
        )];
        let created_event_id = created_event.event_id.clone();
        store.push(serde_json::json!({
            "id": id, "node_id": node_id, "satisfies": satisfies, "kind": kind,
            "title": title, "body": body, "reason": reason, "evidence": evidence,
            "confidence": confidence, "confirmation_required": confirmation_required,
            "expiry_ms": expiry_ms, "status": "proposed", "created_ms": now,
            "created_event_id": created_event_id,
            "alternatives_rejected": [],
            // EXPLICIT AUTHORITY STAMP. A packet may only justify INTERRUPTING the user when its
            // trigger was observed or told (see `knock`). Everything built by an emissary or the
            // night shift is derived from patterns the mind noticed, so it is honestly `inferred`
            // and stays ineligible; `packet_add_told` is the one door that stamps `told`. This was
            // previously left implicit and fell back to reading `reason` (a system-written string),
            // which made eligibility an accident rather than a decision.
            "trigger_provenance": trigger_provenance,
        }));
        // keep the store bounded; drop the oldest terminal packets first
        if store.len() > 200 {
            store.retain(|p| {
                matches!(
                    p.get("status").and_then(|x| x.as_str()),
                    Some("proposed") | Some("confirmed")
                )
            });
        }
        self.save_packets(&store).await;
        if let Some(criterion) = satisfies {
            self.node_tick(node_id, criterion, true).await;
        }
        self.ledger_sent("packet", &format!("prepared: {title}"))
            .await;
        // FLIGHT RECORDER: the packet's own id IS its trace — creation and resolution share it,
        // so `ym why pkt:<hex>` reconstructs proposed→decided from persisted evidence.
        self.recorder.record(created_event);
        id
    }

    /// Tick (or un-tick) one readiness criterion on a FutureNode.
    pub(crate) async fn node_tick(&self, node_id: &str, criterion: &str, done: bool) {
        let mut nodes: Vec<serde_json::Value> = self
            .memory
            .profile_get("future_nodes")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_default();
        for n in nodes.iter_mut() {
            if n.get("id").and_then(|x| x.as_str()) == Some(node_id) {
                if let Some(obj) = n.as_object_mut() {
                    obj.entry("readiness")
                        .or_insert_with(|| serde_json::json!({}));
                    if let Some(r) = obj.get_mut("readiness").and_then(|x| x.as_object_mut()) {
                        r.insert(criterion.to_string(), serde_json::json!(done));
                    }
                }
            }
        }
        let _ = self
            .memory
            .profile_set("future_nodes", &serde_json::Value::Array(nodes).to_string())
            .await;
    }

    /// Lazily expire, then return live packets (proposed first, then confirmed), newest last.
    pub(crate) async fn live_packets(&self) -> Vec<serde_json::Value> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut store = self.load_packets().await;
        let mut changed = false;
        for p in store.iter_mut() {
            let live = matches!(p.get("status").and_then(|x| x.as_str()), Some("proposed"));
            let exp = p
                .get("expiry_ms")
                .and_then(|x| x.as_i64())
                .unwrap_or(i64::MAX);
            if live && exp < now {
                p["status"] = serde_json::json!("expired");
                changed = true;
                // FLIGHT RECORDER: expiry is an outcome too — the charter counts it in the
                // acceptance denominator, and silence about it would flatter the numbers.
                self.recorder.record({
                    let packet_id = p.get("id").and_then(|x| x.as_str()).unwrap_or("pkt:?");
                    let mut e = mind_observability::DecisionEvent::span(
                        packet_id,
                        p.get("created_event_id").and_then(|x| x.as_str()),
                        "packet_expired",
                    );
                    e.object_id = Some(packet_id.to_string());
                    e.goal_id = p.get("node_id").and_then(|x| x.as_str()).map(String::from);
                    e.actor = Some("proactive".into());
                    e.lane = Some("primary".into());
                    e.goal = p.get("title").and_then(|x| x.as_str()).map(String::from);
                    e.trigger = Some("expiry passed with no owner word".into());
                    e.policy.push(format!("expiry_ms={exp}"));
                    e.verdict = Some("expired".into());
                    e.semantic_success = Some(false);
                    e.evaluator_id = Some(PACKET_EXPIRY_EVALUATOR_ID.into());
                    e
                });
                // an expired packet no longer vouches for readiness
                if let (Some(nid), Some(c)) = (
                    p.get("node_id").and_then(|x| x.as_str()).map(String::from),
                    p.get("satisfies")
                        .and_then(|x| x.as_str())
                        .map(String::from),
                ) {
                    self.node_tick(&nid, &c, false).await;
                }
            }
        }
        if changed {
            self.save_packets(&store).await;
        }
        store
            .into_iter()
            .filter(|p| {
                matches!(
                    p.get("status").and_then(|x| x.as_str()),
                    Some("proposed") | Some("confirmed")
                )
            })
            .collect()
    }

    /// `ym packets` — the live board: what's prepared, what needs a word.
    pub async fn packets_view(&self) -> String {
        let live = self.live_packets().await;
        if live.is_empty() {
            return "📦 No live packets. The Night Shift compiles them against the future nodes (`ym future`).".to_string();
        }
        let mut out = String::from("📦 ACTION PACKETS (live)\n");
        for (i, p) in live.iter().enumerate() {
            let title = p.get("title").and_then(|x| x.as_str()).unwrap_or("?");
            let kind = p.get("kind").and_then(|x| x.as_str()).unwrap_or("?");
            let st = p.get("status").and_then(|x| x.as_str()).unwrap_or("?");
            let conf = p
                .get("confirmation_required")
                .and_then(|x| x.as_bool())
                .unwrap_or(false);
            out.push_str(&format!(
                "{}. [{kind}] {title} — {st}{}\n",
                i + 1,
                if conf && st == "proposed" {
                    " · NEEDS YOUR WORD (`approve N` / `reject N`)"
                } else {
                    ""
                }
            ));
        }
        out.push_str("`packet N` shows the full proof (reason, evidence, expiry).");
        out
    }

    /// `ym packet N` — the full proof-carrying view.
    pub async fn packet_show(&self, sel: &str) -> String {
        let live = self.live_packets().await;
        let idx = sel
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1));
        let Some(p) = idx.and_then(|i| live.get(i)) else {
            return "Which packet? `packets` lists them; `packet 2` shows one.".to_string();
        };
        let g = |k: &str| p.get(k).and_then(|x| x.as_str()).unwrap_or("—").to_string();
        let ev: Vec<String> = p
            .get("evidence")
            .and_then(|x| x.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str())
                    .map(|x| format!("  · {x}"))
                    .collect()
            })
            .unwrap_or_default();
        let exp = p
            .get("expiry_ms")
            .and_then(|x| x.as_i64())
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|t| {
                t.with_timezone(local_now().offset())
                    .format("%a %b %-d %H:%M")
                    .to_string()
            })
            .unwrap_or_else(|| "never".into());
        format!(
            "📦 {}\nkind: {} · status: {} · confidence: {:.2} · expires: {exp}\nnode: {} (satisfies: {})\n\nWHY: {}\n\nEVIDENCE:\n{}\n\n{}",
            g("title"),
            g("kind"),
            g("status"),
            p.get("confidence").and_then(|x| x.as_f64()).unwrap_or(0.0),
            g("node_id"),
            g("satisfies"),
            g("reason"),
            if ev.is_empty() { "  (deterministic composition)".to_string() } else { ev.join("\n") },
            g("body"),
        )
    }

    /// `approve N` / `reject N [why]` — the human word. Rejection un-ticks readiness and records
    /// the why as a correction the replay lab learns from.
    pub async fn packet_decide(&self, sel: &str, approve: bool, why: &str) -> String {
        let live = self.live_packets().await;
        let idx = sel
            .trim()
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1));
        let Some(target) = idx.and_then(|i| live.get(i)) else {
            return "Which packet? `packets` lists them by number.".to_string();
        };
        let id = target
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let title = target
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("?")
            .to_string();
        let status = target
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        if status != "proposed" {
            return format!("Already {status}: {title}. No new decision was recorded.");
        }
        let created_event_id = target
            .get("created_event_id")
            .and_then(|x| x.as_str())
            .map(String::from);
        let goal_id = target
            .get("node_id")
            .and_then(|x| x.as_str())
            .map(String::from);
        let expiry_ms = target
            .get("expiry_ms")
            .and_then(|value| value.as_i64())
            .unwrap_or(i64::MAX);
        let mut store = self.load_packets().await;
        for p in store.iter_mut() {
            if p.get("id").and_then(|x| x.as_str()) == Some(id.as_str()) {
                p["status"] = serde_json::json!(if approve { "confirmed" } else { "rejected" });
                if !why.trim().is_empty() {
                    p["decision_why"] = serde_json::json!(why.trim());
                }
            }
        }
        self.save_packets(&store).await;
        // FLIGHT RECORDER: same trace as packet_created — the pair answers "what was predicted,
        // what actually happened" for every human word on prepared work.
        self.recorder.record({
            let mut e = mind_observability::DecisionEvent::span(
                &id,
                created_event_id.as_deref(),
                "packet_resolved",
            );
            e.object_id = Some(id.clone());
            e.goal_id = goal_id;
            e.actor = Some("proactive".into());
            e.lane = Some("primary".into());
            e.goal = Some(title.clone());
            e.trigger = Some(
                if approve {
                    "owner approved"
                } else {
                    "owner rejected"
                }
                .into(),
            );
            e.policy.push(format!("expiry_ms={expiry_ms}"));
            e.outcome = if why.trim().is_empty() {
                None
            } else {
                Some(why.trim().to_string())
            };
            e.verdict = Some(if approve { "confirmed" } else { "rejected" }.into());
            e.semantic_success = Some(approve);
            e.evaluator_id = Some(PACKET_DECISION_EVALUATOR_ID.into());
            e.lesson = if approve {
                Some("packet acceptance — emissary class earns standing".into())
            } else {
                Some("packet rejection — recorded as correction for the replay lab".into())
            };
            e
        });
        if approve {
            self.ledger_resolve(true).await;
            format!(
                "✅ Confirmed: {title}. I'll act within the packet's bounds — nothing beyond it."
            )
        } else {
            if let (Some(nid), Some(c)) = (
                target.get("node_id").and_then(|x| x.as_str()),
                target.get("satisfies").and_then(|x| x.as_str()),
            ) {
                self.node_tick(nid, c, false).await;
            }
            self.ledger_correction(
                "packet",
                &title,
                if why.trim().is_empty() {
                    "rejected"
                } else {
                    why.trim()
                },
            )
            .await;
            format!(
                "🗑 Rejected: {title}{} — noted for the replay lab.",
                if why.trim().is_empty() {
                    String::new()
                } else {
                    format!(" ({})", why.trim())
                }
            )
        }
    }

    /// Rebuild the forward store for the next `days`, preserving existing node state by id.
    /// Returns the nodes sorted by time. Persisted at KV `future_nodes`.
    pub async fn future_scan(&self, days: i64) -> Vec<serde_json::Value> {
        let today = local_now();
        let now = today.timestamp_millis();
        let horizon = now + days * 86_400_000;
        // Existing state to preserve (readiness ticks, packets, dismissals survive rescans).
        let old: std::collections::HashMap<String, serde_json::Value> = self
            .memory
            .profile_get("future_nodes")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str::<Vec<serde_json::Value>>(&x).ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|n| {
                let id = n.get("id").and_then(|x| x.as_str()).map(String::from)?;
                Some((id, n))
            })
            .collect();
        let slug = |t: &str| -> String {
            t.to_lowercase()
                .chars()
                .map(|c| if c.is_alphanumeric() { c } else { '-' })
                .collect::<String>()
                .split('-')
                .filter(|x| !x.is_empty())
                .take(5)
                .collect::<Vec<_>>()
                .join("-")
        };
        // Per-kind readiness criteria: the packet checklist each node kind demands. These defaults
        // seed the compiler; packets tick them off as they ship.
        let criteria = |kind: &str| -> Vec<&'static str> {
            match kind {
                "festival" => vec!["supplies", "logistics+weather", "story+message"],
                "trip" => vec!["packing", "documents", "weather+fallback", "route+timing"],
                "birthday" => vec!["gift", "card", "plan", "collision-check"],
                "deadline" => vec!["prepared-action"],
                _ => vec!["prepared-note"],
            }
        };
        let mut nodes: Vec<serde_json::Value> = Vec::new();
        let mut push =
            |title: String, kind: &str, when_ms: i64, end_ms: i64, participants: Vec<String>| {
                let id = format!("{kind}:{}", slug(&title));
                let mut node = old.get(&id).cloned().unwrap_or_else(|| {
                    serde_json::json!({
                        "id": id, "readiness": {}, "packets": [], "status": "open",
                    })
                });
                node["title"] = serde_json::json!(title);
                node["kind"] = serde_json::json!(kind);
                node["when_ms"] = serde_json::json!(when_ms);
                node["end_ms"] = serde_json::json!(end_ms.max(when_ms));
                node["participants"] = serde_json::json!(participants);
                node["criteria"] = serde_json::json!(criteria(kind));
                nodes.push(node);
            };
        // 1. Calendar entries (festivals carry the fest: prefix; multi-day events keep their window).
        for e in self.load_calendar().await {
            let ms = e.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0);
            if ms < now - 86_400_000 || ms > horizon {
                continue;
            }
            let title = e
                .get("title")
                .and_then(|x| x.as_str())
                .unwrap_or("?")
                .to_string();
            let end = e.get("end_ms").and_then(|x| x.as_i64()).unwrap_or(ms);
            let tl = title.to_lowercase();
            // Trip beats festival when both signals appear ("Olathe trip — Puja at cousin's"):
            // the drive is the operational load; the observance rides along.
            let kind = if tl.contains("trip")
                || tl.contains("travel")
                || tl.contains("resort")
                || tl.contains("hotel")
                || tl.contains("flight")
            {
                "trip"
            } else if title.starts_with("fest:")
                || tl.contains("puja")
                || tl.contains("yatra")
                || tl.contains("ashtami")
            {
                "festival"
            } else {
                "event"
            };
            push(
                title.trim_start_matches("fest:").trim().to_string(),
                kind,
                ms,
                end,
                vec![],
            );
        }
        // 1b. The FESTIVALS registry — the authoritative festival dates (they are NOT calendar
        // entries; the twin must read the registry directly or it misses every festival).
        for e in self.load_festival_dates().await {
            let (Some(name), Some(date)) = (
                e.get("name").and_then(|x| x.as_str()),
                e.get("date").and_then(|x| x.as_str()),
            ) else {
                continue;
            };
            let Ok(d) = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d") else {
                continue;
            };
            let when_ms = d
                .and_hms_opt(9, 0, 0)
                .and_then(|dt| dt.and_local_timezone(*today.offset()).single())
                .map(|dt| dt.timestamp_millis())
                .unwrap_or(0);
            if when_ms < now - 86_400_000 || when_ms > horizon {
                continue;
            }
            push(name.to_string(), "festival", when_ms, when_ms, vec![]);
        }
        // 2. People dates (birthdays/anniversaries) inside the horizon.
        for (name, label, d, _mmdd) in self.upcoming_people_dates(days).await {
            let kind = if label.to_lowercase().contains("birthday") {
                "birthday"
            } else {
                "event"
            };
            push(
                format!("{name}'s {label}"),
                kind,
                now + d * 86_400_000,
                now + d * 86_400_000,
                vec![name],
            );
        }
        // 3. Deadlined reminders.
        let (reminders, _) = self.split_tasks().await;
        for t in &reminders {
            if let Some(ms) = t
                .due_ms
                .map(|m| m as i64)
                .or_else(|| parse_text_date_ms(&t.description, &today))
            {
                if ms >= now && ms <= horizon {
                    push(
                        t.description.chars().take(80).collect(),
                        "deadline",
                        ms,
                        ms,
                        vec![],
                    );
                }
            }
        }
        nodes.sort_by_key(|n| n.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(0));
        let _ = self
            .memory
            .profile_set(
                "future_nodes",
                &serde_json::Value::Array(nodes.clone()).to_string(),
            )
            .await;
        nodes
    }

    /// Fragility ranking: deadline proximity × unmet readiness. This is the Night Shift's
    /// dispatch order — the most-imminent, least-ready node gets worked first.
    pub async fn future_fragile(&self, days: i64) -> Vec<(f64, serde_json::Value)> {
        let now = chrono::Utc::now().timestamp_millis();
        let mut out: Vec<(f64, serde_json::Value)> = self
            .future_scan(days)
            .await
            .into_iter()
            .filter(|n| n.get("status").and_then(|x| x.as_str()) != Some("dismissed"))
            .map(|n| {
                let when = n.get("when_ms").and_then(|x| x.as_i64()).unwrap_or(now);
                let days_left = ((when - now).max(0) as f64 / 86_400_000.0).max(0.25);
                let total = n
                    .get("criteria")
                    .and_then(|x| x.as_array())
                    .map(|a| a.len())
                    .unwrap_or(1)
                    .max(1);
                let done = n
                    .get("readiness")
                    .and_then(|x| x.as_object())
                    .map(|m| m.values().filter(|v| v.as_bool() == Some(true)).count())
                    .unwrap_or(0);
                let unready = 1.0 - (done as f64 / total as f64);
                // proximity dominates as the date closes in; fully-ready nodes fall to ~0.
                (unready * (10.0 / days_left), n)
            })
            .collect();
        out.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// `ym future` — the forward store, fragility-ranked. The twin's first visible face.
    pub async fn future_view(&self) -> String {
        let ranked = self.future_fragile(21).await;
        if ranked.is_empty() {
            return "🔭 Nothing on the 21-day horizon. (`ym calendar add …` seeds it.)".to_string();
        }
        let today = local_now();
        let mut out = String::from(
            "🔭 FUTURE NODES (21d, fragility-ranked — most imminent × least ready first)\n",
        );
        for (score, n) in ranked.iter().take(12) {
            let title = n.get("title").and_then(|x| x.as_str()).unwrap_or("?");
            let kind = n.get("kind").and_then(|x| x.as_str()).unwrap_or("?");
            let when = n
                .get("when_ms")
                .and_then(|x| x.as_i64())
                .and_then(chrono::DateTime::from_timestamp_millis)
                .map(|t| {
                    t.with_timezone(today.offset())
                        .format("%a %b %-d")
                        .to_string()
                })
                .unwrap_or_default();
            let total = n
                .get("criteria")
                .and_then(|x| x.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            let done = n
                .get("readiness")
                .and_then(|x| x.as_object())
                .map(|m| m.values().filter(|v| v.as_bool() == Some(true)).count())
                .unwrap_or(0);
            let unmet: Vec<String> = n
                .get("criteria")
                .and_then(|x| x.as_array())
                .map(|a| {
                    a.iter()
                        .filter_map(|c| c.as_str())
                        .filter(|c| {
                            n.get("readiness")
                                .and_then(|r| r.get(*c))
                                .and_then(|v| v.as_bool())
                                != Some(true)
                        })
                        .map(String::from)
                        .collect()
                })
                .unwrap_or_default();
            out.push_str(&format!(
                "• [{kind}] {title} — {when} · readiness {done}/{total} · fragility {score:.1}{}\n",
                if unmet.is_empty() {
                    String::new()
                } else {
                    format!(" · needs: {}", unmet.join(", "))
                }
            ));
        }
        out
    }

    /// Deterministic classification of one primary ask. No LLM, a few KV reads, called inline.
    pub async fn regret_classify(&self, user_text: &str) {
        let t = user_text.trim();
        // Commands and micro-turns aren't asks; don't pollute the curve with "ym privacy".
        if t.len() < 12 || is_cli_verb(t) || t.starts_with('/') {
            return;
        }
        let stop = [
            "what", "when", "where", "there", "about", "have", "this", "that", "with", "will",
            "would", "could", "should", "going", "know", "need", "want", "does",
        ];
        let words: Vec<String> = t
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.len() >= 4 && !stop.contains(w))
            .map(String::from)
            .collect();
        if words.is_empty() {
            return;
        }
        // Forward spine = calendar (incl. fest: entries) + people dates + deadlined reminders.
        let spine = self.upcoming_spine(21).await;
        let hit: Option<String> = spine.iter().find_map(|(_, label, _)| {
            let ll = label.to_lowercase();
            let ltoks: std::collections::HashSet<String> = ll
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() >= 4 && !stop.contains(w))
                .map(String::from)
                .collect();
            words
                .iter()
                .any(|w| ltoks.contains(w))
                .then(|| label.clone())
        });
        let now = chrono::Utc::now();
        let today = local_now();
        let week = format!(
            "{}-W{:02}",
            today.format("%G"),
            chrono::Datelike::iso_week(&today).week()
        );
        let mut stats: serde_json::Value = self
            .memory
            .profile_get("regret_stats")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let wk = stats
            .as_object_mut()
            .map(|m| {
                m.entry(week.clone()).or_insert_with(
                    || serde_json::json!({"asks":0,"linked":0,"anticipated":0,"missed":0}),
                )
            })
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"asks":0,"linked":0,"anticipated":0,"missed":0}));
        let bump =
            |v: &serde_json::Value, k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0) + 1;
        let mut wk2 = wk.clone();
        wk2["asks"] = serde_json::json!(bump(&wk, "asks"));
        let class = match &hit {
            None => "unforeseeable",
            Some(label) => {
                wk2["linked"] = serde_json::json!(bump(&wk, "linked"));
                // Was there a LIVE PACKET for this subject? Real prepared-work records first
                // (the honest signal); the spoken-recently proxy stays as the soft fallback.
                let ll = label.to_lowercase();
                let subj: Vec<String> = ll
                    .split(|c: char| !c.is_alphanumeric())
                    .filter(|w| w.len() >= 4 && !stop.contains(w))
                    .map(String::from)
                    .collect();
                let packets = self.live_packets().await;
                let packed = packets.iter().any(|p| {
                    let hay = format!(
                        "{} {}",
                        p.get("title").and_then(|x| x.as_str()).unwrap_or(""),
                        p.get("node_id").and_then(|x| x.as_str()).unwrap_or("")
                    )
                    .to_lowercase();
                    subj.iter().any(|w| hay.contains(w.as_str()))
                });
                let recent = self
                    .memory
                    .recent_messages(80, &mind_types::AccessContext::operator_audit())
                    .await
                    .unwrap_or_default();
                let spoken = packed
                    || recent
                        .iter()
                        .filter(|(r, _)| r == "assistant")
                        .any(|(_, txt)| {
                            let xl = txt.to_lowercase();
                            subj.iter().any(|w| xl.contains(w.as_str()))
                        });
                if spoken {
                    wk2["anticipated"] = serde_json::json!(bump(&wk, "anticipated"));
                    "anticipated"
                } else {
                    wk2["missed"] = serde_json::json!(bump(&wk, "missed"));
                    "missed_foreseeable"
                }
            }
        };
        if let Some(m) = stats.as_object_mut() {
            m.insert(week, wk2);
            // keep at most 12 weeks of stats
            if m.len() > 12 {
                let mut keys: Vec<String> = m.keys().cloned().collect();
                keys.sort();
                for old in keys.iter().take(m.len() - 12) {
                    m.remove(old);
                }
            }
        }
        let _ = self
            .memory
            .profile_set("regret_stats", &stats.to_string())
            .await;
        // A miss is a RegretRecord — the raw material for regression tests + self-build goals.
        if class == "missed_foreseeable" {
            let mut log: Vec<serde_json::Value> = self
                .memory
                .profile_get("regret_log")
                .await
                .ok()
                .flatten()
                .and_then(|x| serde_json::from_str(&x).ok())
                .unwrap_or_default();
            log.push(serde_json::json!({
                "ts": now.timestamp_millis(),
                "ask": t.chars().take(160).collect::<String>(),
                "subject": hit,
                "class": class,
                "prepared": serde_json::Value::Null, // becomes a packet id once ActionPackets exist
            }));
            if log.len() > 300 {
                let cut = log.len() - 300;
                log.drain(..cut);
            }
            let _ = self
                .memory
                .profile_set("regret_log", &serde_json::Value::Array(log).to_string())
                .await;
        }
    }

    /// `ym regrets` — the curve so far + the recent misses. This is the metric the Night Shift
    /// will be judged against; week 1 is the untreated baseline.
    pub async fn regrets_report(&self) -> String {
        let stats: serde_json::Value = self
            .memory
            .profile_get("regret_stats")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let mut out = String::from(
            "📉 PREVENTABLE-ASK CURVE (charter metric — must decline once the Night Shift ships)\n",
        );
        let mut weeks: Vec<(String, serde_json::Value)> = stats
            .as_object()
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        weeks.sort_by(|a, b| a.0.cmp(&b.0));
        if weeks.is_empty() {
            out.push_str("(no asks classified yet — the log just turned on)\n");
        }
        for (wk, v) in &weeks {
            let g = |k: &str| v.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
            let (linked, missed) = (g("linked"), g("missed"));
            let rate = if linked > 0 {
                format!("{:.0}%", missed as f64 * 100.0 / linked as f64)
            } else {
                "—".to_string()
            };
            out.push_str(&format!(
                "{wk}: {} asks · {} foreseeable · {} anticipated · {} MISSED → preventable-ask rate {rate}\n",
                g("asks"), linked, g("anticipated"), missed
            ));
        }
        let log: Vec<serde_json::Value> = self
            .memory
            .profile_get("regret_log")
            .await
            .ok()
            .flatten()
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or_default();
        if !log.is_empty() {
            out.push_str("\nRecent misses (what I should have prepared):\n");
            for r in log.iter().rev().take(8) {
                let ask = r.get("ask").and_then(|x| x.as_str()).unwrap_or("?");
                let subj = r.get("subject").and_then(|x| x.as_str()).unwrap_or("?");
                out.push_str(&format!("• \"{ask}\" — foreseeable via: {subj}\n"));
            }
        }
        out
    }
}

#[cfg(test)]
mod flight_recorder_tests {
    use super::*;

    #[test]
    fn packet_ids_do_not_collide_within_one_millisecond() {
        let ids: std::collections::HashSet<String> =
            (0..1_024).map(|_| next_packet_id(42)).collect();
        assert_eq!(ids.len(), 1_024);
        assert!(ids.iter().all(|id| id.starts_with("pkt:2a-")));
    }
    use std::sync::Arc;

    fn engine_with_recorder(tag: &str) -> (Arc<ConversationEngine>, mind_types::scratch::Scratch) {
        let p = mind_types::scratch::file(&format!("flight_{tag}"), "jsonl");
        let mem = mind_memory::MemoryHandle::spawn(":memory:", 8).unwrap();
        let pool = mind_inference::InferencePool::new(
            Arc::new(mind_inference::ScriptedLLM::new("ok")) as Arc<dyn yantrik_ml::LLMBackend>,
            1,
        );
        let eng = Arc::new(
            ConversationEngine::new(
                Arc::new(mem) as Arc<dyn MemoryFacade>,
                pool,
                mind_types::default_persona("the user"),
            )
            .with_recorder(Arc::new(mind_observability::DecisionLog::open(&p))),
        );
        (eng, p)
    }

    /// THE WIRING PROOF: a packet's creation and the owner's word on it land under ONE trace,
    /// and `why` reconstructs predicted-vs-actual from persisted evidence — not narration.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn packet_lifecycle_is_reconstructable_from_the_flight_recorder() {
        let (eng, path) = engine_with_recorder("pkt");
        let id = eng
            .packet_add(
                "node:birthday",
                Some("gift shortlist ready"),
                "checklist",
                "Gift shortlist for the birthday",
                "1. book; 2. wrap",
                "window opened, preparable",
                vec!["E1".into(), "E2".into()],
                0.7,
                false,
                i64::MAX,
            )
            .await;
        assert!(id.starts_with("pkt:"), "{id}");

        // The creation event is already on disk under the packet's own trace.
        let events = eng.recorder().read_trace(&id);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, "packet_created");
        assert!(
            events[0].event_id.is_some(),
            "creation must mint a causal root"
        );
        assert_eq!(events[0].object_id.as_deref(), Some(id.as_str()));
        assert_eq!(events[0].goal_id.as_deref(), Some("node:birthday"));
        assert_eq!(events[0].lane.as_deref(), Some("primary"));
        assert_eq!(
            events[0].evidence_ids,
            vec!["E1".to_string(), "E2".to_string()]
        );

        // The owner rejects it: same trace gains the resolution with its why.
        eng.packet_decide("1", false, "wrong occasion").await;
        let events = eng.recorder().read_trace(&id);
        assert_eq!(events.len(), 2, "create + resolve share one trace");
        assert_eq!(events[1].kind, "packet_resolved");
        assert_eq!(events[1].verdict.as_deref(), Some("rejected"));
        assert_eq!(events[1].outcome.as_deref(), Some("wrong occasion"));
        assert_eq!(events[1].parent_event_id, events[0].event_id);
        assert_eq!(events[1].object_id, events[0].object_id);
        assert_eq!(events[1].goal_id, events[0].goal_id);
        assert_eq!(events[1].lane, events[0].lane);
        assert_eq!(events[1].policy, vec!["expiry_ms=9223372036854775807"]);
        assert_eq!(events[1].semantic_success, Some(false));
        assert_eq!(
            events[1].evaluator_id.as_deref(),
            Some(PACKET_DECISION_EVALUATOR_ID)
        );

        // And `ym why <prefix>` renders it human-readably from persisted evidence only.
        let rendered = eng.why(&id);
        for needle in [
            "packet_created",
            "packet_resolved",
            "verdict: rejected",
            "outcome: wrong occasion",
            "confidence 0.70",
        ] {
            assert!(
                rendered.contains(needle),
                "rendered trace must contain '{needle}':\n{rendered}"
            );
        }
        // Chain integrity survives real writes through the wiring.
        assert_eq!(mind_observability::verify_log(&path), Ok(2));
        let _ = std::fs::remove_file(&path);
    }

    /// E.AGI-A3: the REAL packet gate over the REAL emitters — the field-by-field wiring proof
    /// above cannot see a divergence between what the emitters write and what
    /// `render_packet_chain_completeness` requires (the blind spot that hid E.AGI-A2's goal_id).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn packet_emitters_satisfy_the_packet_gate() {
        let (eng, path) = engine_with_recorder("pkt_gate");
        let id = eng
            .packet_add(
                "node:gate",
                None,
                "checklist",
                "Gate-checked packet",
                "prepared body",
                "window opened",
                vec!["E1".into()],
                0.7,
                false,
                i64::MAX,
            )
            .await;
        eng.packet_decide("1", true, "approved for the gate").await;
        let events = eng
            .recorder()
            .read_tail_verified(50)
            .expect("the chain verifies");
        let report = mind_observability::render_packet_chain_completeness(&events);
        assert!(
            report.contains("1/1 latest packet lifecycle(s) complete (100.0%"),
            "a fresh lifecycle passes the gate it is measured by (packet {id}):\n{report}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn packet_expiry_is_graded_and_parented_to_its_proposal() {
        let (eng, path) = engine_with_recorder("pkt_expiry");
        let id = eng
            .packet_add(
                "node:window",
                None,
                "info",
                "Time-bounded packet",
                "prepared body",
                "window was open",
                vec![],
                0.6,
                false,
                0,
            )
            .await;

        assert!(eng.live_packets().await.is_empty());
        let events = eng.recorder().read_trace(&id);
        assert_eq!(events.len(), 2, "create + expiry share one trace");
        assert_eq!(events[1].kind, "packet_expired");
        assert_eq!(events[1].parent_event_id, events[0].event_id);
        assert_eq!(events[1].object_id, events[0].object_id);
        assert_eq!(events[1].goal_id, events[0].goal_id);
        assert_eq!(events[1].lane, events[0].lane);
        assert_eq!(events[1].policy, vec!["expiry_ms=0"]);
        assert_eq!(events[1].verdict.as_deref(), Some("expired"));
        assert_eq!(events[1].semantic_success, Some(false));
        assert_eq!(
            events[1].evaluator_id.as_deref(),
            Some(PACKET_EXPIRY_EVALUATOR_ID)
        );
        let gate = eng
            .cli_dispatch(
                "why packet-chains",
                &mind_types::AccessContext::operator_audit(),
            )
            .await;
        assert!(
            gate.contains("PACKET CHAIN COMPLETENESS — 1/1 latest packet lifecycle(s) complete"),
            "the public command must read the verified lifecycle end to end:\n{gate}"
        );
        assert_eq!(mind_observability::verify_log(&path), Ok(2));

        // Aggregate gates must fail closed when a parseable event is appended outside the chain.
        // Raw trace reconstruction remains permissive for forensics; promotion metrics do not.
        {
            use std::io::Write as _;
            let forged = serde_json::json!({
                "chain": "forged",
                "event": mind_observability::DecisionEvent::new("forged", "packet_resolved")
            });
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .expect("scratch decision log should remain appendable");
            writeln!(file, "{forged}").expect("forged test tail should be written");
        }
        let refused = eng
            .cli_dispatch(
                "why packet-chains",
                &mind_types::AccessContext::operator_audit(),
            )
            .await;
        assert!(
            refused.starts_with("DECISION ANALYTICS UNAVAILABLE"),
            "packet analytics must not compute through a forged tail:\n{refused}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn packet_trigger_authority_is_atomic_across_store_and_recorder() {
        let (eng, path) = engine_with_recorder("pkt_authority");
        let id = eng
            .packet_add_told(
                "node:promise",
                None,
                "plan",
                "Promised follow-up",
                "prepared body",
                "the owner explicitly asked",
                vec!["promise-evidence".into()],
                0.9,
                false,
                i64::MAX,
            )
            .await;

        let stored = eng.load_packets().await;
        let packet = stored
            .iter()
            .find(|packet| packet.get("id").and_then(|value| value.as_str()) == Some(&id))
            .expect("the told packet must be persisted");
        assert_eq!(
            packet
                .get("trigger_provenance")
                .and_then(|value| value.as_str()),
            Some("told")
        );
        let events = eng.recorder().read_trace(&id);
        assert_eq!(events.len(), 1);
        assert!(
            events[0]
                .policy
                .iter()
                .any(|policy| policy.contains("provenance=told")),
            "recorder and packet store must agree on trigger authority: {:?}",
            events[0].policy
        );
        assert_eq!(mind_observability::verify_log(&path), Ok(1));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn confirmed_packet_decision_is_idempotent() {
        let (eng, path) = engine_with_recorder("pkt_idempotent");
        let id = eng
            .packet_add(
                "node:idempotent",
                None,
                "plan",
                "One owner decision",
                "prepared body",
                "ready for review",
                vec![],
                0.8,
                true,
                i64::MAX,
            )
            .await;

        assert!(eng
            .packet_decide("1", true, "looks right")
            .await
            .contains("Confirmed"));
        let repeated = eng.packet_decide("1", true, "again").await;
        assert!(repeated.contains("Already confirmed"), "{repeated}");
        let reversed = eng.packet_decide("1", false, "changed my mind").await;
        assert!(reversed.contains("Already confirmed"), "{reversed}");

        let events = eng.recorder().read_trace(&id);
        assert_eq!(
            events.len(),
            2,
            "one creation and exactly one terminal decision"
        );
        assert_eq!(events[1].kind, "packet_resolved");
        assert_eq!(events[1].verdict.as_deref(), Some("confirmed"));
        assert_eq!(mind_observability::verify_log(&path), Ok(2));
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn packet_confidence_is_normalized_in_store_and_recorder() {
        let (eng, path) = engine_with_recorder("pkt_confidence");
        let id = eng
            .packet_add(
                "node:confidence",
                None,
                "plan",
                "Conservative confidence",
                "prepared body",
                "invalid producer input",
                vec![],
                f64::NAN,
                false,
                i64::MAX,
            )
            .await;

        let packets = eng.load_packets().await;
        let stored = packets
            .iter()
            .find(|packet| packet.get("id").and_then(|value| value.as_str()) == Some(&id))
            .expect("packet must be stored");
        assert_eq!(
            stored.get("confidence").and_then(|value| value.as_f64()),
            Some(0.0)
        );

        let events = eng.recorder().read_trace(&id);
        assert_eq!(events[0].confidence, Some(0.0));
        assert_eq!(normalize_packet_confidence(-0.1), 0.0);
        assert_eq!(normalize_packet_confidence(1.1), 1.0);
        assert_eq!(mind_observability::verify_log(&path), Ok(1));
        let _ = std::fs::remove_file(&path);
    }
}
