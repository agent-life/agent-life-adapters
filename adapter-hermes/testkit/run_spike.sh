#!/usr/bin/env bash
# Phase 0 spike orchestrator for adapter-hermes.
#
# Proves: a real Hermes state.db can be decomposed to ALF-style records and
# rebuilt such that real Hermes (hermes_state.SessionDB) opens the rebuilt DB
# read-write and FTS search works.
#
# Steps: seed (real SessionDB) → rust rebuild → real-Hermes oracle.
# Runs on the host (needs python3 + cargo) or inside the testkit Docker image.
#
# Env:
#   HERMES_REPO  checkout of NousResearch/hermes-agent (default: /tmp/hermes-agent)
#   HERMES_REF   git ref to pin (default: main)
#   WORK         scratch dir for outputs (default: a mktemp dir)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
HERMES_REPO="${HERMES_REPO:-/tmp/hermes-agent}"
HERMES_REF="${HERMES_REF:-main}"
WORK="${WORK:-$(mktemp -d)}"
VENV="$WORK/venv"

echo "▸ testkit: $HERE"
echo "▸ work dir: $WORK"

# 1. Ensure a Hermes checkout (the schema + storage source of truth).
if [ ! -f "$HERMES_REPO/hermes_state.py" ]; then
  echo "▸ cloning NousResearch/hermes-agent@$HERMES_REF → $HERMES_REPO"
  git clone --depth 1 --branch "$HERMES_REF" https://github.com/NousResearch/hermes-agent "$HERMES_REPO"
fi

# 2. Minimal venv — SessionDB needs no pip installs, only the repo on sys.path.
python3 -m venv "$VENV"
export PYTHONPATH="$HERMES_REPO"
PY="$VENV/bin/python"

# 3. Seed a real state.db through Hermes's own write path (no LLM).
echo "▸ seeding source.db"
"$PY" "$HERE/seed.py" "$WORK/source.db"

# 4. Rust rebuild spike: decompose → rebuild (capture-and-replay DDL).
echo "▸ building + running rust rebuild spike"
cargo build --release --manifest-path "$HERE/rebuild_spike/Cargo.toml"
"$HERE/rebuild_spike/target/release/rebuild_spike" \
  "$WORK/source.db" "$WORK/rebuilt.db" "$WORK/records.json"

# 5. Oracle: real Hermes opens the REBUILT db read-write; FTS must work.
echo "▸ oracle: real Hermes opens rebuilt.db"
"$PY" "$HERE/verify_open.py" "$WORK/rebuilt.db" 3

echo "✓ Phase 0 spike PASSED — outputs in $WORK"
