# Hermes testkit image — a reproducible "real Hermes" box for the adapter-hermes
# Phase 0 spike AND later integration tests.
#
# It bundles a pinned checkout of NousResearch/hermes-agent and the testkit
# scripts. Its job is to (a) SEED authentic state.db fixtures through Hermes's
# own storage code and (b) act as the ORACLE that opens a Rust-rebuilt state.db
# and confirms a real Hermes reads it (FTS included). The Rust rebuild itself
# runs in host/CI cargo — kept out of this image to stay lean.
#
# hermes_state.SessionDB imports with NO third-party installs (only the repo on
# PYTHONPATH), so a slim Python base suffices — mirroring the proven host setup.
#
# Build:  docker build -t hermes-testkit --build-arg HERMES_REF=main adapter-hermes/testkit
# Seed:   docker run --rm -v "$PWD/out:/work" hermes-testkit python seed.py /work/source.db
# Verify: docker run --rm -v "$PWD/out:/work" hermes-testkit python verify_open.py /work/rebuilt.db 3
FROM python:3.13-slim

ARG HERMES_REF=main
ENV PYTHONUNBUFFERED=1 \
    PYTHONDONTWRITEBYTECODE=1 \
    PYTHONPATH=/opt/hermes-agent \
    HERMES_HOME=/work/.hermes

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/*

# Pinned Hermes checkout — the schema + storage source of truth.
RUN git clone --depth 1 --branch "${HERMES_REF}" \
    https://github.com/NousResearch/hermes-agent /opt/hermes-agent

WORKDIR /opt/testkit
COPY seed.py verify_open.py ./

# Fail the build if SessionDB can't import against this Hermes ref (early
# signal that a future Hermes release changed the storage module's deps).
RUN python -c "import sys; sys.path.insert(0,'/opt/hermes-agent'); \
    from hermes_state import SessionDB, SCHEMA_VERSION; \
    print('hermes_state OK, schema_version', SCHEMA_VERSION)"

VOLUME ["/work"]
CMD ["python", "seed.py", "/work/source.db"]
