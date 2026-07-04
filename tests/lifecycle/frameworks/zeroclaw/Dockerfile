# ZeroClaw lifecycle image — hardened pin (WP2).
#
# Standard idiomatic install as a non-root user via the OFFICIAL installer at
# the PINNED tag, with a BUILD-TIME version guard: if the installer resolves a
# different binary version than the tag promises, the build fails here — not
# mid-lifecycle-run. Derived from adapter-zeroclaw/testkit/Dockerfile.multiagent
# (which pins SOFT via master); this one pins HARD.
#
# Invariants: the image contains NO alf (bind-mounted + injected per run, D6)
# and NO secrets (creds arrive via --env-file at `docker run`, never at build).
#
# Build:  docker build -t alf-lifecycle-zeroclaw tests/lifecycle/frameworks/zeroclaw
FROM debian:bookworm-slim

ARG ZEROCLAW_REF=v0.8.2
ARG ZEROCLAW_VERSION=0.8.2
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git sqlite3 tree libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Typical user installs in their own $HOME, not as root.
RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /home/agent

# Idiomatic install: the prebuilt binary path + skip the interactive
# quickstart. Installs `zeroclaw` to ~/.cargo/bin; config dir ~/.zeroclaw.
RUN curl -fsSL "https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/${ZEROCLAW_REF}/install.sh" \
      | sh -s -- --prebuilt --skip-quickstart

ENV PATH="/home/agent/.cargo/bin:${PATH}"

# Build-time version guard: the tag must deliver exactly the pinned version.
RUN test "$(zeroclaw --version)" = "zeroclaw ${ZEROCLAW_VERSION}" \
    || { echo "VERSION GUARD FAILED: expected 'zeroclaw ${ZEROCLAW_VERSION}', got '$(zeroclaw --version)'"; exit 1; }

# The harness starts the container with `sleep infinity` and drives every
# stage via `docker exec` (D7).
CMD ["sleep", "infinity"]
