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
            x.valid_at, x.known_at, x.entity, x.attr, match &x.want { Val::Known(v) => v.to_string(), Val::Unknown => "UNKNOWN".into(), Val::Conflicted(c) => format!("{c:?}"), Val::Stale(v) => format!("STALE({v})") }, verdict
        ));
    }
    let unrepresentable = rows.iter().filter(|r| r.contains("UNREPRESENTABLE")).count();
    let report = format!(
        "PHASE 3A RED BASELINE\n events fed via transcript (no typed-transition seam): {}\n RESTART leg: IMPOSSIBLE on :memory:, no durable world snapshot exists\n bi-temporal cuts: NO API\n conflicted/stale/expired representation: NONE\n purpose-scoped entity reads: beliefs only (scope wall exists; world-cut does not)\n{}\n SCORE: {}/{} expectations representable-through-existing-doors (all WEAK), {} flatly UNREPRESENTABLE\n HARD CONSTRAINTS: knowledge-time leakage UNMEASURABLE · purpose leakage at world layer UNMEASURABLE · replay divergence UNMEASURABLE",
        events.len(), rows.join("\n"), rows.len() - unrepresentable, rows.len(), unrepresentable
    );
    println!("{report}");
    // The baseline is RED by definition: assert so the run leaves a visible scar until 3A turns it green.
    panic!("PHASE 3A BASELINE IS RED BY DESIGN — see printed report; this panic retires when the world model answers these checkpoints");
}


