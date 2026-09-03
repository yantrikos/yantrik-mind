# cb2n profile "nim-flash" (E.CB2-M): the OTHER candidate Pranab named ("deepseek flash or pro").
#
# The id is `-0731`, NOT `-0813`. `-0813` is pro's date suffix; probing it returns a flat
# `404 page not found` and was briefly misread as a retirement (E.MODEL1b was built from that
# mistake). An earlier spot check of flash reported 529 "Service temporarily overloaded" on three of
# five attempts -- but a spot check has now been the wrong instrument three times running, so that
# number decides nothing here. A leg decides it.
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
CB2_MODEL=deepseek-ai/deepseek-v4-flash-0731
CB2_KEY_FILE=/root/cb2/secrets/nim.key
CB2_MIND_LANE=roles
CB2_MIND_PROVIDER=nim
CB2_MIND_KEY_ENV=NVIDIA_API_KEY
CB2_CAP=24
# Measured, not guessed: 24 sequential 600-token calls on this model took 1,163 s -- 65% of the old
# 1800 s wall in model time alone, before any agent or tool time, with a p90 of 80 s and a 110 s
# tail. At 3600 s that same model time is 32%.
CB2_WALL=3600
# E.CB2-B: NVIDIA NIM cuts a request at ~302 s -- measured, four 504s across two models landing in
# 302,155-302,180 ms, a spread of 25 ms. The Mind reads this as YM_PROVIDER_DEADLINE_S and clamps
# any single authoring generation to what that deadline can deliver. Unset means "no declared
# limit" and nothing is clamped, which is why qwen does not carry it.
CB2_PROVIDER_DEADLINE_S=302
