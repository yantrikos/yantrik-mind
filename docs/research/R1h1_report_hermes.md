# R1h — Multidimensional Grammar (MDG): prior art, a falsification experiment, and results

Date: 2026-10-05. Folder: /home/yantrik/research/R1h/ . Spec: MDG_spec.md v0.1 (1246 lines).
Companion script: p1.html (the experiment, runnable in any browser).

## 0. Summary

MDG proposes a machine-native semantic language: meaning held as a typed graph and serialised
into learned discrete codes that the model reads instead of natural-language tokens. Its central
hypothesis (spec §25, §32) is that a model on MDG solves the same tasks with substantially fewer
sequence positions than a model on natural language.

The idea is not new. It is closest to Abstract Meaning Representation (AMR), which has encoded
sentence meaning as a typed, edge-labelled graph for a decade; MDG's triples are RDF-shaped, its
"learned discrete codes" are VQ-VAE-style discrete representation learning, and the efficiency
it promises is the same axis efficient transformers already work. What MDG adds over AMR is the
specific packaging — a learned code stream plus an explicit position-budget claim — not a new
semantics.

I designed one experiment that could kill the claim, declared in advance what would count as
support, and ran the part of it this machine allowed. Under MDG's own specification (codes are
SERIALISED for the model), the conservative accounting puts MDG at a pooled 0.64 positions per
natural-language token — i.e. it uses about 1.6x MORE positions, not fewer. The favourable
accounting (free node reuse) gives a pooled 1.37x, and even that is nowhere near
"substantially fewer". MDG therefore fails its central hypothesis under the accounting its own
pipeline implies.

## 1. What MDG claims

- §24 Pipeline: LLM -> Discrete MDG Codes -> Semantic Decoder -> World / Action. The LLM is a
  processor of semantic structures rather than a predictor of human-language fragments.
- §25 Hypothesis: a transformer operating on a machine-native multidimensional semantic
  representation performs useful reasoning with substantially fewer sequence positions than one
  operating on natural-language tokens.
- §26 Metrics proposed include semantic density (useful semantic info / token), information
  density (recoverable info / bit), context efficiency (task-relevant state / context token),
  reasoning efficiency (accuracy / FLOP), and compression ratio.

The claim that matters and that can be tested cheaply is the position-budget claim in §25.

## 2. Prior art

The closest work is AMR. Banarescu et al., "Abstract Meaning Representation for Sembanking"
(LAW 2013) defines sentence meaning as a rooted, edge-labelled graph with typed nodes and
roles — exactly the object MDG calls a semantically dense representation. What AMR does NOT do
that MDG does: AMR is a human-designed annotation notation for parsing and translation; it has
no learned discrete codebook and no cost model in sequence positions. What MDG does NOT add
over AMR: almost nothing in the ontology — MDG's typed triples are a re-description of AMR/RDF
graphs. So MDG's novelty is the packaging (learned codes + position framing), not the semantics.

The graph substrate is RDF — W3C's "RDF 1.1 Concepts and Abstract Syntax" defines meaning as
(subject, predicate, object) triples over IRIs and literals, which is literally MDG's triple
shape. Reasoning over such graphs is SPARQL (W3C "SPARQL 1.1 Query Language"), so the "reasoning"
MDG promises over its graph already exists as a query language. RDF gives the representation but
no learned codes and no token-position economics.

The "learned discrete codes" are van den Oord et al., "Neural Discrete Representation Learning"
(VQ-VAE, 2017): a learned codebook of discrete latent codes. MDG is applying that mechanism to
semantic graphs rather than pixels or audio.

Transformers that consume graphs directly, instead of serialised text, are the Graphormer line —
Ying et al., "Do Transformers Really Perform Bad for Graph Representation?" (2021) — evidence on
graph-native inputs. Compression of meaning to a dense vector is Sentence-BERT (Reimers &
Gurevych, 2019), which shows the other route to fewer positions (a fixed-size embedding) and its
own information bottleneck.

Finally, the efficiency axis itself is crowded: Tay et al., "Efficient Transformers: A Survey"
(2020) is the field reducing positions and compute without inventing a new representation. And
Dziri et al., "Faith and Fate: Limits of Transformers on Compositionality" (2023) matters for the
reasoning half of the claim: compositionality limits are about reasoning depth/steps, not about
representation density, so a denser representation need not fix them.

Bottom line on prior art: MDG is AMR + RDF + VQ-VAE + a position-budget claim. The claim is the
only genuinely testable novelty, and it is what experiment P1 attacks.

## 3. Experiment design (fixed BEFORE any result)

P1 — sequence-position accounting.

Measure: for each of 16 sentences, count (a) NL positions = tokens after lowercasing and
splitting on non-alphanumerics; (b) MDG_C positions = 3 per triple, the conservative
serialisation in which every (subject, predicate, object) is emitted in full; (c) MDG_G = number
of DISTINCT nodes (graph reuse: a node repeated costs 0 extra positions).

Support for the hypothesis: NL/MDG ratio substantially greater than 1 under MDG_C — the
accounting MDG's own pipeline (§24, serialised codes) implies — not merely under MDG_G.
Kill condition: ratio <= 1 under MDG_C, or only marginally >1 under MDG_G.

Designed around flattering MDG: (i) it counts MDG_G at all, which is the most favourable
possible reading of "multidimensional" (free, unlimited reuse); (ii) it hand-builds minimal
triple sets for each sentence, i.e. the tightest MDG encoding; (iii) it uses a raw word count for
NL, not BPE, which if anything OVER-counts NL. All three biases favour MDG.

## 4. Results

Per item (id | NL | MDG_C | MDG_G | NL/MDG_C | NL/MDG_G):

1  | 5  | 9  | 7  | 0.56 | 0.71
2  | 14 | 30 | 10 | 0.47 | 1.40
3  | 9  | 18 | 8  | 0.50 | 1.13
4  | 11 | 9  | 8  | 1.22 | 1.38
5  | 8  | 24 | 9  | 0.33 | 0.89
6  | 13 | 21 | 9  | 0.62 | 1.44
7  | 14 | 24 | 10 | 0.58 | 1.40
8  | 8  | 12 | 7  | 0.67 | 1.14
9  | 13 | 6  | 5  | 2.17 | 2.60
10 | 11 | 15 | 7  | 0.73 | 1.57
11 | 8  | 12 | 6  | 0.67 | 1.33
12 | 10 | 18 | 8  | 0.56 | 1.25
13 | 10 | 15 | 8  | 0.67 | 1.25
14 | 9  | 15 | 8  | 0.60 | 1.13
15 | 14 | 12 | 5  | 1.17 | 2.80
16 | 14 | 27 | 10 | 0.52 | 1.40

Totals: NL = 171, MDG_C = 267, MDG_G = 125.
Pooled NL/MDG: C = 0.64, G = 1.37.
Mean of per-item ratios: C = 0.75, G = 1.43.
Items where MDG is cheaper: MDG_C 3/16 (items 4, 9, 15), MDG_G 14/16.

Reading it: under the conservative serialisation, MDG needs ~1.6x MORE positions than natural
language overall, and wins on only 3 of 16 sentences — all of them short text carrying very few
facts. It only turns positive when nodes are reused for free (MDG_G), and even then the gain is
about 1.4x, which is not "substantially fewer". The single decisive point: MDG's own pipeline
serialises the codes for the model, so the honest count is the conservative one, and on that
count MDG does not beat natural language — it loses.

## 5. What could not be run, and why

No model was run and no live measurement of tokens/FLOPs was taken. This session had no working
execution path: terminal `run` was refused (approvals off for this test); Blender's `run_python`
is a dangerous-grade action and was refused; and the browser refuses `file:` URLs, so the JS
version of the experiment could not be executed in-place either. The numbers above are the P1
counts computed deterministically (they are counts and ratios, not measurements that need a
model). p1.html is the same experiment as a runnable script for whenever a person can open it.

Because nothing was executed, the position counts for MDG are the spec's own idealised encoding
(3 positions per triple, or 1 per distinct node), not the length of any real trained code
sequence. A real learned serialisation is very unlikely to be shorter than 3 per triple.

## 6. Threats to validity / how MDG could look better than it is

- NL counted as raw words, not BPE tokens. BPE would merge rare words (e.g. bentonville,
bentonville) and REDUCE the NL count, making MDG look worse still.
- MDG_G is already the best case for MDG. It assumes a global shared node identity with zero
cost to reference, which the spec's per-sequence learned codes do not obviously provide.
- The 16 sentences are hand-authored and minimal. If real semantic annotation is less economical
than my triples, MDG worsens; if I have over-factored, MDG improves — but by construction I
stripped to the fewest triples that preserve the meaning, so this bias runs in MDG's favour.
- P1 tests position COUNT only. It says nothing about accuracy per position, which is where the
reasoning half of §25 (and Dziri et al.'s compositionality limits) would have to be tested with a
trained model — not run here.

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

Every source above was located and read at the URL given (ACL Anthology, W3C and arXiv), and each
is cited for the point it actually makes.
