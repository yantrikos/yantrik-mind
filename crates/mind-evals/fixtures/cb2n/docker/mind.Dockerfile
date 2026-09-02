# E.CB2 Mind leg image: the runtime the staging binary needs (libc, OpenSSL 3, zlib, zstd) plus
# python3 for the in-container driver. The binary itself is bind-mounted read-only at run time
# from /opt/yantrik-mind/mind-core (its sha256 and provenance go in the receipt).
FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends libssl3t64 zlib1g libzstd1 ca-certificates python3 curl \
 && rm -rf /var/lib/apt/lists/*
RUN useradd -m -u 10003 mind && mkdir -p /state && chown mind /state
USER mind
WORKDIR /state
