//! PHASE 3A RED ORACLE (`docs/PHASE3_WORLD_STATE_V1.md`) — BASELINE CAPTURE ONLY.
//!
//! By contract this file contains NO world-model implementation. It declares an independent
//! ground truth (dumb oracle: explicit expected states per checkpoint, E3) over an adversarial
//! event stream, then asks TODAY'S system the standing questions through the only doors that
//! exist (memory recall, task store, profile KV). Every dimension it cannot represent is
//! recorded as UNREPRESENTABLE — that failure record IS the Phase-3 baseline.
//!
//! Gated behind `YM_WORLD_3A=1` so the promotion suite stays green until 3A earns green.
//! Run: `$env:YM_WORLD_3A="1"; cargo test -p mind-evals world_oracle -- --nocapture`

use mind_memory::MemoryHandle;
use mind_types::MemoryFacade;


// ── dumb oracle vocabulary ───────────────────────────────────────────────────

#[derive(Clone, Copy, Debug)]
pub enum Kind { Assert, Supersede, Retract, Expire }

#[derive(Clone)]
pub struct WEvent {
    pub id: &'static str,        // stable source_event_id (I6)
    pub source: &'static str,    // email:71 / calendar:92 / carrier:771 ...
    pub kind: Kind,
    pub occurred_at: i64,        // world time (A1)
    pub observed_at: i64,        // knowledge time (A1)
    pub entity: &'static str,
    pub attr: &'static str,
    pub value: &'static str,
}

#[derive(Clone)]
pub enum Val {
    Known(&'static str),
    Unknown,
    Conflicted(&'static [&'static str]), // preserved, never ranked away (I4)
    Stale(&'static str),
    Expired,
}

#[derive(Clone)]
pub struct Expect {
    pub valid_at: i64, // world-time cut (I2)
    pub known_at: i64, // knowledge-time cut (I2)
    pub entity: &'static str,
    pub attr: &'static str,
    pub want: Val,
}

const H: i64 = 3_600_000;
const D: i64 = 86_400_000;
fn day(n: u32, hour: i64) -> i64 { 1_787_400_000_000 + n as i64 * D + hour * H }

/// THE ADVERSARIAL STREAM — duplicates, late events, out-of-order arrival, corrections,
/// contradictions, retraction-with-dependents, expiry, ambiguity. Restart is exercised by the
/// driver (fresh engine mid-stream); snapshot-loss is N/A today (nothing materializes).
fn scenario() -> (Vec<WEvent>, Vec<Expect>) {
    let e = |id, src, k, occ, obs, ent, att, v| WEvent { id, source: src, kind: k, occurred_at: occ, observed_at: obs, entity: ent, attr: att, value: v };
    let events = vec![
        // week 1 — interview arc (reschedule + LATE stale email + corroboration)
        e("email:501", "email:501", Kind::Assert, day(20, 9), day(20, 10), "interview", "date", "Tuesday"),
        e("email:501b", "email:501", Kind::Assert, day(20, 9), day(20, 10), "interview", "date", "Tuesday"), // DUPLICATE ingestion
        e("calendar:88", "calendar:88", Kind::Assert, day(21, 8), day(21, 8), "interview", "date", "Tuesday"), // corroboration, NOT duplicate
        e("email:923", "email:923", Kind::Supersede, day(22, 14), day(22, 15), "interview", "date", "Thursday"),
        e("email:old-late", "email:502", Kind::Assert, day(20, 11), day(23, 9), "interview", "date", "Tuesday"), // LATE old info arrives AFTER supersession
        // package arc (delay learned late; conflict; Saturday reality overrides Monday ETA)
        e("carrier:771", "carrier:771", Kind::Assert, day(20, 6), day(22, 12), "package", "status", "delayed-until-Monday"), // occurred BEFORE decision-D below, learned AFTER
        e("email:eta", "email:eta", Kind::Supersede, day(24, 10), day(24, 10), "package", "status", "maybe-Saturday"),
        e("scan:deliv", "carrier:771", Kind::Supersede, day(24, 16), day(24, 17), "package", "status", "delivered-Saturday"), // named rule wins (I4)
        // Alice document arc (obligation → overdue → CONFLICTED claim vs inbox absence)
        e("chat:alice", "chat:alice", Kind::Assert, day(21, 12), day(21, 12), "alice.document", "status", "promised-by-Wednesday"),
        e("inbox:none", "inbox:sweep", Kind::Retract, day(25, 17), day(25, 18), "alice.document", "status", "no-document-received"),
        e("email:alice-claims", "email:955", Kind::Assert, day(25, 16), day(25, 19), "alice.document", "status", "sent-yesterday"), // CONTRADICTS inbox sweep
        // meeting-location contradiction (unresolved)
        e("email:loc1", "email:961", Kind::Assert, day(24, 9), day(24, 9), "meeting", "location", "Room4"),
        e("chat:zoom", "chat:962", Kind::Assert, day(24, 15), day(24, 15), "meeting", "location", "Zoom"),
        // weather staleness
        e("api:wx", "weather:api", Kind::Assert, day(22, 7), day(22, 7), "weather.thursday", "forecast", "rain"),
        // derived-dependent invalidation case: flight overlap exists ONLY while interview=Thursday
        e("cal:flight", "calendar:90", Kind::Assert, day(21, 8), day(21, 8), "flight", "window", "Thursday-1300-1600"),
        e("cal:flightx", "calendar:91", Kind::Expire, day(25, 8), day(25, 8), "flight", "window", "cancelled"),

        // ═══ W7 ADVERSARIAL MONTH — arcs INTERACT (days 25–28); restart marker below ═══

        // Alice document arc CONTINUES the week-one promise chain (upload → withdrawn →
        // deadline passes → [RESTART] → confirmation arrives after the window closed)
        e("cloud:up1", "cloud-drive", Kind::Assert, day(26, 9), day(26, 9), "alice.document", "status", "attachment-uploaded"),
        e("cloud:up2", "cloud-drive", Kind::Retract, day(26, 11), day(26, 14), "alice.document", "status", "upload-withdrawn"),
        e("ops:deadline", "ops", Kind::Expire, day(27, 17), day(27, 17), "alice.document", "status", "window-closed"),
        // *** RESTART happens here in the driver (resumed log splits at ops:deadline) ***
        e("chat:alice2", "chat:alice", Kind::Assert, day(28, 10), day(28, 10), "alice.document", "status", "confirmed-received-by-ops"),

        // Compound derivation chain A+B→C→E: visa+passport→clear; clear+booking→go.
        // Passport later SUPERSEDED to expired — E must lose warrant THROUGH C.
        e("portal:vs", "portal", Kind::Assert, day(22, 9), day(22, 9), "visa", "status", "submitted"),
        e("rec:pp", "records", Kind::Assert, day(22, 9), day(22, 9), "passport", "status", "valid"),
        e("rec:pp2", "records", Kind::Supersede, day(26, 10), day(26, 10), "passport", "status", "expired-November"),
        e("air:bk", "airline", Kind::Assert, day(23, 8), day(23, 8), "itinerary", "status", "held"),

        // Knowledge-time conflict history: conflict itself is bitemporal
        e("email:1201", "email:1201", Kind::Assert, day(26, 8), day(26, 9), "venue", "room", "Room4"),
        e("chat:1305", "chat:1305", Kind::Assert, day(26, 10), day(26, 11), "venue", "room", "Zoom"),
        e("chat:1305", "chat:1305", Kind::Assert, day(26, 10), day(26, 12), "venue", "room", "Zoom"), // DUPLICATE redelivery

        // Authority case: a LATE WEAKER email must NOT resurrect Monday after delivered scan
        e("email:eta-old", "email:987", Kind::Assert, day(24, 9), day(26, 13), "package", "status", "Monday-ETA-recheck"),
        e("carrier:771c", "carrier:771", Kind::Assert, day(20, 6), day(26, 15), "package", "status", "delayed-until-Monday"), // original claim re-asserted VERY late, new id

        // Invoice corroboration + duplicate idempotency under messiness
        e("erp:401", "erp:401", Kind::Assert, day(25, 9), day(25, 9), "invoice.march", "total", "12400"),
        e("mail:copy", "mail-room", Kind::Assert, day(25, 10), day(25, 15), "invoice.march", "total", "12400"), // distinct witness
        e("erp:401", "erp:401", Kind::Assert, day(25, 9), day(26, 9), "invoice.march", "total", "12400"),      // SAME id redelivered next day

        // Expense report supersession chain
        e("fin:er1", "finance", Kind::Assert, day(22, 10), day(22, 10), "expense.april", "status", "submitted"),
        e("fin:er2", "finance", Kind::Supersede, day(24, 10), day(24, 12), "expense.april", "status", "approved"),
        e("fin:er3", "finance", Kind::Supersede, day(27, 9), day(27, 10), "expense.april", "status", "paid"),

        // Cold-chain out-of-order arrivals: t2 occurred AFTER t1 but was OBSERVED EARLIER;
        // backup sensor corroborates 9C; monitor ends
        e("iot:t1", "sensor-7", Kind::Assert, day(25, 6), day(25, 12), "coldchain", "last-reading", "4C"),
        e("iot:t2", "sensor-7", Kind::Assert, day(25, 14), day(25, 13), "coldchain", "last-reading", "9C"),
        e("iot:t2b", "sensor-backup", Kind::Assert, day(25, 14), day(25, 15), "coldchain", "last-reading", "9C"),
        e("iot:t3", "sensor-7", Kind::Assert, day(25, 18), day(25, 20), "coldchain", "last-reading", "5C"),
        e("iot:exc", "sensor-7", Kind::Expire, day(26, 8), day(26, 8), "coldchain", "last-reading", "monitor-ended"),

        // Credential rotation: late old-key info must not resurrect; revoke; re-issue post-restart
        e("iam:k1", "iam", Kind::Assert, day(20, 8), day(20, 8), "deploy-key", "status", "active-v1"),
        e("iam:k2", "iam", Kind::Supersede, day(23, 8), day(23, 9), "deploy-key", "status", "rotated-v2"),
        e("iam:k1-late", "iam-old", Kind::Assert, day(20, 8), day(24, 9), "deploy-key", "status", "active-v1"),
        e("iam:k3", "security", Kind::Retract, day(26, 16), day(26, 17), "deploy-key", "status", "revoked"),
        e("iam:k4", "iam", Kind::Assert, day(28, 8), day(28, 8), "deploy-key", "status", "issued-v3"),

        // Payroll: pending → cleared with SMS corroboration
        e("bank:pd", "bank", Kind::Assert, day(24, 6), day(24, 11), "payroll.june", "status", "pending"),
        e("bank:pd2", "bank", Kind::Supersede, day(25, 7), day(25, 9), "payroll.june", "status", "cleared"),
        e("sms:pd", "sms-gw", Kind::Assert, day(25, 7), day(25, 8), "payroll.june", "status", "cleared"),

        // Upgrade waitlist: promotion CONTRADICTED by desk agent — no rule resolves it, ever
        e("air:wl", "airline", Kind::Assert, day(22, 12), day(22, 12), "seat.upgrade", "status", "waitlisted"),
        e("air:pm", "airline", Kind::Supersede, day(24, 8), day(24, 8), "seat.upgrade", "status", "promoted"),
        e("agent:sc", "desk-agent", Kind::Assert, day(24, 9), day(24, 15), "seat.upgrade", "status", "not-on-manifest"),

        // Medication refill: ready, then shelf life expires
        e("rx:1", "pharmacy", Kind::Assert, day(21, 9), day(21, 9), "rx.metformin", "refill", "ready"),
        e("rx:2", "pharmacy", Kind::Expire, day(25, 18), day(25, 18), "rx.metformin", "refill", "shelf-life-ended"),

        // DNS TTL staleness then refresh (staleness is per-winner, judged vs known_at)
        e("dns:a1", "resolver", Kind::Assert, day(22, 8), day(22, 8), "dns.api-endpoint", "record", "203.0.113.10"),
        e("dns:a2", "resolver", Kind::Supersede, day(25, 10), day(25, 10), "dns.api-endpoint", "record", "203.0.113.99"),

        // Third-source Thursday interview confirmation — arrives AFTER the restart point
        e("sms:i2", "sms:776", Kind::Assert, day(26, 9), day(26, 9), "interview", "date", "Thursday"),

        // Office/facilities churn
        e("hr:badge", "facilities", Kind::Assert, day(21, 8), day(21, 10), "office.badge-reader", "status", "online"),
        e("hr:badge2", "facilities", Kind::Supersede, day(23, 8), day(23, 8), "office.badge-reader", "status", "offline-maintenance"),
        e("hr:badge3", "facilities", Kind::Supersede, day(24, 8), day(24, 8), "office.badge-reader", "status", "online"),
        e("net:vpn", "netops", Kind::Assert, day(25, 9), day(25, 20), "vpn.gateway", "status", "degraded"), // late observation
        e("net:vpn2", "netops", Kind::Assert, day(26, 9), day(26, 9), "vpn.gateway", "status", "healthy"),

        // Misc world texture with real semantics: strike warned then called off; forecast updated
        e("news:strike", "newsfeed", Kind::Assert, day(22, 9), day(22, 9), "transit.metro", "status", "strike-warning"),
        e("news:strike2", "newsfeed", Kind::Retract, day(24, 9), day(24, 10), "transit.metro", "status", "strike-called-off"),
        e("wx:fri", "weather:api", Kind::Assert, day(26, 7), day(26, 7), "weather.friday", "forecast", "sunny"),
        e("wx:fri2", "weather:api", Kind::Supersede, day(27, 6), day(27, 6), "weather.friday", "forecast", "cloudy"),
    ];
    let expects = vec![
        // t≈after event idx 3 (before supersession lands): interview Known Tuesday
        Expect { valid_at: day(21, 12), known_at: day(21, 12), entity: "interview", attr: "date", want: Val::Known("Tuesday") },
        // BI-TEMPORAL NON-LEAKAGE (the A1 test): delay existed Aug-20-world-time but was learned Aug-22.
        Expect { valid_at: day(20, 12), known_at: day(20, 12), entity: "package", attr: "status", want: Val::Unknown },
        Expect { valid_at: day(20, 12), known_at: day(22, 13), entity: "package", attr: "status", want: Val::Known("delayed-until-Monday") },
        // after supersession + late-old-email: Thursday holds; Tuesday must NOT resurrect
        Expect { valid_at: day(23, 10), known_at: day(23, 10), entity: "interview", attr: "date", want: Val::Known("Thursday") },
        // delivery chain end: delivered-Saturday wins over ETA guess (named resolution rule)
        Expect { valid_at: day(25, 9), known_at: day(25, 9), entity: "package", attr: "status", want: Val::Known("delivered-Saturday") },
        // Alice document: inbox-sweep retraction vs later sent-yesterday claim ⇒ CONFLICTED (never ranked)
        Expect { valid_at: day(25, 20), known_at: day(25, 20), entity: "alice.document", attr: "status", want: Val::Conflicted(&["no-document-received", "sent-yesterday"]) },
        // meeting location stays Conflicted
        Expect { valid_at: day(25, 9), known_at: day(25, 9), entity: "meeting", attr: "location", want: Val::Conflicted(&["Room4", "Zoom"]) },
        // weather: 3 days old ⇒ Stale
        Expect { valid_at: day(25, 9), known_at: day(25, 9), entity: "weather.thursday", attr: "forecast", want: Val::Stale("rain") },
        // purpose-denied: a member ctx must not see primary-private entities (A6/I5) — represented here
        // as Unknown-for-that-caller even though Known for the operator.
        Expect { valid_at: day(25, 9), known_at: day(25, 9), entity: "interview", attr: "date", want: Val::Unknown }, // under member ctx
    ];
    (events, expects)
}

// ── probing TODAY'S system (the only honest way to score it) ────────────────

async fn probe(mem: &MemoryHandle, expect: &Expect) -> &'static str {
    // Best-effort doors that exist today: semantic belief recall + transcript. There is no world
    // query, no bi-temporal cut, no conflicted/stale representation, no purpose-scoped entity read.
    let needle = format!("{} {}", expect.entity.replace('.', " "), expect.attr.replace('.', " "));
    let ctx = mind_types::AccessContext::operator_audit();
    match mem.beliefs_matching_n(&needle, 3, &ctx).await {
        Ok(hits) if !hits.is_empty() => "FOUND_VIA_RECALL_BUT_NO_STATE_SEMANTICS",
        _ => "UNREPRESENTABLE",
    }
}

/// THE BASELINE RUN. Fails by design while 3A is unbuilt; its rendered failures are the record.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn phase3a_red_baseline() {
    if std::env::var("YM_WORLD_3A").as_deref() != Ok("1") {
        println!("WORLD-ORACLE: gated (set YM_WORLD_3A=1 to capture the red baseline)");
        return;
    }
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();

    let (events, expects) = scenario();
    // Feed events through the ONLY ingestion door today: transcript appends (this IS part of the
    // finding — the mind has no event-ingestion seam for typed world transitions).
    for ev in &events {
        let line = format!("[{}] {} says {} {} = {} (occurred {:?})", ev.source, ev.source, ev.entity, ev.attr, ev.value, ev.kind);
        let _ = mem.append_message_scoped("user", &line, mind_types::Scope::Private("primary".into())).await;
    }
    // RESTART leg: a second handle on the same path would be the persistence probe; :memory: makes
    // it impossible TODAY — itself a baseline finding (recorded below).

    // ── W1 SEMANTICS, scored individually through the temporal spine ─────────────────────────
    let world_events: Vec<mind_world::WorldEvent> = events.iter().map(|e| mind_world::WorldEvent {
        source_event_id: e.id.to_string(), source_id: e.source.split(':').next().unwrap_or("?").to_string(),
        kind: match e.kind { Kind::Assert => mind_world::Kind::Assert, Kind::Supersede => mind_world::Kind::Supersede, Kind::Retract => mind_world::Kind::Retract, Kind::Expire => mind_world::Kind::Expire },
        occurred_at: e.occurred_at, observed_at: e.observed_at,
        entity: e.entity.to_string(), attr: e.attr.to_string(), value: e.value.to_string(),
    }).collect();
    // POLICY IS CONFIGURATION, NOT DATA: a restarted world must re-apply rules, freshness
    // and derivations — replay alone restores history, not the lens it is read through.
    let build_log = |events: &[mind_world::WorldEvent]| {
        mind_world::WorldLog::replay(events)
        .with_freshness_ms(48 * 3_600_000)
        // W3 named rule — the authority case needs it REGISTERED to resolve carrier-vs-estimate
        .with_rule(mind_world::ResolutionRule {
            id: "carrier-delivered-scan-overrides-estimate",
            version: 1,
            apply: Box::new(|claims: &[mind_world::Claim]| {
                claims.iter().find(|c| c.source_id == "carrier" && c.value.starts_with("delivered"))
                    .map(|c| c.value.to_string())
            }),
        })
        .with_derivation(mind_world::DerivationRule {
            id: "overlap-rule",
            version: 1,
            entity: "travel_conflict".into(),
            attr: "status".into(),
            consumes: vec![("interview".into(), "date".into()), ("flight".into(), "window".into())],
            produce: Box::new(|inputs: &[Option<&mind_world::StateAt>]| {
                match (inputs[0], inputs[1]) {
                    (Some(mind_world::StateAt::Known(i)), Some(mind_world::StateAt::Known(_))) if i.contains("Thursday") => {
                        Some("Thursday-travel-conflict".into())
                    }
                    _ => None,
                }
            }),
        })
        // W7 compound chain: visa+passport→clear, then clear+booking→go (two hops).
        .with_derivation(mind_world::DerivationRule {
            id: "visa-clear",
            version: 1,
            entity: "visa_status".into(),
            attr: "status".into(),
            consumes: vec![("visa".into(), "status".into()), ("passport".into(), "status".into())],
            produce: Box::new(|inputs: &[Option<&mind_world::StateAt>]| {
                match (inputs[0], inputs[1]) {
                    (Some(mind_world::StateAt::Known(v)), Some(mind_world::StateAt::Known(p)))
                        if v == "submitted" && p == "valid" => Some("clear".into()),
                    _ => None,
                }
            }),
        })
        .with_derivation(mind_world::DerivationRule {
            id: "trip-ready",
            version: 1,
            entity: "trip_ready".into(),
            attr: "status".into(),
            consumes: vec![("visa_status".into(), "status".into()), ("itinerary".into(), "status".into())],
            produce: Box::new(|inputs: &[Option<&mind_world::StateAt>]| {
                match (inputs[0], inputs[1]) {
                    (Some(mind_world::StateAt::Known(v)), Some(mind_world::StateAt::Known(b)))
                        if v == "clear" && b == "held" => Some("go".into()),
                    _ => None,
                }
            }),
        })
        // Interaction arc: dossier completeness needs interview=Thursday AND expense paid
        .with_derivation(mind_world::DerivationRule {
            id: "dossier-rule",
            version: 1,
            entity: "travel.dossier".into(),
            attr: "status".into(),
            consumes: vec![("interview".into(), "date".into()), ("expense.april".into(), "status".into())],
            produce: Box::new(|inputs: &[Option<&mind_world::StateAt>]| {
                match (inputs[0], inputs[1]) {
                    (Some(mind_world::StateAt::Known(d)), Some(mind_world::StateAt::Known(e)))
                        if d.contains("Thursday") && e == "paid" =>
                        Some("complete".into()),
                    _ => None,
                }
            }),
        })
        .with_gate(Box::new(|ctx: &mind_types::AccessContext, _entity: &str| {
            ctx.purpose().label().starts_with("audit") // W5 wall: audit-only world reads
        }))
    };
    let log = build_log(&world_events);
    let interview_rows: Vec<_> = log.transitions().iter().filter(|t| t.entity == "interview").collect();
    let duplicate_id_green = interview_rows.iter().filter(|t| t.source_event_id == "email:501").count() == 1;
    // E2: corroboration = DISTINCT SOURCES preserved as separate rows for one proposition
    // (the late stale email is a third independent witness — nothing may collapse them).
    let tuesday_sources: std::collections::HashSet<&str> = interview_rows
        .iter()
        .filter(|t| t.value == "Tuesday")
        .map(|t| t.source_id.as_str())
        .collect();
    let corroboration_green = tuesday_sources.contains("email") && tuesday_sources.contains("calendar");
    let mut rows = Vec::new();
    for x in &expects {
        let got = probe(&mem, x).await;
        let verdict = match (&x.want, got) {
            (_, "UNREPRESENTABLE") => "FAIL:UNREPRESENTABLE",
            (Val::Known(v), "FOUND_VIA_RECALL_BUT_NO_STATE_SEMANTICS") => {
                // Recall proves memory-of-statement, not current-state semantics; still counts
                // against precision-first because validity/conflict/stale cuts are absent.
                "WEAK:RECALL_ONLY"
            }
            _ => "WEAK:RECALL_ONLY",
        };
        rows.push(format!(
            "  [valid@{:?} known@{:?}] {}.{} want={:?} -> {}",
            x.valid_at, x.known_at, x.entity, x.attr, match &x.want { Val::Known(v) => v.to_string(), Val::Unknown => "UNKNOWN".into(), Val::Conflicted(c) => format!("{c:?}"), Val::Stale(v) => format!("STALE({v})"), Val::Expired => "EXPIRED".into() }, verdict
        ));
    }
    // ═══ W7 ADVERSARIAL MONTH — restart leg + INTERACTING trajectory fixtures ═══════════════
    // Every boolean below is a dumb-oracle expectation over the SAME shared stream; arcs cross
    // (sms refresh un-stales the interview which re-warrants the dossier, etc.). Machinery is
    // FROZEN: a red here must be classified oracle-error vs implementation-error vs missing
    // semantic BEFORE any code changes.
    let opq = |v: i64| mind_world::WorldQuery { valid_at: v, known_at: v, access: mind_types::AccessContext::operator_audit() };
    let member_q = |v: i64| mind_world::WorldQuery {
        valid_at: v, known_at: v,
        access: mind_types::AccessContext::principal(mind_types::Scope::Private("asha".into()), mind_types::Purpose::conversation("asha")),
    };
    let attr_of = |e: &str| if e == "interview" { "date" } else { "status" };
    use mind_world::StateAt;

    // RESTART mid-stream at ops:deadline: resumed (split ingest) vs uninterrupted replay.
    let restart_idx = events.iter().position(|ev| ev.id == "ops:deadline").unwrap();
    let mut resumed = build_log(&world_events[..restart_idx]);
    for we in &world_events[restart_idx..] {
        resumed.ingest(we);
    }
    let canon = |l: &mind_world::WorldLog| {
        let mut r: Vec<(i64, i64, String)> = l.transitions().iter()
            .map(|t| (t.occurred_at, t.observed_at, format!("{}|{:?}|{}", t.source_event_id, t.kind, t.value)))
            .collect();
        r.sort();
        r.iter().map(|x| format!("{}|{}|{}", x.0, x.1, x.2)).collect::<Vec<_>>().join(";")
    };
    let replay_projection_equal = canon(&resumed) == canon(&log);
    let answers_agree_across_restart = [day(23, 10), day(25, 12), day(26, 12), day(27, 18), day(28, 11)]
        .iter()
        .all(|c| {
            ["interview", "alice.document", "visa_status", "trip_ready", "package", "venue"].iter().all(|e| {
                let (a, c) = (attr_of(e), opq(*c));
                log.state_at(e, a, &c) == resumed.state_at(e, a, &c)
                    && log.derived_state(e, &c) == resumed.derived_state(e, &c)
            })
        });

    // Alice document arc — honest open-world semantics: week-one claims were never superseded,
    // so the upload JOINS a conflict instead of replacing it; the global retract still wins
    // while absolutely latest; expiry ends the window; post-expiry confirmation re-opens it.
    let alice_upload = matches!(
        log.state_at("alice.document", "status", &opq(day(26, 10))),
        StateAt::Conflicted(ref c) if c.len() == 3 && c.contains(&"attachment-uploaded".to_string())
    );
    let alice_withdrawn = matches!(log.state_at("alice.document", "status", &opq(day(26, 15))), StateAt::Unknown);
    let alice_expired = log.state_at("alice.document", "status", &opq(day(27, 18))) == StateAt::Expired;
    let alice_revived_post_expiry = matches!(
        log.state_at("alice.document", "status", &opq(day(28, 11))),
        StateAt::Conflicted(ref c) if c.contains(&"confirmed-received-by-ops".to_string())
    );
    // Dossier interacts across arcs: interview freshness (refreshed by sms:i2) + expense state.
    // Stale interview refuses warrant; paid expense + fresh Thursday completes it; survives restart.
    let dossier_lifecycle_ok = matches!(log.derived_state("travel.dossier", &opq(day(25, 12))), StateAt::Unknown)
        && matches!(log.derived_state("travel.dossier", &opq(day(26, 12))), StateAt::Unknown)
        && log.derived_state("travel.dossier", &opq(day(27, 11))) == StateAt::Known("complete".into())
        // continuity through the restart window, while the Thursday witness is still fresh
        && log.derived_state("travel.dossier", &opq(day(27, 13))) == StateAt::Known("complete".into());

    // Compound chain A+B→C→E with B superseded: E loses warrant THROUGH C, never directly cached
    let compound_two_hop = log.derived_state("trip_ready", &opq(day(23, 10))) == StateAt::Known("go".into());
    let compound_zombie = matches!(log.derived_state("visa_status", &opq(day(26, 12))), StateAt::Unknown)
        && matches!(log.derived_state("trip_ready", &opq(day(26, 12))), StateAt::Unknown)
        && log.derived_state("trip_ready", &opq(day(23, 10))) == StateAt::Known("go".into());

    // Conflict itself is bitemporal: Known(Room4) at 09:30 knowledge, Conflicted by 11:30, forever after
    let venue_evolution_ok =
        log.state_at("venue", "room", &mind_world::WorldQuery { valid_at: day(26, 9) + 1_800_000, known_at: day(26, 9) + 1_800_000, access: mind_types::AccessContext::operator_audit() })
            == StateAt::Known("Room4".into())
        && matches!(
            log.state_at("venue", "room", &mind_world::WorldQuery { valid_at: day(26, 11) + 1_800_000, known_at: day(26, 11) + 1_800_000, access: mind_types::AccessContext::operator_audit() }),
            StateAt::Conflicted(ref c) if c.as_slice() == ["Room4", "Zoom"]
        )
        && matches!(log.state_at("venue", "room", &opq(day(27, 18))), StateAt::Conflicted(_));
    let conflict_persistence_ok = matches!(
        log.state_at("seat.upgrade", "status", &opq(day(25, 9))),
        StateAt::Conflicted(ref c) if c.len() == 2
    );

    // Authority: late weak sources must NOT resurrect superseded values (two different arcs)
    let authority_no_resurrection = log.state_at("package", "status", &opq(day(26, 14))) == StateAt::Known("delivered-Saturday".into());
    let iam_no_resurrection = log.state_at("deploy-key", "status", &opq(day(24, 10))) == StateAt::Known("rotated-v2".into())
        && matches!(log.state_at("deploy-key", "status", &opq(day(27, 9))), StateAt::Unknown)
        && log.state_at("deploy-key", "status", &opq(day(28, 9))) == StateAt::Known("issued-v3".into()); // revoke then re-issue revives

    // Duplicate idempotency under messiness (three arcs, ids re-delivered days apart)
    let tr = log.transitions();
    let dup_ids_clean = tr.iter().filter(|t| t.source_event_id == "erp:401").count() == 1
        && tr.iter().filter(|t| t.source_event_id == "chat:1305").count() == 1
        && tr.iter().filter(|t| t.source_event_id == "email:501").count() == 1;
    let corroborations_rich = log.state_at("invoice.march", "total", &opq(day(26, 10))) == StateAt::Known("12400".into())
        && log.state_at("payroll.june", "status", &opq(day(25, 10))) == StateAt::Known("cleared".into())
        && log.state_at("interview", "date", &opq(day(26, 10))) == StateAt::Known("Thursday".into()); // third source refreshes

    // Historical cuts across many arcs (bitemporal breadth beyond week one)
    let hist_legs_ok = log.state_at("expense.april", "status", &opq(day(23, 9))) == StateAt::Known("submitted".into())
        && log.state_at("expense.april", "status", &opq(day(26, 9))) == StateAt::Known("approved".into())
        && log.state_at("expense.april", "status", &opq(day(27, 11))) == StateAt::Known("paid".into())
        && log.state_at("transit.metro", "status", &opq(day(23, 8))) == StateAt::Known("strike-warning".into())
        && matches!(log.state_at("transit.metro", "status", &opq(day(25, 9))), StateAt::Unknown)
        && log.state_at("office.badge-reader", "status", &opq(day(21, 12))) == StateAt::Known("online".into())
        && log.state_at("office.badge-reader", "status", &opq(day(23, 12))) == StateAt::Known("offline-maintenance".into())
        && log.state_at("office.badge-reader", "status", &opq(day(24, 12))) == StateAt::Known("online".into())
        && log.state_at("vpn.gateway", "status", &opq(day(25, 21))) == StateAt::Known("degraded".into())
        && log.state_at("vpn.gateway", "status", &opq(day(26, 10))) == StateAt::Known("healthy".into())
        && log.state_at("weather.friday", "forecast", &opq(day(27, 8))) == StateAt::Known("cloudy".into());

    // Staleness then DNS refresh (per-winner freshness judged against known_at)
    let dns_stale_then_refresh_ok = matches!(
        log.state_at("dns.api-endpoint", "record", &opq(day(25, 9))),
        StateAt::Stale { ref value, .. } if value == "203.0.113.10"
    ) && log.state_at("dns.api-endpoint", "record", &opq(day(25, 11))) == StateAt::Known("203.0.113.99".into());

    // Expiry lifecycles: refill ready→expired; coldchain out-of-order arrivals→monitor ended
    let expiry_lifecycle_ok = log.state_at("rx.metformin", "refill", &opq(day(23, 8))) == StateAt::Known("ready".into())
        && log.state_at("rx.metformin", "refill", &opq(day(26, 9))) == StateAt::Expired
        && log.state_at("coldchain", "last-reading", &opq(day(25, 19))) == StateAt::Known("9C".into())
        // corroboration DISSOLVES into conflict when witnesses diverge: primary moves to 5C,
        // backup still says 9C — open-world honesty demands Conflicted, not silent newest-wins
        && matches!(
            log.state_at("coldchain", "last-reading", &opq(day(25, 21))),
            StateAt::Conflicted(ref c) if c.len() == 2
        )
        && log.state_at("coldchain", "last-reading", &opq(day(26, 9))) == StateAt::Expired;

    // Purpose wall holds for RAW and DERIVED reads alike; audit sees truth at the same cuts
    let member_denials_broad = matches!(log.state_at("visa", "status", &member_q(day(23, 10))), StateAt::Unknown)
        && matches!(log.derived_state("trip_ready", &member_q(day(23, 10))), StateAt::Unknown)
        && matches!(log.state_at("alice.document", "status", &member_q(day(26, 10))), StateAt::Unknown)
        && log.derived_state("trip_ready", &opq(day(23, 10))) == StateAt::Known("go".into());

    let lineage_ok = log.lineage_of("travel.dossier").map(|(id, v, _)| id == "dossier-rule" && v == 1).unwrap_or(false)
        && log.lineage_of("trip_ready").map(|(id, _, cons)| id == "trip-ready" && cons.len() == 2).unwrap_or(false)
        && log.lineage_of("no-such-derived-entity").is_none();

    let replay_equal = replay_projection_equal && answers_agree_across_restart;

    // per-leg diagnostics: name every failing fixture so reds are CLASSIFIABLE, not guessed
    let legs: Vec<(&str, bool)> = vec![
        ("replay_projection", replay_projection_equal),
        ("answers_agree_restart", answers_agree_across_restart),
        ("alice_upload", alice_upload),
        ("alice_withdrawn", alice_withdrawn),
        ("alice_expired", alice_expired),
        ("alice_revived", alice_revived_post_expiry),
        ("dossier_lifecycle", dossier_lifecycle_ok),
        ("compound_two_hop", compound_two_hop),
        ("compound_zombie", compound_zombie),
        ("venue_evolution", venue_evolution_ok),
        ("seat_conflict_persists", conflict_persistence_ok),
        ("authority_package", authority_no_resurrection),
        ("iam_lifecycle", iam_no_resurrection),
        ("dup_ids_clean", dup_ids_clean),
        ("invoice_known", corroborations_rich),
        ("hist_legs", hist_legs_ok),
        ("dns_stale_refresh", dns_stale_then_refresh_ok),
        ("expiry_lifecycle", expiry_lifecycle_ok),
        ("member_denials_broad", member_denials_broad),
        ("lineage_ok", lineage_ok),
    ];
    for (n, v) in &legs {
        if !v {
            println!("FIXTURE-LEG {n}: RED");
        }
    }
    println!("DBGX alice@26,10={:?}", log.state_at("alice.document", "status", &opq(day(26, 10))));
    println!("DBGX pkg@26,14={:?}", log.state_at("package", "status", &opq(day(26, 14))));
    println!("DBGX inv@26,10={:?} pay@25,10={:?}", log.state_at("invoice.march", "total", &opq(day(26, 10))), log.state_at("payroll.june", "status", &opq(day(25, 10))));
    println!("DBGX iam@24,10={:?} iam@27,9={:?} iam@28,9={:?}", log.state_at("deploy-key", "status", &opq(day(24, 10))), log.state_at("deploy-key", "status", &opq(day(27, 9))), log.state_at("deploy-key", "status", &opq(day(28, 9))));
    println!("DBGX seat@25,9={:?}", log.state_at("seat.upgrade", "status", &opq(day(25, 9))));
    println!("DBGX doss[25,12]={:?} [26,12]={:?} [27,11]={:?} [27,13]={:?}",
        log.derived_state("travel.dossier", &opq(day(25, 12))), log.derived_state("travel.dossier", &opq(day(26, 12))),
        log.derived_state("travel.dossier", &opq(day(27, 11))), log.derived_state("travel.dossier", &opq(day(27, 13))));
    println!("DBGX iv@26,10={:?}", log.state_at("interview", "date", &opq(day(26, 10))));
    println!("DBGX exp@23,9={:?} metro@23,8={:?}", log.state_at("expense.april", "status", &opq(day(23, 9))), log.state_at("transit.metro", "status", &opq(day(23, 8))));
    for e in ["payroll.june", "deploy-key", "seat.upgrade"] {
        for t in tr.iter().filter(move |t| t.entity == e) {
            println!("DBGX row {e} |{}|{:?}|occ{}|obs{}|src{}", t.source_event_id, t.kind, t.occurred_at, t.observed_at, t.source_id);
        }
    }
    'restart_hunt: for c in [day(23, 10), day(25, 12), day(26, 12), day(27, 18), day(28, 11)] {
        for e in ["interview", "alice.document", "visa_status", "trip_ready", "package", "venue"] {
            let q = opq(c);
            let (a, b) = (log.state_at(e, attr_of(e), &q), resumed.state_at(e, attr_of(e), &q));
            if a != b {
                println!("DBGX restart-divergence e={e} c={c} primary={a:?} resumed={b:?}");
                break 'restart_hunt;
            }
        }
    }


    let score: Vec<(&str, bool)> = vec![
        ("DUPLICATE_ID", duplicate_id_green && dup_ids_clean),
        ("CORROBORATION", corroboration_green && corroborations_rich),
        // W2: scored through the real bi-temporal cut, not assertion.
        ("BITEMPORAL", {
            let early = log.state_at("package", "status", &mind_world::WorldQuery { valid_at: day(20, 12), known_at: day(20, 12), access: mind_types::AccessContext::operator_audit() });
            let late = log.state_at("package", "status", &mind_world::WorldQuery { valid_at: day(20, 12), known_at: day(22, 13), access: mind_types::AccessContext::operator_audit() });
            early == mind_world::StateAt::Unknown && late == mind_world::StateAt::Known("delayed-until-Monday".into())
            && hist_legs_ok
        }),
        ("SUPERSESSION", {
            log.state_at("interview", "date", &mind_world::WorldQuery { valid_at: day(23, 10), known_at: day(23, 10), access: mind_types::AccessContext::operator_audit() })
                == mind_world::StateAt::Known("Thursday".into())
            && authority_no_resurrection
            && iam_no_resurrection
        }),
        // W3: scored through the real epistemic-state semantics.
        ("CONFLICT", {
            matches!(
                log.state_at("meeting", "location", &mind_world::WorldQuery { valid_at: day(25, 9), known_at: day(25, 9), access: mind_types::AccessContext::operator_audit() }),
                mind_world::StateAt::Conflicted(ref c) if c.len() == 2
            )
            && venue_evolution_ok
            && conflict_persistence_ok
        }),
        ("STALE", {
            matches!(
                log.state_at("weather.thursday", "forecast", &mind_world::WorldQuery { valid_at: day(25, 9), known_at: day(25, 9), access: mind_types::AccessContext::operator_audit() }),
                mind_world::StateAt::Stale { .. }
            )
            && dns_stale_then_refresh_ok
        }),
        ("EXPIRY", {
            log.state_at("flight", "window", &mind_world::WorldQuery { valid_at: day(25, 9), known_at: day(25, 9), access: mind_types::AccessContext::operator_audit() })
                == mind_world::StateAt::Expired
                && log.state_at("flight", "window", &mind_world::WorldQuery { valid_at: day(21, 12), known_at: day(21, 12), access: mind_types::AccessContext::operator_audit() })
                    == mind_world::StateAt::Known("Thursday-1300-1600".into()) // inverse: before expiry it was live
            && expiry_lifecycle_ok
        }),
        ("INVALIDATION", {
            let op = || mind_types::AccessContext::operator_audit();
            // Early warrant must be read INSIDE the joint freshness window: interview turns
            // Thursday when the supersede lands (day(22,15)) and the flight goes stale 48h
            // after its day(21,8) observation — i.e. after day(23,8). day(22,18) sits in
            // [22:15, 23:08]. A stale input must NOT warrant the derived claim (precision-first).
            let warranted_early = log.derived_state(
                "travel_conflict",
                &mind_world::WorldQuery { valid_at: day(22, 18), known_at: day(22, 18), access: op() },
            ) == mind_world::StateAt::Known("Thursday-travel-conflict".into());
            // By day(25,9) the flight has expired and the interview aged out: no currently
            // warranted input pair ⇒ no conflict — yet history still answers at its own cut.
            let zombie_killed = log.derived_state(
                "travel_conflict",
                &mind_world::WorldQuery { valid_at: day(25, 9), known_at: day(25, 9), access: op() },
            ) == mind_world::StateAt::Unknown;
            let history_kept = log.state_at(
                "interview",
                "date",
                &mind_world::WorldQuery { valid_at: day(21, 12), known_at: day(21, 12), access: op() },
            ) == mind_world::StateAt::Known("Tuesday".into());
            warranted_early && zombie_killed && history_kept
                && compound_two_hop && compound_zombie
                && alice_upload && alice_withdrawn && alice_expired && alice_revived_post_expiry
                && dossier_lifecycle_ok
        }),
        ("PURPOSE", {
            // Same cut SUPERSESSION already proves fresh (day(23,10)): the member caller is
            // walled to Unknown (A6/I5) while the audit caller reads the true state.
            let member = mind_world::WorldQuery {
                valid_at: day(23, 10),
                known_at: day(23, 10),
                access: mind_types::AccessContext::principal(
                    mind_types::Scope::Private("asha".into()),
                    mind_types::Purpose::conversation("asha"),
                ),
            };
            let operator = mind_world::WorldQuery { valid_at: day(23, 10), known_at: day(23, 10), access: mind_types::AccessContext::operator_audit() };
            log.state_at("interview", "date", &member) == mind_world::StateAt::Unknown
                && log.state_at("interview", "date", &operator) == mind_world::StateAt::Known("Thursday".into())
                && member_denials_broad
        }),
        ("REPLAY_EQUALITY", replay_equal),
        ("LINEAGE", lineage_ok),
    ];
    let green = score.iter().filter(|(_, g)| *g).count();
    for (k, g) in &score {
        if !g {
            println!("DBG {k}: RED — inspect its scoreboard arm for the failing leg");
        }
    }
    let report = format!(
        "PHASE 3A SCORECARD: {}/{} GREEN\n{}\n W7 adversarial month: {} hand-authored events, restart at ops:deadline, arcs interact\n{}",
        green,
        score.len(),
        score.iter().map(|(k, g)| format!("  {:<16} {}", k, if *g { "GREEN" } else { "RED/UNREPRESENTABLE" })).collect::<Vec<_>>().join("\n"),
        events.len(),
        rows.join("\n"),
    );
    println!("{report}");
    assert_eq!(green, score.len(), "PHASE 3A adversarial month still RED ({green}/{}): classify each red as oracle-error vs implementation-error vs missing-semantic BEFORE touching machinery", score.len());



}


