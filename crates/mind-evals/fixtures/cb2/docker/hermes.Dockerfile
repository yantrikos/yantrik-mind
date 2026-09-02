# E.CB2 Hermes leg image: the pinned Hermes commit, nothing else. Built once on the box with
# network; run under net/cb2net.sh (egress = the owned model endpoint only).
FROM python:3.13-slim
ARG HERMES_REPO=https://github.com/NousResearch/hermes-agent.git
ARG HERMES_COMMIT=3ce1cf2bb768f39026e059f5236522dea2a4afe3
RUN apt-get update && apt-get install -y --no-install-recommends git ripgrep && rm -rf /var/lib/apt/lists/*
RUN pip install --no-cache-dir "git+${HERMES_REPO}@${HERMES_COMMIT}"
RUN useradd -m -u 10001 runner
USER runner
WORKDIR /work
ENTRYPOINT ["hermes"]
