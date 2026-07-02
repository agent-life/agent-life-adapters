#!/bin/sh
# Idiomatic two-agent OpenClaw setup. Runs INSIDE the container.
#
# `openclaw agents add` is the native multi-agent surface: one gateway hosts N
# isolated agents listed in ~/.openclaw/openclaw.json (agents.list[]), each with
# its own git-initialized workspace + state/sessions dir. A default "main" agent
# always pre-exists. --non-interactive requires --workspace.
#
# (Distinct from `openclaw --profile <name>`, which is a separate whole-state
# isolation mechanism under ~/.openclaw-<name> — NOT used here.)
set -eu

filter() { grep -vi 'unsettled top-level await\|if (await\|^[[:space:]]*\^' || true; }

openclaw agents add agent_a --non-interactive \
  --workspace "$HOME/.openclaw/workspace-agent_a" 2>&1 | filter
openclaw agents add agent_b --non-interactive \
  --workspace "$HOME/.openclaw/workspace-agent_b" 2>&1 | filter
openclaw agents list 2>&1 | filter
