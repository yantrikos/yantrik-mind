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
CB2_MODEL=openai/gpt-oss-120b
CB2_KEY_FILE=/root/cb2/secrets/nim.key
CB2_MIND_LANE=roles
CB2_MIND_PROVIDER=nim
CB2_MIND_KEY_ENV=NVIDIA_API_KEY
CB2_CAP=24
