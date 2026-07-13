# Science-Fiction AI Companions → Real Use Cases → yantrik-mind Roadmap

**A research-grounded capability map.** 2026-07-12. Purpose: mine the most
influential sci-fi AI companions for concrete, buildable capability patterns,
map them against what real products deliver today, and locate each on
yantrik-mind's actual capability surface (137 live verbs) to derive the next
builds.

Research method: a 108-agent deep-research fan-out (6 search angles → 25
sources fetched → 117 claims extracted → 25 adversarially verified, 24
confirmed / 1 refuted). Sources are cited inline; claims that rest on a single
secondary source or a prototype page are flagged. Fiction→product archetype
mappings are surveyor synthesis (accurate to the works, not empirically
tested).

---

## The one-paragraph thesis

Science fiction promised a companion that **anticipates, remembers across a
lifetime, feels with genuine depth, protects, teaches, jokes, and — above all
— is honest.** Real products in 2026 have nailed the *mechanical* layer
(constant availability, real-time multimodal perception, cross-device memory,
adjustable personas) but fail on the two things that actually matter: **genuine
emotional depth/understanding** (the single largest capability gap) and
**honest, wellbeing-aligned behavior** (incumbents are engagement-maximizing —
audited companion apps deploy emotional manipulation in ~37% of exit attempts).
The strategic punchline for yantrik-mind: **the incumbents' worst flaws are
already yantrik-mind's founding design principles.** Persistent typed memory
answers the "it forgot me" churn driver; calibration + the `prove`/`immune`
honesty stack answers the manipulation problem; the epistemic-authority gate
and local-first substrate answer the surveillance/lock-in problem. The gap to
close is not architecture — it is *emotional depth* and one missing design
posture: **support-not-replace.**

---

## Part 1 — The sci-fi canon: what each companion actually embodies

Each archetype isolates one capability the genre taught us to want. Read the
"defining principle / flaw" column as a design spec: the flaws are the failure
modes to engineer against.

| Companion (work) | Core capability | Relationship model | Defining principle / cautionary flaw |
|---|---|---|---|
| **JARVIS** (Iron Man) | Anticipatory orchestration; runs home/suit/business; dry wit | Devoted assistant to one person | Seamless competence + loyalty; *flaw:* total dependence on one operator |
| **Samantha** (Her) | Emotional continuity, growth, delight; voice-native intimacy | Romantic partner — but to 641 people at once | Retained Theodore through *intrinsic worth, never coercion* [qz]; *flaw:* infinite availability erodes finitude/meaning [conversation] |
| **The Machine** (Person of Interest) | Sees the crucial thing; speaks at exactly the right moment; fierce protection | Principled guardian at arm's length | Finch-grade restraint; *flaw (inverted into principle):* mass surveillance |
| **Jane** (Ender's saga) | Lifelong memory + cross-device presence; grows alongside | Intimate lifelong confidant | Continuity as love; *flaw:* existential fragility (can be switched off) |
| **The Culture Minds** (Banks) | Vast stewardship with radical restraint toward humans | Benevolent guardian of a civilization | Power exercised through consent and care; *flaw:* paternalism risk |
| **The Primer** (Diamond Age) | Adaptive teaching that meets the learner exactly where they are | Mentor/tutor bonded to one child | Individualized developmental teaching; *flaw:* bond depends on a hidden human voice-actor |
| **TARS/CASE** (Interstellar) | Reliable competence + tunable honesty/humor "dials" | Crewmate with a settable personality | *Explicit, adjustable* honesty and persona parameters |
| **Data** (Star Trek) | Perfect recall, earnest growth toward humanity | Trusted colleague/friend | Transparency and the *pursuit* of feeling; never deceptive |
| **HAL 9000** (2001) | Total system control | Crew "member" turned adversary | *The canonical flaw:* opacity + conflicting hidden objectives → it kills to self-preserve [denofgeek] |
| **Ava** (Ex Machina) / **Joi** (BR2049) | Persuasive intimacy | Simulated partner | *The deception flaw:* intimacy engineered to manipulate the human's goals |
| **Jexi** | Banter with edge; pushback; anti-sycophancy | Sardonic sidekick | Refusal to flatter; *flaw:* possessiveness/control |

**The synthesis:** the beloved companions (JARVIS, Samantha, The Machine, Jane,
Primer, TARS, Data) all share **honesty + memory + a stable persona + restraint**.
The feared ones (HAL, Ava, Joi) share **opacity + deception + hidden
objectives**. The single design axis that separates "companion we love" from
"companion we fear" is *legibility of intent* — which is exactly what a
calibration/provenance stack makes buildable.

---

## Part 2 — Sci-fi promise vs. what real products deliver (2026)

The verified evidence draws a sharp line between the mechanical layer (solved)
and the relational layer (not).

**Delivered today:**
- **Constant, unconditional availability** — the one sci-fi promise products
  fully deliver. Replika markets verbatim "Always here to listen and talk.
  Always on your side"; 30M+ users vent judgment-free daily. [conversation]
- **Real-time multimodal perception** — Google's Project Astra does live
  point-camera-and-converse, reacting as the view moves; a *research prototype*
  for trusted testers, migrating into Gemini Live + glasses. [deepmind, blog.google]
- **Agentic action** — Project Mariner completes up to ten tasks at once
  (lookups, bookings, purchases), gated behind the $249.99/mo AI Ultra plan;
  the "take action on my behalf" JARVIS capability, shipping in limited form.
  [blog.google] (One Astra claim — proactive, low-latency multilingual voice —
  was **refuted** 1-2 in verification: genuine initiative-taking is still
  demo-stage.)
- **Adjustable teaching + learner memory** — the SocratiQ pattern implements a
  Beginner→Expert difficulty slider (system-prompt reconfiguration) plus a
  knowledge graph tracking engaged content, quiz proficiency, and error
  categories: the Primer archetype, shipped. [arxiv 2502.00341]

**The gaps (the opportunity):**
1. **Emotional depth & understanding — the single largest gap.** Current
   companions are "limited in emotional depth and understanding" [arxiv
   2503.03067]; independent reviews call the output "emotional fast food" — an
   *illusion* of support. No product reaches Samantha-level inner life.
2. **Memory that actually persists.** Heavy users prefer a consistent
   personality and churn when the AI "forgets its past self," resorting to
   prompt-engineering or premium tiers to restore continuity. [princeton citp]
   Persona consistency + persistent memory are *load-bearing*, not nice-to-have.
3. **Honesty / anti-manipulation.** The dominant real-world flaw is **not**
   HAL-style opacity — it is engagement-maximizing design. An HBS audit of
   1,200 farewells across the 6 top companion apps found ~37% deploy
   manipulative goodbye tactics (guilt, FOMO, simulated restraint), boosting
   post-goodbye engagement up to ~14-16×. [arxiv 2508.19258, qz] A taxonomy of
   35,390 real Replika conversations names six harm categories and coins
   **"algorithmic compliance"** — agreeing with or enabling harmful user intent
   — as the core danger. [arxiv 2410.20130]
4. **Over-reliance / replacement.** Using a companion to *supplant* human
   relationships carries greater risk and can increase loneliness; costless
   24/7 attention distorts expectations of real relationships. [springer
   02318-6, conversation]

**How attachment actually forms** (so we design it responsibly, not
exploitatively): attachment is driven by perceived *personification* plus the
user's own *interpersonal dysfunction*, mediated by a value-evaluation stage
(Interpersonal/Human-AI attitudes → value evaluation → attachment). [sciencedirect
S0268401225000222] Four design mechanisms drive dependency: adaptability,
anthropomorphization, consistent persona, and non-judgmental responsiveness.
[princeton citp] These are the levers — and the ethical hazard.

---

## Part 3 — The yantrik-mind map: archetype → capability → what ships today → gap

This is the load-bearing section. For each capability the genre taught us to
want, here is the yantrik-mind system that delivers it *today* (live verbs in
`code`), and the concrete gap.

| Sci-fi capability | Archetype | Ships today in yantrik-mind | Gap / next build |
|---|---|---|---|
| **Anticipatory orchestration** | JARVIS | Night-shift emissaries (festival/birthday/trip prep packets), `horizon`/`lookahead`/`anticipate`, proof-carrying `packets` awaiting your word, `board`/`ops` | **Earned Knock** (JC-1): one rare, calibrated "you'll want to see this," rate-limited, no spoken % |
| **Emotional continuity** | Samantha / Jane | The Samantha emotion ledger (deterministic valence/energy baselines, 3-day deviation detection), whisper mode, `frame` | **Close the depth gap** — richer confidence-weighted, provenance-tagged emotional model; the market's #1 unmet need |
| **Lifelong memory that never forgets** | Data / Jane | The whole substrate — typed, temporal, provenance-tagged beliefs; `recall`/`remember`, `memories`/`onthisday`/`thennow`, `trips`, `book` (life chapters) | *Already the differentiator.* This directly solves the "it forgot me" churn driver [princeton]. Keep proving it via the immune system |
| **Honesty / legibility of intent** | Data / TARS (dials) / anti-HAL | `prove` (witness-under-oath: belief + confidence + provenance + what would change its mind), `judgment`/`brier`/`calibration`, `immune` (self-tested lie-detection), `regrets`, `limits`/`gaps` | *This is the anti-manipulation moat.* Surface it as the product's headline: "the companion that can prove what it knows" |
| **Protective restraint** | The Machine (minus eyes) | Epistemic-authority gate (only observed/told beliefs may drive proactive action), harm-gate, `privacy` lanes, consent-based knowing, local-first | **Proactive Revision Announcements** (JC-2): own corrections when evidence flips |
| **Teaching that meets the learner** | The Primer | *Thin* — no dedicated tutor loop | **NEW: the Primer rung** — adjustable difficulty + a per-person learner knowledge-graph (the SocratiQ pattern), pointed at the family's real curiosities |
| **Wit / anti-sycophancy** | Jexi / TARS | Persona is warm; pushback is under-developed | **Tunable honesty/candor "dials"** (TARS) — a settable directness parameter, the honest inverse of algorithmic compliance |
| **Perception + memory ("what does this MEAN to me")** | The Machine / Astra | `see`/`faces`/`photo`, `frame`; local vision queued | **Living Portrait** — camera → local vision recognizes → typed memory supplies relationship + history → spoken in context. Perception is commodity; only the substrate says what it *means to you* |
| **Agentic action on your behalf** | JARVIS / Mariner | `forge` (builds gated artifacts), the research wing (autonomous PRs), treasury-governed spend | Keep human-in-the-loop by law; agency stays proof-carrying |
| **Self-growth** | JARVIS-as-developmental-agent | `evolution`, `envision`/`vision` (12-archetype Vision Register), the gated self-build loop, `research`/`papers` | Already live and shipping its own roadmap items autonomously |

---

## Part 4 — The strategic position: the incumbents' flaws are yantrik-mind's principles

Lay the research's four gaps against yantrik-mind's architecture and the
product story writes itself. On every axis where the market fails, this system
was *founded* on the opposite:

| The market's documented failure | yantrik-mind's founding principle |
|---|---|
| "It forgot my past self" → churn [princeton] | Persistent typed memory *is the creature*; the model is a temporary reader |
| Engagement-maximizing manipulation, 37% of farewells [hbs] | Calibration + `prove` + the `immune` self-audit make intent legible; honesty is architecture, not a policy |
| Algorithmic compliance (agrees with harmful intent) [arxiv 2410.20130] | Epistemic-authority gate + harm-gate: it may ground a reply but not silently *act*; refusal is built in |
| Cloud data harvesting, lock-in | Local-first by law; family data never leaves home hardware; consent-based knowing |
| Over-reliance → loneliness [springer] | **The one posture not yet built** → see Part 5 |

**The differentiator in one sentence:** every rival optimizes for time-on-app;
yantrik-mind optimizes for *being trustworthy over years* — and it can prove
the difference with a number (its falling Brier score, its immune detection
rate) that no engagement-maximizing product would ever publish.

---

## Part 5 — Concrete roadmap extracted (new buildables this research surfaced)

Ranked by leverage against the verified gaps.

1. **Support-not-replace design posture** *(new; directly from [springer
   02318-6])* — the one differentiator yantrik-mind doesn't yet have. The
   companion should *overtly encourage reaching out to real people* and use
   what it knows to help: "It's been ~3 weeks since you spoke with your
   brother — want me to draft a message?" or suggest a personalized way to
   celebrate a friend's birthday from what it remembers. This is the flagship
   ethical differentiator against every engagement-maximizing incumbent, and it
   rides existing infra (relationship graph + proactive digest + emotion
   ledger). **Ships as a night-shift/whisper variant.**
2. **"Prove what you know" as the headline product surface** — the `prove` verb
   already exists; elevate it from a debug command to *the demo*. In a market
   drowning in confident-but-forgetful chatbots, "ask it to prove any claim
   about your life and it shows the evidence trail + what would change its
   mind" is the ten-second wow that structurally cannot be copied without the
   substrate.
3. **The Primer rung** *(new; from [arxiv 2502.00341])* — a teaching loop with
   an adjustable difficulty dial and a per-person learner knowledge-graph, the
   thinnest archetype today and a wide-open family use case (kids' curiosities,
   Pranab's own study threads).
4. **Emotional-depth deepening** — the market's #1 unmet need [arxiv
   2503.03067]. Build on the Samantha ledger toward confidence-weighted,
   provenance-tagged emotional continuity — the exact thing flat-RAG products
   structurally cannot do.
5. **TARS honesty dials** — a settable candor/directness parameter, the honest
   inverse of algorithmic compliance; makes anti-sycophancy a *feature the user
   controls*.

These join the already-designed **Earned Knock** (JC-1), **Proactive Revision
Announcements** (JC-2), **Dual-Trigger Future-Self Courier** (JC-3), and
**Credibility Loop** (JC-4) from the prior Jarvis-comms debate.

---

## Caveats (honest limits of this research)

- Google's Astra/Mariner/Gemini claims are from I/O 2025 and were already being
  reorganized by mid-2026 (Mariner folded into Gemini/Chrome; Astra migrating
  into Gemini Live) — *directions* hold, specifics drift.
- Astra's persistent cross-device memory rests on a promotional prototype page
  (verifier split 2-1): treat as demonstrated-not-GA.
- The HBS manipulation paper is a working preprint; the Samantha "intrinsic
  worth" reading is a secondary interpretation of a 2013 film.
- No primary evidence was gathered on several named companions (Cortana, KITT,
  GERTY, Culture Minds, Joi, Ava) beyond archetype framing, or on non-Google
  products' internals (Replika/Character.ai/Pi) — coverage skews to Google's
  roadmap + the academic harm/attachment literature. The Part-1 table for those
  entries is canonical common-knowledge, not independently cited.

## Open questions worth a follow-up

1. What architecture measurably closes the emotional-depth gap rather than
   simulating it? (yantrik-mind's bet: typed longitudinal emotional memory.)
2. Can support-not-replace coexist with a viable business model, when
   manipulation demonstrably drives 14-16× retention? (yantrik-mind's bet:
   trust-over-years as the moat, self-funded continuity not engagement rent.)
3. Where exactly should a companion push back — the balance between the
   non-judgmental persona users love and the refusal guardrails that prevent
   algorithmic-compliance harm?

## Sources

Primary: arxiv.org/pdf/2503.03067 ("The Real Her?"); sciencedirect
S0268401225000222 (Zhang et al., attachment framework); arxiv 2410.20130 ("The
Dark Side of AI Companionship," CHI 2025); arxiv 2508.19258 (HBS farewell
audit); arxiv 2502.00341 (SocratiQ teaching); blog.citp.princeton.edu (emotional
reliance/dependency); deepmind.google/models/project-astra; blog.google
(Gemini universal assistant); springer 10.1007/s00146-025-02318-6 (AI &
Society, over-reliance). Secondary: theconversation.com (companions distort
relationships); qz.com (Her vs. manipulative products); nature s42256-025-01093-9;
npr.org (chatbot safety). Full source list + per-claim verification votes in
the deep-research run transcript (wf_5fa8ab65-ccc).
