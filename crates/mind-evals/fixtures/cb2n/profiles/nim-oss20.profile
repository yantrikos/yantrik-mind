# cb2n profile "nim-oss20" (E.CB2-M): a CANDIDATE, not a chosen model.
#
# Three pilot legs on deepseek-v4-pro all VOIDED -- 6 of 13 model POSTs returned 5xx, and under this
# harness's rule any 429/5xx on a model request voids the leg, so no reading can be graded on it.
# gpt-oss-20b is alive on the same NIM upstream and is the surviving sibling of openai/gpt-oss-120b,
# which readings 3-6 ran on before NVIDIA retired it at 2026-09-03T08:00:00Z. Family continuity buys
# NOTHING for comparison -- a model change resets the baseline either way -- so it is here only
# because it is alive, free, and much smaller, which makes it the cheapest thing to falsify.
#
# The wall stays at nim-ds's 3600 rather than dropping to 1800: a smaller model should be faster,
# but that is a PREDICTION, and predicting latency is exactly what has been wrong twice today.
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
CB2_MODEL=openai/gpt-oss-20b
CB2_KEY_FILE=/root/cb2/secrets/nim.key
CB2_MIND_LANE=roles
CB2_MIND_PROVIDER=nim
CB2_MIND_KEY_ENV=NVIDIA_API_KEY
CB2_CAP=24
# Measured, not guessed: 24 sequential 600-token calls on this model took 1,163 s -- 65% of the old
# 1800 s wall in model time alone, before any agent or tool time, with a p90 of 80 s and a 110 s
# tail. At 3600 s that same model time is 32%.
CB2_WALL=3600
