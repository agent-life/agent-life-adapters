#!/usr/bin/env bash
# Build the ZeroClaw multi-agent image, run the idiomatic two-agent setup, then
# capture the REAL on-disk layout + memory DB schema into ./captured/.
# The captured files are the source of truth for the report — not the docs.
set -euo pipefail
cd "$(dirname "$0")"

IMAGE=zeroclaw-multiagent
docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured && mkdir -p captured

docker run --rm -v "$PWD/setup-agents.sh:/setup-agents.sh:ro" "$IMAGE" bash -c '
  set -e
  export PATH="$HOME/.cargo/bin:$PATH"
  CAP=/tmp/cap; mkdir -p "$CAP"
  sh /setup-agents.sh 1>&2                       # idiomatic setup; keep stdout clean for the tar
  H="$HOME/.zeroclaw"
  zeroclaw --version > "$CAP/version.txt" 2>&1 || true
  zeroclaw status > "$CAP/status.txt" 2>&1 || true
  cp "$H/config.toml" "$CAP/config.toml"
  ( cd "$HOME" && find .zeroclaw .cargo/bin .local 2>/dev/null | sort ) > "$CAP/tree.txt"
  DB="$H/data/memory/brain.db"
  if [ -f "$DB" ]; then
    sqlite3 "$DB" .schema > "$CAP/brain.db.schema.sql"
    sqlite3 "$DB" "PRAGMA table_info(memories);" > "$CAP/brain.db.memories.columns.txt" 2>&1 || true
    sqlite3 "$DB" "SELECT name FROM sqlite_master WHERE type=\"table\";" > "$CAP/brain.db.tables.txt" 2>&1 || true
  else
    echo "brain.db absent after idiomatic setup (lazy; created on first agent run)" > "$CAP/brain.db.MISSING.txt"
  fi
  tar -C "$CAP" -cf - .
' > /tmp/zc-cap.tar

tar -C captured -xf /tmp/zc-cap.tar
{ echo "== captured =="; ls -la captured; } 1>&2
