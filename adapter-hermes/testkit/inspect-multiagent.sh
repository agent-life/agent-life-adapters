#!/usr/bin/env bash
# Build the Hermes multi-agent image, run the idiomatic two-profile setup, then
# capture the REAL on-disk profile layout + per-profile state.db schema into
# ./captured/. (The authoritative state.db schema is also in ./schema.sql, from
# the phase-0 real-SessionDB seed.)
set -euo pipefail
cd "$(dirname "$0")"

IMAGE=hermes-multiagent
docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured && mkdir -p captured

docker run --rm -v "$PWD/setup-profiles.sh:/setup-profiles.sh:ro" "$IMAGE" bash -lc '
  set -e
  export PATH="$HOME/.local/bin:$PATH"
  CAP=/tmp/cap; mkdir -p "$CAP"
  sh /setup-profiles.sh 1>&2 || true
  hermes --version > "$CAP/version.txt" 2>&1 || true
  hermes profile list > "$CAP/profile-list.txt" 2>&1 || true
  H="$HOME/.hermes"
  # layout, pruning the heavy shared code checkout / venv / node_modules
  ( cd "$HOME" && find .hermes -maxdepth 4 \
      -not -path "*/hermes-agent/*" -not -path "*/node_modules/*" \
      -not -path "*/.venv/*" 2>/dev/null | sort ) > "$CAP/tree.txt"
  for P in default profiles/agent_a profiles/agent_b; do
    [ "$P" = default ] && DIR="$H" || DIR="$H/$P"
    label=$(echo "$P" | tr "/" "_")
    DB="$DIR/state.db"
    if [ -f "$DB" ]; then
      sqlite3 "$DB" .schema > "$CAP/state.db.$label.schema.sql" 2>&1 || true
      sqlite3 "$DB" "SELECT version FROM schema_version;" > "$CAP/state.db.$label.version.txt" 2>&1 || true
    else
      echo "state.db absent in $P (lazy; created on first session run)" >> "$CAP/state.db.lazy.txt"
    fi
  done
  ls -1a "$H" > "$CAP/home-default.files.txt" 2>&1 || true
  tar -C "$CAP" -cf - .
' > /tmp/hermes-cap.tar

tar -C captured -xf /tmp/hermes-cap.tar
{ echo "== captured =="; ls -la captured; } 1>&2
