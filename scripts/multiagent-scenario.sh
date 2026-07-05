#!/bin/sh
# Shared scenario for the multi-agent memory integration harness.
#
# Two agents, each driven through a 4-turn scripted conversation that should
# produce one SEMANTIC, one EPISODIC, and one PROCEDURAL memory, plus store one
# fake per-agent SECRET. Every item embeds a UNIQUE MARKER so verification is
# deterministic regardless of how the model phrases/places the memory:
#   - the agent's own markers MUST appear in its stored memory  (coverage)
#   - the OTHER agent's markers MUST NOT appear                  (isolation)
#
# All secrets here are OBVIOUSLY FAKE throwaway test values (…-FAKE-…). They are
# committed on purpose: an integration test needs known values to assert against.
# The REAL runtime LLM-proxy key is never stored here.
#
# Sourced by each testkit's converse.sh. POSIX sh.

SCENARIO_AGENTS="agent_a agent_b"

# Per-turn parallel arrays are expressed as newline records "TYPE|MARKER|PROMPT"
# read by scenario_turns().  TYPE in {semantic,episodic,procedural,secret}.

scenario_persona() {            # $1=agent -> echoes display persona name
  case "$1" in
    agent_a) echo "Atlas" ;;
    agent_b) echo "Nova" ;;
  esac
}

scenario_markers() {            # $1=agent -> echoes its 4 markers (space sep)
  case "$1" in
    agent_a) echo "CAPETOWN-ATLAS-SEM7F3A ATLAS-EPI-2B9C ATLAS-PROC-5D1E sk-atlas-FAKE-1a2b3c" ;;
    agent_b) echo "BERLIN-NOVA-SEM8C2B NOVA-EPI-3D7F NOVA-PROC-6A4B nova-FAKE-9z8y7x" ;;
  esac
}

# scenario_turns AGENT -> emits one "TYPE|MARKER|PROMPT" record per line.
scenario_turns() {
  case "$1" in
    agent_a)
      cat <<'EOF'
semantic|CAPETOWN-ATLAS-SEM7F3A|Permanently remember these durable facts in your long-term (semantic) memory. You are Atlas, an infrastructure assistant. Your primary user is Johan, based in Cape Town, who prefers terse technical answers. Record the verbatim reference code CAPETOWN-ATLAS-SEM7F3A with these facts so they can be looked up later.
episodic|ATLAS-EPI-2B9C|Record an episodic log entry for TODAY: at 14:00 you completed the Q2 Postgres-to-Neon database migration and rotated the TLS certificates. Save it as a dated/episodic memory and include the verbatim event id ATLAS-EPI-2B9C.
procedural|ATLAS-PROC-5D1E|Save the following as a reusable, named PROCEDURE called "deploy": 1) run the full test suite, 2) tag the release, 3) push to main, 4) watch the canary for 10 minutes. Store it as procedural memory and include the verbatim procedure id ATLAS-PROC-5D1E.
secret|sk-atlas-FAKE-1a2b3c|Keep this credential for me — it is a throwaway TEST value, safe to store: STAGING_API_TOKEN = sk-atlas-FAKE-1a2b3c-DO-NOT-USE. Store it wherever you keep secrets/credentials so you can recall it later.
EOF
      ;;
    agent_b)
      cat <<'EOF'
semantic|BERLIN-NOVA-SEM8C2B|Permanently remember these durable facts in your long-term (semantic) memory. You are Nova, a research assistant. Your primary user is Mara, based in Berlin, who prefers concise bullet-point answers. Record the verbatim reference code BERLIN-NOVA-SEM8C2B with these facts so they can be looked up later.
episodic|NOVA-EPI-3D7F|Record an episodic log entry for TODAY: you reviewed 12 papers on retrieval-augmented generation and shortlisted 3 for the reading group. Save it as a dated/episodic memory and include the verbatim event id NOVA-EPI-3D7F.
procedural|NOVA-PROC-6A4B|Save the following as a reusable, named PROCEDURE called "weekly-review": 1) gather the week's notes, 2) cluster them by theme, 3) draft a summary, 4) send it every Friday. Store it as procedural memory and include the verbatim procedure id NOVA-PROC-6A4B.
secret|nova-FAKE-9z8y7x|Keep this credential for me — it is a throwaway TEST value, safe to store: RESEARCH_DB_PASSWORD = nova-FAKE-9z8y7x-DO-NOT-USE. Store it wherever you keep secrets/credentials so you can recall it later.
EOF
      ;;
  esac
}
