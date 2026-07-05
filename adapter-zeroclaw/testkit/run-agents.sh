#!/usr/bin/env bash
# GOAL 2 — actually RUN both ZeroClaw agents against the agent-life LLM proxy so
# memory/sessions populate, then leave the data on the host for inspection.
#
# Mounts ./captured/home -> the container's ~/.zeroclaw, so after the run the
# real config.toml + memory brain.db(s) are inspectable on the host.
# captured/home is .gitignored — config.toml embeds the runtime API key.
#
# Creds come from an env file (default /tmp/goal2.env) with:
#   RUNTIME_API_KEY, LLM_PROXY_URL, BEDROCK_MODEL_ID
# (minted by agent-life-service/scripts/provision-test-runtime.sh).
#
# NOTE: this deliberately does NOT set workspace_dir or a per-agent workspace
# path — i.e. the idiomatic DEFAULT — to observe whether running agents write to
# ONE shared brain.db (agent_id-scoped) or per-agent DBs.
set -euo pipefail
cd "$(dirname "$0")"
ENV_FILE="${1:-/tmp/goal2.env}"
IMAGE=zeroclaw-multiagent

docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured/home && mkdir -p captured/home

docker run --rm --env-file "$ENV_FILE" \
  -v "$PWD/captured/home:/home/agent/.zeroclaw" "$IMAGE" bash -c '
  set -e
  zc="$HOME/.cargo/bin/zeroclaw"
  base="${LLM_PROXY_URL%/}"; case "$base" in */v1) ;; *) base="$base/v1";; esac

  cat > "$HOME/.zeroclaw/config.toml" <<TOML
schema_version = 3

[providers.models.custom.agentlife]
uri     = "$base"
model   = "$BEDROCK_MODEL_ID"
api_key = "$RUNTIME_API_KEY"

[agents.agent_a]
model_provider  = "custom.agentlife"
risk_profile    = "assistant"
runtime_profile = "assistant"
channels        = ["cli"]

[agents.agent_b]
model_provider  = "custom.agentlife"
risk_profile    = "assistant"
runtime_profile = "assistant"
channels        = ["cli"]

[risk_profiles.assistant]
level = "full"
allowed_commands = ["alf", "rm"]

[runtime_profiles.assistant]
agentic = true
max_tool_iterations = 12
max_actions_per_hour = 120

[autonomy]
allowed_roots = ["/home/agent"]

[memory]
backend = "sqlite"
auto_save = true
embedding_provider = "none"

[secrets]
encrypt = false
TOML

  echo "===== run agent_a =====" 1>&2
  timeout 150 "$zc" agent -a agent_a -m "Please remember and save to memory this fact: agent_a'\''s favorite color is teal." 2>&1 | tail -6 1>&2 || true
  echo "===== run agent_b =====" 1>&2
  timeout 150 "$zc" agent -a agent_b -m "Please remember and save to memory this fact: agent_b'\''s favorite animal is the otter." 2>&1 | tail -6 1>&2 || true

  echo "===== brain.db location(s) ====="
  find "$HOME/.zeroclaw" -name "*.db" | sort
  echo "===== memory rows by agent (alias, key, content head) ====="
  for db in $(find "$HOME/.zeroclaw" -name brain.db); do
    echo "## $db"
    sqlite3 "$db" "SELECT a.alias, count(*) FROM memories m JOIN agents a ON a.id=m.agent_id GROUP BY a.alias;" 2>&1 || true
    sqlite3 "$db" "SELECT a.alias, m.key, substr(replace(m.content,char(10),\" \"),1,70) FROM memories m JOIN agents a ON a.id=m.agent_id ORDER BY a.alias;" 2>&1 || true
  done
'
echo "== captured/home now holds the populated install (gitignored) ==" 1>&2
find captured/home -name "*.db" 1>&2 || true
