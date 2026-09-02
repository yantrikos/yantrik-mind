# E.CB2 Hermes leg image: the pinned Hermes commit, nothing else. The source is a `git archive`
# of commit 3ce1cf2bb768f39026e059f5236522dea2a4afe3 (NousResearch/hermes-agent), shipped to
# the build context as hermes-3ce1cf2.tar.gz (its sha256 is recorded in the run receipt) — the
# commit is pinned by construction, not by a network fetch that a rate limit can break.
FROM python:3.13-slim
RUN apt-get update && apt-get install -y --no-install-recommends git ripgrep && rm -rf /var/lib/apt/lists/*
COPY docker/hermes-3ce1cf2.tar.gz /src/hermes.tar.gz
RUN cd /src && tar xzf hermes.tar.gz && pip install --no-cache-dir ./hermes-agent && rm -rf /src
RUN useradd -m -u 10001 runner
USER runner
WORKDIR /work
ENTRYPOINT ["hermes"]
