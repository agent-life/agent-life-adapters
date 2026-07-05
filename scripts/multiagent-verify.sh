#!/usr/bin/env bash
# Shared verifier for the multi-agent memory harness.
# Usage: multiagent-verify.sh <framework> <dumps_dir> <out_report>
#   <dumps_dir> must contain dump-<agent>.txt (all memory-bearing text for that
#   agent) and optionally placement-<agent>.txt (grep -rln of markers -> files).
# Appends a markdown section to <out_report>. Deterministic: matches on the
# unique markers from multiagent-scenario.sh, so model phrasing is irrelevant.
set -euo pipefail
FW="$1"; DUMPS="$2"; OUT="$3"
HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/multiagent-scenario.sh"

present() { [ -f "$1" ] && grep -Fq "$2" "$1" && echo "✓" || echo "·"; }

{
  echo "## $FW"
  echo
  echo "Memory-type coverage (✓ = the turn's unique marker is present in that agent's stored memory):"
  echo
  echo "| agent | semantic | episodic | procedural | secret |"
  echo "|-------|:--------:|:--------:|:----------:|:------:|"
  total=0; leak=0
  for ag in $SCENARIO_AGENTS; do
    dump="$DUMPS/dump-$ag.txt"
    declare -A got=(); got=()
    while IFS='|' read -r type marker _; do
      [ -z "${type:-}" ] && continue
      got[$type]="$(present "$dump" "$marker")"
      [ "${got[$type]}" = "✓" ] && total=$((total+1))
    done <<EOF
$(scenario_turns "$ag")
EOF
    printf '| %s (%s) | %s | %s | %s | %s |\n' "$ag" "$(scenario_persona "$ag")" \
      "${got[semantic]:-·}" "${got[episodic]:-·}" "${got[procedural]:-·}" "${got[secret]:-·}"
  done
  echo
  echo "Isolation (the *other* agent's markers must NOT appear in this agent's memory):"
  echo
  for ag in $SCENARIO_AGENTS; do
    dump="$DUMPS/dump-$ag.txt"; leaks=""
    for other in $SCENARIO_AGENTS; do
      [ "$other" = "$ag" ] && continue
      for m in $(scenario_markers "$other"); do
        [ -f "$dump" ] && grep -Fq "$m" "$dump" && leaks="$leaks $m"
      done
    done
    if [ -n "$leaks" ]; then echo "- **$ag: LEAK** ->$leaks"; leak=1; else echo "- $ag: clean (no foreign markers)"; fi
  done
  echo
  iso="clean"; [ "$leak" = 1 ] && iso="leak"
  echo "**Verdict:** coverage ${total}/8 memory markers · isolation ${iso}"
  echo
  echo "<!-- VERDICT coverage=${total}/8 isolation=${iso} -->"
  echo
  for ag in $SCENARIO_AGENTS; do
    if [ -f "$DUMPS/placement-$ag.txt" ]; then
      echo "Where $ag's markers landed:"
      echo '```'; cat "$DUMPS/placement-$ag.txt"; echo '```'
    fi
  done
  echo
} >> "$OUT"
