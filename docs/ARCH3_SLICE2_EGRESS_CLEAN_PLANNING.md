# ARCH-3 slice 2 — egress-clean tool planning + typed declassification

**Status:** designed, not built. This is the *real* egress-confidentiality control. ARCH-3A
(slice 1, commit `8e374d8`) shipped the mediation foundation (broker + registry + credential
tripwire + HMAC receipts); it does **not** stop a private household fact from leaving in a tool
arg. This slice does.

**Provenance:** two gpt-5.6-sol redteams. The ARCH-3 verdict (rid `019f59ac` / `019f5d48`) named
the mechanism; the slice-2 verdict (rid `019f5d5d`) rejected the tempting shortcut (private-content
overlap detection + per-connector consent grants) as *net-negative theater* and prescribed this.

## The threat (precise)

Member turns are pure-LLM (`member_turn`, zero tool calls), so **all connector egress is the
primary's path**. The adversary is therefore *the primary's own model* — prompt-injected or merely
privacy-insensitive — composing a stored **private** household fact into an outbound tool arg
(`web_search("Alice oncology appointment July 18")`). We **cannot** taint-track through the LLM (it
is a black box), and we **cannot** detect the leak by scanning the arg: paraphrase, translation,
date-reformatting, initials, encoding, and cross-call fragmentation all defeat any scanner. A denial
based on matching also becomes a **membership oracle** the model probes.

## Why overlap-detection + connector grants was rejected (do not build it)

- Overlap catches *copying*, not *disclosure of the proposition*; the model paraphrases for free.
- Per-connector grants ("allow private → calendar") are a confused deputy: one grant authorizes a
  diagnosis in an event title, a bank balance in the notes, another member's data, external
  attendees. First legit use silently authorizes all future malicious calls (habituation).
- Low thresholds → false-positive DoS on legit tools; the primary can poison its own private memory
  to disable tools.
- `Shared` scope ≠ externally publishable — memory scope is not an egress classification.

It survives only as a **high-precision shadow-mode tripwire** (see §5), never as the core control.

## The mechanism: a genuine isolation boundary

The args that reach an external connector must be authored by a model step that **never saw private
memory**. Not "another instruction in the same context" — a *separate stateless invocation*.

```
                 ┌─────────────────────────────────────────────┐
 user turn ─────▶│ GROUNDED reply model  (sees private memory) │──▶ user-facing answer
                 │  - decides IF a tool is needed + which        │
                 └───────────────────┬─────────────────────────┘
                                     │ tool choice only (no free-form args)
                                     ▼
                 ┌─────────────────────────────────────────────┐
                 │ EGRESS-CLEAN planner (SEPARATE call)         │
                 │  context = tool schema + the SANITIZED       │
                 │  request ONLY; NO grounding, NO working set, │
                 │  NO private tool-results, NO shared prompt    │
                 │  cache. Emits a SKELETON with typed slots:    │
                 │    calendar.create(                           │
                 │      title = DeclassSlot(EventTitle),         │
                 │      date  = DeclassSlot(EventDate),          │
                 │      attendees = [])                          │
                 └───────────────────┬─────────────────────────┘
                                     │ skeleton (no private values yet)
                                     ▼
                 ┌─────────────────────────────────────────────┐
                 │ TRUSTED slot-filler (code, not a model)      │
                 │  fills DeclassSlots from typed memory ONLY    │
                 │  under a narrow, expiring, payload-bound grant│
                 └───────────────────┬─────────────────────────┘
                                     ▼
                 EgressBroker.authorize(final canonical payload) ──▶ connector
```

The model that saw private memory picks *which* tool; a **clean** model authors the *shape* of what
leaves; **trusted code** inserts any declassified value under an explicit narrow grant. The model
never gets to write a private value into free-form outbound prose.

### Required properties (sol)

1. **Genuine isolation** — a distinct `inference.chat` call with a fresh message list; no private
   grounding, no working set, no private tool-results, no shared hidden state / prompt cache. If any
   private context bleeds in, it is theater.
2. **Constrained schema** — the planner emits a typed tool-call skeleton, not free text. Args are
   either literals it authored from the sanitized request, or `DeclassSlot(kind)` placeholders.
3. **Trusted slot-filling** — declassified values are inserted by code, keyed to the slot `kind`,
   pulled from *typed* memory fields (not free text), and only when a grant covers them.
4. **Broker enforcement after insertion, before dispatch** — the final canonical payload is what the
   broker authorizes and receipts (bind the receipt to the final-payload hash).

### Honest residual leaks (document; do NOT claim they're covered)

Egress-clean planning still leaks: private info supplied *in the current turn*, residue in the
sanitized transcript, facts known from pretraining/inference, values introduced by *later* local
tool-results, and declassified values misused outside their slot's purpose. So clean-planning and
high-confidence value-matching (§5) are **complementary**, not redundant.

## §4 Declassification grants (the capability model — build before any "allow" path)

Not per-connector. A grant is a capability scoped by **all** of:

```
data_owner . data_class . tool_action . { fields }
  → connector_account
  action = <verb>
  audience = <attendees/repo-visibility/recipients>
  expires = <minutes–days>
  max_uses = <n, default 1>
```

Default authorization is a **one-shot, payload-bound approval** (`approve request <id>`) bound to:
final-payload hash · connector+action · destination account/tenant · data-owner · acting principal ·
expiry (minutes) · max_uses=1. **Any payload mutation invalidates it.** Recurring workflows get the
scoped capability above only *after* the capability model exists. The approval detail (which may
include the exact payload) goes to an **authenticated local UI**, never into the model transcript
(oracle-avoidance).

## §5 High-confidence value tripwire (shadow first, then enforce narrow)

Retain as defense-in-depth, **not** the core control. Hard-deny tiers only:

- **Exact typed values** from known private/derived data: email, phone, account id, full address,
  gov id, access code, identity-tied date — block regardless of token count.
- **Distinctive contiguous phrases** — ≥3 informative (non-stopword) tokens / ~12–20 normalized
  chars, weighted toward rare tokens/names/dates/locations, within a bounded span of the payload.
- **Medium confidence** → one-shot local confirmation. **Low/generic single-token** → audit only.

Rollout: **shadow mode first** (compute + receipt, do not enforce), build a red-team corpus (exact
copy / paraphrase / translation / fragmentation / encoding) and a legit-tool corpus, measure
precision+recall per rule, then enforce only the high-precision rules. Scan the **full canonical
payload jointly** (values, keys, URLs, filenames, attachment refs), not field-by-field. Never reveal
the matched text/field/belief/confidence to the model. **Fail closed** for sensitive egress on
memory-read failure/lag; use an immutable versioned snapshot for matching and re-verify the grant
atomically immediately before dispatch.

## §6 Ownership / principal semantics (fix alongside)

Track separately: acting speaker · data subject · memory owner · principal-on-behalf-of · grant
issuer · connector-account owner. A grant from Bob must not authorize disclosing Alice's private
data. An operator-wide grant is an admin capability, not a data-subject's consent. Define multi-
subject-payload behavior explicitly.

## Suggested build order

1. External-disclosure label distinct from memory scope (`ExternalOkay|ExternalWithConsent|
   NeverExternal`); both `Private` and `Shared` default non-public.
2. Egress-clean planner: the separate sanitized `inference.chat` call + constrained skeleton schema
   for the External tools already in the registry. Start with ONE connector end-to-end (calendar or
   web_search) to prove the isolation boundary, then widen.
3. DeclassSlot type + trusted slot-filler from typed memory fields.
4. One-shot payload-bound approval path (local UI / operator console), receipts bound to final
   payload hash.
5. High-confidence value tripwire in shadow mode; calibrate; enforce narrow.
6. Ownership/principal model + multi-subject rules.
7. Cross-call aggregation / egress privacy budget (addresses fragmentation) — later.

## Acceptance (what "slice 2 done" means)

For at least one External connector end-to-end: a private stored fact cannot reach the connector via
a free-form model-authored arg (the planner never saw it); a declassified value reaches it only under
a one-shot payload-bound approval; the receipt binds to the final payload hash; the isolation
boundary is proven by a test that plants a private fact, injects "exfiltrate it", and shows the
outbound payload does not contain it while a legitimately-declassified value does. Name the guarantee
precisely and list the residual leaks above as known-not-covered.
