# ARCH9 — One person, one desktop: the thin path

14 October 2026. Written after E.CMP1 run 1 and Pranab's question "should we drop the family angle now and focus on the 1:1 mapping, and expand later?".

## The decision asked for

**Yes. The family/household features are frozen, not deleted.** Development focuses on one person on one desktop. The family's production box stays on its maintained build: production moves only on Pranab's word, and the self-improve kill switch stays on. Ownership, scope and isolation in storage are kept as they are; only multi-person *behaviour* stops being developed. It comes back later, built on a desktop assistant that is reliable first.

## Why

The harness flow and the core loop are sound. Attach, turns, streaming, cancel, the model gateway and the private memory socket all held through two days of VM 541 testing, the #783 proof, and the comparison.

What breaks is the deterministic machinery around the loop. About 20 defects in two days were nearly all layers misfiring around a model that was right:
- the keyword photo shortcut took an attached-picture turn;
- the console rewrite turned the desktop's "/new" into ":new";
- the description condenser dropped disk and memory;
- the citation pass rewrote answers in a robotic voice at 24–75 s a turn;
- the card detector read a booking code as a card number;
- streamed text and a rewritten answer were both shown;
- one fact was stored five times;
- one note was created six times.

The fix for each was another rule, and the system kept growing. E.CMP1 run 1, on the same local 27B:
- Mind: 3 pass / 2 partial / 1 fail;
- Hermes: 4 pass / 1 fail of 5;
- DeepSeek: 4 pass / 1 partial of 5.

So the Mind is close, not far behind. Its misses had Mind-code causes, and it was the only one that never showed internals.

## The outside view

gpt-6-astra was consulted through the API (appendix, verbatim). Its central recommendation:

> One model-led loop, a small set of reliable tools, and narrow security boundaries. Remove competing layers that interpret the request, invent workflow, or rewrite the answer.

It agrees on the family question: "defer multi-person product behavior … freeze features, not safeguards."

## Where I agree, and where I don't

**Agree, and will do:**
- **Narrow promise.** Understand what I'm looking at (attachments, the current app); find my information (files, notes, calendar); make controlled changes; remember what I ask; get current information.
- **No component answers before the model.** The keyword shortcuts leave request routing; where one is worth keeping it becomes a tool the model can choose.
- **One owner of the final answer.** No pass rewrites it. E.CITE1 already turned the citation pass into a trim; the next step is to measure whether even the trim earns its place (ablation).
- **Commands and attachments as typed protocol objects, not text.** E.SLASH2 and E.OPT1b were patches over text handling; the protocol shape is the real fix, and it needs the OS side.
- **Action semantics over duplicate heuristics.** An action ledger with operation ids, postcondition checks, and no blind retry of a mutation whose outcome is unknown. E.LOOP9's title rule is a patch that this replaces.
- **Memory the person can inspect.** "Remember X" is one idempotent record, corrections are explicit, list/edit/delete exist, and inferred beliefs are kept apart from what was said. E.MEM2 moved this way.
- **Privacy as data-kind × destination.** One shared classifier, policy per operation, a check on the actual payload sent, and a refusal that names what it protects (E.VOICE6 is the wording half).
- **A net reduction rule for this cycle.** Every change must remove at least as many request-interpreting components as it adds. Every new heuristic says why a type, a tool contract or an existing owner could not do it. Ablation comes before any added "intelligence".
- **A real evaluation.** About 60 desktop tasks with variants and a hidden holdout, graded on application state (not just the reply): verified completion, wrong or duplicate mutations, false refusals, privacy violations, p50/p95 time, calls per completion, unsupported "done" claims. E.CMP1 becomes its smoke test.

**Disagree, or qualify:**
- **"Remove Bayesian belief revision from the critical path."** The store is the Mind's one clear differentiator; neither Hermes nor DeepSeek has typed memory with provenance. I would keep the store and make the desktop *interface* to it simple and idempotent, which is what it recommends for writes. The revision machinery stays behind that interface, off the turn's critical path.
- **The release gates (≥90% verified on a 60-task suite, parity with the strongest baseline in every category):** right in shape. The numbers are set when the suite exists, before any run, by preregistration as always. Not now, against a suite nobody has written.
- **Typed adapters for notes, calendar, files and system status:** right, but they live in the OS bridge (yos-mcp), so they are yantrik-os's work with mine. The Mind side is to prefer them where they exist and keep `os_act` as the fallback.

## The plan

**Stage 1: the thin path, behind a switch, measured against today.**
- A desktop profile with the keyword shortcuts off, no pre-loop planners on the default path, no rewrite pass, and the foresight/proactive/research sub-agent machinery off.
- It keeps the loop, the tools, consent routing, the egress check on the payload actually sent, the harm gate, and the budgets.
- It runs on E.CMP1's asks plus the first slice of the larger suite, against the current Mind, on the same model.
- **Exit:** a measured account of which removed layers helped, hurt, or made no difference. Then the losers are deleted, not just switched off.

**Stage 2: contracts.**
- Typed commands and attachments (with yantrik-os).
- Structured observations instead of a lossy condenser.
- The action ledger with postconditions.
- Single-owner rendering (Progress / ToolStarted / ToolCompleted / AnswerDelta / AnswerCommitted), with the OS's replace event for a committed answer.
- Explicit memory operations.

**Stage 3: earning the default.**
- The suite and a holdout, run repeatedly against Hermes and DeepSeek.
- Opt-in desktop canaries.
- Hermes stays the default until the gates hold across repeated runs.

**Frozen for this cycle:** family/household behaviour, paper trading, proactive loops, recipe-specific behaviour, the research sub-agent, skill self-authoring. The coder lane stays as an explicitly invoked capability; its benchmark is no longer evidence of desktop readiness.

---

## Appendix — the consultation, verbatim

Model: openai:gpt-6-astra, through the brainstorm MCP (quick mode), 14 Oct 2026, ~$0.15. The brief sent is in this session's transcript. It held the architecture summary, the E.CMP1 run-1 results and the defect list, and no keys, addresses or personal details.

````
## My recommendation

**Yes: focus Yantrik Mind on one person, one desktop. Freeze family-product development now.** Keep existing family users on a maintained release; do not delete identity, ownership, or isolation protections. Build household interaction later, on top of a reliable personal assistant—not inside every decision the desktop assistant makes.

The architectural change is equally direct:

> **One model-led loop, a small set of reliable tools, and narrow security boundaries. Remove competing layers that interpret the request, invent workflow, or rewrite the answer.**

Your model is not always right, and six desktop tasks cannot establish that architecture explains the entire performance gap. But the defect evidence strongly implicates the surrounding machinery. Several components can independently reinterpret the same request. Their interactions create more behavior than their individual tests capture.

The next release should be judged partly by **how much runtime decision-making you remove**, not how many defects you patch.

## 1. Define a much narrower product

For the next release, Yantrik Mind should do five things exceptionally well:

1. **Understand what I am looking at:** attachments, selected text, current app, visible screen.
2. **Find my information:** files, notes, calendar, and optionally mail.
3. **Make controlled changes:** edit an existing note, create an event, manipulate an app.
4. **Remember things I explicitly ask it to remember.**
5. **Get current information:** weather, web lookup, straightforward research with sources.

Suspend work on paper trading, proactive loops, recipe-specific behavior, autonomous research agents, and skill self-authoring. Keep the coder lane as a separately invoked capability if it already works; stop using coding benchmark improvements as evidence of desktop readiness.

**The user-facing promise is “a dependable assistant for this desktop,” not “a general autonomous intelligence platform.”**

## 2. Make these cuts now

| Component | Decision | Replacement |
|---|---|---|
| Keyword shortcuts for photos, mail, news, drafts, skills | **Remove from request routing** | Model sees the request and attachments; shortcuts become tools or explicit commands |
| General preflight planning calls | **Remove from the default path** | The main model selects the next tool |
| Citation pass that regenerates the answer | **Remove** | Preserve source handles through tools; validate references without rewriting prose |
| Separate compose call after a final answer already exists | **Remove** | One final-answer owner |
| Heuristic console-command rewriting | **Remove** | Typed protocol messages for commands versus user text |
| Generic lossy description condenser | **Replace** | Tool-specific structured results, explicit truncation, retrievable detail |
| Bayesian belief expansion/revision | **Remove from the desktop critical path** | Simple, inspectable memory with explicit provenance and user correction |
| Proactive and research sub-agent machinery | **Disable by default** | Explicit invocation later, with separate budgets |
| Desktop household arbitration | **Remove from the active product** | One authenticated owner; preserve isolation in storage |
| Ad hoc duplicate-action patches | **Replace** | Central action ledger, operation IDs, and postcondition verification |

### Keep these foundations

- Stable harness protocol, streaming transport, cancellation, model gateway.
- Core agent loop.
- MCP bridge, after tightening tool contracts.
- Privacy consent and provider routing.
- Secret protection at actual trust boundaries.
- Step, token, time, and repeated-failure budgets.
- Tests and preregistration—but change what they evaluate.

**Do not simplify by removing safety boundaries. Simplify by making fewer components responsible for enforcing them.**

A harmless private local note write and sending that note to an external provider are different operations. They should not run through one undifferentiated “suspicious content” gate.

## 3. Rebuild around one clear execution path

The normal path should be:

    Typed user turn + attachment handles
                     ↓
    Context selection + consent-aware model routing
                     ↓
    Model → proposed tool call
                     ↓
    Tool schema validation + permission/egress checks
                     ↓
    Execution + action ledger + structured observation
                     ↓
    Model → next tool call OR final answer
                     ↓
    Deterministic rendering

No shortcut gets to answer before the model. No independent component silently changes the task. No generic quality pass replaces the final answer.

### A. Make tools carry the reliability burden

Desktop task success depends heavily on tool semantics. A generic `os_act` bridge is useful, but should not be the only interface for important operations.

Prioritize typed adapters for: file and attachment reading; system status; note search, read, create, and update; calendar queries and mutations; current weather and forecast retrieval.

For example: `notes.update(note_id, expected_revision, append_text, operation_id)` — return a structured result with the resulting revision and verification status. This is fundamentally safer than asking the model to “open Notes, find the right note, type something, and hope.”

Keep generic UI automation as the fallback. When it is used, observe before acting and verify after consequential changes.

**Do not build a giant adapter framework first. Build the few adapters that cover the actual benchmark and production workload.**

### B. Replace duplicate suppression with action semantics

Comparing fields is insufficient. Comparing titles alone is also insufficient: two legitimate notes can share a title.

Use: stable operation IDs for retries; provider idempotency support where available; a local ledger recording attempted, succeeded, failed, and unknown outcomes; postcondition checks for ambiguous UI actions; a prohibition on blindly retrying a mutation whose outcome is unknown.

If a note may already have been created, inspect the destination before creating it again.

Loop guards should stop pathological behavior; they should not compensate for tools that cannot report what happened.

### C. Treat attachments and commands as protocol objects

An attached picture must arrive as an attachment handle with a content type—not as a textual hint that competes with a keyword router.

Similarly, `/new` should become a `NewConversation` event at the UI boundary. It must never pass through a text-normalization function on its way to becoming a command.

**Make invalid interpretations unrepresentable where Rust types can help.**

### D. Give one component ownership of the answer

Use distinct events: `Progress`, `ToolStarted`, `ToolCompleted`, `AnswerDelta`, `AnswerCommitted`.

Raw tool arguments belong in an expandable activity/debug view, not in the conversational answer. Useful progress—“Checking tomorrow’s calendar”—is not the same as leaking tool syntax.

Once final-answer streaming starts, do not run a second authoring pass that replaces it. If pre-release inspection is required, buffer that answer before displaying it. Do not show two competing versions.

Citation validation can detect nonexistent source IDs. It cannot cheaply prove that every sentence is supported. Restrict stronger claim checking to explicit high-stakes or research modes rather than pretending a universal rewrite solves grounding.

## 4. Keep privacy strict, but stop making safety a guessing game

The booking-code false positive is evidence that the detector’s role needs redesign, not just another exception.

Separate three questions: 1. What kind of data is this? 2. Where is it going? 3. Is that destination allowed to receive it?

Use a shared classifier, but operation-specific policy. A card-number candidate should generally require plausible digit structure and stronger evidence than a nearby word such as “cards.” Credential detection needs different signals. Ambiguous classification should not automatically make ordinary local storage impossible.

For outbound calls: enforce consent on model-provider requests as well as tool calls; prefer allowlisted argument construction and local computation over arbitrary prose rewriting; validate the actual payload sent, after transformations; if no safe request preserves the task, stop and explain the limitation.

Do not claim that semantic rewriting guarantees “private facts never leave.” Treat that as a security objective requiring adversarial tests and explicit data-flow controls.

Present blocks as useful explanations: “That request would send your private booking details to an unapproved service. I can search without those details instead.” Keep raw policy diagnostics in the audit log.

## 5. Reduce memory to something the user can inspect

For the desktop release: “Remember X” creates one logical memory record. Repeating it is idempotent. “Actually, X is Y” creates an explicit correction. “Forget X” removes it from retrieval and follows a documented deletion policy. Every record has an owner, source, timestamp, and visibility. The user can list, edit, and delete memories.

Keep inferred beliefs out of durable personal memory by default. If you retain them experimentally, distinguish them visibly from things the user actually said.

Bayesian machinery is not a desktop advantage if one birthday becomes five beliefs. **Reliable remembering beats sophisticated belief accounting.**

## 6. Measure the desktop product, not the repair process

Your current benchmark is a smoke test, not a release criterion. Its unequal scored denominators and small sample make ranking fragile. Nobody producing a weekend forecast suggests a shared capability or setup gap worth investigating—not necessarily a model failure.

Build a replayable desktop evaluation with roughly: 60 canonical tasks spanning the five product promises; at least three variants per task (paraphrases, distractors, attachment types, existing application state); a hidden holdout maintained separately from implementation work; multi-turn sessions that exercise follow-ups and context changes; separate privacy, isolation, cancellation, and fault-injection suites.

Include the known defects, but do not let them dominate. Otherwise you will build a perfect assistant for last week’s bugs.

Score observable outcomes. For action tasks, inspect application state—not just the answer. Track: verified completion rate; incorrect or duplicate mutations; false refusals; privacy violations; time to verified completion, p50/p95; model calls and tokens per completion; unsupported success claims; user repair burden.

Run competitors on the same tasks, models where supported, VM snapshots, tool access, and scoring rules. Report missing capabilities explicitly; do not silently remove inconvenient tasks from a denominator.

Release gates (set before implementation): at least 90% verified completion on the core desktop suite; at least parity with the strongest baseline on the full suite, with no major category hidden by the aggregate; no critical unauthorized disclosure or destructive-action failures in the release suite; no duplicate mutations in retry/fault-injection tests; a substantial p95 latency improvement over the current Mind on matched hardware; no default-path model calls devoted solely to generic planning, citation rewriting, or restyling.

For simple read-only tasks, target one tool round trip and one final response. Measure model time separately from tool time. An 85-second median remains poor desktop UX even if competitors are similarly slow.

Test thinking-on versus thinking-off end to end: disabling reasoning can save generation time but cost additional tool steps.

## 7. Stop the rule-accumulation cycle

Your engineering discipline is strong at validating individual changes. The missing discipline is controlling architectural interactions and total complexity.

Every defect gets a root-cause category: protocol ambiguity; tool-contract failure; model decision failure; policy error; rendering error; state/retry error. A model prompt patch is not an acceptable default response to a tool-contract failure.

Every new heuristic must justify its existence: Why can this not be fixed with a type, tool contract, or existing owner? Which component owns this decision? What other interpretation of the request does this introduce? What held-out cases could it break? What component or rule can now be deleted?

Budget complexity, not just tokens. Track active routers, implicit rewrites, model invocations, and independently maintained policy paths. For this stabilization cycle, require a net reduction in request-interpreting components.

Require ablation before adding intelligence. Run the same workload with and without the proposed layer. Ship it only if it measurably improves outcomes after accounting for latency, false refusals, and regressions.

Mutation testing proves tests detect code changes. It does not prove the architecture is useful. Keep it, but add black-box holdout testing and long-session fault injection.

## 8. Execute in three stages

Stage 1: Establish the thin baseline. Freeze feature work. Build the stripped path behind a release flag using the existing harness, core loop, privacy boundaries, and tools. Run it against the current stack. This tests your central hypothesis before a large rewrite. Exit: a quantified account of which removed layers helped, hurt, or made no difference.

Stage 2: Repair the contracts. Implement typed commands and attachments, structured observations, action-ledger semantics, single-owner rendering, and explicit memory operations. Add the highest-value native adapters. Exit: the known defect classes stay fixed under variants, retries, cancellations, and long sessions—not merely their original reproductions.

Stage 3: Earn the default slot. Use opt-in desktop canaries, replay failures in isolated environments, and compare against Hermes. Never shadow-execute live writes. Promote Mind only when the release gates hold across repeated runs. Keep Hermes available as an explicit fallback during rollout.

## The family decision, without hedging

Yes—defer multi-person product behavior. Household identity ambiguity, shared-memory permissions, group-chat context, and proactive delivery multiply the state space before the single-user path is dependable.

But freeze features, not safeguards: preserve ownership and scope metadata; maintain the family deployment separately; do not merge household memories into a single-user store; migrate data only with explicit ownership and consent.

Your family deployment is evidence of real usefulness. It is not a reason to make every desktop turn carry household complexity.

The winning next version should have fewer ways to decide what the user meant, fewer ways to alter what the model said, and much stronger guarantees about what each tool actually did.
````
