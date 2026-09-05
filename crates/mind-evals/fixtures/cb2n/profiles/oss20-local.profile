# cb2n profile "oss20-local" (E.CB2-HTTP, reading 8 "in local"): the only local model, gpt-oss-backup:20b
# on the PZC Ollama at 192.168.4.35:11434 over PLAIN HTTP. Not hostname-verified: the proxy records
# upstream_scheme http / tls_hostname_verified false / upstream_reachable true, and the manifest says so.
# Containment is the same shape as qwen: one pinned address, one declared port, then DROP.
CB2_UPSTREAM=192.168.4.35
CB2_UPSTREAM_IP=192.168.4.35
CB2_UPSTREAM_IPS=192.168.4.35
CB2_UPSTREAM_RESOLVE=0
CB2_UPSTREAM_SCHEME=http
CB2_UPSTREAM_PORT=11434
CB2_MODEL=gpt-oss-backup:20b
CB2_KEY_FILE=
CB2_MIND_LANE=local
CB2_MIND_PROVIDER=
CB2_MIND_KEY_ENV=
