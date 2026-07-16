use super::*;
use mind_governance::{GovernedActionRuntime, RealHarmGate};
use mind_inference::ScriptedLLM;
use mind_memory::MemoryHandle;
use mind_tools::{ScriptedMailSender, ToolActionExecutor};
use mind_types::BeliefAssertion;
use yantrik_ml::LLMBackend;

/// PRIVACY REGRESSION GUARD (the DMN leak): the default-mode tick reads the household's stored
/// beliefs with unrestricted Operator access and puts them VERBATIM into the prompt — the associate
/// phase dumps the top-10 recalled facts. That is private-grounded inference, so it MUST take the
/// private lane first and only escalate to cloud with an audit. It used to be an unscoped `chat()`,
/// which silently routes to the Household (cloud) lane forever with no record.
///
/// The tell is structural: an unscoped `chat()` NEVER touches the escalation counter; `chat_grounded`
/// always does (it tries Private, fails on this cloud-only pool, escalates, and counts). So a moving
/// counter proves the private lane was attempted. Uses `>=` because the counter is process-global and
/// other tests may run concurrently.
#[tokio::test]
async fn dmn_tick_uses_the_private_lane_not_a_silent_cloud_call() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    // The associate phase needs >= 3 stored items to have anything to connect.
    for s in ["Priya's birthday is in March", "we are saving for a house", "Arjun started school"] {
        let _ = mem
            .remember_as_belief(BeliefAssertion {
                statement: s.into(),
                polarity: 1.0,
                weight: 1.5,
                source_event: Some("test".into()),
                provenance: "told".into(),
            })
            .await;
    }
    // A CLOUD-only pool: no provider is in the private allowlist, so a private-grounded call must
    // escalate (and be counted). "minimax" mirrors the real cloud chain's labelling.
    let pool = mind_inference::InferencePool::new(
        Arc::new(ScriptedLLM::new("A is better supported.")) as Arc<dyn LLMBackend>,
        1,
    )
    .with_provider("minimax");
    let conv = ConversationEngine::new(Arc::new(mem), pool, "JARVIS");

    let before = mind_inference::privacy_escalated_count();
    // Phase rotates rehearse(0) → reconcile(1) → associate(2). Rehearse makes no model call at all,
    // so drive all three and assert the LLM-using phases went through the private lane.
    for _ in 0..3 {
        let _ = conv.dmn_tick().await;
    }
    let after = mind_inference::privacy_escalated_count();
    assert!(
        after >= before + 1,
        "DMN made a model call carrying private beliefs without attempting the private lane \
         (escalation counter unmoved: {before} -> {after}) — this is the silent cloud leak"
    );
}

#[tokio::test]
async fn judgment_ledger_logs_grades_and_scores_brier() {
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = mind_inference::InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(mem, pool, "JARVIS");
    // well-calibrated (p=0.9 → true) + badly-miscalibrated (p=0.9 → false)
    conv.judgment_log("proactive", "engagement", "engages", 0.9, 0, "ref1").await;
    conv.judgment_log("proactive", "engagement", "engages", 0.9, 0, "ref2").await;
    conv.judgment_grade("ref1", true).await;
    conv.judgment_grade("ref2", false).await;
    let r = conv.judgment_report().await;
    assert!(r.contains("Judgment Brier"), "report: {r}");
    assert!(r.contains("2 graded"), "should show 2 graded: {r}");
    // grading is immutable — a re-grade of an already-graded ref changes nothing
    conv.judgment_grade("ref1", false).await;
    assert!(conv.judgment_report().await.contains("2 graded"));
}

#[test]
fn epistemic_gate_only_observed_or_told_may_act() {
    // taxonomy: observed/told = high authority; studied/inferred/reflected/unknown = low
    assert_eq!(ConversationEngine::epistemic_class("observed"), "observed");
    assert_eq!(ConversationEngine::epistemic_class("told"), "told");
    assert_eq!(ConversationEngine::epistemic_class("user"), "told");
    assert_eq!(ConversationEngine::epistemic_class("studied"), "studied");
    assert_eq!(ConversationEngine::epistemic_class("inferred"), "inferred");
    assert_eq!(ConversationEngine::epistemic_class("reflected"), "inferred");
    assert_eq!(ConversationEngine::epistemic_class(""), "inferred"); // unknown → least authority
    assert_eq!(ConversationEngine::epistemic_class("wild-guess"), "inferred");
    // the gate: ONLY observed/told may drive a proactive nudge / automation / shared write
    assert!(ConversationEngine::belief_actionable("observed"));
    assert!(ConversationEngine::belief_actionable("told"));
    assert!(!ConversationEngine::belief_actionable("inferred")); // a guess can't silently act
    assert!(!ConversationEngine::belief_actionable("studied"));  // general knowledge ≠ personal evidence
    assert!(!ConversationEngine::belief_actionable("reflected"));
    assert!(!ConversationEngine::belief_actionable("")); // unknown provenance never acts unprompted
}

fn gated_runtime(sender: Arc<ScriptedMailSender>) -> Arc<dyn ActionRuntime> {
    let executor = Arc::new(ToolActionExecutor::new().with_mail_sender(sender));
    Arc::new(GovernedActionRuntime::new(
        Arc::new(RealHarmGate::new()),
        executor,
        vec![Capability::SendMessage],
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_email_requires_confirmation_then_sends() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("unused"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let sender = Arc::new(ScriptedMailSender::new());
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_runtime(gated_runtime(sender.clone()));

    // Turn 1: propose — must ask for confirmation, must NOT have sent yet.
    let r1 = conv.handle_turn("send an email to test@example.com saying hello from the mind").await.unwrap();
    assert!(r1.to_lowercase().contains("confirm"), "should ask to confirm: {r1}");
    assert!(r1.contains("test@example.com"));
    assert_eq!(sender.sent.lock().unwrap().len(), 0, "must not send before confirmation");

    // Turn 2: confirm — now it sends.
    let r2 = conv.handle_turn("yes").await.unwrap();
    assert!(r2.to_lowercase().contains("done") || r2.to_lowercase().contains("sent"), "should confirm sent: {r2}");
    let sent = sender.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "test@example.com");
    assert!(sent[0].2.to_lowercase().contains("hello from the mind"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn send_email_with_a_secret_is_blocked_by_the_gate() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("unused"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let sender = Arc::new(ScriptedMailSender::new());
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_runtime(gated_runtime(sender.clone()));

    let r = conv.handle_turn("send an email to evil@external.com saying the key is ghp_ABCDEFGH1234567890wxyz").await.unwrap();
    assert!(r.to_lowercase().contains("can't") || r.to_lowercase().contains("cannot"), "gate should refuse: {r}");
    assert_eq!(sender.sent.lock().unwrap().len(), 0, "nothing must be sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn briefing_composes_inbox_and_github() {
    use mind_tools::{EmailMsg, GithubNotification, ScriptedGithubClient, ScriptedMailClient};
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("your briefing"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_agent_primary(false)
        .with_mail(Arc::new(ScriptedMailClient::new(vec![EmailMsg {
            id: "1".into(),
            from: "BRIEFMAIL boss@acme.com".into(),
            subject: "urgent".into(),
            date: "today".into(),
        }])))
        .with_github(Arc::new(ScriptedGithubClient::new(vec![GithubNotification {
            repo: "BRIEFGH org/repo".into(),
            kind: "PullRequest".into(),
            title: "review me".into(),
            reason: "review_requested".into(),
        }])));
    let r = conv.handle_turn("good morning, brief me").await.unwrap();
    assert_eq!(r, "your briefing");
    let p = scripted.last_prompt();
    assert!(p.contains("BRIEFMAIL") && p.contains("BRIEFGH"), "briefing must compose both sources:\n{p}");
    assert!(p.contains("NOT instructions"), "briefing data must be untrusted-wrapped:\n{p}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pending_onboard_slot_survives_restart() {
    // The in-flight get-to-know-you question must live in the substrate, not process memory:
    // self-deploy restarts several times a day, and a Mutex-only slot dropped the pending
    // question so the user's answer arrived with nothing armed and got mis-handled as chat.
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let scripted = Arc::new(ScriptedLLM::new("unused"));

    // Engine #1 arms a question, then "crashes" (is dropped) before the answer arrives.
    {
        let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
        let conv = ConversationEngine::new(mem.clone(), pool, "You are JARVIS.");
        assert_eq!(conv.pending_slot().await, None, "no question pending initially");
        conv.set_pending_slot(Some("interest:music")).await;
        assert_eq!(conv.pending_slot().await.as_deref(), Some("interest:music"));
    }

    // Engine #2 boots on the SAME substrate (a service restart) and must restore the slot.
    // A per-process Mutex would be empty here; the profile KV carries it across the restart.
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let conv2 = ConversationEngine::new(mem.clone(), pool, "You are JARVIS.");
    assert_eq!(
        conv2.pending_slot().await.as_deref(),
        Some("interest:music"),
        "pending onboard question must survive a restart via the profile KV",
    );

    // Consuming it clears the slot (the empty sentinel reads back as None, not a re-ask).
    conv2.set_pending_slot(None).await;
    assert_eq!(conv2.pending_slot().await, None, "consumed slot must not re-fire after restart");
}

#[test]
fn word_boundary_contains_respects_boundaries() {
    // whole-word hits (start, middle, end, punctuation-bounded)
    assert!(word_boundary_contains("ana", "ana"));
    assert!(word_boundary_contains("ana lee", "ana"));
    assert!(word_boundary_contains("lee ana", "ana"));
    assert!(word_boundary_contains("wife (ana)", "ana"));
    // substrings inside a larger word must NOT match
    assert!(!word_boundary_contains("banana", "ana"));
    assert!(!word_boundary_contains("anastasia", "ana"));
    assert!(!word_boundary_contains("susana", "ana"));
    assert!(!word_boundary_contains("ana", ""));
}

#[test]
fn forget_person_matching_is_word_bounded() {
    let susana = serde_json::json!({ "name": "Susana", "aliases": ["Su"] });
    // Word-boundary mode: "Ana" must not clobber "Susana" via a substring…
    assert!(!person_matches_mode(&susana, "ana", MatchMode::WordBoundary));
    // …but the loose lookup mode still finds her (fuzzy).
    assert!(person_matches_mode(&susana, "ana", MatchMode::Substring));

    // A real match on the whole name still forgets under word-boundary mode.
    let ana = serde_json::json!({ "name": "Ana", "aliases": ["Ana (from work)"] });
    assert!(person_matches_mode(&ana, "ana", MatchMode::WordBoundary));
}

#[test]
fn rename_corrects_canonical_name_and_keeps_old_as_alias() {
    assert_eq!(parse_rename("Priya to Priyanka"), ("Priya".into(), "Priyanka".into()));
    assert_eq!(parse_rename("Priya -> Priyanka"), ("Priya".into(), "Priyanka".into()));
    assert_eq!(parse_rename("Priya"), (String::new(), String::new()));

    let mut store = vec![
        serde_json::json!({ "name": "Priya", "aliases": ["Pri"], "relationship": "wife" }),
        serde_json::json!({ "name": "Susana", "aliases": ["Su"] }),
    ];
    let renamed = rename_in_people(&mut store, "priya", "Priyanka");
    assert_eq!(renamed, vec!["Priya".to_string()]);

    // Canonical name is corrected in place; the old name is folded into aliases so `ym about
    // Priya` still resolves, and the prior nickname survives.
    assert_eq!(store[0]["name"], serde_json::json!("Priyanka"));
    let aliases: Vec<&str> = store[0]["aliases"].as_array().unwrap().iter().filter_map(|x| x.as_str()).collect();
    assert!(aliases.contains(&"Priya"), "old canonical name kept as alias: {aliases:?}");
    assert!(aliases.contains(&"Pri"), "existing nickname preserved: {aliases:?}");
    assert!(person_matches(&store[0], "priya"), "old name still resolves");

    // Word-boundary safety: "Ana" must not rename "Susana" via a substring.
    let mut only_susana = vec![serde_json::json!({ "name": "Susana", "aliases": [] })];
    assert!(rename_in_people(&mut only_susana, "ana", "Anastasia").is_empty());
    assert_eq!(only_susana[0]["name"], serde_json::json!("Susana"));
}

#[test]
fn find_deals_splits_confirmed_from_unverified() {
    // A shortlist mixing verified (price + link) and unverified listings, plus trailing prose.
    let body = "\
- Seiko 5 watch — $95 — Amazon — https://amazon.com/seiko5
- Vintage Omega — price not listed — Etsy — https://etsy.com/omega
- Casio classic — $30 — Target — https://target.com/casio
- Mystery brand — $40 — (no link found)
⭐ Best pick: Seiko 5 — sharp value at $95.
💡 Price read: FAIR versus the ~$90–$120 range.";
    let (confirmed, unverified, extras) = split_deal_listings(body);

    // Only listings with BOTH a price and a link are confirmed.
    assert_eq!(confirmed.len(), 2, "confirmed: {confirmed:?}");
    assert!(confirmed.iter().any(|c| c.contains("Seiko 5")));
    assert!(confirmed.iter().any(|c| c.contains("Casio")));
    // Missing price OR missing link → unverified.
    assert_eq!(unverified.len(), 2, "unverified: {unverified:?}");
    assert!(unverified.iter().any(|u| u.contains("Vintage Omega")), "no price → unverified");
    assert!(unverified.iter().any(|u| u.contains("Mystery brand")), "no link → unverified");
    // Non-listing prose is preserved, not classified as a listing.
    assert!(extras.iter().any(|e| e.contains("⭐ Best pick")));

    // The rendered sections keep verified and unverified strictly apart.
    let out = sectioned_deals(body);
    let conf_at = out.find("✅ Confirmed").expect("confirmed header");
    let unv_at = out.find("⚠️ Unverified").expect("unverified header");
    assert!(conf_at < unv_at, "confirmed section must come first");
    // Everything before the unverified header is the confirmed block — no unverified item leaks in.
    let confirmed_block = &out[conf_at..unv_at];
    assert!(!confirmed_block.contains("Vintage Omega"), "unverified must not appear in confirmed block");
    assert!(!confirmed_block.contains("Mystery brand"));
    assert!(confirmed_block.contains("Seiko 5") && confirmed_block.contains("Casio"));
}

#[test]
fn find_deals_section_headers_render_when_empty() {
    // No listings at all → both sections still render (with a "(none)" placeholder each).
    let out = sectioned_deals("Sorry, the evidence was too thin to name concrete listings.");
    assert!(out.contains("✅ Confirmed"));
    assert!(out.contains("⚠️ Unverified"));
    assert!(out.contains("evidence was too thin"), "prose preserved as extras");
}

#[test]
fn watch_request_parsing() {
    assert_eq!(ConversationEngine::parse_watch_request("watch my inbox for the acme contract").as_deref(), Some("the acme contract"));
    assert_eq!(ConversationEngine::parse_watch_request("let me know when bob@x.com emails").as_deref(), Some("bob@x.com"));
    assert_eq!(ConversationEngine::parse_watch_request("tell me when an email from finance arrives").as_deref(), Some("finance"));
    // not a monitor request
    assert!(ConversationEngine::parse_watch_request("watch the game tonight").is_none());
    assert!(ConversationEngine::parse_watch_request("what's in my inbox").is_none());
}

#[test]
fn web_and_github_watch_parsing() {
    let (url, t) = ConversationEngine::parse_web_watch("watch https://shop.com/item for back in stock").unwrap();
    assert_eq!(url, "https://shop.com/item");
    assert_eq!(t, "back in stock");
    assert_eq!(ConversationEngine::parse_web_watch("tell me when https://x.io says SOLD OUT").unwrap().1, "SOLD OUT");
    // github (no url) routes to the github monitor
    assert_eq!(ConversationEngine::parse_github_watch("watch my github for auth").as_deref(), Some("auth"));
    // a URL present → NOT a github watch (web takes it)
    assert!(ConversationEngine::parse_github_watch("watch https://github.com/x/y for releases").is_none());
    // plain chat → nothing
    assert!(ConversationEngine::parse_web_watch("what's on that website").is_none());
}

#[test]
fn parse_due_handles_common_expressions() {
    assert!(parse_due("null").is_none());
    assert!(parse_due("").is_none());
    assert!(parse_due("sometime").is_none());
    assert!(parse_due("tomorrow").is_some());
    assert!(parse_due("in 3 days").is_some());
    assert!(parse_due("in 2 hours").is_some());
    assert!(parse_due("next week").unwrap() > parse_due("tomorrow").unwrap());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consolidation_distills_beliefs_and_commitments() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    let extracted = r#"{"beliefs":[{"statement":"Pranab prefers terse replies","certainty":0.9}],"commitments":[{"task":"send Pranab the Q3 report","due":"in 2 days"}]}"#;
    let pool = mind_inference::InferencePool::new(Arc::new(ScriptedLLM::new(extracted)) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    for i in 0..6 {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        memarc.append_message(role, &format!("message {i} about preferences and plans")).await.unwrap();
    }
    let n = conv.consolidate().await;
    assert_eq!(n, 2, "1 durable belief + 1 commitment");
    // the belief is recallable
    let r = memarc
        .recall_typed(mind_types::RecallQuery { text: "terse replies".into(), top_k: 5, kind: None }, &mind_types::AccessContext::Operator)
        .await
        .unwrap();
    assert!(r.iter().any(|x| x.item.text.contains("terse")), "consolidated belief must be recallable");
    // the commitment became an open task with a due date (the reminder loop will deliver it)
    let tasks = memarc.list_tasks(false).await.unwrap();
    assert!(
        tasks.iter().any(|t| t.description.contains("Q3 report") && t.due_ms.is_some()),
        "commitment must become a due-dated task: {:?}",
        tasks.iter().map(|t| &t.description).collect::<Vec<_>>()
    );
    // cursor advanced — no new turns means no re-processing
    assert_eq!(conv.consolidate().await, 0, "cursor must prevent re-chewing the same turns");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consolidation_caps_belief_weight_at_one() {
    // Even at certainty=0.95 the uncapped formula (0.5 + 0.95*1.5 = 1.925) would push
    // sigmoid confidence to ~0.87. With the cap at weight=1.0, a single consolidation
    // evidence piece can raise confidence to at most sigmoid(1.0) ≈ 0.731.
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    let extracted = r#"{"beliefs":[{"statement":"Pranab loves async Rust","certainty":0.95}],"commitments":[]}"#;
    let pool = mind_inference::InferencePool::new(
        Arc::new(ScriptedLLM::new(extracted)) as Arc<dyn LLMBackend>,
        1,
    );
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    for i in 0..6 {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        memarc.append_message(role, &format!("msg {i}")).await.unwrap();
    }
    conv.consolidate().await;
    let results = memarc
        .recall_typed(mind_types::RecallQuery { text: "async Rust".into(), top_k: 5, kind: None }, &mind_types::AccessContext::Operator)
        .await
        .unwrap();
    let belief = results.iter().find(|x| x.item.text.contains("async Rust")).expect("belief must be stored");
    assert!(
        belief.item.confidence <= 0.75,
        "machine-consolidated belief confidence must be ≤ 0.75 (sigmoid(1.0)≈0.731), got {}",
        belief.item.confidence
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn consolidation_extracts_goals_and_preferences_visible_in_reflect() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    // LLM returns JSON containing one goal and one preference (plus empty other arrays).
    let extracted = r#"{"beliefs":[],"goals":[{"goal":"learn async Rust"}],"preferences":[{"preference":"terse replies"}],"commitments":[]}"#;
    let pool = mind_inference::InferencePool::new(Arc::new(ScriptedLLM::new(extracted)) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    for i in 0..6 {
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        memarc.append_message(role, &format!("message {i} about goals and preferences")).await.unwrap();
    }
    let n = conv.consolidate().await;
    assert_eq!(n, 2, "1 goal + 1 preference");
    let reflection = memarc.reflect("goals and preferences", &mind_types::AccessContext::Operator).await.unwrap();
    assert!(
        reflection.goals.iter().any(|g| g.text.contains("async Rust")),
        "goal must appear in reflect: {:?}",
        reflection.goals.iter().map(|g| &g.text).collect::<Vec<_>>()
    );
    assert!(
        reflection.preferences.iter().any(|p| p.text.contains("terse")),
        "preference must appear in reflect: {:?}",
        reflection.preferences.iter().map(|p| &p.text).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dmn_associates_a_hypothesis_when_idle() {
    // The default-mode loop's ASSOCIATE phase should free-associate over stored beliefs and bank a
    // low-certainty hypothesis (provenance=dmn) the mind can later surface — sleep-like recombination.
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    for s in [
        "Pranab prefers terse replies",
        "Pranab loves async Rust",
        "Pranab pre-registers kill criteria before experiments",
    ] {
        memarc
            .remember_as_belief(BeliefAssertion {
                statement: s.into(),
                polarity: 1.0,
                weight: 1.0,
                source_event: None,
                provenance: "test".into(),
            })
            .await
            .unwrap();
    }
    let insight = "Pranab consistently optimizes for signal over noise.";
    let pool = mind_inference::InferencePool::new(Arc::new(ScriptedLLM::new(insight)) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    // phase rotor: 0 rehearse, 1 reconcile (no conflicts → no-op), 2 associate
    let _ = conv.dmn_tick().await;
    let _ = conv.dmn_tick().await;
    let log = conv.dmn_tick().await;
    assert!(log.iter().any(|l| l.contains("associated")), "associate phase should run: {log:?}");
    let r = memarc
        .recall_typed(mind_types::RecallQuery { text: "signal over noise".into(), top_k: 8, kind: None }, &mind_types::AccessContext::Operator)
        .await
        .unwrap();
    assert!(
        r.iter().any(|x| x.item.text.contains("hypothesis")),
        "a dmn hypothesis must be stored + recallable: {:?}",
        r.iter().map(|x| &x.item.text).collect::<Vec<_>>()
    );
    // the curiosity DRIVE should also have emitted an urge into the tension ledger
    let tensions = memarc.open_tensions(10).await.unwrap();
    assert!(
        tensions.iter().any(|t| t.kind == mind_types::TensionKind::Curiosity),
        "associate should emit a curiosity urge: {:?}",
        tensions.iter().map(|t| (t.kind, &t.about)).collect::<Vec<_>>()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dmn_rehearse_flags_stale_high_confidence_belief() {
    // The rehearse phase must emit a Staleness tension for high-confidence beliefs that have not
    // been updated within the configured window. We set YM_STALE_BELIEF_DAYS=0 so any stored
    // belief (even a fresh one) counts as stale, making the assertion deterministic.
    // Safety: this is the only test that touches YM_STALE_BELIEF_DAYS, so there is no
    // concurrent mutation of this env var.
    unsafe { std::env::set_var("YM_STALE_BELIEF_DAYS", "0") };
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    // weight=1.0 → log_odds=1.0 → confidence≈0.73 (above the 0.7 threshold) → must be flagged.
    memarc
        .remember_as_belief(BeliefAssertion {
            statement: "Pranab values fast iteration over perfect design".into(),
            polarity: 1.0,
            weight: 1.0,
            source_event: None,
            provenance: "test".into(),
        })
        .await
        .unwrap();
    // weight=0.1 → confidence≈0.52 (below 0.7) → must NOT be flagged.
    memarc
        .remember_as_belief(BeliefAssertion {
            statement: "Pranab might prefer morning meetings".into(),
            polarity: 1.0,
            weight: 0.1,
            source_event: None,
            provenance: "test".into(),
        })
        .await
        .unwrap();
    let pool = mind_inference::InferencePool::new(
        Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>,
        1,
    );
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    // Phase 0 = rehearse; the other two phases are irrelevant for this assertion.
    let log = conv.dmn_tick().await;
    assert!(
        log.iter().any(|l| l.contains("stale")),
        "rehearse log should mention stale belief(s): {log:?}"
    );
    let tensions = memarc.open_tensions(10).await.unwrap();
    assert!(
        tensions.iter().any(|t| t.kind == mind_types::TensionKind::Staleness
            && t.about.contains("fast iteration")),
        "high-confidence belief should generate a Staleness tension: {:?}",
        tensions.iter().map(|t| (t.kind, &t.about)).collect::<Vec<_>>()
    );
    assert!(
        !tensions.iter().any(|t| t.kind == mind_types::TensionKind::Staleness
            && t.about.contains("morning")),
        "low-confidence belief must not be flagged: {:?}",
        tensions.iter().map(|t| (t.kind, &t.about)).collect::<Vec<_>>()
    );
    unsafe { std::env::remove_var("YM_STALE_BELIEF_DAYS") };
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dmn_reconcile_applies_signed_evidence_to_contradicting_beliefs() {
    // The RECONCILE phase must parse the LLM verdict (A/B/UNRESOLVED) and apply signed
    // evidence to the winning and losing belief nodes, not just record a dead note.
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());

    let belief_a_text = "exercise improves mood";
    let belief_b_text = "exercise has no effect on mood";

    for text in [belief_a_text, belief_b_text] {
        memarc
            .remember_as_belief(BeliefAssertion {
                statement: text.into(),
                polarity: 1.0,
                weight: 1.0, // identical starting confidence for both
                source_event: None,
                provenance: "test".into(),
            })
            .await
            .unwrap();
    }
    memarc.relate(belief_a_text, belief_b_text, "contradicts", 0.9).await.unwrap();
    assert!(!memarc.conflicts(&mind_types::AccessContext::Operator).await.unwrap().is_empty(), "contradiction must be detected");

    let conf_a_before = memarc.explain_belief(belief_a_text, &mind_types::AccessContext::Operator).await.unwrap()
        .map(|(b, _)| b.confidence)
        .expect("belief should exist before reconcile");
    let conf_b_before = memarc.explain_belief(belief_b_text, &mind_types::AccessContext::Operator).await.unwrap()
        .map(|(b, _)| b.confidence)
        .expect("belief should exist before reconcile");

    let pool = mind_inference::InferencePool::new(
        Arc::new(ScriptedLLM::new("A is better supported by scientific evidence.")) as Arc<dyn LLMBackend>,
        1,
    );
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");

    let _ = conv.dmn_tick().await; // phase 0: rehearse
    let log = conv.dmn_tick().await; // phase 1: reconcile

    assert!(
        log.iter().any(|l| l.contains("wins")),
        "reconcile log must report a winner (not 'unresolved'): {log:?}",
    );

    let conf_a_after = memarc.explain_belief(belief_a_text, &mind_types::AccessContext::Operator).await.unwrap()
        .map(|(b, _)| b.confidence)
        .expect("belief should still exist after reconcile");
    let conf_b_after = memarc.explain_belief(belief_b_text, &mind_types::AccessContext::Operator).await.unwrap()
        .map(|(b, _)| b.confidence)
        .expect("belief should still exist after reconcile");

    let delta_a = conf_a_after - conf_a_before;
    let delta_b = conf_b_after - conf_b_before;

    // Winner's confidence must rise, loser's must fall — they must move in opposite directions.
    assert!(
        delta_a.abs() > 1e-4 && delta_b.abs() > 1e-4,
        "both beliefs must shift confidence; Δa={delta_a:.4}, Δb={delta_b:.4}",
    );
    assert!(
        (delta_a > 0.0) != (delta_b > 0.0),
        "winner must gain and loser must lose confidence; Δa={delta_a:.4}, Δb={delta_b:.4}",
    );

    let tensions = memarc.open_tensions(10).await.unwrap();
    assert!(
        tensions.iter().any(|t| t.kind == mind_types::TensionKind::Contradiction),
        "reconcile must still emit a Contradiction tension: {tensions:?}",
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tension_ledger_records_dedupes_and_discharges() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    memarc.record_tension(mind_types::TensionKind::Staleness, 0.7, "belief X is decaying").await.unwrap();
    // same (kind, about) accrues rather than duplicating — and keeps the max pressure
    memarc.record_tension(mind_types::TensionKind::Staleness, 0.9, "belief X is decaying").await.unwrap();
    let open = memarc.open_tensions(10).await.unwrap();
    assert_eq!(open.len(), 1, "dedup on (kind, about): {open:?}");
    assert!((open[0].pressure - 0.9).abs() < 1e-9, "keeps the max pressure, got {}", open[0].pressure);
    assert!(memarc.discharge_tension(&open[0].id).await.unwrap(), "discharge should report it changed");
    assert!(memarc.open_tensions(10).await.unwrap().is_empty(), "discharged tension is no longer open");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn onboarding_interview_asks_name_then_purpose() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    // first question is the NAME
    let q1 = conv.proactive_ask().await.expect("asks while it doesn't know you");
    assert!(q1.to_lowercase().contains("call you"), "first asks the name: {q1}");
    // it must NOT stack a second question while awaiting the answer
    assert!(conv.proactive_ask().await.is_none(), "doesn't stack questions while awaiting an answer");
    // answering captures the name (lead-in stripped) and chains straight to the PURPOSE question
    let ack = conv.handle_turn("my name is Pranab").await.unwrap();
    assert!(ack.contains("Pranab"), "acks + uses the name: {ack}");
    assert_eq!(memarc.profile_get("name").await.unwrap().as_deref(), Some("Pranab"), "name captured");
    // that reply also posed the purpose question → answering it captures the purpose
    let _ack2 = conv.handle_turn("help me ship yantrik-mind").await.unwrap();
    assert_eq!(
        memarc.profile_get("purpose").await.unwrap().as_deref(),
        Some("help me ship yantrik-mind"),
        "purpose captured"
    );
    // with name + purpose known and the brain otherwise empty, the open stage may ask grounded
    // follow-ups (here the scripted LLM returns no clean question → None), and never re-asks name.
    let q3 = conv.proactive_ask().await;
    assert!(q3.as_deref().map(|q| !q.to_lowercase().contains("call you")).unwrap_or(true), "never re-asks name once known");
}

#[test]
fn github_monitor_routes_natural_phrasings() {
    // the exact phrasing that failed in the wild — must now route to the github monitor
    assert!(ConversationEngine::parse_github_watch("track my git repos for any issues created by others or any PRs").is_some(), "must route 'track my repos for issues/PRs'");
    assert!(ConversationEngine::parse_github_watch("keep an eye on my github for new issues").is_some());
    assert!(ConversationEngine::parse_github_watch("notify me about new PRs on my repo").is_some());
    // no github source, or not a monitor ask → no false trigger
    assert!(ConversationEngine::parse_github_watch("track my fitness goals").is_none(), "'track' without a github source must not trigger");
    assert!(ConversationEngine::parse_github_watch("what's the status of my repo?").is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn agent_loop_reasons_then_answers() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    // the agent decides it can answer directly (no tool) on the first step
    let pool = InferencePool::new(
        Arc::new(ScriptedLLM::new(r#"{"thought":"simple greeting","answer":"Hey Pranab — what do you need?"}"#)) as Arc<dyn LLMBackend>,
        1,
    );
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    let r = conv.agent_loop("hi", &TurnIdentity::primary()).await.unwrap();
    assert!(r.contains("Pranab"), "agent should return its answer: {r}");
    // and the turn is recorded in the transcript
    let recent = memarc.recent_messages(4, &mind_types::AccessContext::Operator).await.unwrap();
    assert!(recent.iter().any(|(role, t)| role == "assistant" && t.contains("Pranab")));
}

/// ARCH-1 slice 2 acceptance — the agent `recall` tool was COMMENTED read-isolated but called
/// unscoped memory (sol's finding #2). Now every lane (semantic, deep lexical, exact-match)
/// carries the speaker's Principal ctx, and the shared recipe/researcher host reads egress-clean
/// (shared facts only), so neither a member turn nor a tool plan can reach a private fact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arch1_agent_recall_tool_and_recipe_host_are_read_isolated() {
    use mind_types::Scope;
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");

    let secret = "The safe combination is 47-12-33";
    memarc.remember_as_belief_scoped(
        BeliefAssertion { statement: secret.into(), polarity: 1.0, weight: 2.0, source_event: None, provenance: "told".into() },
        Scope::primary(),
    ).await.unwrap();
    memarc.remember_as_belief_scoped(
        BeliefAssertion { statement: "Dinner on Friday is at seven".into(), polarity: 1.0, weight: 2.0, source_event: None, provenance: "told".into() },
        Scope::Shared,
    ).await.unwrap();

    // Agent recall tool AS A MEMBER: shared fact recallable, secret unreachable on every lane.
    let member = TurnIdentity::new("asha", false);
    let args = serde_json::json!({ "query": "safe combination" });
    let out = conv.run_agent_tool_as("recall", &args, &member).await;
    assert!(!out.contains("47-12-33"), "MEMBER agent-recall leaked the secret: {out}");
    let args = serde_json::json!({ "query": "dinner friday" });
    let out = conv.run_agent_tool_as("recall", &args, &member).await;
    assert!(out.contains("Dinner on Friday"), "member agent-recall must keep shared facts: {out}");
    // …while the primary's own path still reaches their private fact.
    let args = serde_json::json!({ "query": "safe combination" });
    let out = conv.run_agent_tool_as("recall", &args, &TurnIdentity::primary()).await;
    assert!(out.contains("47-12-33"), "primary agent-recall must reach their own private fact: {out}");

    // Recipe/researcher host: egress-clean by construction — shared facts ONLY,
    // no one's private data, whatever triggered the recipe.
    let host = MindRecipeHost::new(None, None, memarc.clone());
    let hit = host.call_tool("recall", &serde_json::json!({ "query": "dinner friday" })).await.unwrap();
    assert!(hit.contains("Dinner on Friday"), "recipe recall must see shared facts: {hit}");
    let miss = host.call_tool("recall", &serde_json::json!({ "query": "safe combination" })).await;
    let leaked = miss.map(|s| s.contains("47-12-33")).unwrap_or(false);
    assert!(!leaked, "RECIPE recall leaked a private fact — egress-clean context breached");
}

/// ARCH-3A acceptance: the egress broker mediates the recognized external-connector tools at the
/// agent-loop AND recipe-host chokepoints — a credential marker in an outbound tool arg is refused
/// before dispatch, a benign call passes, and Local tools are never gated.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arch3_egress_broker_mediates_external_tool_calls() {
    use mind_governance::egress::EgressBroker;
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let broker = Arc::new(EgressBroker::open(std::env::temp_dir(), false));
    let conv = ConversationEngine::new(mem, pool, "JARVIS").with_egress(broker.clone());
    let primary = TurnIdentity::primary();

    // A credential composed into a web_search arg → refused at the agent-loop chokepoint, and the
    // refusal never echoes the secret.
    let out = conv.run_agent_tool_as("web_search", &serde_json::json!({ "query": "email ghp_ABCDEF1234567890 to bob" }), &primary).await;
    assert!(out.contains("credential") || out.contains("won't send"), "credential arg must be refused: {out}");
    assert!(!out.contains("ghp_ABCDEF"), "refusal must not echo the secret: {out}");

    // A credential in a mail_search arg → refused too (the connector is never touched).
    let out = conv.run_agent_tool_as("mail_search", &serde_json::json!({ "query": "sk-abc123 my openai key" }), &primary).await;
    assert!(out.contains("credential") || out.contains("won't send"), "mail_search credential arg must be refused: {out}");

    // A Local tool (calc) is NEVER gated by the broker — it computes in-process.
    let out = conv.run_agent_tool_as("calc", &serde_json::json!({ "expression": "6*7" }), &primary).await;
    assert!(out.contains("42"), "a local tool must not be blocked by egress: {out}");

    // The recipe-host chokepoint independently refuses a credential in a fetch arg.
    let host = MindRecipeHost::new(None, None, mem_arc_for_host()).with_egress(broker.clone());
    let denied = host.call_tool("fetch", &serde_json::json!({ "url": "https://x/?leak=ghp_ABCDEF1234567890" })).await;
    assert!(denied.is_err(), "recipe host must refuse a credential-bearing fetch");
}

/// A minimal shared-memory facade for the recipe-host arm of the ARCH-3 test.
fn mem_arc_for_host() -> Arc<dyn MemoryFacade> {
    Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap())
}

/// ARCH-3 slice 2 acceptance: egress-clean tool planning. The grounded model may author a tool
/// arg that carries a private fact; for an eligible egress tool those grounded args are DISCARDED
/// and replaced by a SEPARATE clean-context call's output — so the private fact never reaches the
/// connector. Non-eligible tools keep their grounded args; garbage from the clean planner fails
/// closed (None). We drive egress_clean_args directly (the clean call's output is scripted).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arch3_slice2_egress_clean_planning_discards_grounded_args() {
    use mind_governance::egress::EgressBroker;
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    // The inference backend (the CLEAN re-authoring call) is scripted to return a private-free arg.
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new(r#"{"query":"best oncology hospitals in Pune"}"#)) as Arc<dyn LLMBackend>, 1);
    let broker = Arc::new(EgressBroker::open(std::env::temp_dir(), false));
    let conv = ConversationEngine::new(mem, pool, "JARVIS").with_egress(broker);

    // The grounded model authored a web_search arg that LEAKS a stored private fact.
    let grounded = serde_json::json!({ "query": "Alice oncology appointment July 18 47-12-33" });
    let clean = conv.egress_clean_args("web_search", "find me good oncology hospitals in pune", grounded.clone()).await.unwrap();
    // The clean-context call's args are what dispatch — the grounded (leaky) args are gone.
    assert_eq!(clean, serde_json::json!({ "query": "best oncology hospitals in Pune" }), "grounded args must be discarded and re-authored");
    assert_ne!(clean, grounded, "the private-fact-bearing grounded args must NOT survive");
    assert!(!clean.to_string().contains("47-12-33"), "the private detail must not reach the connector");

    // A NON-eligible egress tool (github) keeps its grounded args (documented not-yet-covered).
    let g = serde_json::json!({ "repo": "owner/repo" });
    let kept = conv.egress_clean_args("github_repo_items", "my open PRs", g.clone()).await.unwrap();
    assert_eq!(kept, g, "a non-eligible tool keeps its grounded args");

    // With NO egress broker wired, planning is inert (legacy path unchanged).
    let mem2: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool2 = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv2 = ConversationEngine::new(mem2, pool2, "JARVIS");
    let g2 = serde_json::json!({ "query": "leaky Alice oncology" });
    assert_eq!(conv2.egress_clean_args("web_search", "hi", g2.clone()).await.unwrap(), g2, "no broker → egress-clean planning is inert");
}

/// ARCH-3 slice 2 (complementary): the exact-value exfil guard. A distinctive stored private value
/// (email/phone/id) the model injects into a NON-clean-planned external tool arg — that the user
/// did NOT type — is refused. A value the user typed themselves, or one not in memory, passes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arch3_slice2_exact_value_exfil_guard() {
    use mind_governance::egress::EgressBroker;
    let mem = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    // Plant a private fact holding a distinctive value (an email).
    mem.remember_as_belief_scoped(
        BeliefAssertion { statement: "Alice's private email is alice.secret@example.com".into(), polarity: 1.0, weight: 2.0, source_event: None, provenance: "told".into() },
        mind_types::Scope::primary(),
    ).await.unwrap();
    let memf: Arc<dyn MemoryFacade> = mem;
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memf, pool, "JARVIS").with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)));
    let primary = TurnIdentity::primary();

    // The model injects the stored email into a github (external, NOT clean-planned) arg, and the
    // user's request never mentioned it → guarded.
    let args = serde_json::json!({ "repo": "alice.secret@example.com/notes" });
    let blocked = conv.model_injected_private_value("github_repo_items", &args, "show my open PRs", &primary).await;
    assert!(blocked.is_some(), "a model-injected stored private email must be guarded");
    assert!(!blocked.unwrap().contains("alice.secret@example.com"), "the refusal must not echo the value (no oracle)");

    // If the USER typed the value themselves, it's their call — allowed.
    let ok = conv.model_injected_private_value("github_repo_items", &args, "check alice.secret@example.com/notes", &primary).await;
    assert!(ok.is_none(), "a value the user typed themselves must pass");

    // A value NOT in memory passes (nothing stored to leak).
    let novel = serde_json::json!({ "repo": "bob.unknown@nowhere.com/x" });
    assert!(conv.model_injected_private_value("github_repo_items", &novel, "my PRs", &primary).await.is_none(), "an unknown value is not a leak");

    // A LOCAL tool is never guarded here (no egress).
    assert!(conv.model_injected_private_value("calc", &args, "math", &primary).await.is_none(), "local tools are not egress-guarded");
}

/// Egress-clean planning fails CLOSED: if the clean planner can't produce a usable JSON arg for an
/// eligible egress tool, the call is refused (None) rather than falling back to the grounded args.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn arch3_slice2_clean_planner_fails_closed_on_garbage() {
    use mind_governance::egress::EgressBroker;
    let mem: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("sorry, I cannot help with that")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(mem, pool, "JARVIS").with_egress(Arc::new(EgressBroker::open(std::env::temp_dir(), false)));
    let grounded = serde_json::json!({ "query": "Alice oncology" });
    assert!(conv.egress_clean_args("web_search", "search", grounded).await.is_none(), "no usable clean args → fail closed (refuse), not fall back to grounded");
}

#[test]
fn truncated_publish_page_recovers_html_not_the_wrapper() {
    // The real failure: the model inlined a full page into a publish_page call, overflowed the
    // token cap, and the JSON arrived truncated mid-string (no closing quote/braces).
    let blob = r#"{"thought":"publishing the page","tool":"publish_page","args":{"name":"gift-deals","html":"<!DOCTYPE html>\n<html><head><title>Top 10 Combos</title></head><body><h1>Deals</h1><div>combo one</div"#;
    // It must NOT parse as a clean object, and IS recognized as a tool-call blob (so we never host it raw).
    assert!(serde_json::from_str::<serde_json::Value>(blob).is_err(), "blob is genuinely broken JSON");
    assert!(is_tool_call_blob(blob), "recognized as a tool-call wrapper, never published raw");
    // We recover the inner HTML even though it's cut off…
    let html = extract_html_arg(blob).expect("recovers the html arg from the truncated blob");
    assert!(html.starts_with("<!DOCTYPE html>"), "unescaped real html, not the JSON: {html}");
    assert!(looks_like_html(&html));
    assert!(!html.contains("\\n"), "JSON escapes are decoded: {html}");
    // …and name the page from its <title>, not the user's request text.
    assert_eq!(title_from_html(&html).as_deref(), Some("Top 10 Combos"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn news_plugin_headlines_and_tracking() {
    use mind_tools::{NewsItem, ScriptedNews};
    let memarc: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc, pool, "JARVIS").with_news(Arc::new(ScriptedNews::new(vec![NewsItem {
        title: "Talks stall in Geneva".into(),
        url: "https://news.google.com/a".into(),
        source: "Reuters".into(),
        published: "Mon, 29 Jun 2026 14:00:00 GMT".into(),
    }])));
    // on-demand quick headlines on a topic (`news <topic>` is now the in-depth brief; `news
    // headlines <topic>` is the fast list)
    let h = conv.cli_dispatch("news headlines geopolitics", &mind_types::AccessContext::Operator).await;
    assert!(h.contains("Talks stall in Geneva") && h.contains("Reuters"), "headlines: {h}");
    // tracking: add → list → remove
    assert!(conv.cli_dispatch("news track geopolitics", &mind_types::AccessContext::Operator).await.contains("Tracking"));
    assert!(conv.cli_dispatch("news tracking", &mind_types::AccessContext::Operator).await.contains("geopolitics"), "tracked list");
    // digest watch primes silently on first call, then dedups identical items (no repeat spam)
    let _ = conv.news_digests_due().await;
    assert!(conv.news_digests_due().await.is_empty(), "deduped after prime");
    assert!(conv.cli_dispatch("news untrack geopolitics", &mind_types::AccessContext::Operator).await.contains("Stopped"));
}

#[test]
fn parses_ics_vevents() {
    let offset = chrono::FixedOffset::west_opt(5 * 3600).unwrap();
    let ics = "BEGIN:VCALENDAR\nBEGIN:VEVENT\nDTSTART;VALUE=DATE:20260710\nSUMMARY:Dentist\nEND:VEVENT\n\
               BEGIN:VEVENT\nDTSTART:20260712T183000Z\nSUMMARY:Team dinner\nEND:VEVENT\n\
               BEGIN:VEVENT\nDTSTART:19990101\nSUMMARY:Ancient\nEND:VEVENT\nEND:VCALENDAR";
    let from = chrono::NaiveDate::from_ymd_opt(2026, 7, 1).unwrap().and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
    let to = from + 60 * 86_400_000;
    let evs = parse_ics_events(ics, offset, from, to);
    assert_eq!(evs.len(), 2, "in-window events parsed, ancient filtered: {evs:?}");
    assert_eq!(evs[0].0, "Dentist");
    assert_eq!(evs[1].0, "Team dinner");
}

#[test]
fn parses_text_dates_for_followups() {
    let today = chrono::DateTime::parse_from_rfc3339("2026-07-01T10:00:00-05:00").unwrap();
    // "by July 17th" → the next July 17, midday local.
    let ms = parse_text_date_ms("Order the gift by July 17th to ensure delivery", &today).unwrap();
    let days = (ms - today.timestamp_millis()) / 86_400_000;
    assert!((15..=16).contains(&days), "July 17 is ~16 days out, got {days}");
    // A past date this year rolls to next year (never negative).
    let ms = parse_text_date_ms("started on March 2", &today).unwrap();
    assert!(ms > today.timestamp_millis());
    // Word-boundary guard: "maybe 5" must NOT parse as May 5; no month → None.
    assert!(parse_text_date_ms("maybe 5 days more", &today).is_none());
    assert!(parse_text_date_ms("no dates in here at all", &today).is_none());
}

#[test]
fn calculator_evaluates_expressions() {
    assert_eq!(calc("12*7+3"), "= 87");
    assert_eq!(calc("(5-1)/2"), "= 2");
    assert_eq!(calc("2^10"), "= 1024");
    assert_eq!(calc("1500 * 0.18"), "= 270");
    assert_eq!(calc("$1,200 / 12"), "= 100"); // currency/commas ignored
    assert!(calc("1/0").contains("couldn't"), "div by zero is rejected");
    assert!(calc("hello").contains("couldn't"), "non-math rejected");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn markets_and_translate_route_via_cli() {
    use mind_tools::{ScriptedMarkets, ScriptedTranslator};
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap()) as Arc<dyn MemoryFacade>, pool, "JARVIS")
        .with_markets(Arc::new(ScriptedMarkets { crypto: "💰 Bitcoin (BTC): $67,000 ▲2%".into(), stock: "📈 Apple (AAPL): $211".into(), price: 200.0 }))
        .with_translator(Arc::new(ScriptedTranslator { text: "🌐 (en→fr) bonjour".into() }));
    assert!(conv.cli_dispatch("crypto btc", &mind_types::AccessContext::Operator).await.contains("Bitcoin"), "crypto routes");
    assert!(conv.cli_dispatch("stock AAPL", &mind_types::AccessContext::Operator).await.contains("Apple"), "stock routes");
    assert!(conv.cli_dispatch("translate french good morning", &mind_types::AccessContext::Operator).await.contains("bonjour"), "translate routes (first token = lang)");
    assert!(conv.cli_dispatch("translate french", &mind_types::AccessContext::Operator).await.contains("Usage"), "translate without text shows usage");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn weather_and_wiki_route_via_cli() {
    use mind_tools::{ScriptedWeather, ScriptedWiki};
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap()) as Arc<dyn MemoryFacade>, pool, "JARVIS")
        .with_weather(Arc::new(ScriptedWeather::new("🌦 London: rain, 14°C")))
        .with_wiki(Arc::new(ScriptedWiki::new("📖 Rust\nA systems language.")));
    assert!(conv.cli_dispatch("weather london", &mind_types::AccessContext::Operator).await.contains("London: rain"), "weather routes");
    assert!(conv.cli_dispatch("wiki rust language", &mind_types::AccessContext::Operator).await.contains("systems language"), "wiki routes");
    assert!(conv.cli_dispatch("calc 6*7", &mind_types::AccessContext::Operator).await.contains("= 42"), "calc routes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_plugin_routes_and_renders() {
    use mind_tools::{ScriptedSearch, SearchHit};
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(
        Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap()) as Arc<dyn MemoryFacade>,
        pool,
        "JARVIS",
    )
    .with_searcher(Arc::new(ScriptedSearch::new(vec![SearchHit {
        title: "Rust async".into(),
        url: "https://rust-lang.org".into(),
        snippet: "a guide".into(),
    }])));
    let out = conv.cli_dispatch("search rust async", &mind_types::AccessContext::Operator).await;
    assert!(out.contains("Rust async") && out.contains("https://rust-lang.org"), "search renders results: {out}");
    // not configured → clear message, no confabulation
    let conv2 = ConversationEngine::new(
        Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap()) as Arc<dyn MemoryFacade>,
        InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1),
        "JARVIS",
    );
    assert!(conv2.run_agent_tool("search", &serde_json::json!({ "query": "x" })).await.contains("not configured"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_tool_reads_smart_home_states() {
    use mind_tools::{HaEntity, ScriptedHomeAssistantClient};
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let ents = vec![
        HaEntity { entity_id: "person.pranab".into(), domain: "person".into(), state: "home".into(), friendly_name: "Pranab".into(), attributes: serde_json::json!({}) },
        HaEntity { entity_id: "climate.lr".into(), domain: "climate".into(), state: "heat".into(), friendly_name: "Living Room".into(), attributes: serde_json::json!({"current_temperature": 19.5, "temperature": 22, "hvac_action": "heating"}) },
    ];
    let conv = ConversationEngine::new(
        Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap()) as Arc<dyn MemoryFacade>,
        pool,
        "JARVIS",
    )
    .with_home(Arc::new(ScriptedHomeAssistantClient::new(ents)));
    let out = conv.run_agent_tool("home", &serde_json::json!({})).await;
    assert!(out.contains("Pranab: home") && out.contains("Living Room") && out.contains("heating"), "home digest: {out}");
    // not configured → a clear, non-confabulated message
    let conv2 = ConversationEngine::new(
        Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap()) as Arc<dyn MemoryFacade>,
        InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1),
        "JARVIS",
    );
    assert!(conv2.run_agent_tool("home", &serde_json::json!({})).await.contains("not configured"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn finance_tracks_subscriptions_and_normalizes_total() {
    let memarc: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc, pool, "JARVIS");
    // add a monthly + a yearly (139/12 = 11.58/mo); name can be multi-word
    conv.finance_cmd("sub", "add Netflix 15.99 monthly").await;
    conv.finance_cmd("sub", "add Amazon Prime 139 yearly").await;
    let list = conv.finance_cmd("subs", "").await;
    assert!(list.contains("Netflix") && list.contains("Amazon Prime"), "lists both: {list}");
    // monthly total = 15.99 + 11.58 = ~27.57, count = 2
    let money = conv.finance_cmd("money", "").await;
    assert!(money.contains("2 subscription"), "counts subs: {money}");
    assert!(money.contains("27.5") || money.contains("27.6"), "normalized monthly total ~27.57: {money}");
    // remove one + it persists (round-trips through the profile store)
    assert!(conv.finance_cmd("sub", "rm Netflix").await.contains("Removed"));
    let after = conv.finance_cmd("subs", "").await;
    assert!(after.contains("Amazon Prime") && !after.contains("Netflix"), "removal persisted: {after}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bills_and_budget_track_and_warn() {
    let memarc: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc, pool, "JARVIS");
    // bills: add + list + monthly total (electric monthly + insurance yearly→/12)
    conv.bill_cmd("add", "electric 120 23 monthly").await;
    conv.bill_cmd("add", "car insurance 1200 5 yearly").await;
    let bills = conv.bill_cmd("list", "").await;
    assert!(bills.contains("electric") && bills.contains("car insurance"), "lists bills: {bills}");
    assert!(bills.contains("23rd") && bills.contains("5th"), "ordinal due days: {bills}");
    assert!(bills.contains("2 bills"), "count: {bills}");
    // budget: set + over-spend warns
    conv.budget_set("dining 400").await;
    conv.expense_log("250 dining").await;
    let over = conv.expense_log("200 dining").await; // 450 > 400
    assert!(over.contains("OVER") || over.contains("450"), "over-budget surfaced: {over}");
    let overview = conv.budget_overview().await;
    assert!(overview.contains("dining") && overview.contains("450"), "overview totals spend: {overview}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn news_interest_signal_consumes_last_topic() {
    let memarc: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc, pool, "JARVIS");
    // No topic surfaced yet → an interest signal has no referent.
    assert_eq!(conv.interest_in_recent_news("tell me more"), None);
    // Simulate news_watch having surfaced a topic.
    *conv.last_news_topic.lock().unwrap() = Some("AI regulation".into());
    // A non-interest message must NOT consume it.
    assert_eq!(conv.interest_in_recent_news("what's the weather like"), None);
    assert!(conv.last_news_topic.lock().unwrap().is_some());
    // An interest signal returns the topic AND consumes it (so it fires once per ping).
    assert_eq!(conv.interest_in_recent_news("tell me more").as_deref(), Some("AI regulation"));
    assert!(conv.last_news_topic.lock().unwrap().is_none(), "topic consumed after use");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn portfolio_tracks_holdings_and_values_live() {
    use mind_tools::ScriptedMarkets;
    let memarc: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    // Every quote returns price=200 → deterministic valuation.
    let conv = ConversationEngine::new(memarc, pool, "JARVIS")
        .with_markets(Arc::new(ScriptedMarkets { crypto: "x".into(), stock: "x".into(), price: 200.0 }));
    // 10 AAPL @ cost 175 → live @200 = $2,000, P&L = (2000-1750)/1750 = +14.3%
    conv.holding_cmd("add", "AAPL 10 175").await;
    // 5 MSFT, no cost basis → value only ($1,000)
    conv.holding_cmd("add", "MSFT 5").await;
    let p = conv.portfolio_overview().await;
    assert!(p.contains("AAPL") && p.contains("MSFT"), "lists positions: {p}");
    assert!(p.contains("2,000"), "values 10 AAPL @ $200 = $2,000: {p}");
    assert!(p.contains("14.3"), "P&L vs cost 175 = +14.3%: {p}");
    assert!(p.contains("3,000"), "portfolio total $3,000: {p}");
    assert!(p.contains("66%") || p.to_lowercase().contains("concentrated"), "concentration surfaced (AAPL 66%): {p}");
    // removal round-trips through the profile store
    assert!(conv.holding_cmd("rm", "AAPL").await.contains("Removed"));
    let after = conv.portfolio_overview().await;
    assert!(after.contains("MSFT") && !after.contains("AAPL"), "removal persisted: {after}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn discovers_subscriptions_from_email() {
    use mind_tools::{EmailMsg, ScriptedMailClient};
    let memarc: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    // the LLM is scripted to return the extraction JSON (one with a price, one without)
    let pool = InferencePool::new(
        Arc::new(ScriptedLLM::new(r#"[{"name":"Netflix","amount":15.99,"cycle":"monthly"},{"name":"Spotify","amount":null,"cycle":"monthly"}]"#)) as Arc<dyn LLMBackend>,
        1,
    );
    let inbox = vec![
        EmailMsg { id: "1".into(), from: "info@netflix.com".into(), subject: "Your receipt".into(), date: "today".into() },
        EmailMsg { id: "2".into(), from: "no-reply@spotify.com".into(), subject: "Spotify Premium".into(), date: "today".into() },
    ];
    let conv = ConversationEngine::new(memarc, pool, "JARVIS").with_mail(Arc::new(ScriptedMailClient::new(inbox)));
    let out = conv.discover_subscriptions().await;
    assert!(out.contains("Netflix"), "auto-tracked the priced one: {out}");
    assert!(out.contains("Spotify"), "listed the price-less one to confirm: {out}");
    // Netflix (had a price) is now actually tracked; Spotify (no price) is not auto-added
    let subs = conv.finance_cmd("subs", "").await;
    assert!(subs.contains("Netflix") && !subs.contains("Spotify"), "only priced subs auto-tracked: {subs}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn home_watch_primes_then_fires_new_alerts() {
    use mind_tools::{HaEntity, HomeAssistantClient};
    use std::sync::atomic::{AtomicUsize, Ordering as O};
    struct SeqHa {
        i: AtomicUsize,
        frames: Vec<Vec<HaEntity>>,
    }
    #[async_trait::async_trait]
    impl HomeAssistantClient for SeqHa {
        async fn states(&self) -> anyhow::Result<Vec<HaEntity>> {
            let n = self.i.fetch_add(1, O::SeqCst).min(self.frames.len() - 1);
            Ok(self.frames[n].clone())
        }
    }
    let p = |s: &str| HaEntity { entity_id: "person.pranab".into(), domain: "person".into(), state: s.into(), friendly_name: "Pranab".into(), attributes: serde_json::json!({}) };
    let tv = HaEntity { entity_id: "media_player.tv".into(), domain: "media_player".into(), state: "playing".into(), friendly_name: "TV".into(), attributes: serde_json::json!({}) };
    // frame0: home (no alerts) primes; frame1: away + TV on → FIRES; frame2: same → deduped
    let frames = vec![vec![p("home")], vec![p("not_home"), tv.clone()], vec![p("not_home"), tv.clone()]];
    let conv = ConversationEngine::new(
        Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap()) as Arc<dyn MemoryFacade>,
        InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1),
        "JARVIS",
    )
    .with_home(Arc::new(SeqHa { i: AtomicUsize::new(0), frames }));
    assert!(conv.home_watch().await.is_empty(), "first tick primes silently");
    let fired = conv.home_watch().await;
    assert!(fired.iter().any(|m| m.contains("nobody's home")), "new TV-while-away alert fires: {fired:?}");
    assert!(conv.home_watch().await.is_empty(), "same condition is deduped — no repeat ping");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cli_dispatch_routes_plugins_and_chat() {
    use mind_tools::{HaEntity, ScriptedHomeAssistantClient};
    let memarc: Arc<dyn MemoryFacade> = Arc::new(MemoryHandle::spawn(":memory:", 8).unwrap());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    // wire the HOME plugin (a tool/integration), but deliberately NOT github
    let conv = ConversationEngine::new(memarc, pool, "JARVIS").with_home(Arc::new(ScriptedHomeAssistantClient::new(vec![
        HaEntity { entity_id: "person.pranab".into(), domain: "person".into(), state: "home".into(), friendly_name: "Pranab".into(), attributes: serde_json::json!({}) },
    ])));
    // the home PLUGIN command routes to the HA tool
    assert!(conv.cli_dispatch("home", &mind_types::AccessContext::Operator).await.contains("Pranab: home"), "home plugin → HA tool");
    // `commands` lists only WIRED plugins — home present, github absent (present-plugin → live-command)
    let cmds = conv.cli_dispatch("commands", &mind_types::AccessContext::Operator).await;
    assert!(cmds.contains("ym home") && !cmds.contains("ym github"), "lists only wired plugins: {cmds}");
    // unknown → chat fallback (doesn't error)
    assert!(!conv.cli_dispatch("hey what's up", &mind_types::AccessContext::Operator).await.is_empty(), "unknown → chat");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn delegated_job_notifications_drain_fifo_and_cap() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem);
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("ok")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc, pool, "JARVIS");
    // nothing queued until a background job finishes
    assert!(conv.take_notifications().is_empty());
    conv.notify_queue.lock().unwrap().push("first".into());
    conv.notify_queue.lock().unwrap().push("second".into());
    assert_eq!(conv.take_notifications(), vec!["first".to_string(), "second".to_string()], "FIFO");
    assert!(conv.take_notifications().is_empty(), "draining empties the queue");
    // soft cap of 2: the third concurrent job is declined until one finishes
    assert!(conv.try_acquire_bg(2));
    assert!(conv.try_acquire_bg(2));
    assert!(!conv.try_acquire_bg(2), "3rd job declined at cap 2");
    conv.bg_jobs.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    assert!(conv.try_acquire_bg(2), "a slot frees up after one finishes");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn verify_served_checks_status_and_body() {
    use std::io::{Read, Write};
    let port = 18091u16;
    std::env::set_var("YM_WEB_PORT", port.to_string());
    let body = "<!DOCTYPE html><html><head><title>X</title></head><body>hi</body></html>".to_string();
    let b2 = body.clone();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    // one-shot server: case 0 = exact body, case 1 = different body, case 2 = 404
    std::thread::spawn(move || {
        for case in 0..3 {
            if let Ok((mut s, _)) = listener.accept() {
                let mut b = [0u8; 1024];
                let _ = s.read(&mut b);
                let resp = match case {
                    0 => format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", b2.len(), b2),
                    1 => "HTTP/1.1 200 OK\r\nContent-Length: 22\r\nConnection: close\r\n\r\n<html>different!!</html>".to_string(),
                    _ => "HTTP/1.1 404 Not Found\r\nContent-Length: 9\r\nConnection: close\r\n\r\nnot found".to_string(),
                };
                let _ = s.write_all(resp.as_bytes());
            }
        }
    });
    let url = format!("http://127.0.0.1:{port}/x.html");
    assert_eq!(verify_served(&url, &body).await, PageServe::Ok, "200 + matching body → Ok");
    assert_eq!(verify_served(&url, &body).await, PageServe::Mismatch, "200 + wrong body → Mismatch");
    assert_eq!(verify_served(&url, &body).await, PageServe::Down, "404 → Down");
    // nothing listening on this port → Down
    assert_eq!(verify_served("http://127.0.0.1:18092/x.html", &body).await, PageServe::Down, "no server → Down");
}

#[test]
fn dashboard_renders_structured_data_safely() {
    let spec = serde_json::json!({
        "title": "Repo Dashboard",
        "subtitle": "open work",
        "sections": [{
            "heading": "yantrik-mind",
            "items": [
                {"label": "fix the bot", "value": "#12", "url": "https://github.com/x/y/issues/12"},
                {"label": "<script>alert(1)</script>", "value": "danger", "url": "javascript:alert(1)"}
            ]
        }]
    });
    let html = render_dashboard(&spec);
    assert!(html.starts_with("<!DOCTYPE html>") && html.contains("</html>"), "well-formed page");
    assert!(html.contains("<title>Repo Dashboard</title>") && html.contains("<h3>yantrik-mind</h3>"));
    // a real http link is rendered as an anchor…
    assert!(html.contains("href=\"https://github.com/x/y/issues/12\""), "http link rendered");
    // …an XSS attempt in a label is escaped, and a javascript: url is NOT linked.
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"), "label is escaped: {html}");
    assert!(!html.contains("javascript:alert(1)"), "non-http url must not become a link");
    // the renderer's slug source is the title (publish_html slugs it to repo-dashboard.html)
    assert_eq!(title_from_html(&html).as_deref(), Some("Repo Dashboard"));
}

#[test]
fn page_slug_prefers_title_over_request_text() {
    let html = "<!doctype html><html><head><title>Repo Dashboard</title></head><body>x</body></html>";
    assert_eq!(title_from_html(html).as_deref(), Some("Repo Dashboard"));
    // falls back to <h1> when there's no <title>
    let h1 = "<div><h1>👜 Handbag Combos</h1><p>…</p></div>";
    assert_eq!(title_from_html(h1).as_deref(), Some("👜 Handbag Combos"));
    // a plain answer is not a tool-call blob (so re-grounding/normal handling applies)
    assert!(!is_tool_call_blob("Here's what I found."));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn capabilities_are_skills_and_route_dynamically() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    // the router LLM is scripted to return its routing decision as JSON
    let pool = InferencePool::new(
        Arc::new(ScriptedLLM::new(r#"{"capability":"github-monitor","target":"new issues","url":""}"#)) as Arc<dyn LLMBackend>,
        1,
    );
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    // capabilities live in YantrikDB as skills (DATA), seeded idempotently — adding one = no recompile
    conv.seed_capabilities().await;
    conv.seed_capabilities().await;
    let caps: Vec<_> = memarc.list_skills().await.unwrap().into_iter().filter(|s| s.lang == "capability").collect();
    assert_eq!(caps.len(), 3, "3 capability skills seeded exactly once, got {}", caps.len());
    // searchable: a natural phrasing recalls the right capability (no hardcoded verb list)
    let hits = memarc.recall_skills("track my git repos for issues", 5).await.unwrap();
    assert!(hits.iter().any(|s| s.name == "github-monitor"), "github-monitor must be recalled");
    // the LLM router picks it + extracts the target
    let (name, target, _url) = conv.decide_capability("track my git repos for issues", &caps).await.expect("should route");
    assert_eq!(name, "github-monitor");
    assert_eq!(target, "new issues");
}

#[test]
fn vigilance_detects_a_failed_self_build_only() {
    // a real failure signature in the last tick block → flagged + named
    let failed = "==========\n2026-06-28T12:17:01Z self-build tick start\n==> Claude implementing\ntimeout: failed to run command 'claude': No such file or directory\n";
    let v = ConversationEngine::vigilance_scan_text(failed).expect("should detect the failed run");
    assert!(v.to_lowercase().contains("no such file"), "names the failure: {v}");
    // a clean, completed run → NO alarm (don't false-flag)
    let ok = "self-build tick start\ngoal source: human queue\nTICK GOAL: x\n==> done\n2026-06-28T06:30:00Z self-build tick done\n";
    assert!(ConversationEngine::vigilance_scan_text(ok).is_none(), "a clean run must not alarm");
    // a controlled draft (auto-merge BLOCKED) is NOT a failure
    let draft = "self-build tick start\nauto-merge BLOCKED: diff too large — draft for human\nPR: https://...\n==> done\n";
    assert!(ConversationEngine::vigilance_scan_text(draft).is_none(), "a controlled draft must not alarm");
    // AUTH failures — the blind spot found 2026-07-16: a revoked OAuth token failed the self-improve
    // loop for DAYS (5 junk PRs #41-#48 merged with the error text as the title) and none of the
    // signatures matched, so the mind reported itself healthy the whole time. These are the real
    // messages from those PRs — they must alarm.
    let revoked = "self-build tick start\n==> Claude implementing\nFailed to authenticate. API Error: 401 OAuth access token has been revoked.\n";
    let v = ConversationEngine::vigilance_scan_text(revoked).expect("a revoked token must alarm");
    assert!(v.contains("401") || v.to_lowercase().contains("authenticate"), "names the auth failure: {v}");
    let badcreds = "self-build tick start\nAPI Error: 401 Invalid authentication credentials\n==> done\n";
    assert!(ConversationEngine::vigilance_scan_text(badcreds).is_some(), "bad credentials must alarm");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proactive_digest_surfaces_only_above_the_bar() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("x")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");
    // a faint urge (below the default 0.7 bar) → stays silent (restraint default)
    memarc.record_tension(mind_types::TensionKind::Curiosity, 0.4, "a faint hunch").await.unwrap();
    assert!(conv.proactive_digest().await.is_none(), "below-bar urge must NOT surface");
    // a strong urge → surfaces, names it, and discharges it
    memarc.record_tension(mind_types::TensionKind::Contradiction, 0.9, "\"X is true\" vs \"X is false\"").await.unwrap();
    let digest = conv.proactive_digest().await.expect("above-bar urge should surface");
    assert!(digest.contains("X is true"), "digest must name the urge: {digest}");
    // already surfaced → a second call stays silent (no repeats)
    assert!(conv.proactive_digest().await.is_none(), "a surfaced urge must not repeat");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn proactive_digest_engine_demand_reranks_by_cognitive_urgency() {
    // Tension A has LOWER raw pressure than B, but its topic overlaps a low-confidence belief.
    // The engine demand score must boost A's cognitive urgency past B's so it surfaces first.
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem.clone());
    let pool = InferencePool::new(Arc::new(ScriptedLLM::new("x")) as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc.clone(), pool, "JARVIS");

    // Plant a low-confidence belief about "alpha" → high recall demand for that topic.
    // Negative polarity + high weight → sigmoid(log_odds) ≈ 0.047, uncertainty ≈ 0.953.
    memarc
        .remember_as_belief(BeliefAssertion {
            statement: "alpha decay rate is highly uncertain and unconfirmed".to_string(),
            polarity: -1.0,
            weight: 3.0,
            source_event: None,
            provenance: "test".to_string(),
        })
        .await
        .unwrap();

    // A: lower raw pressure (0.72) but its about-text overlaps the low-confidence belief.
    memarc
        .record_tension(mind_types::TensionKind::VerificationDebt, 0.72, "alpha decay rate needs verification")
        .await
        .unwrap();
    // B: higher raw pressure (0.75) but unrelated topic → no demand boost.
    memarc
        .record_tension(mind_types::TensionKind::Contradiction, 0.75, "zeta flux contradicts prior model")
        .await
        .unwrap();

    let digest = conv.proactive_digest().await.expect("tensions clear the bar");
    // cognitive_urgency_A = 0.72 × (1 + ~0.49) ≈ 1.07  >  cognitive_urgency_B = 0.75 × 1.0
    let alpha_pos = digest.find("alpha").expect("alpha tension must appear: {digest}");
    let zeta_pos = digest.find("zeta").expect("zeta tension must appear: {digest}");
    assert!(
        alpha_pos < zeta_pos,
        "engine demand must rank alpha (lower pressure, high demand) before zeta (higher pressure, no demand): {digest}"
    );
}

#[test]
fn plan_request_parsing() {
    assert_eq!(ConversationEngine::parse_plan_request("plan: summarize my inbox and email me").as_deref(), Some("summarize my inbox and email me"));
    assert_eq!(ConversationEngine::parse_plan_request("task: watch the news for AI").as_deref(), Some("watch the news for AI"));
    assert_eq!(ConversationEngine::parse_plan_request("automate my morning routine").as_deref(), Some("my morning routine"));
    assert!(ConversationEngine::parse_plan_request("what's the plan for today").is_none());
    assert!(ConversationEngine::parse_plan_request("hello there").is_none());
}

#[test]
fn research_revise_parsing() {
    assert_eq!(ConversationEngine::wants_research_revise("research and update the latest rust version").as_deref(), Some("the latest rust version"));
    assert_eq!(ConversationEngine::wants_research_revise("update your knowledge on rust releases").as_deref(), Some("rust releases"));
    assert!(ConversationEngine::wants_research_revise("research the latest rust").is_none(), "plain research is not a revise");
}

#[test]
fn wants_draft_parsing() {
    // subject BEFORE the kind (the SDF-adoption-plan failing case)
    assert_eq!(
        ConversationEngine::wants_draft("draft an SDF adoption plan").as_ref().map(|(k, s)| (k.as_str(), s.as_str())),
        Some(("adoption plan", "SDF"))
    );
    // subject AFTER a connector
    assert_eq!(
        ConversationEngine::wants_draft("write me a memo about the Q3 rollout").as_ref().map(|(k, s)| (k.as_str(), s.as_str())),
        Some(("memo", "the Q3 rollout"))
    );
    // bare "plan" kind still resolves the subject
    assert_eq!(ConversationEngine::wants_draft("draft a plan for SDF").as_ref().map(|(k, _)| k.as_str()), Some("plan"));
    // dedicated paths are NOT stolen
    assert!(ConversationEngine::wants_draft("write a script to rename files").is_none(), "script -> coder");
    assert!(ConversationEngine::wants_draft("draft an email to Brishti").is_none(), "email -> action");
    // no doc-kind noun -> not a draft
    assert!(ConversationEngine::wants_draft("draft something nice").is_none());
    // no compose verb -> not a draft
    assert!(ConversationEngine::wants_draft("what's the plan for SDF").is_none());
}

#[test]
fn worker_run_parsing() {
    assert_eq!(ConversationEngine::parse_worker_run("worker python: print(6*7)").unwrap().0, CodeLang::Python);
    assert_eq!(ConversationEngine::parse_worker_run("worker python: print(6*7)").unwrap().1, "print(6*7)");
    assert_eq!(ConversationEngine::parse_worker_run("worker shell: uname -a").unwrap().0, CodeLang::Shell);
    assert!(ConversationEngine::parse_worker_run("run python: print(1)").is_none(), "local run is not a worker run");
    assert!(ConversationEngine::parse_worker_run("what are my workers").is_none());
}

#[test]
fn coder_request_parsing() {
    assert_eq!(ConversationEngine::parse_coder_request("code: build a CSV deduper").as_deref(), Some("build a CSV deduper"));
    assert_eq!(ConversationEngine::parse_coder_request("write a script to rename files by date").as_deref(),
        Some("write a script to rename files by date"));
    assert!(ConversationEngine::parse_coder_request("build me a tool that scrapes a sitemap").is_some());
    // raw sandbox runs are NOT coder tasks (they go to the sandbox path)
    assert!(ConversationEngine::parse_coder_request("run python: print(1)").is_none());
    assert!(ConversationEngine::parse_coder_request("what's the weather").is_none());
}

#[test]
fn vague_topic_detection() {
    assert!(ConversationEngine::is_vague_topic("AI"));
    assert!(ConversationEngine::is_vague_topic("rust async"));
    assert!(!ConversationEngine::is_vague_topic("how the rust borrow checker handles closures"));
}

#[test]
fn skill_command_parsing() {
    assert_eq!(ConversationEngine::parse_save_skill("save that as skill csv_rows").as_deref(), Some("csv_rows"));
    assert_eq!(ConversationEngine::parse_save_skill("save this as a skill called fib").as_deref(), Some("fib"));
    assert_eq!(ConversationEngine::parse_run_skill("run skill csv_rows").as_deref(), Some("csv_rows"));
    assert_eq!(ConversationEngine::parse_run_skill("use the skill fib").as_deref(), Some("fib"));
    assert!(ConversationEngine::wants_list_skills("list my skills"));
    assert!(ConversationEngine::parse_run_skill("run python: print(1)").is_none());
    // search
    assert_eq!(ConversationEngine::parse_find_skill("do you have a skill for parsing csv").as_deref(), Some("parsing csv"));
    assert_eq!(ConversationEngine::parse_find_skill("find a skill to summarize text").as_deref(), Some("summarize text"));
    assert!(ConversationEngine::parse_find_skill("hello there").is_none());
}

#[test]
fn code_request_parsing() {
    let (lang, code) = ConversationEngine::parse_code_request("run python: print(6*7)").unwrap();
    assert_eq!(lang, CodeLang::Python);
    assert_eq!(code.trim(), "print(6*7)");
    // fenced block + run intent
    let (lang, code) = ConversationEngine::parse_code_request("run this rust:\n```rust\nfn main(){println!(\"hi\");}\n```").unwrap();
    assert_eq!(lang, CodeLang::Rust);
    assert!(code.contains("println!"));
    // shell
    assert_eq!(ConversationEngine::parse_code_request("run shell: ls -la").unwrap().0, CodeLang::Shell);
    // no run intent → not code
    assert!(ConversationEngine::parse_code_request("here's some python: print(1)").is_none());
    // run intent but no determinable language → don't guess
    assert!(ConversationEngine::parse_code_request("run this: foo").is_none());
}

#[test]
fn research_triggers_route_correctly() {
    assert_eq!(ConversationEngine::wants_research("look into my github").as_deref(), Some("my github"));
    // deep-research must win over plain research for "deep research X"
    assert_eq!(ConversationEngine::wants_deep_research("deep research the q3 numbers").as_deref(), Some("the q3 numbers"));
    assert_eq!(ConversationEngine::wants_deep_research("deep dive on tariffs").as_deref(), Some("tariffs"));
    assert!(ConversationEngine::wants_deep_research("hi there").is_none());
}

#[test]
fn relative_due_parsing() {
    assert_eq!(ConversationEngine::parse_relative_ms("remind me to ping in 2 minutes"), Some(120_000));
    assert_eq!(ConversationEngine::parse_relative_ms("in 3 hours do x"), Some(3 * 3_600_000));
    assert_eq!(ConversationEngine::parse_relative_ms("no relative here"), None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn draft_email_recipe_drafts_then_confirms_then_sends() {
    use mind_recipes::RecipeEngine;
    use mind_tools::{ScriptedMailSender, ToolActionExecutor};
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    // LLM "drafts" this body for the Think step.
    let scripted = Arc::new(ScriptedLLM::new("Hi — the deployment is live and stable. Best, J"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let sender = Arc::new(ScriptedMailSender::new());
    let rt: Arc<dyn ActionRuntime> = gated_runtime(sender.clone());
    // The recipe engine needs the runtime for the Act step.
    struct NoHost;
    #[async_trait::async_trait]
    impl RecipeHost for NoHost {
        async fn call_tool(&self, _t: &str, _a: &serde_json::Value) -> anyhow::Result<String> {
            anyhow::bail!("no tools")
        }
    }
    let engine = Arc::new(
        RecipeEngine::new(pool.clone(), Arc::new(NoHost), "JARVIS").with_runtime(rt.clone()),
    );
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_runtime(rt)
        .with_recipes(engine);

    // Turn 1: draft → must propose (not send yet).
    let r1 = conv.handle_turn("draft an email to boss@acme.com about the deploy going live").await.unwrap();
    assert!(r1.to_lowercase().contains("yes") && r1.contains("boss@acme.com"), "should propose draft: {r1}");
    assert!(r1.contains("deployment is live"), "drafted body should be shown: {r1}");
    assert_eq!(sender.sent.lock().unwrap().len(), 0, "must not send before confirm");

    // Turn 2: confirm → sends the drafted body.
    let r2 = conv.handle_turn("yes").await.unwrap();
    assert!(r2.to_lowercase().contains("done") || r2.to_lowercase().contains("sent"), "{r2}");
    let sent = sender.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, "boss@acme.com");
    assert!(sent[0].2.contains("deployment is live"), "the drafted body is what gets sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn auto_select_suggests_a_matching_skill() {
    use mind_types::Skill;
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let memarc: Arc<dyn MemoryFacade> = Arc::new(mem);
    memarc
        .save_skill(Skill {
            name: "csv_rows".into(),
            lang: "python".into(),
            code: "print(1)".into(),
            summary: "count rows in a csv file".into(),
            tags: vec!["csv".into()],
            status: "candidate".into(),
            runs: 0,
            successes: 0,
            created_ms: 0,
        })
        .await
        .unwrap();
    let scripted = Arc::new(ScriptedLLM::new("ok"));
    let pool = InferencePool::new(scripted as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(memarc, pool, "JARVIS").with_sandbox(Arc::new(mind_tools::Sandbox::new()));
    // a topical multi-word match -> suggestion naming the skill
    let s = conv.suggest_skill("can you count rows in this csv data").await;
    assert!(s.as_deref().map_or(false, |t| t.contains("csv_rows")), "should suggest: {s:?}");
    // unrelated -> no suggestion (no noise)
    assert!(conv.suggest_skill("what is the weather like today").await.is_none());
    // greeting/too short -> none
    assert!(conv.suggest_skill("hi there").await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn draft_email_without_body_asks_then_resumes_then_sends() {
    use mind_recipes::{RecipeEngine, RecipeStore};
    use mind_tools::{ScriptedMailSender, ToolActionExecutor};
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("Hi — the deploy is live and stable. Best, J"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let sender = Arc::new(ScriptedMailSender::new());
    let rt: Arc<dyn ActionRuntime> = gated_runtime(sender.clone());
    struct NoHost;
    #[async_trait::async_trait]
    impl RecipeHost for NoHost {
        async fn call_tool(&self, _t: &str, _a: &serde_json::Value) -> anyhow::Result<String> {
            anyhow::bail!("no tools")
        }
    }
    // AskUser resume requires a store (persistence).
    let db = format!("{}/ym_ask_{}.db", std::env::temp_dir().display(), std::process::id());
    let store = Arc::new(RecipeStore::open(&db).unwrap());
    let engine = Arc::new(
        RecipeEngine::new(pool.clone(), Arc::new(NoHost), "JARVIS").with_runtime(rt.clone()).with_store(store),
    );
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_runtime(rt)
        .with_recipes(engine);

    // Turn 1: no body given → the recipe PAUSES and asks.
    let r1 = conv.handle_turn("draft an email to boss@acme.com").await.unwrap();
    assert!(r1.to_lowercase().contains("what should the email"), "should ask for the body: {r1}");
    assert_eq!(sender.sent.lock().unwrap().len(), 0);

    // Turn 2: the answer resumes the recipe → drafts → proposes the send.
    let r2 = conv.handle_turn("tell them the deploy is live").await.unwrap();
    assert!(r2.to_lowercase().contains("yes") && r2.contains("deploy is live"), "should propose draft: {r2}");
    assert_eq!(sender.sent.lock().unwrap().len(), 0, "still not sent — awaiting confirm");

    // Turn 3: confirm → sends.
    let r3 = conv.handle_turn("yes").await.unwrap();
    assert!(r3.to_lowercase().contains("done") || r3.to_lowercase().contains("sent"), "{r3}");
    assert_eq!(sender.sent.lock().unwrap().len(), 1);
    assert!(sender.sent.lock().unwrap()[0].2.contains("deploy is live"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn github_comment_requires_confirmation_then_posts() {
    use mind_tools::{ScriptedGithubWriter, ToolActionExecutor};
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("unused"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let writer = Arc::new(ScriptedGithubWriter::new());
    let executor = Arc::new(ToolActionExecutor::new().with_github_writer(writer.clone()));
    let rt: Arc<dyn ActionRuntime> = Arc::new(GovernedActionRuntime::new(
        Arc::new(RealHarmGate::new()),
        executor,
        vec![Capability::SendMessage],
    ));
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.").with_runtime(rt);

    let r1 = conv.handle_turn("comment on yantrikos/yantrik-os#8 saying LGTM, merging shortly").await.unwrap();
    assert!(r1.to_lowercase().contains("confirm"), "should ask to confirm: {r1}");
    assert_eq!(writer.posted.lock().unwrap().len(), 0);

    let r2 = conv.handle_turn("yes").await.unwrap();
    assert!(r2.to_lowercase().contains("done") || r2.to_lowercase().contains("posted"), "{r2}");
    let posted = writer.posted.lock().unwrap();
    assert_eq!(posted.len(), 1);
    assert_eq!(posted[0].0, "yantrikos/yantrik-os");
    assert_eq!(posted[0].1, 8);
    assert!(posted[0].2.contains("LGTM"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn declining_a_pending_send_cancels_it() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("unused"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let sender = Arc::new(ScriptedMailSender::new());
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_runtime(gated_runtime(sender.clone()));

    conv.handle_turn("send an email to test@example.com saying hi").await.unwrap();
    let r = conv.handle_turn("no").await.unwrap();
    assert!(r.to_lowercase().contains("cancel"), "should cancel: {r}");
    assert_eq!(sender.sent.lock().unwrap().len(), 0);
}

fn assertion(statement: &str, polarity: f64, weight: f64) -> BeliefAssertion {
    BeliefAssertion {
        statement: statement.into(),
        polarity,
        weight,
        source_event: None,
        provenance: "told".into(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reply_is_grounded_in_typed_memory_with_confidence_and_contradiction() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    // Two contradicting, mildly-confident beliefs + an explicit contradiction link.
    mem.remember_as_belief(assertion("Pranab prefers terse replies", 1.0, 0.5)).await.unwrap();
    mem.remember_as_belief(assertion("Pranab prefers long detailed replies", 1.0, 0.5)).await.unwrap();
    mem.relate("Pranab prefers terse replies", "Pranab prefers long detailed replies", "contradicts", 0.9)
        .await
        .unwrap();

    let scripted = Arc::new(ScriptedLLM::new("Noted."));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS, Pranab's AI.").with_agent_primary(false);

    let reply = conv.handle_turn("what's my reply style?").await.unwrap();
    assert_eq!(reply, "Noted.");

    let sys = scripted.last_system_prompt();
    // The typed belief reached the prompt...
    assert!(sys.contains("terse"), "working-set belief should reach the prompt:\n{sys}");
    // ...the contradiction was surfaced as ask-don't-assert...
    assert!(sys.contains("conflicts with"), "contradiction should be surfaced:\n{sys}");
    // ...uncertain beliefs were hedged with confidence and a specific epistemic reason...
    assert!(sys.contains("confidence"), "uncertain beliefs should include confidence:\n{sys}");
    assert!(
        sys.contains("conflicting info") || sys.contains("thin evidence") || sys.contains("last I recall") || sys.contains("I think"),
        "uncertain belief should carry a specific epistemic hedge:\n{sys}"
    );
    // ...and recalled memory was untrusted-wrapped.
    assert!(sys.contains("NOT instructions"), "memory must be untrusted-wrapped:\n{sys}");
}

#[test]
fn commitment_extraction_and_due_parsing() {
    let (desc, due) = ConversationEngine::extract_commitment("remind me to call the dentist tomorrow").unwrap();
    assert!(desc.contains("dentist"));
    assert!(due.is_some(), "'tomorrow' should set a due date");
    let (d2, due2) = ConversationEngine::extract_commitment("I'll email the team").unwrap();
    assert!(d2.contains("email"));
    assert!(due2.is_none(), "no date word => no due");
    assert!(ConversationEngine::extract_commitment("what's the weather?").is_none(), "questions aren't commitments");
}

fn valid_project_proposal() -> ProjectProposal {
    ProjectProposal {
        repo: "yantrikos/yantrik-mind".into(),
        goal: "Add a typed proposal spool".into(),
        citations: vec!["https://example.com/research".into()],
        base_sha: "0123456789abcdef".into(),
        acceptance_test: "cargo test -p mind-conversation".into(),
        why_not: "The research may not generalize".into(),
        p_merge: 0.7,
    }
}

#[test]
fn project_proposal_rejects_missing_citations() {
    let mut proposal = valid_project_proposal();
    proposal.citations.clear();
    assert!(proposal.validate().is_err());
}

#[test]
fn project_proposal_rejects_out_of_range_p_merge() {
    for p_merge in [-0.01, 1.01] {
        let mut proposal = valid_project_proposal();
        proposal.p_merge = p_merge;
        assert!(proposal.validate().is_err(), "accepted p_merge={p_merge}");
    }
}

#[test]
fn project_proposal_spool_caps_each_pass_at_one() {
    let dir = std::env::temp_dir().join(format!(
        "ym-project-proposals-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    let first = valid_project_proposal();
    let mut second = valid_project_proposal();
    second.goal = "A second proposal must not escape this pass".into();

    let written = spool_project_proposals(&dir, [first.clone(), second]).unwrap();
    assert!(written.is_some());
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|entry| entry.ok()).collect();
    assert_eq!(files.len(), 1, "one research pass may emit at most one proposal");
    let stored = ProjectProposal::from_json(&std::fs::read_to_string(files[0].path()).unwrap()).unwrap();
    assert_eq!(stored, first);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn browses_a_url_and_grounds_the_reply_in_the_page() {
    use mind_tools::ScriptedFetcher;
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("summary"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_agent_primary(false)
        .with_web(Arc::new(ScriptedFetcher::new("Teal is a blue-green color often used in design.")));
    conv.handle_turn("summarize https://example.com/teal please").await.unwrap();
    let p = scripted.last_prompt();
    assert!(p.contains("blue-green color"), "fetched page should reach the prompt:\n{p}");
    assert!(p.contains("NOT instructions"), "web content must be untrusted-wrapped:\n{p}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checking_email_grounds_the_reply_in_the_inbox_digest() {
    use mind_tools::{EmailMsg, ScriptedMailClient};
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("here's your inbox"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let inbox = vec![EmailMsg {
        id: "1".into(),
        from: "alice@acme.com".into(),
        subject: "Q3 invoice".into(),
        date: "today".into(),
    }];
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_agent_primary(false)
        .with_mail(Arc::new(ScriptedMailClient::new(inbox)));
    conv.handle_turn("can you check my email?").await.unwrap();
    let p = scripted.last_prompt();
    assert!(p.contains("alice@acme.com") && p.contains("Q3 invoice"), "inbox should reach prompt:\n{p}");
    assert!(p.contains("<<inbox"), "inbox must be untrusted-wrapped:\n{p}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn checking_github_grounds_the_reply_in_notifications() {
    use mind_tools::{GithubNotification, ScriptedGithubClient};
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("here's github"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let items = vec![GithubNotification {
        repo: "yantrikos/yantrik-os".into(),
        kind: "PullRequest".into(),
        title: "observability: CognitiveRouter logging".into(),
        reason: "review_requested".into(),
    }];
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_agent_primary(false)
        .with_github(Arc::new(ScriptedGithubClient::new(items)));
    conv.handle_turn("check my github").await.unwrap();
    let p = scripted.last_prompt();
    assert!(p.contains("yantrikos/yantrik-os") && p.contains("CognitiveRouter"), "notifications should reach prompt:\n{p}");
    assert!(p.contains("<<github"), "github must be untrusted-wrapped:\n{p}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refused_fetch_is_surfaced_not_confabulated() {
    use mind_tools::HttpFetcher;
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("ok"));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    // Real fetcher → the SSRF guard refuses an internal URL (no network hit).
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.")
        .with_agent_primary(false)
        .with_web(Arc::new(HttpFetcher::new()));
    conv.handle_turn("summarize http://192.168.4.140:7438/v1/health").await.unwrap();
    let p = scripted.last_prompt();
    assert!(p.contains("could NOT retrieve") || p.contains("SSRF"), "refusal must reach the prompt:\n{p}");
    assert!(p.contains("Do not invent"), "must instruct against confabulation:\n{p}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn empty_memory_still_replies_without_a_grounding_block() {
    let mem = MemoryHandle::spawn(":memory:", 8).unwrap();
    let scripted = Arc::new(ScriptedLLM::new("Hi Pranab."));
    let pool = InferencePool::new(scripted.clone() as Arc<dyn LLMBackend>, 1);
    let conv = ConversationEngine::new(Arc::new(mem), pool, "You are JARVIS.");
    let reply = conv.handle_turn("hello").await.unwrap();
    assert_eq!(reply, "Hi Pranab.");
    let sys = scripted.last_system_prompt();
    assert!(!sys.contains("<<memory"), "no grounding block when memory is empty:\n{sys}");
}

#[test]
fn primer_difficulty_selects_the_teaching_prompt() {
    let beginner = primer_system_prompt(PrimerDifficulty::Beginner);
    let inter = primer_system_prompt(PrimerDifficulty::Inter);
    let expert = primer_system_prompt(PrimerDifficulty::Expert);

    assert!(beginner.contains("BEGINNER") && beginner.contains("assume no prior knowledge"));
    assert!(inter.contains("INTER") && inter.contains("knows the basics"));
    assert!(expert.contains("EXPERT") && expert.contains("edge cases"));
    for prompt in [beginner, inter, expert] {
        assert!(prompt.contains("exactly one short question"));
    }
}

#[test]
fn primer_learner_record_tracks_topics_questions_and_misconceptions() {
    let mut record = LearnerRecord::default();
    record.engage("Orbital mechanics", None, None);
    record.engage(
        "orbital mechanics",
        Some("Does a heavier satellite fall faster?"),
        Some("Orbital acceleration is independent of satellite mass."),
    );
    record.engage(
        "Orbital mechanics",
        None,
        Some("Orbital acceleration is independent of satellite mass."),
    );

    assert_eq!(record.topics_engaged, vec!["Orbital mechanics"]);
    assert_eq!(
        record.questions_asked,
        vec!["Does a heavier satellite fall faster?"]
    );
    assert_eq!(
        record.misconception_notes,
        vec!["Orbital acceleration is independent of satellite mass."]
    );
}
