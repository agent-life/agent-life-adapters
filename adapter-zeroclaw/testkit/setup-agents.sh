#!/bin/sh
# Idiomatic two-agent ZeroClaw setup. Runs INSIDE the container as the non-root
# user. Each `agents create` appends an [agents.<alias>] block to the single
# ~/.zeroclaw/config.toml — this is the framework's native multi-agent surface.
#
# NOTE (verified on disk, not from docs): a fresh `agents create` writes ONLY
# config; the sqlite memory backend (~/.zeroclaw/data/memory/brain.db) is
# materialized lazily. `memory reindex` initializes it without needing an LLM,
# so the real schema lands on disk for inspection. In normal use the agent
# creates it on first run.
set -eu

zc="$(command -v zeroclaw || echo "$HOME/.cargo/bin/zeroclaw")"

"$zc" agents create agent_a
"$zc" agents create agent_b
"$zc" agents list

# Initialize the (single, shared) sqlite backend so brain.db + schema exist.
"$zc" memory reindex || true
