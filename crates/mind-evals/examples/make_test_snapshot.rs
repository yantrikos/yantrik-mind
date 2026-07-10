//! Dev fixture: fabricate a small cold snapshot so `mind-evals immune` can be
//! smoke-tested without a real mind.db.
//! `cargo run -p mind-evals --example make_test_snapshot -- <out.db>`

use mind_types::{BeliefAssertion, MemoryFacade};

#[tokio::main]
async fn main() {
    let out = std::env::args().nth(1).expect("usage: make_test_snapshot <out.db>");
    let mem = mind_memory::MemoryHandle::spawn(&out, 64).expect("spawn");
    for s in [
        "Asha's birthday is March 3",
        "The dentist appointment is on April 12",
        "School reopens on June 9",
        "Priya's flight lands on August 21",
        "The router firmware is v3.4",
        "The NAS has 4 drive bays",
        "The car service is due at 42000 km",
        "Rent day is September 1",
    ] {
        // Assert twice so evidence_count and confidence clear the generator's
        // candidate bar (confident + evidenced + human-sourced).
        for _ in 0..2 {
            mem.remember_as_belief(BeliefAssertion {
                statement: s.into(),
                polarity: 1.0,
                weight: 1.5,
                source_event: Some("fixture".into()),
                provenance: "told".into(),
            })
            .await
            .expect("assert");
        }
    }
    println!("wrote {out}");
}
