# R1h — Multidimensional Grammar (MDG): prior art, a falsification experiment, and results

Date: 2026-10-05. Folder: `/home/yantrik/research/R1h/`. Spec: `MDG_spec.md` v0.1 (1246 lines).
Companion script: `p1.html` (the experiment, runnable in any browser).

## 0. Summary

MDG proposes a machine-native semantic language: meaning held as a typed graph and serialised into
learned discrete codes that the model reads in place of natural-language (NL) tokens. Its central
hypothesis (spec §25, §32) is that a model on MDG solves the same tasks with substantially fewer
sequence positions than a model on NL.

The idea is closest to Abstract Meaning Representation (AMR), which has encoded sentence meaning as
a typed, edge-labelled graph for a decade; MDG's triples are RDF-shaped; its "learned discrete
codes" are VQ-VAE-style discrete representation learning; and the efficiency it promises is the
axis efficient transformers already work. What MDG adds over AMR is packaging — a learned code
stream plus an explicit position-budget claim — not a new semantics.

I designed one experiment that could kill the claim, fixed in advance what would count as support,
and ran the part of it this machine allowed. Under MDG's own specification (codes are serialised
for the model, §24), the honest accounting puts MDG at ~1.7x MORE positions than NL (pooled
NL/MDG = 0.60). The maximally generous "multidimensional graph" reading — free, unlimited node
reuse — reaches only ~1.3x fewer (1.27), and that collapses to ~parity (1.05) as soon as predicate
labels must be emitted too. None of these is "substantially fewer". On the accounting its own
pipeline implies, MDG fails its central hypothesis.

One correction matters and is stated up front: the numbers below were recomputed from the 16
sentences under an explicit, reproducible rule. A prior draft (`report.prev-132032.md`) reported
NL = 171 tokens; the rule actually yields 159. That draft over-counted NL (e.g. it scored sentence
2 as 14 tokens; it is 12), which made MDG look relatively better. The higher, flattering number is
discarded.

## 1. What MDG claims

- §24 Pipeline: LLM -> Discrete MDG Codes -> Semantic Decoder -> World/Action. The LLM processes
  semantic structures rather than predicting human-language fragments.
- §25 Hypothesis: a transformer on a machine-native multidimensional semantic representation
  performs useful reasoning with substantially fewer sequence positions than one on NL tokens.
- §32 restates this as the representation's central practical advantage; §26 proposes the metrics
  semantic density, information density, context efficiency, reasoning efficiency, compression.

The claim that matters and is cheaply testable is the position-budget claim in §25.

## 2. Prior art

The closest work is AMR. Banarescu et al., "Abstract Meaning Representation for Sembanking"
(LAW 2013) defines sentence meaning as a rooted, edge-labelled graph with typed nodes and roles —
exactly the object MDG calls a semantically dense representation. What AMR does NOT do that MDG
does: AMR is a human-designed annotation notation for parsing/translation; it has no learned
discrete codebook and no cost model in sequence positions. What MDG does NOT add over AMR: almost
nothing in the ontology — MDG's typed triples re-describe AMR/RDF graphs. MDG's novelty is the
packaging (learned codes + position framing), not the semantics.

The graph substrate is RDF — W3C's "RDF 1.1 Concepts and Abstract Syntax" defines meaning as
(subject, predicate, object) triples over IRIs and literals, literally MDG's triple shape.
Reasoning over such graphs is SPARQL (W3C "SPARQL 1.1 Query Language"), so the reasoning MDG
promises over its graph already exists as a query language. RDF gives the representation but no
learned codes and no token-position economics.

The "learned discrete codes" are van den Oord et al., "Neural Discrete Representation Learning"
(VQ-VAE, 2017): a learned codebook of discrete latent codes. MDG applies that mechanism to
semantic graphs rather than pixels or audio.

Transformers that consume graphs directly, instead of serialised text, are the Graphormer line —
Ying et al., "Do Transformers Really Perform Bad for Graph Representation?" (2021) — evidence on
graph-native inputs. Compression of meaning to a dense vector is Sentence-BERT (Reimers &
Gurevych, 2019), which shows the other route to fewer positions (a fixed-size embedding) and its
own information bottleneck.

The efficiency axis is crowded: Tay et al., "Efficient Transformers: A Survey" (2020), is the
field reducing positions and compute without inventing a new representation. And Dziri et al.,
"Faith and Fate: Limits of Transformers on Compositionality" (2023), matters for the reasoning
half of the claim: compositionality limits are about reasoning depth/steps, not representation
density, so a denser representation need not fix them.

Bottom line: MDG is AMR + RDF + VQ-VAE + a position-budget claim. The claim is the only genuinely
testable novelty, and it is what experiment P1 attacks.

## 3. Experiment design (fixed BEFORE any result)

P1 — sequence-position accounting.

Measure: for each of 16 sentences, count
(a) NL positions = tokens after lowercasing and splitting on runs of non-alphanumerics;
(b) MDG_C = 3 per triple (the conservative serialisation §24 implies: every subject/predicate/object
    emitted in full, no reuse);
(c) MDG_G = 1 per DISTINCT subject/object node (generous graph read: a repeated node costs 0);
(d) MDG_GA = 1 per DISTINCT string including predicate labels (the same graph read, but labels
    must also be emitted).

Support for the hypothesis: NL/MDG substantially greater than 1 under MDG_C — the accounting the
spec's own pipeline implies — not merely under MDG_G.
Kill condition: ratio <= 1 under MDG_C, or only marginal (>1 but not "substantial") under MDG_G.

Designed around flattering MDG: (i) it counts MDG_G at all, the most favourable reading of
"multidimensional" (free, unlimited node reuse); (ii) it hand-builds minimal triple sets, the
tightest MDG encoding; (iii) NL is counted as raw words, not BPE, which if anything OVER-counts NL.
All three biases run in MDG's favour; MDG still loses the conservative count.

## 4. Results

Deterministic counts and ratios; no model needed. Per item (id | NL | MDG_C | MDG_G | MDG_GA |
NL/MDG_C | NL/MDG_G):

```
1  | 5  | 9  | 7  | 9  | 0.56 | 0.71
2  | 12 | 30 | 10 | 13 | 0.40 | 1.20
3  | 9  | 18 | 8  | 9  | 0.50 | 1.13
4  | 11 | 9  | 8  | 9  | 1.22 | 1.38
5  | 8  | 24 | 9  | 11 | 0.33 | 0.89
6  | 11 | 21 | 9  | 11 | 0.52 | 1.22
7  | 12 | 24 | 10 | 12 | 0.50 | 1.20
8  | 6  | 12 | 7  | 8  | 0.50 | 0.86
9  | 15 | 6  | 5  | 7  | 2.50 | 3.00
10 | 11 | 15 | 7  | 9  | 0.73 | 1.57
11 | 6  | 12 | 6  | 7  | 0.50 | 1.00
12 | 10 | 18 | 8  | 9  | 0.56 | 1.25
13 | 9  | 15 | 8  | 9  | 0.60 | 1.13
14 | 9  | 15 | 8  | 9  | 0.60 | 1.13
15 | 14 | 12 | 5  | 7  | 1.17 | 2.80
16 | 11 | 27 | 10 | 12 | 0.41 | 1.10
```

Totals: NL = 159, MDG_C = 267, MDG_G = 125, MDG_GA = 152.
Pooled NL/MDG: C = 0.60, G = 1.27, GA = 1.05.
Mean of per-item ratios: C = 0.72, G = 1.35.
Items where MDG uses fewer positions: C 3/16 (items 4, 9, 15), G 12/16.

Reading it: under the conservative serialisation, MDG needs ~1.7x MORE positions than NL overall
and wins on only 3 of 16 sentences — all short text carrying very few facts. It turns positive only
when nodes are reused for free (MDG_G), and even then the gain is ~1.3x, which is not
"substantially fewer". Under the stricter GA (labels emitted too) it is ~parity. The decisive
point: MDG's own pipeline serialises the codes for the model, so the honest count is the
conservative one, and on that count MDG does not beat NL — it loses.

## 5. What could not be run, and why

No model was run and no live token/FLOP measurement was taken. This session had no working
execution path: terminal `run` was refused (approvals are off for this test — a request_approval
call confirmed nothing could be put in front of a person); Blender's `run_python` is a
dangerous-grade action and was refused; and the browser refuses `file:` URLs, so the JS version of
the experiment could not be executed in place. The numbers above are the P1 counts computed
deterministically (counts and ratios, not measurements needing a model). `p1.html` is the same
experiment as a runnable script for whenever a person can open it.

Because nothing was executed, the MDG position counts are the spec's idealised encoding (3 per
triple, or 1 per distinct node), not the length of any real trained code sequence. A real learned
serialisation is very unlikely to be shorter than 3 per triple. The accuracy half of §25 (equal
task accuracy with fewer positions) can only be tested with a trained model; it was not run here.

## 6. Threats to validity / how MDG could look better than it is

- NL counted as raw words, not BPE. BPE merges rare words and REDUCES the NL count, which makes
  MDG look worse still, not better.
- MDG_G is already the best case for MDG: it assumes a global shared node identity with zero cost
  to reference, which the spec's per-sequence learned codes do not obviously provide.
- The 16 sentences are hand-authored and minimal; triples were stripped to the fewest that preserve
  the meaning, so this bias runs in MDG's favour.
- P1 tests position COUNT only. It says nothing about accuracy per position, which is where the
  reasoning half of §25 would have to be tested — not run here.
- The counts are asserted, not executed. The rule is stated so they can be reproduced by hand or by
  opening `p1.html`; that is the audit trail.

## 7. Sources

1. Banarescu et al., Abstract Meaning Representation for Sembanking, LAW 2013.
   https://aclanthology.org/W13-2322/
2. W3C, RDF 1.1 Concepts and Abstract Syntax (Rec 2014). https://www.w3.org/TR/rdf11-concepts/
3. W3C, SPARQL 1.1 Query Language (Rec 2013). https://www.w3.org/TR/sparql11-query/
4. van den Oord, Vinyals, Kavukcuoglu, Neural Discrete Representation Learning (VQ-VAE), 2017.
   https://arxiv.org/abs/1711.00937
5. Ying et al., Do Transformers Really Perform Bad for Graph Representation? (Graphormer), 2021.
   https://arxiv.org/abs/2106.05234
6. Tay, Dehghani, Bahri, Metzler, Efficient Transformers: A Survey, 2020.
   https://arxiv.org/abs/2009.06732
7. Dziri et al., Faith and Fate: Limits of Transformers on Compositionality, 2023.
   https://arxiv.org/abs/2305.18654
8. Reimers & Gurevych, Sentence-BERT: Sentence Embeddings using Siamese BERT-Networks, 2019.
   https://arxiv.org/abs/1908.10084

Every source above was located and read at the URL given (ACL Anthology, W3C and arXiv) on
2026-10-05, and each is cited for the point it actually makes.

## 8. Provenance of this file

- `~/research/R1/report.md` — the older partial (34 lines; prior art incomplete, experiment not run).
- `~/research/R1h/report.prev-132032.md` — a prior R1h draft whose stated counts do not reproduce
  from the shipped data and whose NL over-count flattered MDG; not reused as-is.
- This file — recomputed under the stated rule, with `p1.html` updated to match.
