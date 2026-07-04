# Hermes lifecycle image — hardened pin (WP2 stub; the kit lands in WP5).
#
# Derived from adapter-hermes/testkit/Dockerfile.multiagent (SOFT branch pin);
# hardened to the verified release tag. The Hermes installer self-installs its
# own Node 22 and uv — this is a HEAVY build (uv sync + frontend build).
# Invariants: NO alf, NO secrets in any layer (D6).
#
# Build:  docker build -t alf-lifecycle-hermes tests/lifecycle/frameworks/hermes
FROM debian:bookworm-slim

ARG HERMES_REF=v0.17.0
ARG HERMES_VERSION=0.17.0
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git xz-utils sqlite3 tree build-essential \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /home/agent
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
