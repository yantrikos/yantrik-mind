fn main() {
    let t0 = std::time::Instant::now();
    let v = match mind_tools::voice::VoiceSession::start() {
        Ok(v) => v,
        Err(e) => {
            println!("  voice failed to start: {e}");
            return;
        }
    };
    println!(
        "  boot (load + prerender holds): {}ms",
        t0.elapsed().as_millis()
    );
    match v.hold(0) {
        Some((t, wav)) => println!("  hold line ready instantly: {:?} ({} bytes)", t, wav.len()),
        None => println!("  NO hold cache"),
    }
    let reply = "The Nifty is at 24,053, down about a quarter percent. Reliance never came back cleanly, so I won't guess it. Want me to re-pull it?";
    let turn = v.begin_turn();
    let t1 = std::time::Instant::now();
    let mut first_ms = None;
    let said = v.speak_reply(reply, turn, |sp| {
        if first_ms.is_none() {
            first_ms = Some(t1.elapsed().as_millis());
        }
        println!(
            "    spoke {:.2}s of audio (synth {}ms)",
            sp.secs, sp.synth_ms
        );
    });
    println!("  FIRST AUDIO after {}ms", first_ms.unwrap_or(0));
    println!("  said all of it: {}", said.len() >= reply.len() - 10);
    // Now interrupt after the first chunk.
    let turn2 = v.begin_turn();
    let mut n = 0;
    let partial = v.speak_reply(reply, turn2, |_| {
        n += 1;
        if n == 1 {
            v.interrupt();
        }
    });
    println!(
        "  after interrupt: spoke {} chunk(s), {} chars of {} — stopped early: {}",
        n,
        partial.len(),
        reply.len(),
        partial.len() < reply.len()
    );
    v.close();
}
