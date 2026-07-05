# Hermes lifecycle image (WP5). Hardened pin; kit-driven stages.
#
# Derived from adapter-hermes/testkit/Dockerfile.multiagent (SOFT branch pin);
# hardened to the verified release tag. The Hermes installer self-installs its
# own Node 22 and uv — this is a HEAVY build (uv sync + frontend build).
# Invariants: NO alf, NO secrets in any layer (D6).
#
# TYPICAL COLOCATED INSTALL: a default Hermes install puts BOTH the runtime
# (hermes-agent/venv/node/bin) AND agent data under HERMES_HOME (~/.hermes) —
# exactly what a real user has. The lifecycle harness bind-mounts a fresh host
# dir over HERMES_HOME per run, which would shadow the colocated runtime; the
# runner therefore SEEDS each run's home from this image's ~/.hermes first (kit
# `seed_home_from_image`), so the mounted home IS the real install (runtime +
# data) and the venv's absolute `/home/agent/.hermes/...` paths resolve. No
# code/data split — the image install stays the standard one users get.
#
# Build:  docker build -t alf-lifecycle-hermes tests/lifecycle/frameworks/hermes
FROM debian:bookworm-slim

# Upstream tags releases by DATE (`v2026.6.19`), not by the semver the CLI
# reports. `hermes --version` prints "Hermes Agent v0.17.0 (2026.6.19) …", so the
# 0.17.0 release is git tag v2026.6.19 — HERMES_REF is the clone ref, and
# HERMES_VERSION is the CLI-reported semver the guard/kit match on.
ARG HERMES_REF=v2026.6.19
ARG HERMES_VERSION=0.17.0
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git xz-utils sqlite3 tree build-essential \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /home/agent
# Standard colocated install: runtime + data both under HERMES_HOME (what users
# get). The runner seeds each run's mounted home from this dir (see header).
ENV HERMES_HOME=/home/agent/.hermes

# Idiomatic install, non-interactive: skip the API-key wizard and the heavy
# Playwright/Chromium download; pin to the release tag.
RUN curl -fsSL https://hermes-agent.nousresearch.com/install.sh \
      | bash -s -- --non-interactive --skip-setup --skip-browser --branch "${HERMES_REF}"

ENV PATH="/home/agent/.local/bin:${PATH}"

# Build-time version guard (best-effort text match; WP5 tightens this when the
# kit lands and the CLI surface is pinned down).
RUN hermes --version 2>/dev/null | grep -q "${HERMES_VERSION}" \
    || { echo "VERSION GUARD FAILED: expected ${HERMES_VERSION}, got '$(hermes --version 2>/dev/null)'"; exit 1; }

CMD ["sleep", "infinity"]
