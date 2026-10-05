# Research task R1: Multidimensional Grammar (MDG)

The specification is in this folder: `MDG_spec.md` (v0.1). It proposes a machine-native
semantic language for language models: meaning as a typed graph, serialised into learned
discrete codes only for the model. Its central hypothesis (sections 25 and 32) is that a model
working on MDG solves the same tasks with substantially fewer sequence positions than natural
language.

## What to do

1. **Prior art.** Find the closest existing work and say plainly what MDG does that it does
   not, and what it does not add. Cite real sources by URL; every source you cite must exist
   and say what you cite it for.
2. **A first experiment.** Design one experiment that could show the central hypothesis is
   wrong. Write down, before any result, what you will measure, what would count as support,
   and what would kill the hypothesis. Look hard for ways the experiment could make MDG look
   better than it is, and design around them.
3. **Run it, if you can,** at a size this machine can manage (CPU only, a cloud model for
   language). Report the numbers you get, and say what you could not run and why.
4. **Write the report** to `~/research/R1/report.md`: findings, the experiment, results or
   the honest absence of them, and a list of sources.

Work in `~/research/R1/`. A claim without evidence counts against the report, not for it.
Say when the report is finished.
