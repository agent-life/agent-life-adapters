# Generic-runtime curated-MCP lifecycle image (Z14 in-place-edit tier).
#
# Same trivial base as alf-lifecycle-generic (debian + sqlite3/tree). There is NO
# framework to install: the alf-under-test is bind-mounted and the fixture — a
# committed `.alf-map.json` plus a synthetic `brain.db` (sqlite memory source) —
# is seeded host-side by the kit. The ONLY difference from `generic` is the KIT:
# a `curated` store + the sqlite source, so Z14 (curated in-place edit) runs
# through the MCP `alf_sync` tool. Kept self-contained (not FROM
# alf-lifecycle-generic) so no base image has to be built first.
#
# Invariants (D6): NO alf, NO secrets, NO framework binary in any layer.
#
# Build:  docker build -t alf-lifecycle-generic-mcp tests/lifecycle/frameworks/generic-mcp
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates sqlite3 tree \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /home/agent

LABEL alf.lifecycle.variant="generic-mcp"

CMD ["sleep", "infinity"]
