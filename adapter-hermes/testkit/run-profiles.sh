#!/usr/bin/env bash
# GOAL 2 — actually RUN both Hermes profiles (agents) against the agent-life LLM
# proxy so each profile's session DB + memory populate, then leave the data on
# the host for inspection.
#
# Mounts ./captured/home -> the container's ~/.hermes/profiles, so after the run
# each profile's state.db + memories/ are inspectable on the host. captured/home
# is .gitignored — config.yaml/.env embed the runtime API key.
#
# Creds: env file (default /tmp/goal2.env) with RUNTIME_API_KEY / LLM_PROXY_URL /
# BEDROCK_MODEL_ID (from provision-test-runtime.sh).
set -euo pipefail
cd "$(dirname "$0")"
ENV_FILE="${1:-/tmp/goal2.env}"
IMAGE=hermes-multiagent

docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured/home && mkdir -p captured/home

docker run --rm --env-file "$ENV_FILE" \
  -v "$PWD/captured/home:/home/agent/.hermes/profiles" "$IMAGE" bash -lc '
  set -e
  export PATH="$HOME/.local/bin:$PATH"
  base="${LLM_PROXY_URL%/}"; case "$base" in */v1) ;; *) base="$base/v1";; esac

  for p in agent_a agent_b; do hermes profile create "$p" --no-skills 1>&2 || true; done

  # Write the proxy wiring into each profiles config.yaml + .env (named profiles
  # do not get a config.yaml from `profile create`). Hermes custom provider reads
  # model.api_key from config.yaml.
  for p in agent_a agent_b; do
    D="$HOME/.hermes/profiles/$p"; mkdir -p "$D/memories"
    cat > "$D/config.yaml" <<YAML
model:
  default: "$BEDROCK_MODEL_ID"
  provider: "custom"
  base_url: "$base"
  api_key: "$RUNTIME_API_KEY"
agent:
  max_turns: 12
YAML
    { echo "OPENAI_API_KEY=$RUNTIME_API_KEY"; echo "OPENAI_BASE_URL=$base"; } > "$D/.env"
  done

  echo "===== run agent_a =====" 1>&2
  timeout 170 hermes -p agent_a chat -Q -q "Remember and save to memory: agent_a favorite color is teal." 2>&1 | tail -4 1>&2 || true
  echo "===== run agent_b =====" 1>&2
  timeout 170 hermes -p agent_b chat -Q -q "Remember and save to memory: agent_b favorite animal is the otter." 2>&1 | tail -4 1>&2 || true

  for p in agent_a agent_b; do
    D="$HOME/.hermes/profiles/$p"
    echo "===== profile $p ====="
    echo "## state.db:"; find "$D" -name "state.db"
    if [ -f "$D/state.db" ]; then
      echo "## sessions / messages counts:"; sqlite3 "$D/state.db" "SELECT (SELECT count(*) FROM sessions) AS sessions, (SELECT count(*) FROM messages) AS messages;" 2>&1 || true
      echo "## messages (role, content head):"; sqlite3 "$D/state.db" "SELECT role, substr(replace(coalesce(content,\"\"),char(10),\" \"),1,70) FROM messages ORDER BY id;" 2>&1 || true
    fi
    echo "## memories/:"; ls -1 "$D/memories" 2>/dev/null || true
    [ -f "$D/memories/MEMORY.md" ] && { echo "## MEMORY.md:"; cat "$D/memories/MEMORY.md"; } || true
  done
'
echo "== captured/home holds both populated profiles (gitignored) ==" 1>&2
