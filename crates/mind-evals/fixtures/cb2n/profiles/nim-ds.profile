# cb2n profile "nim-ds" (E.CB2-D): identical to "nim-cap24" except for the model.
#
# openai/gpt-oss-120b was retired by NVIDIA at 2026-09-03T08:00:00Z, mid-line, and readings 3-6 all
# ran on it. They stay reproducible because THIS IS A NEW FILE: nim.profile and nim-cap24.profile
# are untouched.
#
# The model is Pranab's call -- deepseek, free on NIM -- and pro rather than flash is a measurement:
# on realistic 400-token coding requests flash returned 529 "Service temporarily overloaded" on
# three of five attempts, and a 5xx is an upstream error that VOIDS a leg under this harness's own
# rule. pro answered 4 of 4 at 18.5s / 19.4s / 62.3s / 29.9s.
#
# WALL RISK, recorded here because it is the one that would arrive disguised as a system failure:
# pro's median is ~25s per call against gpt-oss-20b's 8.7s. At cap 24 that is ~1,488s of pure model
# time in the worst observed case, against an 1800s wall per leg. Reading 6 already lost a Hermes
# leg to that wall. Run one pilot leg and measure before committing a sequence; raise the wall
# deliberately if the tail demands it, and record the change.
# Original header follows.
# cb2n profile "nim-cap24" (E.CB2-N reading 4): identical to "nim" except for CB2_CAP. The cap of
# 8 was equal to Hermes's configured agent.max_turns 8, so eight turns cost at least eight
# requests and any retry was a guaranteed violation — a budget that made one competitor's own
# turn limit unsatisfiable. 24 is three times that limit: the cap stops runaway spend and no
# longer binds normal operation. "nim" is left byte-identical so reading 3 stays reproducible.
# Original header follows.
# cb2n profile "nim" (E.CB2-N): NVIDIA NIM upstream, its IPv4 addresses resolved on the box ONCE
# per run into the immutable run state (allowlisted exclusively, recorded in every receipt); the
# key file (uid 10002, mode 0400) mounted read-only into the PROXY container only and injected as
# the Authorization header on every forward; both work containers hold placeholder keys. One
# model for both systems. The Mind runs with YM_PRIMARY_BRAIN=nim:<model> and all six roles equal
# to it, behind YM_PROVIDER_BASE_URL_NIM (the proxy).
CB2_UPSTREAM=integrate.api.nvidia.com
CB2_UPSTREAM_IP=
CB2_UPSTREAM_IPS=
CB2_UPSTREAM_RESOLVE=1
CB2_MODEL=deepseek-ai/deepseek-v4-pro-0813
CB2_KEY_FILE=/root/cb2/secrets/nim.key
CB2_MIND_LANE=roles
CB2_MIND_PROVIDER=nim
CB2_MIND_KEY_ENV=NVIDIA_API_KEY
CB2_CAP=24
# Measured, not guessed: 24 sequential 600-token calls on this model took 1,163 s -- 65% of the old
# 1800 s wall in model time alone, before any agent or tool time, with a p90 of 80 s and a 110 s
# tail. At 3600 s that same model time is 32%.
CB2_WALL=3600
