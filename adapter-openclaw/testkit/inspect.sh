#!/usr/bin/env bash
# Build the OpenClaw multi-agent image, run the idiomatic two-agent setup, then
# capture the REAL on-disk layout + the shared sqlite schema into ./captured/.
set -euo pipefail
cd "$(dirname "$0")"

IMAGE=openclaw-multiagent
docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured && mkdir -p captured

docker run --rm -v "$PWD/setup-agents.sh:/setup-agents.sh:ro" "$IMAGE" bash -c '
  set -e
  CAP=/tmp/cap; mkdir -p "$CAP"
  sh /setup-agents.sh 1>&2
  H="$HOME/.openclaw"
  openclaw --version > "$CAP/version.txt" 2>&1 || true
  openclaw agents list > "$CAP/agents-list.txt" 2>&1 || true
  cp "$H/openclaw.json" "$CAP/openclaw.json"
  # tree without .git internals (note: each workspace IS a git repo)
  ( cd "$HOME" && find .openclaw -not -path "*/.git/*" 2>/dev/null | sort ) > "$CAP/tree.txt"
  ls -1 "$H"/workspace-agent_a > "$CAP/workspace-agent_a.files.txt" 2>&1 || true
  DB="$H/state/openclaw.sqlite"
  if [ -f "$DB" ]; then
    sqlite3 "$DB" .schema > "$CAP/openclaw.sqlite.schema.sql" 2>&1 || true
    sqlite3 "$DB" "SELECT name FROM sqlite_master WHERE type=\"table\";" > "$CAP/openclaw.sqlite.tables.txt" 2>&1 || true
  fi
  tar -C "$CAP" -cf - .
' > /tmp/oc-cap.tar

tar -C captured -xf /tmp/oc-cap.tar
{ echo "== captured =="; ls -la captured; } 1>&2
