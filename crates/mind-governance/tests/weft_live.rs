//! Live Weft attestation test — ignored by default; it needs a real `weftd` with a genesis repo.
//!
//!   weftd &                          # a gate on :8747
//!   weft init --name mind-trust      # bootstrap a repo, writes .weft-cli.key
//!   YM_WEFT_URL=http://127.0.0.1:8747 YM_WEFT_KEY=$(cat .weft-cli.key) \
//!     cargo test -p mind-governance --test weft_live -- --ignored --nocapture
//!
//! This is the test that separates "compiles against my reading of the API" from "a signed note
//! actually landed on a Weft repo".

use mind_governance::weft::{Attestation, Attestor, WeftAttestor};

#[test]
#[ignore = "needs a live weftd with a genesis repo (see module docs)"]
fn a_verdict_lands_as_a_signed_note() {
    let a = WeftAttestor::from_env().expect("set YM_WEFT_URL + YM_WEFT_KEY");
    let doc = br#"{"pack":"live_check","skills":[]}"#;

    let pass = Attestation {
        subject: "pack:live_check".into(),
        verdict: true,
        digest: Attestation::digest_of(doc),
        evidence: vec![
            "   ✓ skill_exists(live)".into(),
            "   ✓ tool_contains(calc ⊇ \"4\")".into(),
        ],
    };
    let oid = a.attest(&pass).expect("certification must land");
    assert_eq!(
        oid.len(),
        64,
        "a weft oid is a 32-byte content address: {oid}"
    );

    // A demotion of the SAME subject lands as its own claim — trust history is append-only.
    let fail = Attestation {
        verdict: false,
        ..pass
    };
    let oid2 = a.attest(&fail).expect("demotion must land");
    assert_ne!(oid, oid2, "distinct verdicts are distinct objects");

    // Both are readable back from the ledger, by anyone, without trusting the mind's own state.
    let notes: String = ureq::get(&format!("{}/notes", std::env::var("YM_WEFT_URL").unwrap()))
        .call()
        .expect("read notes")
        .into_string()
        .expect("body");
    assert!(
        notes.contains("CERTIFIED pack:live_check"),
        "certification is on the ledger"
    );
    assert!(
        notes.contains("DEMOTED pack:live_check"),
        "demotion is on the ledger"
    );
    assert!(
        notes.contains(&Attestation::digest_of(doc)),
        "the claim is bound to the document digest"
    );
    println!(
        "landed: {oid} (certified) / {oid2} (demoted), identity {}",
        a.identity()
    );
}
