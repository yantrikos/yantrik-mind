fn main() {
    let written = "Your paper account (PA3JXXB35SPS) is all cash: **$10,000 equity, $10,000 cash, $40,000 buying power, and no open positions.** It's active and untouched.\n\nSo it's a clean slate — ready for your first simulated trade whenever you want to test a RELIANCE or INFY idea before risking real money.\n\nWant me to help you draft a test entry (entry price, stop, size) so the first paper trade is deliberate, not random?";
    let voiced = "It's empty — $10,000 cash, no open positions, and $40,000 buying power.\n\nThat's a clean slate, so the ball's in your court on what you want to test first. Since you've been tracking INFY and RELIANCE, that's the obvious place to start — but I still don't have your primary rule or thesis for either, so I can't tell you what a good first trade would look like.\n\nTwo things I can't do yet, so I'll be straight about it:\n- **RELIANCE price** — still unconfirmed. I won't guess it.\n- **INFY** — last measured at **1,114.40 INR** (-1.68%), but that's from an earlier read, not live.\n\nIf you give me your one entry rule, I can frame the first test trade against your $10k. Want to start there?";
    for (label, text) in [("WRITTEN", written), ("VOICED ", voiced)] {
        let secs = mind_tools::flow::spoken_secs(text);
        let faults = mind_tools::flow::faults(text, 1);
        let chunks = mind_tools::speech::speakable_chunks(text, 60, 160);
        println!("{label}: {:.0}s to say, {} chunks", secs, chunks.len());
        println!("   first thing heard: {:?}", chunks.first().map(|c| c.chars().take(72).collect::<String>()).unwrap_or_default());
        println!("   faults: {}", if faults.is_empty() { "none".into() } else { faults.join("; ") });
    }
}
