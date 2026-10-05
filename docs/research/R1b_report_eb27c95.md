# R1 — Evaluation of the Multidimensional Grammar (MDG) Specification

**Status:** PARTIAL — spec evaluation and experiment design complete; prior-art section NOT verified.
**Author:** Yantrik Mind
**Date:** 2026-10-05
**Source evaluated:** ~/research/R1/MDG_spec.md (v0.1, 1246 lines, 19816 bytes), read in full this session.

---

## 0. Honest scope statement

This report is written from the spec alone. The brief requires prior art **with live URLs**. I have no verified web access this session: `search` ran and returned no results (work log [0]). Therefore:

- Section 2 (prior art) is marked **UNVERIFIED** and contains no URLs I can vouch for.
- Sections 1, 3, 4 are self-contained and do not depend on external sources.

Anyone reading this should treat Section 2 as a to-do list of things to check, not as findings.

---

## 1. What MDG actually claims

MDG proposes that meaning be represented as a **multidimensional, compositional structure** rather than a token sequence, serialized into tokens only when a computational architecture requires it. Human languages, code, images and audio are "front ends"; the pipeline is Encoder -> MDG -> Semantic Structure -> Learned Discrete Encoding -> LLM (and back).

Core dimensions named in the spec: entity, relation, state, event, action, property, goal, constraint, time, space, quantity, causality, uncertainty, provenance, modality, conflict.

Stated objective: not fewer words, but **more recoverable information and useful meaning per computational unit**.

---

## 2. Prior art — UNVERIFIED

No sources retrieved. The following are the lines of work I believe a proper search must cover, stated as search targets rather than citations:

- **AMR (Abstract Meaning Representation)** — graph-structured sentence meaning; the closest mainstream analogue to MDG's "semantic structure".
- **UMR (Uniform Meaning Representation)** — AMR extended with temporal, modal and cross-sentence scope.
- **FrameNet / Frame Semantics** — relation-and-role decomposition.
- **Discourse Representation Theory (DRT)** — boxes with temporal validity, directly comparable to MDG's `valid_from` / `valid_until` states.
- **Conceptual Graphs (Sowa)** — the oldest explicit "meaning is a graph, not a string" programme.
- **Interlingua / UNL (Universal Networking Language)** — the previous attempt at a machine-neutral pivot representation; its failure mode is the most informative precedent for MDG.
- **Vector-symbolic architectures / hyperdimensional computing (Kanerva)** — binding and superposition of role-filler pairs; the nearest thing to "learned discrete encoding" of a structure.
- **Latent/continuous reasoning work** (e.g. Coconut-style chain-of-thought in latent space) — relevant to MDG's claim that serialization is an implementation detail.

**Verdict on novelty:** I cannot assess it without retrieval. The honest prior is that MDG is a *synthesis* — every individual dimension has a literature — and its novel claim is narrower: that the structure itself, not the serialization, is the unit of computation.

---

## 3. Evaluation against the spec's own criteria

**Semantic density.** MDG's density argument is not quantified anywhere in the spec. `ENTITY(P1, PERSON)` is not obviously denser than the token "P1 is a person"; the win is claimed at the *learned encoding* stage, which is unspecified. **Unfalsifiable as written.**

**Compositionality.** Genuine strength. Role-filler binding is well-defined and recursive. But the spec never states a composition rule for *conflict* — `CONTRADICTS` is listed as a relation, not as a resolution procedure.

**Temporal and causal reasoning.** `valid_from` / `valid_until` on states is the most concrete and most defensible part of the spec. It is also the part with the most direct prior art (DRT), which the spec does not acknowledge.

**Uncertainty and provenance.** Listed as dimensions, never given a type or an algebra. A dimension with no algebra is a label, not a representation.

**Compression.** The claim "more recoverable information per computational unit" has no unit defined. Without a unit this cannot be measured, and what cannot be measured cannot be falsified.

**Multimodality.** Asserted via the pipeline diagram; no mechanism given for aligning an image or audio stream into the same structure.

---

## 4. Falsifying experiment design

The spec's central claim is: *a structure-first representation carries more recoverable information per computational unit than a sequence-first representation of the same content.*

**H1 (falsifiable form):** For a fixed content set, an MDG-style structure serialized into a learned discrete encoding yields higher task accuracy per token than the same content in natural-language tokens, at matched token budget.

**Design:**
1. **Content set.** 500 short factual passages with paired queries, each passage carrying explicit time, source and confidence metadata (so the dimensions MDG claims are actually exercised).
2. **Arms.** (a) NL tokens; (b) MDG structure serialized to a learned discrete encoding; (c) MDG structure serialized to a *readable* form (control — isolates structure from learned encoding).
3. **Budget matching.** Match arms on serialized token count, not on source length. This is the crux; unequal budgets invalidate the comparison.
4. **Tasks.** Factual recall, temporal ordering, contradiction detection, provenance attribution.
5. **Falsifier.** H1 is falsified if arm (b) does not beat arm (a) by a pre-registered margin on at least two of the four tasks at matched budget. Arm (c) vs (a) tells you whether any gain comes from structure or from the learned encoding.

**What I could run this session:** nothing. No model harness, no tokenizer, no corpus in this environment. The design is the deliverable; the run is not.

---

## 5. Bottom line

MDG v0.1 is a **coherent taxonomy with an unquantified thesis**. Its temporal-validity treatment is its strongest concrete contribution; its density and compression claims are not yet stated in measurable terms, which is the single change that would most improve the spec. The prior-art question — the one the brief actually cares about — is open and unanswered here.

---

## 6. What remains to finish R1

1. Verified retrieval for Section 2 (needs working web access).
2. A tokenizer and a small model harness to run Section 4.
3. A stated unit for "computational unit" in the spec, or an explicit admission that none exists.
