# OpenClaw lifecycle image — hardened pin (WP2 stub; the kit lands in WP4).
#
# Derived from adapter-openclaw/testkit/Dockerfile.multiagent (already a HARD
# npm pin). Invariants: NO alf, NO secrets in any layer (D6).
#
# Build:  docker build -t alf-lifecycle-openclaw tests/lifecycle/frameworks/openclaw
FROM debian:bookworm-slim

ARG OPENCLAW_VERSION=2026.6.11
ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl git sqlite3 tree gnupg \
    && rm -rf /var/lib/apt/lists/*

# OpenClaw is an npm package; engines require Node >=22.19.
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y --no-install-recommends nodejs \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g "openclaw@${OPENCLAW_VERSION}"

# Build-time version guard: the pinned package must deliver its own version.
# `openclaw --version` prints "OpenClaw <version> (<commit>)", so substring-match
# the pinned version rather than demanding a bare equal.
RUN v="$(openclaw --version 2>/dev/null | tail -1)"; \
    case "$v" in \
      *"${OPENCLAW_VERSION}"*) echo "version guard ok: $v" ;; \
      *) echo "VERSION GUARD FAILED: expected '${OPENCLAW_VERSION}', got '$v'"; exit 1 ;; \
    esac

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /home/agent

CMD ["sleep", "infinity"]
