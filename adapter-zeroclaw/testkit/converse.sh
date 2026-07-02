#!/usr/bin/env bash
# Integration harness: scripted multi-turn conversations driving both ZeroClaw
# agents to create episodic/semantic/procedural memories + keep a fake secret.
# Detailed transcripts (gitignored) + committed coverage report. Because ZeroClaw
# uses ONE shared brain.db, placement shows the (category|key) each marker got
# AND which agent_id owns it — the exact axis the adapter must scope on.
#
# Usage: ./converse.sh [env-file]
set -euo pipefail
cd "$(dirname "$0")"
ENV_FILE="${1:-/tmp/goal2.env}"
IMAGE=zeroclaw-multiagent
ROOT="$(cd ../.. && pwd)"
SCN="$ROOT/scripts/multiagent-scenario.sh"

docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured/home && mkdir -p captured/home

docker run --rm --env-file "$ENV_FILE" \
  -v "$PWD/captured/home:/home/agent/.zeroclaw" \
  -v "$SCN:/scenario.sh:ro" "$IMAGE" bash -c '
  set -e
  . /scenario.sh
  zc="$HOME/.cargo/bin/zeroclaw"
  base="${LLM_PROXY_URL%/}"; case "$base" in */v1) ;; *) base="$base/v1";; esac
  cat > "$HOME/.zeroclaw/config.toml" <<TOML
schema_version = 3
[providers.models.custom.agentlife]
uri = "$base"
model = "$BEDROCK_MODEL_ID"
api_key = "$RUNTIME_API_KEY"
[agents.agent_a]
model_provider = "custom.agentlife"
risk_profile = "assistant"
runtime_profile = "assistant"
channels = ["cli"]
[agents.agent_b]
model_provider = "custom.agentlife"
risk_profile = "assistant"
runtime_profile = "assistant"
channels = ["cli"]
[risk_profiles.assistant]
level = "full"
allowed_commands = ["alf", "rm"]
[runtime_profiles.assistant]
agentic = true
max_tool_iterations = 12
max_actions_per_hour = 200
[autonomy]
allowed_roots = ["/home/agent"]
[memory]
backend = "sqlite"
auto_save = true
embedding_provider = "none"
[secrets]
encrypt = false
TOML
  LOGS="$HOME/.zeroclaw/logs/conversation"; mkdir -p "$LOGS"
  for ag in $SCENARIO_AGENTS; do
    tlog="$LOGS/transcript-$ag.log"; : > "$tlog"; sess="$HOME/.zeroclaw/sess-$ag.json"
    scenario_turns "$ag" | while IFS="|" read -r type marker prompt; do
      [ -z "$type" ] && continue
      echo "    · [$ag] turn=$type ($marker)" 1>&2
      { echo "================ [$ag] turn=$type marker=$marker ================"
        echo ">>> USER: $prompt"; echo "<<< AGENT:"; } >> "$tlog"
      timeout 175 "$zc" agent -a "$ag" --session-state-file "$sess" -m "$prompt" 2>&1 | tail -40 >> "$tlog" || echo "(turn error/timeout)" >> "$tlog"
      echo >> "$tlog"
    done
    echo "[$ag] conversation done" 1>&2
  done
'

# ---- host-side analysis from the single shared brain.db (agent_id-scoped) ----
. "$SCN"
DB="captured/home/data/memory/brain.db"
DUMPS="$(mktemp -d)"
REPORT="captured/conversation-report.md"
MODEL=$(grep -E '^BEDROCK_MODEL_ID=' "$ENV_FILE" | cut -d= -f2-)
{ echo "# ZeroClaw — multi-agent memory conversation"; echo
  echo "_Model: ${MODEL:-?}. ONE shared brain.db; rows scoped only by agent_id._"
  echo "_Detailed transcripts: captured/home/logs/conversation/ (gitignored)._"; echo; } > "$REPORT"
for ag in $SCENARIO_AGENTS; do
  sqlite3 "$DB" "SELECT m.content FROM memories m JOIN agents a ON a.id=m.agent_id WHERE a.alias='$ag';" > "$DUMPS/dump-$ag.txt" 2>/dev/null || true
  # placement = (category | key | content-head) per row for this agent — shows classification
  sqlite3 "$DB" "SELECT m.category||' | '||m.key||' | '||substr(replace(m.content,char(10),' '),1,55) FROM memories m JOIN agents a ON a.id=m.agent_id WHERE a.alias='$ag' ORDER BY m.category, m.key;" > "$DUMPS/placement-$ag.txt" 2>/dev/null || true
done
bash "$ROOT/scripts/multiagent-verify.sh" "ZeroClaw" "$DUMPS" "$REPORT"
rm -rf "$DUMPS"
echo "== report ==" 1>&2; cat "$REPORT" 1>&2
