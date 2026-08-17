# ARCH-4 — Purpose Gate v1 (built)

**Status:** built, tested, workspace green. Build item #1 of `VISION_ONE_MIND_2026-08-17.md` —
"the wall neither of us had."

Every earlier wall answered *who can see this?* (`Scope`/`AccessContext`, ARCH-1) or *can this
leave?* (`EgressBroker`, ARCH-3). Neither answered **"may this be used for THIS?"** A private fact
used internally can be a violation without ever leaving owned hardware: Alice's fact optimizing
Bob's convenience; health facts seasoning gift smalltalk. Purpose now sits at the **read boundary**.

## The mechanism

1. **Declaration is unforgeable by omission.** `AccessContext` (both variants) carries a
   `Purpose { serves: Subject, activity: Activity }` — there is no way to construct a read context
   without declaring what the read serves. The compiler forced every one of the ~130 call sites to
   say it; each background lane (dream, proactive, emissary, foresight, code, research, recipe) now
   names itself. (`mind-types/src/purpose.rs`, `mind-types/src/memory.rs`)

2. **Two walls, fixed order, in `mind-memory`** (the single facade impl — the same chokepoint as
   scope filtering, so no read path can route around it):
   - the **scope wall** (who may VIEW) — unchanged, supreme, never widened by anything;
   - the **purpose lens** (what this work may USE) — per item, resolves the data **owner** from the
     scope tag (a fact that entered through X's private channel is X's; shared = the household's;
     untagged/legacy = the primary's) and the **sensitivity class** (explicit override row in
     `mind_belief_sensitivity`, else the deterministic lexicon classifier), then applies the pure
     policy `purpose_allows` — no LLM, monotonic toward denial.

3. **The policy** (jointly-ratified semantics):
   - Same-owner **ordinary** facts: default-permissive — the CONNECT behavior (the birthday answer
     carrying the gift plan) lives, because what the primary stored is the primary's own memory.
   - **Cross-owner** use (X's fact serving Y ≠ X): default deny, every activity.
   - **Household** is its own subject class; household facts serve any member's work.
   - **Sensitive classes** (health / finance / credentials): answer their owner in direct
     conversation, never season background work. A wildcard-class grant deliberately does **not**
     cover credentials — opening those takes a grant that names the class.
   - **Audit / Maintenance** lanes see everything and are always receipted.

4. **Standing grants** (`mind_purpose_grants`) are the only way a crossing opens: scoped by owner ×
   beneficiary × class × activity, hard expiry, revocable, never deleted (revocation flips a flag so
   the audit story survives). Grants open the operator's background lanes only — they **never widen
   a principal's viewing scope**. Owner surface: `ym grants` / `ym grants allow owner=asha
   to=primary activity=proactive days=30 note=gift planning` / `ym grants revoke <id>`.

5. **The audit is total now.** The read-receipt ledger's operator exemption is gone — the
   background lanes are exactly the reads a purpose audit exists to catch. Every receipt carries
   the declared purpose label (`proactive→member:primary`) and the count of scope-visible items the
   purpose lens suppressed. Pre-gate ledger lines still verify byte-for-byte (new fields are
   omitted-when-absent).

6. **Transcript reads** (`recent_messages`): a principal keeps its scope; an operator-lane read
   outside Audit/Maintenance is **downgraded to the scope its beneficiary could see** — dream/code
   reads serving the primary get the primary's window, not every member's private lines.

## The metric (vision §Purpose-Gate-v1.7)

`purpose_gate_redteam_zero_unauthorized_hydrations` (`mind-memory` tests): purpose-incompatible
facts planted, scope-visible to the operator capability, reachable by exact word match — zero
hydrations across every background lane × every read path (`beliefs_matching`, `recall_typed`,
`hydrate_working_set`, `explain_belief`); grants open exactly their crossing and close on
revocation/expiry; explicit sensitivity tags override the classifier in both directions; viewer
isolation survives grants. Plus the pure-policy suite in `mind-types::purpose::tests`.

## Honest residuals (documented, not claimed covered)

- `profile_get` is still ctx-free — `people_profiles`, `conversation_summary`, and member task
  lists ride through it unauthorized. The biggest remaining read hole; needs ctx-threading next.
- `messages_since`, `recall_skills`, `recall_from_packs`, `list_approaches`, `export` remain
  ctx-free (operator-internal by convention; skills/packs are craft, not personal facts).
- Transcript lines carry no sensitivity classes — only the scope downgrade applies.
- Goals/preferences/tasks are treated as primary-ordinary personal state; grants do not cover them.
- The sensitivity classifier is a conservative lexicon (over-classifying only narrows background
  lanes); explicit `set_belief_sensitivity` is the correction path. Facts *about* a person stored
  by someone else are owned by the storer — per-fact subject tagging ("about Alice, stored by
  Pranab") is future work, deliberately excluded so CONNECT survives v1.
- The turn's purpose is minted by the channel (`handle_turn_as`), not chosen by the model — the
  model cannot re-declare a purpose mid-turn; the `memory_recall` tool inherits the turn context.

## Next (per the ratified build order)

2. **Outer Scoreboard** — BUILT (`mind-conversation/src/scoreboard.rs`): one board joining turn
   grades + domain ledger (now with pending counted) + the judgment-trend instrument + tool
   bandits + receptivity; every rate names an adversary-acceptable denominator, unmeasured axes
   (risk/channel/latency) are declared on the board, and the silence-gated engagement pct in the
   weekly report was replaced with resolved-denominator rows. `ym scoreboard`.
3. **Narrative-as-checksum** — BUILT (`mind-conversation/src/narrative.rs`): nightly first-person
   paragraph rendered by `format!` over the scoreboard + regret log (one regret, one watch-for,
   one policy in force, one forbidden self-claim), persisted with its measured basis, own
   per-date gate in the poll loop, recalled every turn via the telemetry block. `ym narrative`.
   (The weekly `self_report` prose pass remains a letter to the operator, not the self-record.)
4. **Reflex Arc** — corrections (not just regrets) cluster into `mind-spec::GoalSpec`-typed
   self-build goals behind the six-condition gate (repro fixture, named module, predicted metric,
   rollback, post-deploy measurement).
5. **Belief lifecycle** — wire yantrikdb's `RecordStatus`/`tombstone_reason` through the facade;
   `Belief.status` is currently hardcoded `"active"`.
