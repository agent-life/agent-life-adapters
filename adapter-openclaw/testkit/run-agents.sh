#!/usr/bin/env bash
# GOAL 2 — actually RUN both OpenClaw agents against the agent-life LLM proxy so
# memory/sessions populate, then leave the data on the host for inspection.
#
# Mounts ./captured/home -> the container's ~/.openclaw, so after the run the
# per-agent workspaces (incl. MEMORY.md) + state DB are inspectable on the host.
# captured/home is .gitignored — openclaw.json embeds the runtime API key.
#
# Creds: env file (default /tmp/goal2.env) with RUNTIME_API_KEY / LLM_PROXY_URL /
# BEDROCK_MODEL_ID (from provision-test-runtime.sh).
set -euo pipefail
cd "$(dirname "$0")"
ENV_FILE="${1:-/tmp/goal2.env}"
IMAGE=openclaw-multiagent

docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured/home && mkdir -p captured/home

docker run --rm --env-file "$ENV_FILE" \
  -v "$PWD/captured/home:/home/agent/.openclaw" "$IMAGE" bash -c '
  set -e
  filter(){ grep -viE "unsettled top-level await|if \(await|^\s*\^" || true; }
  openclaw agents add agent_a --non-interactive --workspace "$HOME/.openclaw/workspace-agent_a" 2>&1 | filter >/dev/null
  openclaw agents add agent_b --non-interactive --workspace "$HOME/.openclaw/workspace-agent_b" 2>&1 | filter >/dev/null

  base="${LLM_PROXY_URL%/}"; case "$base" in */v1) ;; *) base="$base/v1";; esac
  prov=$(printf "{\"baseUrl\":\"%s\",\"apiKey\":\"%s\",\"api\":\"openai-completions\",\"models\":[{\"id\":\"%s\",\"name\":\"MiniMax\"}]}" "$base" "$RUNTIME_API_KEY" "$BEDROCK_MODEL_ID")
  openclaw config set models.providers.agent-life "$prov"     2>&1 | filter | grep -v "$RUNTIME_API_KEY" >/dev/null || true
  openclaw config set agents.defaults.model "agent-life/$BEDROCK_MODEL_ID" 2>&1 | filter >/dev/null

  echo "===== run agent_a =====" 1>&2
  timeout 170 openclaw agent --local --agent agent_a -m "Remember in long-term memory and write to MEMORY.md: agent_a favorite color is teal." 2>&1 | filter | tail -3 1>&2 || true
  echo "===== run agent_b =====" 1>&2
  timeout 170 openclaw agent --local --agent agent_b -m "Remember in long-term memory and write to MEMORY.md: agent_b favorite animal is the otter." 2>&1 | filter | tail -3 1>&2 || true

  echo "===== per-agent MEMORY.md (isolation check) ====="
  for w in workspace-agent_a workspace-agent_b; do
    echo "## $w/MEMORY.md"; cat "$HOME/.openclaw/$w/MEMORY.md" 2>/dev/null || echo "(none)"
  done
  echo "===== per-agent workspace markdown ====="
  find "$HOME/.openclaw"/workspace-* -maxdepth 2 \( -name "*.md" -o -path "*/memory/*" \) -not -path "*/.git/*" | sort
  echo "===== sessions ====="; find "$HOME/.openclaw/agents" -type f -not -name "*.lock" | sort
  echo "===== sqlite files ====="; find "$HOME/.openclaw" -name "*.sqlite*" | sort
'
echo "== captured/home holds the populated install (gitignored) ==" 1>&2
