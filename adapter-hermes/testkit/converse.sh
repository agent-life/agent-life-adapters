#!/usr/bin/env bash
# Integration harness: scripted multi-turn conversations driving both Hermes
# profiles (agents) to create episodic/semantic/procedural memories + keep a fake
# secret. Detailed transcripts (gitignored) + committed coverage report.
# Each profile is a fully isolated HERMES_HOME (own state.db + memories/).
#
# Usage: ./converse.sh [env-file]
set -euo pipefail
cd "$(dirname "$0")"
ENV_FILE="${1:-/tmp/goal2.env}"
IMAGE=hermes-multiagent
ROOT="$(cd ../.. && pwd)"
SCN="$ROOT/scripts/multiagent-scenario.sh"

docker build -f Dockerfile.multiagent -t "$IMAGE" . 1>&2
rm -rf captured/home && mkdir -p captured/home

docker run --rm --env-file "$ENV_FILE" \
  -v "$PWD/captured/home:/home/agent/.hermes/profiles" \
  -v "$SCN:/scenario.sh:ro" "$IMAGE" bash -lc '
  set -e
  export PATH="$HOME/.local/bin:$PATH"
  . /scenario.sh
  base="${LLM_PROXY_URL%/}"; case "$base" in */v1) ;; *) base="$base/v1";; esac
  for ag in $SCENARIO_AGENTS; do hermes profile create "$ag" --no-skills 1>&2 || true; done
  for ag in $SCENARIO_AGENTS; do
    D="$HOME/.hermes/profiles/$ag"; mkdir -p "$D/memories" "$D/logs/conversation"
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
    tlog="$D/logs/conversation/transcript.log"; : > "$tlog"; n=0
    scenario_turns "$ag" | while IFS="|" read -r type marker prompt; do
      [ -z "$type" ] && continue
      echo "    · [$ag] turn=$type ($marker)" 1>&2
      n=$((n+1)); cont=""; [ "$n" -gt 1 ] && cont="-c"
      { echo "================ [$ag] turn=$type marker=$marker ================"
        echo ">>> USER: $prompt"; echo "<<< AGENT:"; } >> "$tlog"
      timeout 175 hermes -p "$ag" chat -Q $cont -q "$prompt" 2>&1 | tail -30 >> "$tlog" || echo "(turn error/timeout)" >> "$tlog"
      echo >> "$tlog"
    done
    echo "[$ag] conversation done" 1>&2
  done
'

# ---- host-side analysis: per-profile memories/ + own state.db ----
. "$SCN"
DUMPS="$(mktemp -d)"
REPORT="captured/conversation-report.md"
MODEL=$(grep -E '^BEDROCK_MODEL_ID=' "$ENV_FILE" | cut -d= -f2-)
{ echo "# Hermes — multi-agent memory conversation"; echo
  echo "_Model: ${MODEL:-?}. Each profile is a fully isolated HERMES_HOME (own state.db)._"
  echo "_Detailed transcripts: captured/home/<agent>/logs/conversation/ (gitignored)._"; echo; } > "$REPORT"
for ag in $SCENARIO_AGENTS; do
  D="captured/home/$ag"
  msgs=$(sqlite3 "$D/state.db" "SELECT coalesce(content,'')||' '||coalesce(tool_calls,'') FROM messages;" 2>/dev/null || true)
  { find "$D/memories" -name '*.md' -exec sh -c 'echo "### $1"; cat "$1"' _ {} \; 2>/dev/null; echo "$msgs"; } > "$DUMPS/dump-$ag.txt" || true
  : > "$DUMPS/placement-$ag.txt"
  for m in $(scenario_markers "$ag"); do
    files=$(grep -rl -F "$m" "$D" --include='*.md' --include='*.yaml' --include='*.json' 2>/dev/null | sed 's#captured/home/##' | tr '\n' ' ' || true)
    indb=""; if printf '%s' "$msgs" | grep -qF "$m"; then indb="state.db(messages)"; fi
    echo "$m -> ${files}${indb}" >> "$DUMPS/placement-$ag.txt"
  done
done
bash "$ROOT/scripts/multiagent-verify.sh" "Hermes" "$DUMPS" "$REPORT"
rm -rf "$DUMPS"
echo "== report ==" 1>&2; cat "$REPORT" 1>&2
