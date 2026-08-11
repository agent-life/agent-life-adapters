#!/usr/bin/env bash
# ============================================================================
# Multi-agent memory walkthrough — end-to-end, developer-facing.
# ============================================================================
# Mints a runtime LLM-proxy key, builds each framework image ("spins up the
# machine"), runs the scripted two-agent conversation (episodic / semantic /
# procedural / fake-secret) for one or MORE LLMs, leaves the populated home at
# adapter-<fw>/testkit/captured/home, prints a coverage report + a model x
# framework comparison matrix, then ALWAYS deprovisions the test agent + removes
# the temp key.
#
# Modes:
#   interactive (default on a TTY) — pauses at each step; prompts for the LLM(s)
#   non-interactive (--no-pause)   — runs straight through (auto when not a TTY)
#
# Usage:
#   scripts/multiagent-walkthrough.sh [options]
#     -f, --framework <openclaw|zeroclaw|hermes|all>   (default: all)
#     -m, --model  <alias|id>          run one model (skips the interactive menu)
#         --models <a,b,c>             run several models and compare
#         --list-models                print the model catalog and exit
#     -i, --pause | --interactive      force pauses + model menu
#     -y, --no-pause | --non-interactive | --ci        never pause
#         --keep-agent                 skip cloud deprovision
#         --variant <v>                provision variant (default openclaw)
#     -h, --help
#
# Requires: docker, and the sibling agent-life-service checkout (for the e2e
# mint/scavenge binaries only — all config comes from adapters/.env).
# ============================================================================
set -uo pipefail   # NOT -e: we handle step failures and ALWAYS run cleanup.

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
# SERVICE is the location of the e2e mint/scavenge BINARIES only — its .env is
# never read. All config comes from adapters/.env, loaded just below.
SERVICE="${ALF_SERVICE_REPO:-$ROOT/../agent-life-service}"

# adapters/.env is the ONLY config source. Export the keys the e2e mint/scavenge
# bins read (NEON_DATABASE_URL, S3_BUCKET_NAME, AWS_*, LLM_PROXY_URL, ALF_API_URL)
# so the bins — invoked directly, NOT via the service .env-loading wrappers —
# see this repo's values.
if [ -f "$ROOT/.env" ]; then
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in ''|\#*) continue;; esac
    [[ "$line" =~ ^([A-Za-z_][A-Za-z0-9_]*)=(.*)$ ]] || continue
    key="${BASH_REMATCH[1]}"; value="${BASH_REMATCH[2]}"
    value="${value%\"}"; value="${value#\"}"; value="${value%\'}"; value="${value#\'}"
    export "$key=$value"
  done < "$ROOT/.env"
fi
# The mint bin echoes ALF_API_URL; map it from API_BASE_URL when unset.
[ -z "${ALF_API_URL:-}" ] && [ -n "${API_BASE_URL:-}" ] && export ALF_API_URL="$API_BASE_URL"
FRAMEWORKS_ALL="openclaw zeroclaw hermes"
FRAMEWORKS="$FRAMEWORKS_ALL"
VARIANT="openclaw"
KEEP_AGENT=0
INTERACTIVE=auto
MODEL_ARG=""
DEFAULT_MODEL="us.amazon.nova-lite-v1:0"
TS="$(date -u +%Y%m%dT%H%M%SZ)"
LOG="${TMPDIR:-/tmp}/multiagent-walkthrough-$TS.log"
ENVFILE=""
AGENT_ID=""
declare -a SELECTED=()          # full model ids to run
declare -A MATRIX=()            # "fw|id" -> short verdict (e.g. "8/8" or "6/8!" or "ERR")

# Model catalog served by the LLM proxy (id | alias | label). All verified 200.
MODEL_CATALOG=(
  "us.amazon.nova-lite-v1:0|nova-lite|Amazon Nova Lite"
  "us.amazon.nova-2-lite-v1:0|nova2-lite|Amazon Nova 2 Lite"
  "global.anthropic.claude-haiku-4-5-20251001-v1:0|claude-haiku|Claude 4.5 Haiku"
  "minimax.minimax-m2.5|minimax|MiniMax M2.5"
  "moonshotai.kimi-k2.5|kimi|Kimi K2.5"
  "deepseek.v3.2|deepseek|DeepSeek V3.2"
)
usage(){ sed -n '2,37p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }
list_models(){ printf '  %-2s %-14s %-22s %s\n' '#' alias label id
  local i=1 e id al lb mark; for e in "${MODEL_CATALOG[@]}"; do IFS='|' read -r id al lb <<<"$e"
    mark=""; [ "$id" = "$DEFAULT_MODEL" ] && mark="  ← default"
    printf '  %-2s %-14s %-22s %s%s\n' "$i" "$al" "$lb" "$id" "$mark"; i=$((i+1)); done; }
model_id_for(){ # token (index|alias|id) -> full id (or empty)
  local t="$1" i=1 e id al lb
  case "$t" in a|all|A|ALL) for e in "${MODEL_CATALOG[@]}"; do echo "${e%%|*}"; done; return;; esac
  for e in "${MODEL_CATALOG[@]}"; do IFS='|' read -r id al lb <<<"$e"
    if [ "$t" = "$i" ] || [ "$t" = "$al" ] || [ "$t" = "$id" ]; then echo "$id"; return; fi; i=$((i+1)); done; }
alias_for(){ local id="$1" e i al l; for e in "${MODEL_CATALOG[@]}"; do IFS='|' read -r i al l <<<"$e"
    [ "$i" = "$id" ] && { echo "$al"; return; }; done; echo "${id//[:\/.]/-}"; }

while [ $# -gt 0 ]; do case "$1" in
  -f|--framework) FRAMEWORKS="$2"; [ "$2" = all ] && FRAMEWORKS="$FRAMEWORKS_ALL"; shift 2;;
  -m|--model|--models) MODEL_ARG="$2"; shift 2;;
  --list-models) list_models; exit 0;;
  -i|--pause|--interactive) INTERACTIVE=on; shift;;
  -y|--no-pause|--non-interactive|--ci) INTERACTIVE=off; shift;;
  --keep-agent) KEEP_AGENT=1; shift;;
  --variant) VARIANT="$2"; shift 2;;
  -h|--help) usage 0;;
  *) echo "unknown option: $1" >&2; usage 2;;
esac; done
[ "$INTERACTIVE" = auto ] && { [ -t 0 ] && INTERACTIVE=on || INTERACTIVE=off; }

# ---- pretty output (color to console, de-colored to the log) ----
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  B=$'\033[1m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; C=$'\033[36m'; Z=$'\033[0m'
else B='' G='' Y='' R='' C='' Z=''; fi
STEP=0
say(){ printf '%s\n' "$*"; printf '%s\n' "$*" | sed $'s/\033\\[[0-9;]*m//g' >> "$LOG"; }
step(){ STEP=$((STEP+1)); say ""; say "${B}${C}━━ Step $STEP — $* ━━${Z}"; }
ok(){ say "  ${G}✓${Z} $*"; }; warn(){ say "  ${Y}⚠${Z} $*"; }; err(){ say "  ${R}✗${Z} $*"; }
pause(){ [ "$INTERACTIVE" = on ] || return 0
  printf '%s' "${Y}  ↪ ENTER to continue · q to quit: ${Z}"
  read -r a </dev/tty 2>/dev/null || a=q; [ "$a" = q ] && { warn "aborted by developer"; exit 130; }; }
run(){ say "  ${C}\$ $*${Z}"; "$@" 2>&1 | tee -a "$LOG"; return "${PIPESTATUS[0]}"; }

print_matrix(){ # rows=framework, cols=model alias
  [ "${#MATRIX[@]}" -gt 0 ] || return 0
  local id line sep fw v
  line="$(printf '  %-9s' framework)"; for id in "${SELECTED[@]}"; do line+="$(printf ' | %-13s' "$(alias_for "$id")")"; done; say "$line"
  local sep="  ---------"; for id in "${SELECTED[@]}"; do sep+="-+--------------"; done; say "$sep"
  local fw v; for fw in $FRAMEWORKS; do line="$(printf '  %-9s' "$fw")"
    for id in "${SELECTED[@]}"; do v="${MATRIX[$fw|$id]:-–}"; line+="$(printf ' | %-13s' "$v")"; done; say "$line"; done
  say ""; say "  legend: N/8 memory markers · '!' = isolation leak · ERR = run error"
}

cleanup(){
  trap - EXIT INT TERM
  step "Cleanup"
  if [ "$KEEP_AGENT" = 1 ] && [ -n "$AGENT_ID" ]; then
    warn "--keep-agent: leaving test agent $AGENT_ID"
  elif [ -n "$AGENT_ID" ]; then
    say "  deprovisioning test agent $AGENT_ID …"
    if ( cd "$SERVICE" && cargo run -q -p e2e --bin scavenge_test_runtimes -- --agent "$AGENT_ID" --delete ) >>"$LOG" 2>&1
      then ok "deprovisioned (DB + S3 cascaded)"; else warn "deprovision failed — see $LOG (agent $AGENT_ID)"; fi
  fi
  [ -n "$ENVFILE" ] && rm -f "$ENVFILE" && ok "removed temp credentials"
  say ""; say "${B}Comparison (model × framework):${Z}"; print_matrix
  say ""; say "${B}Populated homes (last model per framework) left for inspection:${Z}"
  for fw in $FRAMEWORKS; do d="adapter-$fw/testkit/captured/home"; [ -d "$ROOT/$d" ] && say "  $d/" || say "  $d/ ${Y}(none)${Z}"; done
  say "${B}Run log:${Z} $LOG"
}
trap cleanup EXIT INT TERM
cd "$ROOT"

# ---- Step: preflight ----
step "Preflight"
command -v docker >/dev/null 2>&1 && ok "docker present" || { err "docker not found"; exit 1; }
[ -f "$SERVICE/tests/e2e/Cargo.toml" ] && ok "service e2e crate (mint/scavenge bins): $SERVICE" \
  || { err "service e2e crate not found (set ALF_SERVICE_REPO)"; exit 1; }
for fw in $FRAMEWORKS; do [ -x "adapter-$fw/testkit/converse.sh" ] || { err "adapter-$fw/testkit/converse.sh missing"; exit 1; }; done
say "  frameworks: $FRAMEWORKS   mode: $INTERACTIVE"
pause

# ---- Step: select LLM model(s) ----
step "Select LLM model(s)"
if [ -n "$MODEL_ARG" ]; then
  IFS=', ' read -ra toks <<<"$MODEL_ARG"
  for t in "${toks[@]}"; do [ -z "$t" ] && continue
    while read -r id; do [ -n "$id" ] && SELECTED+=("$id"); done < <(model_id_for "$t")
    [ -z "$(model_id_for "$t")" ] && warn "unknown model token: $t"; done
elif [ "$INTERACTIVE" = on ]; then
  say "  Available models:"; list_models | tee -a "$LOG"
  printf '%s' "${Y}  Select model(s) — numbers/aliases (e.g. '3' or '3 4' or 'minimax,claude-haiku'), 'a'=all, ENTER=$(alias_for "$DEFAULT_MODEL") (default): ${Z}"
  read -r line </dev/tty 2>/dev/null || line=""
  if [ -z "$line" ]; then SELECTED=("$DEFAULT_MODEL")
  else IFS=', ' read -ra toks <<<"$line"
    for t in "${toks[@]}"; do [ -z "$t" ] && continue
      while read -r id; do [ -n "$id" ] && SELECTED+=("$id"); done < <(model_id_for "$t")
      [ -z "$(model_id_for "$t")" ] && warn "unknown: $t"; done; fi
else
  SELECTED=("$DEFAULT_MODEL")
fi
# dedupe preserving order
declare -A seen=(); uniq=(); for id in "${SELECTED[@]}"; do [ -n "${seen[$id]:-}" ] || { uniq+=("$id"); seen[$id]=1; }; done; SELECTED=("${uniq[@]}")
[ "${#SELECTED[@]}" -eq 0 ] && SELECTED=("$DEFAULT_MODEL")
sel_disp=""; for id in "${SELECTED[@]}"; do sel_disp="$sel_disp $(alias_for "$id")"; done
ok "will run ${#SELECTED[@]} model(s):${sel_disp}"
pause

# ---- Step: mint credentials ----
step "Mint runtime credentials (only runtime keys can reach the LLM proxy)"
ENVFILE="$(mktemp)"; chmod 600 "$ENVFILE"; provout="$(mktemp)"
if ! ( cd "$SERVICE" && cargo run -q -p e2e --bin provision_test_runtime -- --variant "$VARIANT" ) >"$provout" 2>&1; then
  err "mint failed:"; tail -n 20 "$provout" | tee -a "$LOG"; rm -f "$provout"; exit 1; fi
AGENT_ID="$(python3 - "$provout" "$ENVFILE" <<'PY'
import re,sys,os
t=open(sys.argv[1]).read()
def g(k):
    m=re.search(rf'{k}\s*=\s*"?([^"\n]+)"?',t); return m.group(1).strip().strip('"') if m else ''
open(sys.argv[2],'w').write(f"RUNTIME_API_KEY={g('runtime_api_key')}\nLLM_PROXY_URL={g('llm_proxy_url')}\nBEDROCK_MODEL_ID={g('llm_model_id')}\n")
os.chmod(sys.argv[2],0o600); print(g('agent_id'))
PY
)"
rm -f "$provout"; . "$ENVFILE"
[ -n "${RUNTIME_API_KEY:-}" ] && [ -n "${LLM_PROXY_URL:-}" ] && [ -n "$AGENT_ID" ] \
  && ok "minted runtime key (len ${#RUNTIME_API_KEY}) · agent $AGENT_ID" || { err "could not parse provisioner output"; exit 1; }
pause

# ---- Step(s): per model → per framework: converse + report ----
MULTI=0; [ "${#SELECTED[@]}" -gt 1 ] && MULTI=1
for id in "${SELECTED[@]}"; do
  al="$(alias_for "$id")"
  # point the shared env file at this model (converse.sh reads BEDROCK_MODEL_ID)
  { grep -v '^BEDROCK_MODEL_ID=' "$ENVFILE"; echo "BEDROCK_MODEL_ID=$id"; } > "$ENVFILE.tmp" && mv "$ENVFILE.tmp" "$ENVFILE"; chmod 600 "$ENVFILE"
  step "Model [$al] ($id): engage agents across frameworks"
  for fw in $FRAMEWORKS; do
    say "  ${B}▸ [$al/$fw]${Z} converse…"
    if run bash "adapter-$fw/testkit/converse.sh" "$ENVFILE"; then :; else warn "[$al/$fw] converse.sh non-zero"; fi
    rpt="adapter-$fw/testkit/captured/conversation-report.md"
    if [ -f "$rpt" ]; then
      verdict="$(grep -oE 'VERDICT coverage=[0-9]+/8 isolation=[a-z]+' "$rpt" | head -1)"
      cov="$(printf '%s' "$verdict" | grep -oE 'coverage=[0-9]+/8' | cut -d= -f2)"
      iso="$(printf '%s' "$verdict" | grep -oE 'isolation=[a-z]+' | cut -d= -f2)"
      short="${cov:-?}"; [ "$iso" = leak ] && short="${short}!"
      MATRIX[$fw|$id]="$short"
      say "    → [$al/$fw]: coverage ${cov:-?} · isolation ${iso:-?}"
      # preserve per-model report when comparing >1 model
      [ "$MULTI" = 1 ] && { mkdir -p "adapter-$fw/testkit/captured/by-model"; cp "$rpt" "adapter-$fw/testkit/captured/by-model/$al.md"; }
    else MATRIX[$fw|$id]="ERR"; warn "[$al/$fw] no report"; fi
  done
  pause
done

# ---- Step: comparison ----
step "Comparison matrix"
print_matrix
ok "walkthrough complete"
