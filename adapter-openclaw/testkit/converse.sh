#!/usr/bin/env bash
# Integration harness: scripted multi-turn conversations that drive both OpenClaw
# agents to create episodic/semantic/procedural memories + keep a fake secret.
# Produces detailed transcripts (gitignored) + a committed coverage report.
#
# Usage: ./converse.sh [env-file]   (env-file: RUNTIME_API_KEY/LLM_PROXY_URL/BEDROCK_MODEL_ID)
set -euo pipefail
cd "$(dirname "$0")"
ENV_FILE="${1:-/tmp/goal2.env}"
IMAGE=openclaw-multiagent
ROOT="$(cd ../.. && pwd)"
SCN="$ROOT/scripts/multiagent-scenario.sh"

docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured/home && mkdir -p captured/home

docker run --rm --env-file "$ENV_FILE" \
  -v "$PWD/captured/home:/home/agent/.openclaw" \
  -v "$SCN:/scenario.sh:ro" "$IMAGE" bash -c '
  set -e
  . /scenario.sh
  filter(){ grep -viE "unsettled top-level await|if \(await|^\s*\^" || true; }
  base="${LLM_PROXY_URL%/}"; case "$base" in */v1) ;; *) base="$base/v1";; esac
  prov=$(printf "{\"baseUrl\":\"%s\",\"apiKey\":\"%s\",\"api\":\"openai-completions\",\"models\":[{\"id\":\"%s\",\"name\":\"%s\"}]}" "$base" "$RUNTIME_API_KEY" "$BEDROCK_MODEL_ID" "$BEDROCK_MODEL_ID")
  openclaw config set models.providers.agent-life "$prov" >/dev/null 2>&1 || true
  openclaw config set agents.defaults.model "agent-life/$BEDROCK_MODEL_ID" >/dev/null 2>&1 || true
  LOGS="$HOME/.openclaw/logs/conversation"; mkdir -p "$LOGS"
  for ag in $SCENARIO_AGENTS; do
    openclaw agents add "$ag" --non-interactive --workspace "$HOME/.openclaw/workspace-$ag" 2>&1 | filter >/dev/null
    tlog="$LOGS/transcript-$ag.log"; : > "$tlog"
    scenario_turns "$ag" | while IFS="|" read -r type marker prompt; do
      [ -z "$type" ] && continue
      echo "    · [$ag] turn=$type ($marker)" 1>&2
      { echo "================ [$ag] turn=$type marker=$marker ================"
        echo ">>> USER: $prompt"; echo "<<< AGENT:"; } >> "$tlog"
      timeout 175 openclaw agent --local --agent "$ag" --session-id "conv-$ag" -m "$prompt" 2>&1 | filter >> "$tlog" || echo "(turn error/timeout)" >> "$tlog"
      echo >> "$tlog"
    done
    echo "[$ag] conversation done" 1>&2
  done
'

# ---- host-side analysis: dump + placement + verify -> committed report ----
. "$SCN"
DUMPS="$(mktemp -d)"
REPORT="captured/conversation-report.md"
MODEL=$(grep -E '^BEDROCK_MODEL_ID=' "$ENV_FILE" | cut -d= -f2-)
{ echo "# OpenClaw — multi-agent memory conversation"; echo
  echo "_Model: ${MODEL:-?}. Detailed transcripts: captured/home/logs/conversation/ (gitignored)._"; echo; } > "$REPORT"
for ag in $SCENARIO_AGENTS; do
  WS="captured/home/workspace-$ag"; SESS="captured/home/agents/$ag/sessions"
  { find "$WS" -name '*.md' -not -path '*/.git/*' -exec sh -c 'echo "### $1"; cat "$1"' _ {} \; 2>/dev/null
    find "$SESS" -name '*.jsonl' -exec cat {} \; 2>/dev/null; } > "$DUMPS/dump-$ag.txt" || true
  : > "$DUMPS/placement-$ag.txt"
  for m in $(scenario_markers "$ag"); do
    hits=$(grep -rl -F "$m" "$WS" "$SESS" 2>/dev/null | sed 's#captured/home/##' | tr '\n' ' ' || true)
    echo "$m -> ${hits:-(not stored)}" >> "$DUMPS/placement-$ag.txt"
  done
done
bash "$ROOT/scripts/multiagent-verify.sh" "OpenClaw" "$DUMPS" "$REPORT"
rm -rf "$DUMPS"
echo "== report ==" 1>&2; cat "$REPORT" 1>&2
