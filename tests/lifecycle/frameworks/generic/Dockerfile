# Generic-runtime lifecycle image (WP-M4) — the toy MCP runtime.
#
# Unlike zeroclaw/openclaw/hermes there is NO framework to install: the "generic"
# runtime is any MCP-capable host, and its agent is described by a committed
# `.alf-map.json` (the WP-M1 toy fixture, seeded host-side by the kit). So this
# image is just the debian base + the small tools the harness reads with
# (sqlite3/tree). The alf-under-test is bind-mounted at run time, and the MCP
# server (`alf mcp serve`) runs inside via a persistent `docker exec -i` session.
#
# Invariants (D6): NO alf, NO secrets, NO framework binary in any layer — the
# only "install" is the fixture the kit lays down at Z1, so there is no build-time
# version guard (the framework version comes from the map's `framework_version`).
#
# Build:  docker build -t alf-lifecycle-generic tests/lifecycle/frameworks/generic
FROM debian:bookworm-slim

ENV DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates sqlite3 tree \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -m -s /bin/bash agent
USER agent
WORKDIR /home/agent

CMD ["sleep", "infinity"]
